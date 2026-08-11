//! A deterministic, span-free text dump of [`ErrorSetResult`].
//!
//! Mirrors `brasa_typeck::dump` for the same reason: insta snapshots.
//! Function and method sets sort by definition (items first, methods
//! grouped under their owner), lambda sets by expression index, and
//! tags print in [`crate::ErrorTag`]'s `Ord` — items by declaration,
//! then primitives, then opaque names — so two runs over the same
//! input produce byte-identical dumps.

use brasa_hir::{Hir, Item, ItemId};
use brasa_resolver::DefRef;

use crate::{ErrorSet, ErrorSetResult, ErrorTag};

/// Renders every inferred set, one section for functions/methods and
/// one for lambdas (empty sections are skipped).
pub fn dump(hir: &Hir, result: &ErrorSetResult) -> String {
    let mut out = String::new();

    let mut defs: Vec<DefRef> = result.sets.keys().copied().collect();
    defs.sort_by_key(def_ref_key);
    let lines = defs
        .into_iter()
        .map(|def| {
            format!(
                "{} -> {}",
                def_ref_name(hir, def),
                render_set(hir, &result.sets[&def])
            )
        })
        .collect();
    section(&mut out, "error sets", lines);

    let mut lambdas: Vec<_> = result.lambda_sets.keys().copied().collect();
    lambdas.sort_by_key(|id| id.index());
    let lines = lambdas
        .into_iter()
        .map(|id| {
            format!(
                "e{} -> {}",
                id.index(),
                render_set(hir, &result.lambda_sets[&id])
            )
        })
        .collect();
    section(&mut out, "lambda error sets", lines);

    out
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

fn render_set(hir: &Hir, set: &ErrorSet) -> String {
    let tags: Vec<String> = set.tags.iter().map(|tag| tag_name(hir, tag)).collect();

    let mut rendered = format!("{{{}}}", tags.join(", "));
    if set.open {
        rendered.push_str(" (open)");
    }
    rendered
}

/// The user-facing name of a tag; shared with the checks' messages.
pub(crate) fn tag_name(hir: &Hir, tag: &ErrorTag) -> String {
    match tag {
        ErrorTag::Item(item) => item_name(hir, *item),
        ErrorTag::Primitive(primitive) => primitive.name().to_string(),
        ErrorTag::Opaque(name) => name.to_string(),
    }
}

/// Sort key giving items first (by index), then methods grouped under
/// their owner in declaration order (mirrors `brasa_resolver::dump`).
fn def_ref_key(def: &DefRef) -> (u32, u8, usize) {
    match def {
        DefRef::Item(item) => (item.index(), 0, 0),
        DefRef::Method { owner, index } => (owner.index(), 1, *index),
    }
}

fn item_name(hir: &Hir, id: ItemId) -> String {
    match hir.item(id) {
        Item::FuncDef(func) => func.name.clone(),
        Item::StructDef(def) => def.name.clone(),
        Item::EnumDef(def) => def.name.clone(),
        Item::InterfaceDef(def) => def.name.clone(),
        Item::TopLet(top_let) => top_let.let_stmt.name.clone(),
        Item::Import(_) | Item::Stmt(_) => "<item>".to_string(),
    }
}

/// The user-facing name of a function or method; shared with the
/// checks' messages.
pub(crate) fn def_ref_name(hir: &Hir, def: DefRef) -> String {
    match def {
        DefRef::Item(item) => item_name(hir, item),
        DefRef::Method { owner, index } => {
            let method = match hir.item(owner) {
                Item::StructDef(def) => def.methods.get(index).map(|m| m.name.as_str()),
                _ => None,
            };
            format!("{}.{}", item_name(hir, owner), method.unwrap_or("<method>"))
        }
    }
}
