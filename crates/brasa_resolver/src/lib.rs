//! Name resolution for Brasa (BRS-15): scopes, imports, and the symbol
//! tables the type checker consumes.
//!
//! Consumes a lowered [`brasa_hir::Hir`] plus its root items and
//! produces [`Resolutions`]: for every value reference
//! (`Expr::Ident`/`Expr::SelfExpr`), constructor reference
//! (`Expr::EnumCtor`/`Pattern::Ctor`), type reference
//! (`TypeExpr::Named`, struct-literal names, generic constraints), and
//! binding site, what it resolved to — queryable by HIR node ID so later
//! phases never re-walk scopes.
//!
//! The language rules implemented here come from `docs/spec`: two
//! namespaces (`02-grammar.md`), inner-scope-only shadowing
//! (`01-syntax.md`, `03-types.md`), module-wide item visibility with
//! in-order top-level execution (`01-syntax.md`), qualified-only imports
//! binding the last segment or file stem (`01-syntax.md`), the prelude
//! (`05-stdlib.md`) and stdlib interfaces (`03-types.md`). Where the
//! spec is silent this crate fixes two decisions, documented at the
//! rule sites in [`resolver`](crate): re-binding a name in the same
//! scope is a duplicate-definition error, and top-level code sees only
//! top-level `let`s declared earlier (items are always visible).
//!
//! Out of scope for M1: file-import loading and cycle detection (the
//! module loader is a later work item), `catch` arm type resolution
//! (error-set inference is M2, `04-errors.md`), and everything
//! type-shaped (member lookup after `.`, mutability enforcement,
//! constructor disambiguation by expected type).

pub mod dump;
mod resolver;
mod tables;

pub use tables::{
    BinderKind, BuiltinType, BuiltinValue, CtorRes, DefRef, LocalId, LocalInfo, Res, Resolutions,
    TypeRes,
};

use brasa_diagnostics::Diagnostic;
use brasa_hir::{Hir, ItemId};

/// The output of resolving one module: the symbol tables and every
/// diagnostic collected along the way. Like the other phases, this never
/// fails outright and never renders diagnostics.
pub struct ResolveResult {
    pub resolutions: Resolutions,
    pub diagnostics: Vec<Diagnostic>,
}

/// Resolves every name in the module rooted at `roots`.
pub fn resolve(hir: &Hir, roots: &[ItemId]) -> ResolveResult {
    let (resolutions, mut diagnostics) = resolver::run(hir, roots);

    diagnostics.sort_by_key(|d| (d.primary_span.start.0, d.primary_span.end.0));
    dedup_identical_diagnostics(&mut diagnostics);

    ResolveResult {
        resolutions,
        diagnostics,
    }
}

/// Backstop against diagnostic cascades, mirroring `brasa_parser`: drops
/// any diagnostic repeating the exact `(message, primary_span)` of one
/// already kept.
fn dedup_identical_diagnostics(diagnostics: &mut Vec<Diagnostic>) {
    let mut seen = std::collections::HashSet::new();
    diagnostics.retain(|d| seen.insert((d.message.clone(), d.primary_span)));
}
