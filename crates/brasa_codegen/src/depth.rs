//! The operand-depth pass: an abstract interpretation over the finished
//! chunk that assigns every reachable instruction its entry depth.
//!
//! Two outputs (`docs/spec/07-bytecode.md`): each handler entry's
//! `depth` (the operand depth to restore before its dispatch sequence —
//! the depth at the subject's first instruction) and the function's
//! `max_stack` (the maximum operand depth above the locals boundary,
//! recorded so the VM can reserve stack space on frame entry).
//!
//! Structured code generation reaches every join point at one
//! deterministic depth, so a plain worklist suffices; a revisit at a
//! different depth is a code-generator bug and asserts in debug builds.
//! Dispatch sequences are reachable only through their handler entry:
//! they are seeded at `handler.depth + 1` (the caught signal is pushed)
//! once the subject depth is known. Dead code (after a terminal
//! `return`, for example) stays unvisited and contributes nothing.

use brasa_bytecode::{CodeIx, Handler, Op, StructShape};

/// Fixes every handler's `depth` and returns the chunk's `max_stack`.
pub(crate) fn finalize(code: &[Op], handlers: &mut [Handler], structs: &[StructShape]) -> u16 {
    let mut depths: Vec<Option<u16>> = vec![None; code.len()];
    let mut max = 0u16;
    let mut work: Vec<(usize, u16)> = vec![(0, 0)];
    let mut seeded = vec![false; handlers.len()];

    loop {
        while let Some((ix, depth)) = work.pop() {
            if ix >= code.len() {
                debug_assert!(false, "jump past the end of the chunk");
                continue;
            }
            if let Some(existing) = depths[ix] {
                debug_assert_eq!(existing, depth, "inconsistent depth at instruction {ix}");
                continue;
            }

            depths[ix] = Some(depth);
            max = max.max(depth);

            let (fall, branch) = transition(&code[ix], structs);
            if let Some((target, delta)) = branch {
                let next = apply(depth, delta);
                max = max.max(next);
                work.push((target.0 as usize, next));
            }
            if let Some(delta) = fall {
                let next = apply(depth, delta);
                max = max.max(next);
                work.push((ix + 1, next));
            }
        }

        let mut progressed = false;
        for (h_ix, handler) in handlers.iter_mut().enumerate() {
            if seeded[h_ix] {
                continue;
            }
            if let Some(depth) = depths[handler.start.0 as usize] {
                handler.depth = depth;
                seeded[h_ix] = true;

                let dispatch = depth + 1;
                max = max.max(dispatch);
                work.push((handler.target.0 as usize, dispatch));
                progressed = true;
            }
        }

        if !progressed && work.is_empty() {
            break;
        }
    }

    max
}

fn apply(depth: u16, delta: i32) -> u16 {
    u16::try_from(i32::from(depth) + delta).expect("operand depth underflow")
}

/// The successors of one instruction as depth deltas: the fall-through
/// delta (if it falls through) and the branch target with its delta (if
/// it branches). Terminal instructions return neither.
fn transition(op: &Op, structs: &[StructShape]) -> (Option<i32>, Option<(CodeIx, i32)>) {
    match *op {
        Op::Jump(target) => (None, Some((target, 0))),
        Op::JumpIfFalse(target) => (Some(-1), Some((target, -1))),
        // `&&`/`||`: the branch keeps the value, the fall-through pops.
        Op::JumpIfFalseOrPop(target) | Op::JumpIfTrueOrPop(target) => (Some(-1), Some((target, 0))),
        // Peeking tests leave the value in place on both paths.
        Op::JumpIfVariantNe { target, .. }
        | Op::JumpIfNone(target)
        | Op::JumpIfPanic(target)
        | Op::JumpIfTagNe { target, .. } => (Some(0), Some((target, 0))),
        // `it -> it v` on fall-through; iterator popped on exhaustion.
        Op::IterNext(target) => (Some(1), Some((target, -1))),
        Op::Ret | Op::Throw | Op::Rethrow => (None, None),
        _ => (Some(net_effect(op, structs)), None),
    }
}

/// Net stack effect of a non-branching, non-terminal instruction.
fn net_effect(op: &Op, structs: &[StructShape]) -> i32 {
    match *op {
        Op::Const(_)
        | Op::LoadUnit
        | Op::LoadTrue
        | Op::LoadFalse
        | Op::LoadNone
        | Op::Dup
        | Op::LoadLocal(_)
        | Op::LoadGlobal(_)
        | Op::LoadFunc(_)
        | Op::CaughtValue
        | Op::CaughtDetail => 1,

        Op::Pop | Op::StoreLocal(_) | Op::StoreGlobal(_) => -1,

        Op::AddInt
        | Op::SubInt
        | Op::MulInt
        | Op::DivInt
        | Op::RemInt
        | Op::PowInt
        | Op::AddFloat
        | Op::SubFloat
        | Op::MulFloat
        | Op::DivFloat
        | Op::RemFloat
        | Op::PowFloat
        | Op::Concat
        | Op::Eq
        | Op::Lt
        | Op::Le
        | Op::Gt
        | Op::Ge
        | Op::GetIndex
        | Op::MakeRange { .. } => -1,

        Op::NegInt
        | Op::NegFloat
        | Op::Not
        | Op::WrapSome
        | Op::WrapSomeDynamic
        | Op::UnwrapSome
        | Op::TupleField(_)
        | Op::EnumField(_)
        | Op::GetField(_)
        | Op::BindMethod(_)
        | Op::BindBuiltin(_)
        | Op::ToString
        | Op::IterNew
        | Op::MakeSetFromVector => 0,

        Op::SetField(_) => -2,
        Op::SetIndex => -3,

        Op::Call { argc, .. } => 1 - i32::from(argc),
        Op::CallValue { argc } => -i32::from(argc),
        Op::CallBuiltin { argc, .. } => 1 - i32::from(argc),

        Op::MakeVector(n) | Op::MakeTuple(n) => 1 - i32::from(n),
        Op::MakeMap(n) => 1 - 2 * i32::from(n),
        Op::MakeStruct(s) => {
            let fields = structs[s.0 as usize].fields.len();
            1 - i32::try_from(fields).expect("field count overflow")
        }
        Op::MakeEnum { argc, .. } => 1 - i32::from(argc),
        Op::MakeClosure { captures, .. } => 1 - i32::from(captures),

        Op::Jump(_)
        | Op::JumpIfFalse(_)
        | Op::JumpIfFalseOrPop(_)
        | Op::JumpIfTrueOrPop(_)
        | Op::JumpIfVariantNe { .. }
        | Op::JumpIfNone(_)
        | Op::JumpIfPanic(_)
        | Op::JumpIfTagNe { .. }
        | Op::IterNext(_)
        | Op::Ret
        | Op::Throw
        | Op::Rethrow => unreachable!("branching and terminal ops are handled by `transition`"),
    }
}
