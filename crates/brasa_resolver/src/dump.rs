//! A deterministic, span-free text dump of [`Resolutions`].
//!
//! Mirrors `brasa_hir::dump` for the same reason: insta snapshots.
//! Spans are never printed, and every section is sorted by node index,
//! so two resolutions of the same input produce byte-identical dumps.
//! Each line names the reference (or binding site), what it resolved
//! to, and enough context (names, owners) to read the snapshot without
//! the source.

use std::collections::HashMap;

use brasa_arena::Id;
use brasa_hir::{Expr, Hir, Item, ItemId, Pattern};

use crate::resolver::import_binding_name;
use crate::tables::{BinderKind, CtorRes, DefRef, LocalId, Res, Resolutions, TypeRes};

/// Renders every table in `res`, section by section (empty sections are
/// skipped).
pub fn dump(hir: &Hir, res: &Resolutions) -> String {
    let mut out = String::new();

    dump_locals(res, &mut out);
    dump_func_params(hir, res, &mut out);
    dump_exprs(hir, res, &mut out);
    dump_ctor_exprs(hir, res, &mut out);
    dump_ctor_patterns(hir, res, &mut out);
    dump_pattern_locals(hir, res, &mut out);
    dump_stmt_locals(res, &mut out);
    dump_lambda_params(res, &mut out);
    dump_catch_bindings(hir, res, &mut out);
    dump_constraints(hir, res, &mut out);
    dump_struct_lits(hir, res, &mut out);
    dump_types(hir, res, &mut out);

    out
}

fn sorted_ids<T, V>(map: &HashMap<Id<T>, V>) -> Vec<Id<T>> {
    let mut ids: Vec<Id<T>> = map.keys().copied().collect();
    ids.sort_by_key(Id::index);
    ids
}

/// Sort key giving items first (by index), then methods grouped under
/// their owner in declaration order.
fn def_ref_key(def: &DefRef) -> (u32, u8, usize) {
    match def {
        DefRef::Item(item) => (item.index(), 0, 0),
        DefRef::Method { owner, index } => (owner.index(), 1, *index),
    }
}

fn item_name(hir: &Hir, id: ItemId) -> String {
    match hir.item(id) {
        Item::Import(import) => import_binding_name(import)
            .unwrap_or("<import>")
            .to_string(),
        Item::FuncDef(func) => func.name.clone(),
        Item::StructDef(def) => def.name.clone(),
        Item::EnumDef(def) => def.name.clone(),
        Item::InterfaceDef(def) => def.name.clone(),
        Item::TopLet(top_let) => top_let.let_stmt.name.clone(),
        Item::Stmt(_) => "<stmt>".to_string(),
    }
}

fn def_ref_name(hir: &Hir, def: DefRef) -> String {
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

fn generic_name(hir: &Hir, owner: DefRef, index: usize) -> String {
    let generics = match owner {
        DefRef::Item(item) => match hir.item(item) {
            Item::FuncDef(func) => Some(&func.generics),
            Item::StructDef(def) => Some(&def.generics),
            Item::EnumDef(def) => Some(&def.generics),
            Item::InterfaceDef(def) => Some(&def.generics),
            _ => None,
        },
        DefRef::Method { owner, index } => match hir.item(owner) {
            Item::StructDef(def) => def.methods.get(index).map(|m| &m.generics),
            _ => None,
        },
    };

    generics
        .and_then(|g| g.get(index))
        .map(|g| g.name.clone())
        .unwrap_or_else(|| "<generic>".to_string())
}

fn res_str(hir: &Hir, res: Res) -> String {
    match res {
        Res::Local(local) => format!("local l{}", local.0),
        Res::Item(item) => format!("item {}", item_name(hir, item)),
        Res::Module(item) => format!("module {}", item_name(hir, item)),
        Res::Builtin(builtin) => format!("builtin {}", builtin.name()),
        Res::SelfParam => "self".to_string(),
    }
}

fn type_res_str(hir: &Hir, res: TypeRes) -> String {
    match res {
        TypeRes::Item(item) => {
            let kind = match hir.item(item) {
                Item::StructDef(_) => "struct",
                Item::EnumDef(_) => "enum",
                Item::InterfaceDef(_) => "interface",
                _ => "item",
            };
            format!("{kind} {}", item_name(hir, item))
        }
        TypeRes::Builtin(builtin) => format!("builtin {}", builtin.name()),
        TypeRes::GenericParam { owner, index } => format!(
            "generic {} of {}",
            generic_name(hir, owner, index),
            def_ref_name(hir, owner)
        ),
        TypeRes::SelfType => "Self".to_string(),
    }
}

fn ctor_res_str(hir: &Hir, res: CtorRes) -> String {
    match res {
        CtorRes::OptionSome => "Option::Some".to_string(),
        CtorRes::OptionNone => "Option::None".to_string(),
        CtorRes::EnumVariant {
            enum_item,
            variant_index,
        } => {
            let variant = match hir.item(enum_item) {
                Item::EnumDef(def) => def
                    .variants
                    .get(variant_index)
                    .map(|v| v.name.as_str())
                    .unwrap_or("<variant>"),
                _ => "<variant>",
            };
            format!("{}::{}", item_name(hir, enum_item), variant)
        }
    }
}

fn binder_kind_str(kind: BinderKind) -> &'static str {
    match kind {
        BinderKind::Param => "param",
        BinderKind::Let => "let",
        BinderKind::LambdaParam => "lambda-param",
        BinderKind::PatternBinding => "pattern",
        BinderKind::CatchBinding => "catch",
    }
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

fn param_slots(slots: &[Option<LocalId>]) -> String {
    let rendered: Vec<String> = slots
        .iter()
        .map(|slot| match slot {
            Some(local) => format!("l{}", local.0),
            None => "self".to_string(),
        })
        .collect();
    format!("[{}]", rendered.join(", "))
}

fn dump_locals(res: &Resolutions, out: &mut String) {
    let lines = res
        .locals
        .iter()
        .enumerate()
        .map(|(i, local)| {
            let mut_str = if local.mutable { " mut" } else { "" };
            format!(
                "l{i} {}{mut_str} {}",
                binder_kind_str(local.kind),
                local.name
            )
        })
        .collect();
    section(out, "locals", lines);
}

fn dump_func_params(hir: &Hir, res: &Resolutions, out: &mut String) {
    let mut defs: Vec<&DefRef> = res.func_params.keys().collect();
    defs.sort_by_key(|def| def_ref_key(def));

    let lines = defs
        .iter()
        .map(|&&def| {
            format!(
                "{} -> {}",
                def_ref_name(hir, def),
                param_slots(&res.func_params[&def])
            )
        })
        .collect();
    section(out, "func params", lines);
}

fn dump_exprs(hir: &Hir, res: &Resolutions, out: &mut String) {
    let lines = sorted_ids(&res.expr_res)
        .into_iter()
        .map(|id| {
            let name = match hir.expr(id) {
                Expr::Ident(name) => name.as_str(),
                Expr::SelfExpr => "self",
                _ => "<expr>",
            };
            format!(
                "e{} {} -> {}",
                id.index(),
                name,
                res_str(hir, res.expr_res[&id])
            )
        })
        .collect();
    section(out, "exprs", lines);
}

fn dump_ctor_exprs(hir: &Hir, res: &Resolutions, out: &mut String) {
    let lines = sorted_ids(&res.ctor_expr_res)
        .into_iter()
        .map(|id| {
            let name = match hir.expr(id) {
                Expr::EnumCtor { name, .. } => name.as_str(),
                _ => "<ctor>",
            };
            format!(
                "e{} {} -> {}",
                id.index(),
                name,
                ctor_res_str(hir, res.ctor_expr_res[&id])
            )
        })
        .collect();
    section(out, "ctor exprs", lines);
}

fn dump_ctor_patterns(hir: &Hir, res: &Resolutions, out: &mut String) {
    let lines = sorted_ids(&res.ctor_pattern_res)
        .into_iter()
        .map(|id| {
            let name = match hir.pattern(id) {
                Pattern::Ctor { name, .. } => name.as_str(),
                _ => "<ctor>",
            };
            format!(
                "p{} {} -> {}",
                id.index(),
                name,
                ctor_res_str(hir, res.ctor_pattern_res[&id])
            )
        })
        .collect();
    section(out, "ctor patterns", lines);
}

fn dump_pattern_locals(hir: &Hir, res: &Resolutions, out: &mut String) {
    let lines = sorted_ids(&res.pattern_locals)
        .into_iter()
        .map(|id| {
            let name = match hir.pattern(id) {
                Pattern::Binding(name) => name.as_str(),
                _ => "<binding>",
            };
            format!("p{} {} -> l{}", id.index(), name, res.pattern_locals[&id].0)
        })
        .collect();
    section(out, "pattern bindings", lines);
}

fn dump_stmt_locals(res: &Resolutions, out: &mut String) {
    let lines = sorted_ids(&res.stmt_locals)
        .into_iter()
        .map(|id| format!("s{} -> l{}", id.index(), res.stmt_locals[&id].0))
        .collect();
    section(out, "stmt lets", lines);
}

fn dump_lambda_params(res: &Resolutions, out: &mut String) {
    let lines = sorted_ids(&res.lambda_params)
        .into_iter()
        .map(|id| {
            let rendered: Vec<String> = res.lambda_params[&id]
                .iter()
                .map(|local| format!("l{}", local.0))
                .collect();
            format!("e{} -> [{}]", id.index(), rendered.join(", "))
        })
        .collect();
    section(out, "lambda params", lines);
}

fn dump_catch_bindings(hir: &Hir, res: &Resolutions, out: &mut String) {
    let lines = sorted_ids(&res.catch_bindings)
        .into_iter()
        .map(|id| {
            let name = match hir.expr(id) {
                Expr::Catch { binding, .. } => binding.as_str(),
                _ => "<binding>",
            };
            format!("e{} {} -> l{}", id.index(), name, res.catch_bindings[&id].0)
        })
        .collect();
    section(out, "catch bindings", lines);
}

fn dump_constraints(hir: &Hir, res: &Resolutions, out: &mut String) {
    let mut keys: Vec<&(DefRef, usize)> = res.constraint_res.keys().collect();
    keys.sort_by_key(|(def, index)| (def_ref_key(def), *index));

    let lines = keys
        .into_iter()
        .map(|&key| {
            let (owner, index) = key;
            format!(
                "{}<{}> -> {}",
                def_ref_name(hir, owner),
                generic_name(hir, owner, index),
                type_res_str(hir, res.constraint_res[&key])
            )
        })
        .collect();
    section(out, "generic constraints", lines);
}

fn dump_struct_lits(hir: &Hir, res: &Resolutions, out: &mut String) {
    let lines = sorted_ids(&res.struct_lit_res)
        .into_iter()
        .map(|id| {
            let name = match hir.expr(id) {
                Expr::StructLit { type_name, .. } => type_name.as_str(),
                _ => "<struct-lit>",
            };
            format!(
                "e{} {} -> {}",
                id.index(),
                name,
                type_res_str(hir, res.struct_lit_res[&id])
            )
        })
        .collect();
    section(out, "struct lits", lines);
}

fn dump_types(hir: &Hir, res: &Resolutions, out: &mut String) {
    let lines = sorted_ids(&res.type_res)
        .into_iter()
        .map(|id| {
            let name = match hir.type_expr(id) {
                brasa_hir::TypeExpr::Named { name, .. } => name.as_str(),
                _ => "<type>",
            };
            format!(
                "t{} {} -> {}",
                id.index(),
                name,
                type_res_str(hir, res.type_res[&id])
            )
        })
        .collect();
    section(out, "types", lines);
}
