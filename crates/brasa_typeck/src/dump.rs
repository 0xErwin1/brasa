//! A deterministic, span-free text dump of [`TypeTables`].
//!
//! Mirrors `brasa_resolver::dump` for the same reason: insta snapshots.
//! Spans are never printed, and every section is sorted by node index,
//! so two checks of the same input produce byte-identical dumps.

use std::collections::HashMap;

use brasa_arena::Id;
use brasa_hir::Hir;
use brasa_resolver::Resolutions;

use crate::TypeTables;
use crate::types::{WrapDecision, item_name};

/// Renders every table in `tables`, section by section (empty sections
/// are skipped).
pub fn dump(hir: &Hir, res: &Resolutions, tables: &TypeTables) -> String {
    let mut out = String::new();

    dump_item_types(hir, tables, &mut out);
    dump_local_types(hir, res, tables, &mut out);
    dump_expr_types(hir, tables, &mut out);
    dump_wrap_decisions(tables, &mut out);

    out
}

fn sorted_ids<T, V>(map: &HashMap<Id<T>, V>) -> Vec<Id<T>> {
    let mut ids: Vec<Id<T>> = map.keys().copied().collect();
    ids.sort_by_key(Id::index);
    ids
}

fn section(out: &mut String, title: &str, lines: Vec<String>) {
    if lines.is_empty() {
        return;
    }

    out.push_str(title);
    out.push('\n');
    for line in lines {
        out.push_str("  ");
        out.push_str(&line);
        out.push('\n');
    }
}

fn dump_item_types(hir: &Hir, tables: &TypeTables, out: &mut String) {
    let lines = sorted_ids(&tables.item_types)
        .into_iter()
        .map(|id| {
            format!(
                "i{} {} -> {}",
                id.index(),
                item_name(hir, id),
                tables.item_types[&id].display(hir)
            )
        })
        .collect();
    section(out, "item types", lines);
}

fn dump_local_types(hir: &Hir, res: &Resolutions, tables: &TypeTables, out: &mut String) {
    let mut ids: Vec<_> = tables.local_types.keys().copied().collect();
    ids.sort_by_key(|local| local.0);

    let lines = ids
        .into_iter()
        .map(|local| {
            format!(
                "l{} {} -> {}",
                local.0,
                res.local(local).name,
                tables.local_types[&local].display(hir)
            )
        })
        .collect();
    section(out, "local types", lines);
}

fn dump_expr_types(hir: &Hir, tables: &TypeTables, out: &mut String) {
    let lines = sorted_ids(&tables.expr_types)
        .into_iter()
        .map(|id| format!("e{} -> {}", id.index(), tables.expr_types[&id].display(hir)))
        .collect();
    section(out, "expr types", lines);
}

fn dump_wrap_decisions(tables: &TypeTables, out: &mut String) {
    let lines = sorted_ids(&tables.wrap_decisions)
        .into_iter()
        .map(|id| {
            let decision = match tables.wrap_decisions[&id] {
                WrapDecision::Wrap => "wrap",
                WrapDecision::NoOp => "no-op",
            };
            format!("e{} -> {}", id.index(), decision)
        })
        .collect();
    section(out, "wrap decisions", lines);
}
