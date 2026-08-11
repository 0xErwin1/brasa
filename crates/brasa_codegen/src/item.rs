//! Item compilation: the synthetic `<toplevel>` function, declared
//! functions, and struct methods.
//!
//! Entry convention (`docs/spec/07-bytecode.md`, module execution):
//! `functions[0]` runs top-level statements and top-`let` initializers
//! in source order; the driver then calls `main` (a regular
//! function-table entry) if the file defines one.

use brasa_bytecode::{FuncId, Op, SlotIx};
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

    let function = f.finish("<toplevel>".to_string(), 0, 0);
    cx.define_function(FuncId(0), function);
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

    let mut f = FuncCx::new(cx, FnKind::Func { returns_value });

    // Arguments land directly in their parameter slots (`call`'s frame
    // base is `sp - argc`); a method's `self` is slot 0.
    for (position, param) in params.iter().enumerate() {
        let slot = SlotIx(u16::try_from(position).expect("parameter slot overflow"));
        match param {
            Some(local) => f.assign_slot(*local, slot),
            None => {
                f.self_slot = Some(slot);
                f.reserve_slot_floor(slot.0 + 1);
            }
        }
    }

    if returns_value {
        block_value(&mut f, &body, span);
    } else {
        block_stmts(&mut f, &body);
        f.emit(Op::LoadUnit, span);
    }
    f.emit(Op::Ret, span);

    let arity = u8::try_from(params.len()).expect("arity overflow");
    let function = f.finish(name, arity, 0);
    cx.define_function(func_id, function);
}
