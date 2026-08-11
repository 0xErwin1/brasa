//! The BRS-23 checks that consume the inferred sets: unreachable
//! `catch` arms (E001), `catch_all` exhaustiveness (E002/E003), and
//! `throws` contract verification (E004/E005). Wording and kind
//! boundaries follow `docs/spec/06-diagnostics.md`.
//!
//! Precision rules on open sets (`docs/spec/04-errors.md`): the tags of
//! an open set are a sound lower bound, so openness never suppresses a
//! "this CAN be thrown" finding, but every "this CANNOT be thrown"
//! claim — unreachability, exhaustiveness, `throws never` emptiness —
//! is skipped or reported as unverifiable instead.
//!
//! Decisions recorded here:
//!
//! - A `_` arm is flagged unreachable only inside `catch_all`, and only
//!   when the closed subject set minus the unguarded named arms is
//!   empty (`docs/spec/04-errors.md` forbids unreachable arms in
//!   `catch_all`). In a plain `catch`, a defensive `_` is never
//!   flagged: non-exhaustive handling is the default there.
//! - An open subject under `catch_all` is E003, erring on the side of
//!   soundness: an incomplete list cannot prove exhaustiveness.
//! - `throws` over-declaration (declaring a type the body never
//!   throws) gets no diagnostic: the spec is silent, and a widened
//!   contract is harmless.
//! - With an open actual set, a declared `throws` list still checks
//!   the tags that WERE found (E004) but tolerates the openness: the
//!   declaration is the contract, and unlike `catch_all` there is no
//!   exhaustiveness claim to prove. This is deliberately asymmetric
//!   with E003.
//! - A declared `throws` name resolving to something that is not a
//!   throwable nominal or primitive (an interface, a generic
//!   parameter) maps to no tag and is skipped, like the equivalent
//!   `catch` arm subtraction.
//! - Interface-method `throws` contracts are NOT checked: verifying
//!   them needs interface-satisfaction integration (which struct
//!   satisfies which interface), deferred to a follow-up unit.
//! - E004/E005 point at the declaring item's span; `FuncDef` carries
//!   no dedicated name span yet, so sharpening to the name is a future
//!   follow-up (`docs/spec/06-diagnostics.md`, span rules).

use std::collections::{BTreeSet, HashMap};

use brasa_diagnostics::{Diagnostic, Severity, codes};
use brasa_hir::{CatchArm, CatchType, ExprId, FuncDef, Hir, Item, Throws};
use brasa_resolver::{DefRef, Resolutions};
use brasa_source::Span;

use crate::collect::caught_tag;
use crate::dump::{def_ref_name, tag_name};
use crate::{ErrorSet, ErrorTag};

fn err(code: &'static str, span: Span, message: String, label: &str) -> Diagnostic {
    Diagnostic::new(Severity::Error, message, code.to_string(), span)
        .with_label(span, label.to_string())
}

/// Renders a tag set for a message: `` `X` ``, `` `X` and `Y` ``,
/// `` `X`, `Y` and `Z` ``.
fn tag_list(hir: &Hir, tags: &BTreeSet<ErrorTag>) -> String {
    let names: Vec<String> = tags
        .iter()
        .map(|tag| format!("`{}`", tag_name(hir, tag)))
        .collect();

    match names.as_slice() {
        [] => String::new(),
        [only] => only.clone(),
        [init @ .., last] => format!("{} and {last}", init.join(", ")),
    }
}

/// Checks one `catch`/`catch_all` expression against its subject's
/// contribution set (computed before arm subtraction): E001 for
/// unreachable arms, E002/E003 for `catch_all` exhaustiveness.
pub(crate) fn catch_expr(
    hir: &Hir,
    res: &Resolutions,
    id: ExprId,
    exhaustive: bool,
    arms: &[CatchArm],
    subject: &ErrorSet,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let span = hir.span_of_expr(id);

    // E001, named arms: a type a CLOSED subject set cannot throw is
    // unreachable whether the arm is guarded or not — the guard runs
    // only after the type matches. Dotted names resolve in M4 and
    // unresolved names subtract nothing, so both are skipped, like the
    // subtraction in `Collector::catch`.
    if !subject.open {
        for (arm_index, arm) in arms.iter().enumerate() {
            for (type_index, catch_type) in arm.types.iter().enumerate() {
                let CatchType::Named { name, span } = catch_type else {
                    continue;
                };
                let arm_res = res.catch_arm_types.get(&(id, arm_index, type_index));
                let Some(tag) = arm_res.and_then(|&arm_res| caught_tag(hir, arm_res)) else {
                    continue;
                };

                if !subject.tags.contains(&tag) {
                    let tag = tag_name(hir, &tag);
                    diagnostics.push(err(
                        codes::E_UNREACHABLE_ARM,
                        *span,
                        format!("unreachable `catch` arm: `{tag}` is not in the error-set here"),
                        &format!("this expression cannot throw `{name}`"),
                    ));
                }
            }
        }
    }

    if !exhaustive {
        return;
    }

    // E003: an open subject set may throw types the arms cannot name,
    // so exhaustiveness is unprovable.
    if subject.open {
        diagnostics.push(
            err(
                codes::E_UNVERIFIABLE_EXHAUSTIVENESS,
                span,
                "catch_all cannot be verified: the subject's error-set is open".to_string(),
                "the error-set of this expression is open",
            )
            .with_note(
                "an indirect call or a throw of unknown type makes the set open".to_string(),
            ),
        );
        return;
    }

    // Guarded arms count for nothing — the guard may be false — the
    // same rule the set subtraction uses.
    let mut remaining = subject.tags.clone();
    let mut unguarded_wildcard = false;
    for (arm_index, arm) in arms.iter().enumerate() {
        if arm.guard.is_some() {
            continue;
        }

        for (type_index, catch_type) in arm.types.iter().enumerate() {
            match catch_type {
                CatchType::Wildcard => unguarded_wildcard = true,
                CatchType::Named { .. } => {
                    let arm_res = res.catch_arm_types.get(&(id, arm_index, type_index));
                    if let Some(tag) = arm_res.and_then(|&arm_res| caught_tag(hir, arm_res)) {
                        remaining.remove(&tag);
                    }
                }
            }
        }
    }

    if remaining.is_empty() {
        // E001, `_` arms: with every error already named, `_` can never
        // match. `CatchType::Wildcard` carries no span of its own, so
        // the diagnostic points at the whole catch expression.
        for arm in arms {
            if arm.types.contains(&CatchType::Wildcard) {
                diagnostics.push(err(
                    codes::E_UNREACHABLE_ARM,
                    span,
                    "`_` is unreachable: every error is already handled".to_string(),
                    "`_` can never match here",
                ));
            }
        }
    } else if !unguarded_wildcard {
        diagnostics.push(err(
            codes::E_CATCH_ALL_NOT_EXHAUSTIVE,
            span,
            format!("catch_all does not handle {}", tag_list(hir, &remaining)),
            "add arms or `_`",
        ));
    }
}

/// Checks one function/method's declared `throws` contract against its
/// converged set: E004 for undeclared throws, E005 for `throws never`.
pub(crate) fn throws_contract(
    hir: &Hir,
    res: &Resolutions,
    sets: &HashMap<DefRef, ErrorSet>,
    def: DefRef,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let Some(func) = func_of(hir, def) else {
        return;
    };
    let Some(set) = sets.get(&def) else {
        return;
    };

    let span = hir.span_of_item(match def {
        DefRef::Item(item) => item,
        DefRef::Method { owner, .. } => owner,
    });
    let name = def_ref_name(hir, def);

    match &func.throws {
        None => {}
        Some(Throws::Never) => {
            if !set.tags.is_empty() {
                diagnostics.push(err(
                    codes::E_THROWS_NEVER_VIOLATED,
                    span,
                    format!(
                        "`{name}` declares `throws never` but can throw {}",
                        tag_list(hir, &set.tags)
                    ),
                    "this function can throw",
                ));
            } else if set.open {
                diagnostics.push(err(
                    codes::E_THROWS_NEVER_VIOLATED,
                    span,
                    format!("cannot verify `throws never`: `{name}`'s error-set is open"),
                    "the error-set of this function is open",
                ));
            }
        }
        Some(Throws::Types(_)) => {
            let Some(resolved) = res.throws_types.get(&def) else {
                return;
            };

            let declared: BTreeSet<ErrorTag> = resolved
                .iter()
                .flatten()
                .filter_map(|&type_res| caught_tag(hir, type_res))
                .collect();

            for tag in &set.tags {
                if !declared.contains(tag) {
                    let tag = tag_name(hir, tag);
                    diagnostics.push(err(
                        codes::E_UNDECLARED_THROW,
                        span,
                        format!("`{name}` throws `{tag}` but does not declare it"),
                        &format!("this function can throw `{tag}`"),
                    ));
                }
            }
        }
    }
}

/// The `FuncDef` a [`DefRef`] addresses, when it is one.
fn func_of(hir: &Hir, def: DefRef) -> Option<&FuncDef> {
    match def {
        DefRef::Item(item) => match hir.item(item) {
            Item::FuncDef(func) => Some(func),
            _ => None,
        },
        DefRef::Method { owner, index } => match hir.item(owner) {
            Item::StructDef(struct_def) => struct_def.methods.get(index),
            _ => None,
        },
    }
}
