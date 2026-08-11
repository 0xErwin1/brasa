//! Statement and block lowering: statements have net-zero stack effect;
//! `block_value` pushes exactly one value (the trailing expression
//! statement, or a trailing `if` when the block is consumed as a value
//! — mirroring the checker's and walker's block typing exactly).

use brasa_bytecode::Op;
use brasa_hir::{Block, Expr, ExprId, IfNode, Item, Stmt, StmtId};
use brasa_resolver::Res;
use brasa_source::Span;
use brasa_typeck::Type;

use crate::expr::compile_expr;
use crate::func::{FnKind, FuncCx, LoopCx, PLACEHOLDER};
use crate::pattern::compile_for_binding;

pub(crate) fn compile_stmt(f: &mut FuncCx, id: StmtId) {
    let span = f.cx.hir.span_of_stmt(id);

    match f.cx.hir.stmt(id).clone() {
        Stmt::Let(let_stmt) => {
            compile_expr(f, let_stmt.value);
            match f.cx.res.stmt_locals.get(&id).copied() {
                Some(local) => {
                    let slot = f.slot_of(local);
                    f.emit(Op::StoreLocal(slot), span);
                }
                None => {
                    f.emit(Op::Pop, span);
                }
            }
        }
        Stmt::Assign { target, value } => assign(f, target, value, span),
        Stmt::Return(value) => compile_return(f, value, span),
        Stmt::Break => {
            let Some(pops_iterator) = f.loops.last().map(|l| l.pops_iterator_on_break) else {
                f.emit_fatal("brasa: `break` outside a loop", span);
                f.emit(Op::Pop, span);
                return;
            };
            if pops_iterator {
                f.emit(Op::Pop, span);
            }
            let jump = f.emit(Op::Jump(PLACEHOLDER), span);
            f.loops
                .last_mut()
                .expect("loop context checked above")
                .break_jumps
                .push(jump);
        }
        Stmt::Continue => match f.loops.last().map(|l| l.head) {
            Some(head) => {
                f.emit(Op::Jump(head), span);
            }
            None => {
                f.emit_fatal("brasa: `continue` outside a loop", span);
                f.emit(Op::Pop, span);
            }
        },
        Stmt::Throw(value) => {
            compile_expr(f, value);
            f.emit(Op::Throw, span);
        }
        Stmt::If(node) => if_stmt(f, &node, span),
        Stmt::While { cond, body } => {
            let head = f.here();
            compile_expr(f, cond);
            let exit_jump = f.emit(Op::JumpIfFalse(PLACEHOLDER), span);

            f.loops.push(LoopCx {
                head,
                break_jumps: Vec::new(),
                pops_iterator_on_break: false,
            });
            block_stmts(f, &body);
            f.emit(Op::Jump(head), span);

            let loop_cx = f.loops.pop().expect("loop context pushed above");
            let exit = f.here();
            f.patch(exit_jump, exit);
            for jump in loop_cx.break_jumps {
                f.patch(jump, exit);
            }
        }
        Stmt::For {
            pattern,
            iterable,
            body,
        } => {
            compile_expr(f, iterable);
            f.emit(Op::IterNew, span);
            let head = f.emit(Op::IterNext(PLACEHOLDER), span);

            f.loops.push(LoopCx {
                head,
                break_jumps: Vec::new(),
                pops_iterator_on_break: true,
            });
            compile_for_binding(f, pattern, span);
            block_stmts(f, &body);
            f.emit(Op::Jump(head), span);

            let loop_cx = f.loops.pop().expect("loop context pushed above");
            let exit = f.here();
            f.patch(head, exit);
            for jump in loop_cx.break_jumps {
                f.patch(jump, exit);
            }
        }
        Stmt::Expr(expr) => {
            compile_expr(f, expr);
            f.emit(Op::Pop, span);
        }
    }
}

fn assign(f: &mut FuncCx, target: ExprId, value: ExprId, span: Span) {
    match f.cx.hir.expr(target).clone() {
        Expr::Ident(name) => {
            compile_expr(f, value);
            match f.cx.res.expr_res.get(&target).copied() {
                Some(Res::Local(local)) => {
                    let slot = f.slot_of(local);
                    f.emit(Op::StoreLocal(slot), span);
                }
                Some(Res::Item(item)) if f.cx.global_of_item.contains_key(&item) => {
                    let global = f.cx.global_of_item[&item];
                    f.emit(Op::StoreGlobal(global), span);
                }
                _ => {
                    f.emit(Op::Pop, span);
                    f.emit_fatal(&format!("brasa: cannot assign to `{name}`"), span);
                    f.emit(Op::Pop, span);
                }
            }
        }
        Expr::Field { recv, name } => {
            compile_expr(f, recv);
            compile_expr(f, value);

            let index = match f.cx.types.expr_types.get(&recv) {
                Some(Type::Struct(item, _)) => match f.cx.hir.item(*item) {
                    Item::StructDef(def) => def
                        .fields
                        .iter()
                        .position(|field| field.name == name)
                        .map(|i| u16::try_from(i).unwrap_or(u16::MAX)),
                    _ => None,
                },
                _ => None,
            };

            match index {
                Some(index) => {
                    f.emit(Op::SetField(index), span);
                }
                None => {
                    f.emit(Op::Pop, span);
                    f.emit(Op::Pop, span);
                    f.emit_fatal(&format!("brasa: cannot assign to field `{name}`"), span);
                    f.emit(Op::Pop, span);
                }
            }
        }
        Expr::Index { recv, index } => {
            compile_expr(f, recv);
            compile_expr(f, index);
            compile_expr(f, value);
            f.emit(Op::SetIndex, span);
        }
        _ => {
            f.emit_fatal("brasa: invalid assignment target", span);
            f.emit(Op::Pop, span);
        }
    }
}

fn compile_return(f: &mut FuncCx, value: Option<ExprId>, span: Span) {
    match f.kind {
        FnKind::Lambda
        | FnKind::Func {
            returns_value: true,
        } => {
            match value {
                Some(expr) => compile_expr(f, expr),
                None => {
                    f.emit(Op::LoadUnit, span);
                }
            }
            f.emit(Op::Ret, span);
        }
        // A function without a declared return type returns `unit`
        // regardless of the `return` expression's value.
        FnKind::Func {
            returns_value: false,
        } => {
            if let Some(expr) = value {
                compile_expr(f, expr);
                f.emit(Op::Pop, span);
            }
            f.emit(Op::LoadUnit, span);
            f.emit(Op::Ret, span);
        }
        FnKind::Toplevel => {
            if let Some(expr) = value {
                compile_expr(f, expr);
                f.emit(Op::Pop, span);
            }
            f.emit_fatal("brasa: control-flow signal escaped to the top level", span);
            f.emit(Op::Pop, span);
        }
    }
}

pub(crate) fn block_stmts(f: &mut FuncCx, block: &Block) {
    for &stmt in block {
        compile_stmt(f, stmt);
    }
}

pub(crate) fn block_value(f: &mut FuncCx, block: &Block, span: Span) {
    let Some((&last, init)) = block.split_last() else {
        f.emit(Op::LoadUnit, span);
        return;
    };

    for &stmt in init {
        compile_stmt(f, stmt);
    }

    match f.cx.hir.stmt(last).clone() {
        Stmt::Expr(expr) => compile_expr(f, expr),
        Stmt::If(node) => if_value(f, &node, f.cx.hir.span_of_stmt(last)),
        _ => {
            compile_stmt(f, last);
            f.emit(Op::LoadUnit, span);
        }
    }
}

fn if_stmt(f: &mut FuncCx, node: &IfNode, span: Span) {
    let mut end_jumps = Vec::new();

    for (cond, block) in &node.branches {
        compile_expr(f, *cond);
        let next_branch = f.emit(Op::JumpIfFalse(PLACEHOLDER), span);
        block_stmts(f, block);
        end_jumps.push(f.emit(Op::Jump(PLACEHOLDER), span));
        let next = f.here();
        f.patch(next_branch, next);
    }

    if let Some(block) = &node.else_ {
        block_stmts(f, block);
    }

    let end = f.here();
    for jump in end_jumps {
        f.patch(jump, end);
    }
}

pub(crate) fn if_value(f: &mut FuncCx, node: &IfNode, span: Span) {
    let mut end_jumps = Vec::new();

    for (cond, block) in &node.branches {
        compile_expr(f, *cond);
        let next_branch = f.emit(Op::JumpIfFalse(PLACEHOLDER), span);
        block_value(f, block, span);
        end_jumps.push(f.emit(Op::Jump(PLACEHOLDER), span));
        let next = f.here();
        f.patch(next_branch, next);
    }

    match &node.else_ {
        Some(block) => block_value(f, block, span),
        None => {
            f.emit(Op::LoadUnit, span);
        }
    }

    let end = f.here();
    for jump in end_jumps {
        f.patch(jump, end);
    }
}
