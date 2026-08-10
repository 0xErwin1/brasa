//! AST for Brasa: index arenas, typed node IDs, and span side tables.
//!
//! Nodes live in per-kind [`brasa_arena::Store`]s and reference each other
//! through `Copy` IDs (`ExprId`, `StmtId`, ...) rather than through boxes
//! or references — the rustc/rust-analyzer pattern. A node is immutable
//! once allocated and never stores its own span; instead, [`Ast`] keeps a
//! side table of spans per category, indexed by the same ID, so a node
//! stays plain data and later phases can attach their own side tables the
//! same way. A `visitor` module and any `Display`/pretty-printing are out
//! of scope here (BRS-10 exercises this AST for real, through the
//! parser's snapshot tests).
//!
//! String interning is likewise out of scope: names are plain `String`
//! for now.

pub mod expr;
pub mod item;
pub mod pattern;
pub mod stmt;
pub mod types;

pub use expr::*;
pub use item::*;
pub use pattern::*;
pub use stmt::*;
pub use types::*;

use brasa_arena::{Id, Store};
use brasa_source::Span;

pub type ExprId = Id<Expr>;
pub type StmtId = Id<Stmt>;
pub type ItemId = Id<Item>;
pub type PatternId = Id<Pattern>;
pub type TypeExprId = Id<TypeExpr>;

/// A sequence of statements (`block = ( stmt NL )*`).
pub type Block = Vec<StmtId>;

/// Owns every node category as its own arena, plus a parallel span table
/// per category. `Ast::alloc_*` is the only way to create a node ID, so an
/// ID is always paired with a span from the moment it exists.
#[derive(Debug)]
pub struct Ast {
    exprs: Store<Expr>,
    expr_spans: Vec<Span>,
    stmts: Store<Stmt>,
    stmt_spans: Vec<Span>,
    items: Store<Item>,
    item_spans: Vec<Span>,
    patterns: Store<Pattern>,
    pattern_spans: Vec<Span>,
    type_exprs: Store<TypeExpr>,
    type_expr_spans: Vec<Span>,
}

impl Default for Ast {
    fn default() -> Self {
        Self::new()
    }
}

impl Ast {
    pub fn new() -> Self {
        Self {
            exprs: Store::new(),
            expr_spans: Vec::new(),
            stmts: Store::new(),
            stmt_spans: Vec::new(),
            items: Store::new(),
            item_spans: Vec::new(),
            patterns: Store::new(),
            pattern_spans: Vec::new(),
            type_exprs: Store::new(),
            type_expr_spans: Vec::new(),
        }
    }

    pub fn alloc_expr(&mut self, expr: Expr, span: Span) -> ExprId {
        let id = self.exprs.alloc(expr);
        self.expr_spans.push(span);
        id
    }

    pub fn expr(&self, id: ExprId) -> &Expr {
        self.exprs.get(&id)
    }

    pub fn span_of_expr(&self, id: ExprId) -> Span {
        self.expr_spans[id.index() as usize]
    }

    pub fn alloc_stmt(&mut self, stmt: Stmt, span: Span) -> StmtId {
        let id = self.stmts.alloc(stmt);
        self.stmt_spans.push(span);
        id
    }

    pub fn stmt(&self, id: StmtId) -> &Stmt {
        self.stmts.get(&id)
    }

    pub fn span_of_stmt(&self, id: StmtId) -> Span {
        self.stmt_spans[id.index() as usize]
    }

    pub fn alloc_item(&mut self, item: Item, span: Span) -> ItemId {
        let id = self.items.alloc(item);
        self.item_spans.push(span);
        id
    }

    pub fn item(&self, id: ItemId) -> &Item {
        self.items.get(&id)
    }

    pub fn span_of_item(&self, id: ItemId) -> Span {
        self.item_spans[id.index() as usize]
    }

    pub fn alloc_pattern(&mut self, pattern: Pattern, span: Span) -> PatternId {
        let id = self.patterns.alloc(pattern);
        self.pattern_spans.push(span);
        id
    }

    pub fn pattern(&self, id: PatternId) -> &Pattern {
        self.patterns.get(&id)
    }

    pub fn span_of_pattern(&self, id: PatternId) -> Span {
        self.pattern_spans[id.index() as usize]
    }

    pub fn alloc_type_expr(&mut self, type_expr: TypeExpr, span: Span) -> TypeExprId {
        let id = self.type_exprs.alloc(type_expr);
        self.type_expr_spans.push(span);
        id
    }

    pub fn type_expr(&self, id: TypeExprId) -> &TypeExpr {
        self.type_exprs.get(&id)
    }

    pub fn span_of_type_expr(&self, id: TypeExprId) -> Span {
        self.type_expr_spans[id.index() as usize]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use brasa_source::{BytePosition, FileId};

    fn span(file: FileId, start: u32, end: u32) -> Span {
        Span::new(file, BytePosition(start), BytePosition(end))
    }

    /// Builds the shape of:
    ///
    /// ```text
    /// def fib(n: int): int
    ///   if n < 2
    ///     n
    ///   else
    ///     fib(n - 1) + fib(n - 2)
    ///   end
    /// end
    /// ```
    ///
    /// This is a smoke test for cross-category navigation (item -> stmt ->
    /// expr) and `span_of_*`, not an exhaustive AST test; the parser's
    /// snapshot tests (BRS-10) exercise the full grammar.
    #[test]
    fn builds_and_navigates_a_fib_like_function() {
        let file = FileId::new(0);
        let mut ast = Ast::new();

        let int_ty = ast.alloc_type_expr(
            TypeExpr::Named {
                name: "int".to_string(),
                args: vec![],
            },
            span(file, 17, 20),
        );

        let cond_lhs = ast.alloc_expr(Expr::Ident("n".to_string()), span(file, 24, 25));
        let cond_rhs = ast.alloc_expr(Expr::Int(2), span(file, 28, 29));
        let cond = ast.alloc_expr(
            Expr::Binary {
                op: BinaryOp::Lt,
                lhs: cond_lhs,
                rhs: cond_rhs,
            },
            span(file, 24, 29),
        );

        let then_n = ast.alloc_expr(Expr::Ident("n".to_string()), span(file, 34, 35));
        let then_block: Block = vec![ast.alloc_stmt(Stmt::Expr(then_n), span(file, 34, 35))];

        let lhs_n = ast.alloc_expr(Expr::Ident("n".to_string()), span(file, 44, 45));
        let one = ast.alloc_expr(Expr::Int(1), span(file, 48, 49));
        let n_minus_1 = ast.alloc_expr(
            Expr::Binary {
                op: BinaryOp::Sub,
                lhs: lhs_n,
                rhs: one,
            },
            span(file, 44, 49),
        );
        let fib_a = ast.alloc_expr(Expr::Ident("fib".to_string()), span(file, 40, 44));
        let call_a = ast.alloc_expr(
            Expr::Call {
                callee: fib_a,
                args: vec![n_minus_1],
            },
            span(file, 40, 50),
        );

        let rhs_n = ast.alloc_expr(Expr::Ident("n".to_string()), span(file, 57, 58));
        let two = ast.alloc_expr(Expr::Int(2), span(file, 61, 62));
        let n_minus_2 = ast.alloc_expr(
            Expr::Binary {
                op: BinaryOp::Sub,
                lhs: rhs_n,
                rhs: two,
            },
            span(file, 57, 62),
        );
        let fib_b = ast.alloc_expr(Expr::Ident("fib".to_string()), span(file, 53, 57));
        let call_b = ast.alloc_expr(
            Expr::Call {
                callee: fib_b,
                args: vec![n_minus_2],
            },
            span(file, 53, 63),
        );

        let sum = ast.alloc_expr(
            Expr::Binary {
                op: BinaryOp::Add,
                lhs: call_a,
                rhs: call_b,
            },
            span(file, 40, 63),
        );
        let else_block: Block = vec![ast.alloc_stmt(Stmt::Expr(sum), span(file, 40, 63))];

        let if_stmt = ast.alloc_stmt(
            Stmt::If(IfNode {
                branches: vec![(cond, then_block)],
                else_: Some(else_block),
            }),
            span(file, 21, 65),
        );

        let func = FuncDef {
            is_pub: false,
            name: "fib".to_string(),
            generics: vec![],
            params: vec![Param::Named {
                name: "n".to_string(),
                ty: int_ty,
            }],
            ret: Some(int_ty),
            throws: None,
            body: vec![if_stmt],
        };
        let item = ast.alloc_item(Item::FuncDef(func), span(file, 0, 69));

        let Item::FuncDef(f) = ast.item(item) else {
            panic!("expected Item::FuncDef, got {:?}", ast.item(item));
        };
        assert_eq!(f.name, "fib");
        assert_eq!(f.body.len(), 1);

        let Stmt::If(IfNode { branches, else_ }) = ast.stmt(f.body[0]) else {
            panic!("expected Stmt::If, got {:?}", ast.stmt(f.body[0]));
        };
        assert_eq!(branches.len(), 1);
        assert!(else_.is_some());

        let (cond_id, then_branch) = &branches[0];
        assert!(matches!(
            ast.expr(*cond_id),
            Expr::Binary {
                op: BinaryOp::Lt,
                ..
            }
        ));
        assert_eq!(then_branch.len(), 1);

        assert_eq!(ast.span_of_item(item), span(file, 0, 69));
        assert_eq!(ast.span_of_type_expr(int_ty), span(file, 17, 20));
    }
}
