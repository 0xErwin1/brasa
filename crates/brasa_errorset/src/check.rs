//! The BRS-23 checks that consume the inferred sets: unreachable
//! `catch` arms (E001), `catch!` exhaustiveness (E002/E003), `throws`
//! contract verification (E004/E005/E006), and the rendering contract
//! (E007). Wording and kind boundaries follow
//! spec: 06 — Diagnósticos.
//!
//! Precision rules on open sets (spec: 04 — Sistema de errores): the tags of
//! an open set are a sound lower bound, so openness never suppresses a
//! "this CAN be thrown" finding, but every "this CANNOT be thrown"
//! claim — unreachability, exhaustiveness, `throws never` emptiness —
//! is skipped or reported as unverifiable instead.
//!
//! Decisions recorded here:
//!
//! - A `_` arm is flagged unreachable only inside `catch!`, and only
//!   when the closed subject set minus the unguarded named arms is
//!   empty (spec: 04 — Sistema de errores forbids unreachable arms in
//!   `catch!`). In a plain `catch`, a defensive `_` is never
//!   flagged: non-exhaustive handling is the default there.
//! - An open subject under `catch!` is E003, erring on the side of
//!   soundness: an incomplete list cannot prove exhaustiveness.
//! - `throws` over-declaration (declaring a type the body never
//!   throws) gets no diagnostic: the spec is silent, and a widened
//!   contract is harmless.
//! - A declared `throws` list over an open set is E004 in its
//!   unverifiable wording, alongside any concrete undeclared tag. A
//!   `throws` list names everything the body can throw, so an open set
//!   leaves that claim unproven, and a `catch` written on the strength
//!   of the declaration would not handle what escapes. This is the
//!   same rule E003 applies to `catch!` and E005 to `throws never`.
//! - A declared `throws` name resolving to something that is not a
//!   throwable nominal or primitive (an interface, a generic
//!   parameter) maps to no tag and is skipped, like the equivalent
//!   `catch` arm subtraction.
//! - A declared stdlib-native error (`fs.NotFound`) maps to its
//!   `Opaque` tag, so it satisfies the contract exactly like a
//!   user-declared type would. The two halves of the contract stay
//!   symmetric: whatever a `catch` arm can name, `throws` can declare.
//! - A declared `panics.X` is E006, not E004: a panic is not an error,
//!   so this is a category error rather than a mis-sized error-set.
//! - Interface-method `throws` contracts are NOT checked here: the
//!   resolver validates the declared names (`R003`), but enforcing the
//!   contract — a satisfying method must not throw more than the
//!   member declares — needs interface-satisfaction integration
//!   (typeck would have to record which method satisfied which
//!   member), deferred to M3+.
//! - A `toString` whose inferred set is open is E007, not a pass. The
//!   rule is a "cannot throw" claim, and every other check here refuses
//!   to grant one over an incomplete list (E003, E004, E005). It also
//!   keeps the method consistent with itself: an open `toString` could
//!   not have declared `throws never` either.
//! - E007 and E005 both fire on a `toString` that declares
//!   `throws never` and throws anyway. They judge different things —
//!   one the written contract, the other the method's identity — and
//!   suppressing E007 on the strength of a declaration would put the
//!   declaration back in charge of a rule that exists precisely because
//!   declarations are optional.
//! - E004/E005/E007 point at the declaring function's name
//!   (spec: 06 — Diagnósticos, span rules).

use std::collections::{BTreeSet, HashMap, HashSet};

use brasa_diagnostics::{Diagnostic, Severity, codes};
use brasa_hir::{CatchArm, CatchType, ExprId, FuncDef, Hir, Item, Throws, ThrowsType};
use brasa_resolver::{DefRef, Resolutions};
use brasa_source::Span;

use crate::collect::{arm_tag, caught_tag, throws_tag};
use crate::dump::{def_ref_name, tag_name};
use crate::{ErrorSet, ErrorTag};

/// What every "cannot be verified" diagnostic tells the reader, since
/// openness is a property of the analysis rather than of any one line.
const OPEN_SET_NOTE: &str = "an indirect call or a throw of unknown type makes the set open";

/// Why rendering has to be infallible. Worded exactly as `T034`'s first
/// note: the two codes are one rule caught at two moments, so they
/// explain themselves the same way.
const RENDER_REACH_NOTE: &str = "`toString` is reached from `puts`, string interpolation, and every container, `Option`, tuple, and enum that renders its elements, as well as from error reporting itself — a failure there has nowhere left to be reported";

/// The two exits, worded exactly as `T034`'s second note. Declaring the
/// throw is deliberately not among them: `T034` rejects a `throws`
/// clause on `toString` outright, so pointing there would send the
/// reader into the sibling diagnostic.
const RENDER_REPAIR_NOTE: &str = "handle the failure inside `toString` with a `catch` and render a fallback, or move the fallible work to a method with another name";

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

/// Checks one `catch`/`catch!` expression against its subject's
/// contribution set (computed before arm subtraction): E001 for
/// unreachable arms, E002/E003 for `catch!` exhaustiveness.
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
    // only after the type matches. Native-error arms (BRS-41) check
    // like named types: their `Opaque` tag is in the set or the arm is
    // unreachable. Panic arms (`panics.X`, in `catch_arm_panics`, never
    // read here) handle panics rather than errors and are exempt;
    // dotted names in namespaces that have not landed (M4) and
    // unresolved names map to no tag, so they are skipped, like the
    // subtraction in `Collector::catch`.
    if !subject.open {
        for (arm_index, arm) in arms.iter().enumerate() {
            for (type_index, catch_type) in arm.types.iter().enumerate() {
                let CatchType::Named { name, span } = catch_type else {
                    continue;
                };
                let Some(tag) = arm_tag(hir, res, (id, arm_index, type_index)) else {
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
                "catch! cannot be verified: the subject's error-set is open".to_string(),
                "the error-set of this expression is open",
            )
            .with_note(OPEN_SET_NOTE.to_string()),
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
                CatchType::Wildcard { .. } => unguarded_wildcard = true,
                CatchType::Named { .. } => {
                    if let Some(tag) = arm_tag(hir, res, (id, arm_index, type_index)) {
                        remaining.remove(&tag);
                    }
                }
            }
        }
    }

    if remaining.is_empty() {
        // E001, `_` arms: with every error already named, `_` can never
        // match; the diagnostic points at the `_` token itself.
        for arm in arms {
            for catch_type in &arm.types {
                if let CatchType::Wildcard { span } = catch_type {
                    diagnostics.push(err(
                        codes::E_UNREACHABLE_ARM,
                        *span,
                        "`_` is unreachable: every error is already handled".to_string(),
                        "`_` can never match here",
                    ));
                }
            }
        }
    } else if !unguarded_wildcard {
        diagnostics.push(err(
            codes::E_CATCH_ALL_NOT_EXHAUSTIVE,
            span,
            format!("catch! does not handle {}", tag_list(hir, &remaining)),
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

    let span = func.name_span;
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
                diagnostics.push(
                    err(
                        codes::E_THROWS_NEVER_VIOLATED,
                        span,
                        format!("cannot verify `throws never`: `{name}`'s error-set is open"),
                        "the error-set of this function is open",
                    )
                    .with_note(OPEN_SET_NOTE.to_string()),
                );
            }
        }
        Some(Throws::Types(names)) => {
            reject_declared_panics(names, diagnostics);

            let declared: BTreeSet<ErrorTag> = (0..names.len())
                .filter_map(|index| throws_tag(hir, res, (def, index)))
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

            // The undeclared tags above are what the set proves; this is
            // what it leaves unproven. Both can fire on the same
            // function, and neither causes the other: one asks for a
            // wider declaration, the other for a call the analysis can
            // see through.
            if set.open {
                diagnostics.push(
                    err(
                        codes::E_UNDECLARED_THROW,
                        span,
                        format!("cannot verify `throws`: `{name}`'s error-set is open"),
                        "the error-set of this function is open",
                    )
                    .with_note(OPEN_SET_NOTE.to_string()),
                );
            }
        }
    }
}

/// E007: a `toString` override whose inferred error-set is not provably
/// empty.
///
/// `T034` states this rule where the contract is written, but `throws`
/// is inferred (spec: 04 — Sistema de errores), so a `toString` that throws
/// without writing a clause slips past a declaration-site check
/// entirely. The set the fixpoint derives is the real subject of the
/// rule, and it must be empty: rendering is reached from `puts`, string
/// interpolation, `Vector.join`, every container, `Option`, tuple and
/// enum that renders its elements, and from error reporting itself, so
/// a throw there has no channel left to travel on. The two rendering
/// paths the collector deliberately treats as clean — `Expr::ToString`,
/// which never descends into the receiver's override, and the derived
/// `toString` of a struct that declares none — are sound exactly
/// because of this check.
///
/// The span is the method's name, like E004/E005, rather than a
/// throwing expression. Renaming the method is one of the two repairs,
/// so the name is what the diagnostic is about; and a set inherited
/// from a callee has no throwing expression in this body to point at,
/// so the name is also the only span that always exists.
///
/// Only struct methods are checked. A free function named `toString` is
/// not an override and no rendering path reaches it.
pub(crate) fn render_contract(
    hir: &Hir,
    sets: &HashMap<DefRef, ErrorSet>,
    def: DefRef,
    diagnostics: &mut Vec<Diagnostic>,
) {
    if !matches!(def, DefRef::Method { .. }) {
        return;
    }

    let Some(func) = func_of(hir, def) else {
        return;
    };
    if func.name != "toString" {
        return;
    }

    let Some(set) = sets.get(&def) else {
        return;
    };

    let span = func.name_span;
    let name = def_ref_name(hir, def);

    let diagnostic = if !set.tags.is_empty() {
        err(
            codes::E_TO_STRING_CAN_THROW,
            span,
            format!(
                "`{name}` can throw {}: rendering has to be infallible",
                tag_list(hir, &set.tags)
            ),
            "this method can throw",
        )
    } else if set.open {
        err(
            codes::E_TO_STRING_CAN_THROW,
            span,
            format!("cannot verify that `{name}` is infallible: its error-set is open"),
            "the error-set of this method is open",
        )
        .with_note(OPEN_SET_NOTE.to_string())
    } else {
        return;
    };

    diagnostics.push(
        diagnostic
            .with_note(RENDER_REACH_NOTE.to_string())
            .with_note(RENDER_REPAIR_NOTE.to_string()),
    );
}

/// E006: a `throws` list naming a member of the `panics.` union.
///
/// A panic is a separate channel (spec: 04 — Sistema de errores): it never
/// enters an error-set, so the declaration is a claim about the body
/// that nothing the body does could ever satisfy — and, unlike an
/// over-declared error type, it is not a harmlessly wider contract but
/// a category error. Reported per name, before the set comparison, and
/// independently of it: the panic contributes no tag either way.
fn reject_declared_panics(names: &[ThrowsType], diagnostics: &mut Vec<Diagnostic>) {
    for throws_type in names {
        if !throws_type.name.starts_with("panics.") {
            continue;
        }

        diagnostics.push(
            err(
                codes::E_PANIC_IN_THROWS,
                throws_type.span,
                format!("`throws` cannot name a panic: `{}`", throws_type.name),
                "a panic is not an error",
            )
            .with_note(
                "panics never enter an error-set: drop it here and name it in a `catch` arm \
                 instead"
                    .to_string(),
            ),
        );
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

/// E008: a struct accepted as satisfying an interface has a method that
/// throws more than the member it satisfies declares (BRS-141).
///
/// Structural satisfaction compares SIGNATURES — parameters and result
/// — so a method whose shape matches passes even when its error-set
/// does not fit the contract the member states. Nothing else catches
/// it: interfaces have no conformance declarations
/// (spec: 03 — Sistema de tipos), so a struct has no site of its own
/// where its promises could be read.
///
/// The subject is the pairing rather than either half, so the span is
/// the call that demanded it — the place where a reader can see both
/// the concrete type and the constraint it was passed to. A struct
/// whose method throws freely is not wrong on its own; it becomes
/// wrong when someone passes it where a narrower contract was promised.
///
/// An open set is reported like a mismatched one, for the reason E004
/// gives: the rule is a "throws at most" claim, and an incomplete list
/// cannot support one.
pub(crate) fn iface_throws_contracts(
    hir: &Hir,
    res: &Resolutions,
    types: &brasa_typeck::TypeTables,
    sets: &HashMap<DefRef, ErrorSet>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let mut seen = HashSet::new();

    for &(struct_item, iface_item, span) in &types.iface_uses {
        // One report per pairing, not per call: the mismatch is a
        // property of the two declarations, and a struct used through
        // an interface in a loop is still one thing to fix.
        if !seen.insert((struct_item, iface_item)) {
            continue;
        }

        let (Item::StructDef(struct_def), Item::InterfaceDef(iface)) =
            (hir.item(struct_item), hir.item(iface_item))
        else {
            continue;
        };

        for (index, member) in iface.methods.iter().enumerate() {
            // A member with no clause promises nothing, so nothing to
            // hold the method to. `throws never` promises the most.
            let declared: BTreeSet<ErrorTag> = match &member.throws {
                None => continue,
                Some(Throws::Never) => BTreeSet::new(),
                Some(Throws::Types(names)) => (0..names.len())
                    .filter_map(|name_index| {
                        iface_member_tag(hir, res, iface_item, index, name_index)
                    })
                    .collect(),
            };

            let Some(method_index) = struct_def
                .methods
                .iter()
                .position(|m| m.name == member.name)
            else {
                continue;
            };
            let Some(set) = sets.get(&DefRef::Method {
                owner: struct_item,
                index: method_index,
            }) else {
                continue;
            };

            let struct_name = &struct_def.name;
            let iface_name = &iface.name;
            let member_name = &member.name;

            for tag in &set.tags {
                if !declared.contains(tag) {
                    let tag = tag_name(hir, tag);
                    diagnostics.push(err(
                        codes::E_IFACE_THROWS_VIOLATED,
                        span,
                        format!(
                            "`{struct_name}.{member_name}` throws `{tag}`, which \
                             `{iface_name}.{member_name}` does not declare"
                        ),
                        &format!("`{struct_name}` is used as `{iface_name}` here"),
                    ));
                }
            }

            if set.open {
                diagnostics.push(
                    err(
                        codes::E_IFACE_THROWS_VIOLATED,
                        span,
                        format!(
                            "cannot verify `{iface_name}.{member_name}`'s contract: \
                             `{struct_name}.{member_name}`'s error-set is open"
                        ),
                        &format!("`{struct_name}` is used as `{iface_name}` here"),
                    )
                    .with_note(OPEN_SET_NOTE.to_string()),
                );
            }
        }
    }
}

/// The tag one name of an interface member's `throws` clause stands
/// for — [`crate::collect::throws_tag`]'s twin over the resolver's
/// interface tables, which are keyed by the member's position rather
/// than by a `DefRef` an interface member does not have.
fn iface_member_tag(
    hir: &Hir,
    res: &Resolutions,
    iface: brasa_hir::ItemId,
    member: usize,
    name: usize,
) -> Option<ErrorTag> {
    if let Some(&native) = res.iface_member_throws_natives.get(&(iface, member, name)) {
        return Some(ErrorTag::Opaque(native));
    }

    res.iface_member_throws
        .get(&(iface, member))
        .and_then(|declared| declared.get(name).copied().flatten())
        .and_then(|type_res| caught_tag(hir, type_res))
}
