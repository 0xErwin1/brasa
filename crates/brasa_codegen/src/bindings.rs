//! Which local bindings must live in a heap cell rather than directly
//! in a frame slot.
//!
//! A closure captures a lexical BINDING, not a snapshot of its value
//! (spec: 01 — Sintaxis, lambdas): rebinding the name is visible
//! from both the capturing and the captured side. A frame slot cannot
//! express that on its own — the closure outlives the frame, and the
//! capture is a separate slot in a separate frame — so a shared binding
//! is boxed into a cell both sides point at.
//!
//! The boxing is not universal, and that is the one liberty the rule
//! leaves: when nothing ever rebinds a binding, its cell would only
//! ever hold the value it was created with, so copying the value is
//! indistinguishable from sharing the cell. Only a binding that is BOTH
//! captured by some lambda AND the target of an assignment somewhere
//! therefore gets one. Everything else keeps the cheaper representation,
//! and no program can tell.
//!
//! Both sets are whole-program: `LocalId`s are globally unique, so the
//! answer for one local does not depend on which function is being
//! compiled. The scan reads the HIR arenas directly rather than walking
//! bodies, because a tree walker that missed a form would UNDER-report
//! and silently un-share a binding; an arena scan can only ever
//! over-report, and an unnecessary cell is not observable.

use std::collections::HashSet;

use brasa_hir::{Expr, Hir, Stmt};
use brasa_resolver::{LocalId, Res, Resolutions};

use crate::captures::lambda_captures;

/// The locals that must be boxed: captured by a lambda and rebound
/// somewhere.
pub(crate) fn shared_bindings(hir: &Hir, res: &Resolutions) -> HashSet<LocalId> {
    let mut rebound = HashSet::new();
    for (_, stmt) in hir.stmts() {
        let Stmt::Assign { target, .. } = stmt else {
            continue;
        };
        if let Some(Res::Local(local)) = res.expr_res.get(target) {
            rebound.insert(*local);
        }
    }

    if rebound.is_empty() {
        return HashSet::new();
    }

    let mut shared = HashSet::new();
    for (id, expr) in hir.exprs() {
        if !matches!(expr, Expr::Lambda { .. }) {
            continue;
        }
        for local in lambda_captures(hir, res, id).locals {
            if rebound.contains(&local) {
                shared.insert(local);
            }
        }
    }

    shared
}
