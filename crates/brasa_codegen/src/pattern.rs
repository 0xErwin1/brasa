//! `match` lowering and pattern tests over the spec's decision-tree
//! primitives.
//!
//! Strategy (recorded in the crate docs): straightforward left-to-right
//! arm testing. The scrutinee is evaluated once and kept on the stack;
//! each arm `dup`s it, runs its pattern test against the copy, then its
//! guard; a selected arm pops the original and yields its body value.
//! Guards that fail fall through to the next arm; when no arm matches
//! the compiled code raises `panics.AssertionFailed` with the walker's
//! exact detail.
//!
//! Pattern-test invariant: the tested value is on top of the stack on
//! entry and is fully consumed on both outcomes — fall through with
//! bindings stored on a match, or jump to a collected fail site with
//! the value and every projection popped. Sub-tests that fail while a
//! projection source is still on the stack route through a local
//! cleanup (`pop` + jump) so the invariant holds recursively.

use brasa_bytecode::{CodeIx, Constant, Op};
use brasa_hir::{ArmBody, ExprId, Literal, MatchArm, Pattern, PatternId};
use brasa_resolver::CtorRes;
use brasa_source::Span;

use crate::expr::compile_expr;
use crate::func::{FuncCx, PLACEHOLDER};
use crate::stmt::block_value;

pub(crate) fn compile_match(f: &mut FuncCx, scrutinee: ExprId, arms: &[MatchArm], span: Span) {
    compile_expr(f, scrutinee);

    let mut end_jumps = Vec::new();
    let mut next_arm_jumps: Vec<CodeIx> = Vec::new();

    for arm in arms {
        let arm_start = f.here();
        for jump in next_arm_jumps.drain(..) {
            f.patch(jump, arm_start);
        }

        // Test a copy; the original survives for the next arm.
        f.emit(Op::Dup, span);
        compile_pattern_test(f, arm.pattern, &mut next_arm_jumps);

        if let Some(guard) = arm.guard {
            compile_expr(f, guard);
            next_arm_jumps.push(f.emit(Op::JumpIfFalse(PLACEHOLDER), span));
        }

        f.emit(Op::Pop, span);
        compile_arm_body(f, &arm.body, span);
        end_jumps.push(f.emit(Op::Jump(PLACEHOLDER), span));
    }

    // Guards can leave a value unmatched at runtime even though the
    // checker proved shape exhaustiveness.
    let fall_through = f.here();
    for jump in next_arm_jumps {
        f.patch(jump, fall_through);
    }
    f.emit(Op::Pop, span);
    f.emit_assert_failed("no match arm matched the value", span);

    let end = f.here();
    for jump in end_jumps {
        f.patch(jump, end);
    }
}

pub(crate) fn compile_arm_body(f: &mut FuncCx, body: &ArmBody, span: Span) {
    match body {
        ArmBody::Expr(expr) => compile_expr(f, *expr),
        ArmBody::Block(block) => block_value(f, block, span),
    }
}

/// Binds one `for` element (on top of the stack) to the loop pattern.
/// A mismatch raises `panics.AssertionFailed`.
pub(crate) fn compile_for_binding(f: &mut FuncCx, pattern: PatternId, span: Span) {
    let mut fails = Vec::new();
    compile_pattern_test(f, pattern, &mut fails);

    if fails.is_empty() {
        return;
    }

    let matched = f.emit(Op::Jump(PLACEHOLDER), span);
    let fail = f.here();
    for jump in fails {
        f.patch(jump, fail);
    }
    f.emit_assert_failed("`for` pattern did not match the element", span);
    f.emit(Op::Pop, span);
    let done = f.here();
    f.patch(matched, done);
}

/// See the module docs for the pattern-test invariant.
fn compile_pattern_test(f: &mut FuncCx, pattern: PatternId, fails: &mut Vec<CodeIx>) {
    let span = f.cx.hir.span_of_pattern(pattern);

    match f.cx.hir.pattern(pattern).clone() {
        Pattern::Wildcard => {
            f.emit(Op::Pop, span);
        }
        Pattern::Binding(_) => match f.cx.res.pattern_locals.get(&pattern).copied() {
            Some(local) => f.bind_local(local, span),
            None => {
                f.emit(Op::Pop, span);
            }
        },
        Pattern::Literal(literal) => {
            match literal {
                Literal::Int(v) => {
                    f.emit_const(Constant::Int(v), span);
                }
                Literal::Float(v) => {
                    f.emit_const(Constant::Float(v), span);
                }
                Literal::Bool(v) => {
                    f.emit(if v { Op::LoadTrue } else { Op::LoadFalse }, span);
                }
                Literal::Char(v) => {
                    f.emit_const(Constant::Char(v), span);
                }
                Literal::Str(v) => {
                    f.emit_const(Constant::Str(v), span);
                }
            }
            f.emit(Op::Eq, span);
            fails.push(f.emit(Op::JumpIfFalse(PLACEHOLDER), span));
        }
        Pattern::Tuple(elements) => {
            compound_test(f, &elements, Op::TupleField, Vec::new(), fails, span);
        }
        Pattern::Ctor { args, .. } => match f.cx.res.ctor_pattern_res.get(&pattern).copied() {
            Some(CtorRes::OptionSome) => {
                // Peek: on `None` the option is still on the stack, so
                // the fail path routes through a cleanup pop.
                let none = f.emit(Op::JumpIfNone(PLACEHOLDER), span);
                f.emit(Op::UnwrapSome, span);
                match args.as_slice() {
                    [payload] => compile_pattern_test(f, *payload, fails),
                    [] => {
                        f.emit(Op::Pop, span);
                    }
                    _ => {
                        f.emit(Op::Pop, span);
                        fails.push(f.emit(Op::Jump(PLACEHOLDER), span));
                    }
                }
                cleanup(f, fails, vec![none], span);
            }
            Some(CtorRes::OptionNone) => {
                let is_none = f.emit(Op::JumpIfNone(PLACEHOLDER), span);
                // Fall through means `Some`: no match.
                f.emit(Op::Pop, span);
                fails.push(f.emit(Op::Jump(PLACEHOLDER), span));
                let matched = f.here();
                f.patch(is_none, matched);
                f.emit(Op::Pop, span);
            }
            Some(CtorRes::EnumVariant { variant_index, .. }) => {
                // Reported by `Cx::collect` before any body is lowered.
                let variant = u16::try_from(variant_index).unwrap_or(u16::MAX);
                let wrong_variant = f.emit(
                    Op::JumpIfVariantNe {
                        variant,
                        target: PLACEHOLDER,
                    },
                    span,
                );
                compound_test(f, &args, Op::EnumField, vec![wrong_variant], fails, span);
            }
            // `Set` never resolves in pattern position; an unresolved
            // constructor pattern is a fatal.
            None | Some(CtorRes::SetCtor) => {
                f.emit(Op::Pop, span);
                f.emit_fatal("brasa: unresolved constructor pattern", span);
                f.emit(Op::Pop, span);
            }
        },
    }
}

/// Tests the elements of a compound value (tuple or enum payload) whose
/// container is on top of the stack, consuming it. `dirty` carries jump
/// sites that fail while the container is still on the stack (the
/// variant test, and every non-last element's sub-test).
fn compound_test(
    f: &mut FuncCx,
    elements: &[PatternId],
    project: fn(u16) -> Op,
    mut dirty: Vec<CodeIx>,
    fails: &mut Vec<CodeIx>,
    span: Span,
) {
    match elements.split_last() {
        None => {
            f.emit(Op::Pop, span);
        }
        Some((&last, init)) => {
            for (index, &element) in init.iter().enumerate() {
                let index = u16::try_from(index).unwrap_or(u16::MAX);
                f.emit(Op::Dup, span);
                f.emit(project(index), span);
                compile_pattern_test(f, element, &mut dirty);
            }

            // The last projection consumes the container, so its
            // sub-test fails are already clean.
            let index = u16::try_from(init.len()).unwrap_or(u16::MAX);
            f.emit(project(index), span);
            compile_pattern_test(f, last, fails);
        }
    }

    cleanup(f, fails, dirty, span);
}

/// Routes fail jumps that still have one extra value on the stack
/// through a `pop`, restoring the pattern-test invariant.
fn cleanup(f: &mut FuncCx, fails: &mut Vec<CodeIx>, dirty: Vec<CodeIx>, span: Span) {
    if dirty.is_empty() {
        return;
    }

    let matched = f.emit(Op::Jump(PLACEHOLDER), span);
    let pop_site = f.here();
    for jump in dirty {
        f.patch(jump, pop_site);
    }
    f.emit(Op::Pop, span);
    fails.push(f.emit(Op::Jump(PLACEHOLDER), span));

    let cont = f.here();
    f.patch(matched, cont);
}
