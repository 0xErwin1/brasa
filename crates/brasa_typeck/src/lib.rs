//! Type checking for Brasa (BRS-16): local inference over the resolved
//! HIR.
//!
//! Consumes a lowered [`brasa_hir::Hir`], its root items, and the
//! [`brasa_resolver::Resolutions`] tables, and produces per-node type
//! tables the tree-walker consumes: every expression's type, every
//! local binding's type, and the `?.` wrap decisions.
//!
//! The M1 (CORE) scope covers local inference, `let`/`let mut` with
//! mutability enforcement, functions, structs, operators, control flow,
//! collection literals, a minimal builtin method table, and generics
//! with structural interface constraints (BRS-17: inference at every
//! use site, constraint satisfaction, no interface-typed values)
//! (`docs/spec/03-types.md`, `docs/spec/05-stdlib.md`). Deferred and
//! traversed with a silently-unifying `Unknown` type: `match`
//! exhaustiveness (BRS-18), `Option` operator refinement (BRS-19),
//! error sets (M2), and stdlib module signatures (M4).

pub mod builtins;
pub mod dump;

mod check;
mod types;

pub use types::{Type, WrapDecision, unify};

use std::collections::HashMap;

use brasa_diagnostics::Diagnostic;
use brasa_hir::{ExprId, Hir, ItemId};
use brasa_resolver::{LocalId, Resolutions};

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
}

/// The output of type checking one module: the type tables and every
/// diagnostic collected along the way. Like the other phases, this never
/// fails outright and never renders diagnostics.
pub struct TypeckResult {
    pub types: TypeTables,
    pub diagnostics: Vec<Diagnostic>,
}

/// Type-checks the module rooted at `roots`.
pub fn check(hir: &Hir, roots: &[ItemId], resolutions: &Resolutions) -> TypeckResult {
    let (types, mut diagnostics) = check::run(hir, roots, resolutions);

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
