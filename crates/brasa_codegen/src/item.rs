//! Item compilation: the synthetic `<toplevel>` function, declared
//! functions, and struct methods.
//!
//! Entry convention (spec: 07 — Diseño del bytecode, module execution):
//! `functions[0]` runs top-level statements and top-`let` initializers
//! in source order; the driver then calls `main` (a regular
//! function-table entry) if the file defines one.

use brasa_bytecode::{FuncId, Op, SlotIx, TestEntry};
use brasa_hir::{FuncDef, Item, ItemId};
use brasa_resolver::DefRef;
use brasa_source::Span;

use crate::context::Cx;
use crate::expr::compile_expr;
use crate::func::{FnKind, FuncCx};
use crate::stmt::{block_stmts, block_value, compile_stmt};

pub(crate) fn compile_toplevel(cx: &mut Cx, roots: &[ItemId]) {
    let mut f = FuncCx::new(cx, FnKind::Toplevel);

    for &item_id in roots {
        let span = f.cx.hir.span_of_item(item_id);
        match f.cx.hir.item(item_id).clone() {
            Item::Stmt(block) => {
                for &stmt in &block {
                    compile_stmt(&mut f, stmt);
                }
            }
            Item::TopLet(top_let) => {
                compile_expr(&mut f, top_let.let_stmt.value);
                let global = f.cx.global_of_item[&item_id];
                f.emit(Op::StoreGlobal(global), span);
            }
            _ => {}
        }
    }

    f.emit(Op::LoadUnit, Span::default());
    f.emit(Op::Ret, Span::default());

    let function = f.finish("<toplevel>".to_string(), 0, 0, Span::default());
    cx.define_function(FuncId(0), function);
}

/// The `main` the driver calls after `<toplevel>` returns: a top-level
/// `def main` declared by the executed file. A struct method named
/// `main` is not a candidate — it has no `ItemId` of its own and never
/// appears among the roots — and neither is an imported module's `main`,
/// which is why only the entry file's roots are searched.
pub(crate) fn find_entry(cx: &Cx, entry_roots: &[ItemId]) -> Option<FuncId> {
    entry_roots.iter().find_map(|item_id| {
        let Item::FuncDef(def) = cx.hir.item(*item_id) else {
            return None;
        };
        (def.name == "main").then(|| cx.func_of_item[item_id])
    })
}

pub(crate) fn compile_items(cx: &mut Cx, roots: &[ItemId]) {
    for &item_id in roots {
        match cx.hir.item(item_id) {
            Item::FuncDef(_) => {
                let func_id = cx.func_of_item[&item_id];
                compile_function(cx, DefRef::Item(item_id), func_id);
            }
            Item::StructDef(def) => {
                for index in 0..def.methods.len() {
                    let func_id = cx.func_of_method[&(item_id, index)];
                    compile_function(
                        cx,
                        DefRef::Method {
                            owner: item_id,
                            index,
                        },
                        func_id,
                    );
                }
            }
            _ => {}
        }
    }
}

/// Compiles each reserved `test` body as a zero-argument function and
/// records it in the module's test table.
///
/// A test is a `FnKind::Func` with no declared return type, so its body
/// is checked and lowered exactly like a `def` returning nothing — the
/// runner calls it and looks only at how it ended.
pub(crate) fn compile_tests(cx: &mut Cx) {
    for (item_id, func_id) in std::mem::take(&mut cx.func_of_test) {
        let Item::TestDef(def) = cx.hir.item(item_id) else {
            unreachable!("test ids only map to TestDef items");
        };

        let name = def.name.clone();
        let span = def.name_span;
        let body = def.body.clone();

        let mut f = FuncCx::new(
            cx,
            FnKind::Func {
                returns_value: false,
            },
        );
        block_stmts(&mut f, &body);
        f.emit(Op::LoadUnit, span);
        f.emit(Op::Ret, span);

        let function = f.finish(format!("<test {name}>"), 0, 0, span);
        cx.define_function(func_id, function);
        cx.tests.push(TestEntry {
            name,
            func: func_id,
        });
    }
}

fn compile_function(cx: &mut Cx, def_ref: DefRef, func_id: FuncId) {
    let def: &FuncDef = match def_ref {
        DefRef::Item(item) => match cx.hir.item(item) {
            Item::FuncDef(def) => def,
            _ => unreachable!("function ids only map to FuncDef items"),
        },
        DefRef::Method { owner, index } => match cx.hir.item(owner) {
            Item::StructDef(def) => &def.methods[index],
            _ => unreachable!("method ids only map to struct owners"),
        },
    };

    let name = def.name.clone();
    let span = def.name_span;
    let body = def.body.clone();
    let returns_value = def.ret.is_some();
    let params = cx
        .res
        .func_params
        .get(&def_ref)
        .cloned()
        .unwrap_or_default();

    let arity = cx.arity(&format!("`{name}`"), params.len(), span);

    let mut f = FuncCx::new(cx, FnKind::Func { returns_value });

    // Arguments land directly in their parameter slots (`call`'s frame
    // base is `sp - argc`); a method's `self` is slot 0. The position
    // fits the slot operand: a parameter list longer than `MAX_PARAMS`
    // has already been reported.
    for (position, param) in params.iter().enumerate() {
        let slot = SlotIx(u16::try_from(position).unwrap_or(u16::MAX));
        match param {
            Some(local) => f.assign_slot(*local, slot),
            None => {
                f.self_slot = Some(slot);
                f.reserve_slot_floor(slot.0 + 1);
            }
        }
    }

    f.bind_shared_params(params.iter().flatten().copied(), span);

    if returns_value {
        block_value(&mut f, &body, span);
    } else {
        block_stmts(&mut f, &body);
        f.emit(Op::LoadUnit, span);
    }
    f.emit(Op::Ret, span);

    let function = f.finish(name, arity, 0, span);
    cx.define_function(func_id, function);
}
