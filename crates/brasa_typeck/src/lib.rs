//! Type checking for Brasa (BRS-16): local inference over the resolved
//! HIR.
//!
//! Consumes a lowered [`brasa_hir::Hir`], its root items, and the
//! [`brasa_resolver::Resolutions`] tables, and produces per-node type
//! tables code generation consumes: every expression's type, every
//! local binding's type, and the `?.` wrap decisions.
//!
//! The M1 (CORE) scope covers local inference, `let`/`let mut` with
//! mutability enforcement, functions, structs, operators, control flow,
//! collection literals, a minimal builtin method table, and generics
//! with structural interface constraints (BRS-17: inference at every
//! use site, constraint satisfaction, no interface-typed values)
//! (spec: 03 — Sistema de tipos, spec: 05 — Stdlib de scripting), plus `match`
//! exhaustiveness (BRS-18) and the `?.`/`??` operator rules with
//! source-language diagnostics (BRS-19). Deferred and traversed with a
//! silently-unifying `Unknown` type: error sets (M2) and stdlib module
//! signatures (M4).

pub mod builtins;
pub mod dump;

mod check;
mod exhaust;
mod types;

pub use types::{Type, WrapDecision, unify};

use std::collections::HashMap;

use brasa_diagnostics::Diagnostic;
use brasa_hir::{ExprId, Hir, ItemId, SugarOrigin};
use brasa_resolver::{LocalId, Resolutions};
use brasa_source::Span;

/// Every table produced by [`check`], keyed by HIR arena IDs like the
/// resolver's tables.
#[derive(Debug, Default)]
pub struct TypeTables {
    /// The type of every checked expression.
    pub expr_types: HashMap<ExprId, Type>,
    /// The type of every local binding site.
    pub local_types: HashMap<LocalId, Type>,
    /// The type of every `FuncDef` (its function type) and `TopLet`.
    pub item_types: HashMap<ItemId, Type>,
    /// The flatten decision for every `Expr::OptionWrap` whose operand
    /// type is known; nodes inside deferred constructs stay absent.
    pub wrap_decisions: HashMap<ExprId, WrapDecision>,
    /// Every place a concrete struct was accepted as satisfying a user
    /// interface, as `(struct, interface)` plus the span that demanded
    /// it (BRS-141).
    ///
    /// Interfaces are STRUCTURAL (spec: 03 — Sistema de tipos): a struct
    /// never declares that it implements one, so there is no
    /// declaration site at which to check the members against their
    /// contracts. The only moment the pairing exists is here, when a
    /// constraint is solved — which is also the only moment it can
    /// matter, since a struct nobody uses through an interface cannot
    /// break anyone.
    ///
    /// Recorded rather than checked here because the contract is about
    /// ERROR SETS, and those are inferred by `brasa_errorset` after
    /// this pass has finished.
    pub iface_uses: Vec<(ItemId, ItemId, Span)>,
    /// The field types, in declaration order, of every struct reachable
    /// as a `json.decode` target (BRS-144).
    ///
    /// Code generation synthesizes a decoder per target struct and
    /// needs each field's TYPE to pick the accessor for it, but a
    /// field's type is a `TypeExpr` that only this pass resolves. The
    /// check that proves a target decodable already converts every one
    /// of them, so it records what it converted rather than making a
    /// later phase redo the resolution — and the table's presence is
    /// itself the proof: a struct is here only if decoding it was
    /// accepted.
    pub decode_fields: HashMap<ItemId, Vec<Type>>,
}

/// The output of type checking one module: the type tables and every
/// diagnostic collected along the way. Like the other phases, this never
/// fails outright and never renders diagnostics.
pub struct TypeckResult {
    pub types: TypeTables,
    pub diagnostics: Vec<Diagnostic>,
}

/// Type-checks the module rooted at `roots`. `sugar_origins` is
/// lowering's side table marking which `match` expressions were
/// desugared from `?.`/`??`, so their misuse reports in source terms
/// (spec: 06 — Diagnósticos, T028–T030).
pub fn check(
    hir: &Hir,
    roots: &[ItemId],
    resolutions: &Resolutions,
    sugar_origins: &HashMap<ExprId, SugarOrigin>,
) -> TypeckResult {
    let (types, mut diagnostics) = check::run(hir, roots, resolutions, sugar_origins);

    diagnostics.sort_by_key(|d| (d.primary_span.start.0, d.primary_span.end.0));
    dedup_identical_diagnostics(&mut diagnostics);

    TypeckResult { types, diagnostics }
}

/// Backstop against diagnostic cascades, mirroring `brasa_resolver`:
/// drops any diagnostic repeating the exact `(message, primary_span)` of
/// one already kept.
fn dedup_identical_diagnostics(diagnostics: &mut Vec<Diagnostic>) {
    let mut seen = std::collections::HashSet::new();
    diagnostics.retain(|d| seen.insert((d.message.clone(), d.primary_span)));
}
