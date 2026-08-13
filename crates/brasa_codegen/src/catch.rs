//! `catch`/`catch!` lowering to static handler tables plus a
//! dispatch sequence (`docs/spec/07-bytecode.md`, throw/catch).
//!
//! The handler entry covers the compiled subject only; the dispatch
//! sequence tests the caught-signal value arm by arm: `jump_if_panic`
//! for `_` (an error, never a panic), `jump_if_tag_ne` for named arms,
//! `caught_value`/`caught_detail` for the binding (the error value for
//! user arms and `_`, the detail/message string for dotted panic and
//! native-error names — a compile-time choice, decided by what the arm
//! RESOLVED to rather than by whether it was spelled with a dot, since
//! `lib.Boom` is a user type written with one), guards after the
//! binding store, and a `rethrow` tail for whatever no arm handles.

use brasa_bytecode::{CodeIx, Handler, Op};
use brasa_hir::{CatchArm, CatchType, ExprId, Item, ItemId};
use brasa_resolver::TypeRes;
use brasa_source::Span;

use crate::expr::compile_expr;
use crate::func::{FuncCx, PLACEHOLDER};
use crate::pattern::compile_arm_body;

/// The runtime nominal tag of a type item — the name a thrown value
/// carries. Only structs and enums are nominal; an interface names no
/// value, so it has no tag.
fn nominal_name(f: &FuncCx, item: ItemId) -> Option<String> {
    match f.cx.hir.item(item) {
        Item::StructDef(def) => Some(def.name.clone()),
        Item::EnumDef(def) => Some(def.name.clone()),
        _ => None,
    }
}

pub(crate) fn compile_catch(
    f: &mut FuncCx,
    id: ExprId,
    subject: ExprId,
    arms: &[CatchArm],
    span: Span,
) {
    let start = f.here();
    compile_expr(f, subject);
    let end = f.here();

    let mut end_jumps = vec![f.emit(Op::Jump(PLACEHOLDER), span)];

    // Register after the subject: inner `catch` subjects already pushed
    // their entries, keeping the table innermost-first. The depth pass
    // fills in the real operand depth.
    let target = f.here();
    f.handlers.push(Handler {
        start,
        end,
        target,
        depth: 0,
    });

    let binding = f.cx.res.catch_bindings.get(&id).copied();
    let mut next_arm_jumps: Vec<CodeIx> = Vec::new();

    for (arm_index, arm) in arms.iter().enumerate() {
        let arm_start = f.here();
        for jump in next_arm_jumps.drain(..) {
            f.patch(jump, arm_start);
        }

        let mut matched_jumps = Vec::new();
        let mut fail_prev: Option<CodeIx> = None;
        let last_type = arm.types.len().saturating_sub(1);

        for (type_index, catch_type) in arm.types.iter().enumerate() {
            if let Some(jump) = fail_prev.take() {
                let here = f.here();
                f.patch(jump, here);
            }

            let (fail, bind_op) = match catch_type {
                // `_` catches any error, never a panic.
                CatchType::Wildcard { .. } => {
                    (f.emit(Op::JumpIfPanic(PLACEHOLDER), span), Op::CaughtValue)
                }
                CatchType::Named { name, .. } => {
                    let key = (id, arm_index, type_index);

                    // A user error type is matched by its NOMINAL tag,
                    // which is the declared name — not the path the arm
                    // was written with. The two differ once an arm names
                    // a type from another module (`lib.Boom` matches a
                    // value tagged `Boom`), so the resolution decides,
                    // and only a name that resolved to no type falls
                    // back to the written spelling.
                    let resolved = match f.cx.res.catch_arm_types.get(&key) {
                        Some(TypeRes::Item(item)) => nominal_name(f, *item),
                        _ => None,
                    };

                    let dotted = resolved.is_none()
                        && (f.cx.res.catch_arm_panics.contains_key(&key)
                            || f.cx.res.catch_arm_native_errors.contains_key(&key)
                            || name.contains('.'));

                    let tag = match &resolved {
                        Some(nominal) => f.cx.const_str(nominal),
                        None => f.cx.const_str(name),
                    };
                    let fail = f.emit(
                        Op::JumpIfTagNe {
                            tag,
                            target: PLACEHOLDER,
                        },
                        span,
                    );
                    let bind_op = if dotted {
                        Op::CaughtDetail
                    } else {
                        Op::CaughtValue
                    };
                    (fail, bind_op)
                }
            };

            emit_binding(f, bind_op, binding, span);
            if type_index < last_type {
                matched_jumps.push(f.emit(Op::Jump(PLACEHOLDER), span));
            }
            fail_prev = Some(fail);
        }

        if let Some(jump) = fail_prev {
            next_arm_jumps.push(jump);
        }
        let guard_start = f.here();
        for jump in matched_jumps {
            f.patch(jump, guard_start);
        }

        // A false guard falls through to the next arm's tests; the
        // caught signal survives as an ordinary stack value.
        if let Some(guard) = arm.guard {
            compile_expr(f, guard);
            next_arm_jumps.push(f.emit(Op::JumpIfFalse(PLACEHOLDER), span));
        }

        f.emit(Op::Pop, span);
        compile_arm_body(f, &arm.body, span);
        end_jumps.push(f.emit(Op::Jump(PLACEHOLDER), span));
    }

    // Non-exhaustive `catch` propagates what it does not handle.
    let rethrow = f.here();
    for jump in next_arm_jumps {
        f.patch(jump, rethrow);
    }
    f.emit(Op::Rethrow, span);

    let done = f.here();
    for jump in end_jumps {
        f.patch(jump, done);
    }
}

fn emit_binding(f: &mut FuncCx, bind_op: Op, binding: Option<brasa_resolver::LocalId>, span: Span) {
    f.emit(bind_op, span);
    match binding {
        Some(local) => {
            let slot = f.slot_of(local);
            f.emit(Op::StoreLocal(slot), span);
        }
        None => {
            f.emit(Op::Pop, span);
        }
    }
}
