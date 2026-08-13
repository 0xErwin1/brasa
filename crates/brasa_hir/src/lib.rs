//! HIR for Brasa: the desugared core the rest of the compiler works on.
//!
//! Same container pattern as `brasa_ast` — per-kind [`brasa_arena::Store`]
//! arenas, typed `Copy` IDs, and a span side table per category — but the
//! node set is the small core left after AST→HIR lowering removes every
//! piece of sugar (`docs/spec/00-vision.md`'s HIR row): `|>` becomes a
//! plain call, `?.`/`??` become `match` over `Option`, string
//! interpolation becomes concatenation, and compound assignment becomes
//! plain assignment. The checker, error-set inference, tree-walker, and
//! codegen all consume this core only.
//!
//! HIR IDs are distinct types from AST IDs (an `ExprId` here indexes the
//! [`Hir`] arenas, never a `brasa_ast::Ast`), and the HIR is
//! self-contained: patterns, type expressions, and item structure are
//! copied into these arenas during lowering so the AST can be dropped
//! afterwards. Pure value types with no node IDs inside (operator enums,
//! literals, imports, `throws` clauses) are shared with `brasa_ast`
//! verbatim instead of duplicated.

pub mod dump;
pub mod expr;
pub mod item;
pub mod lower;
pub mod pattern;
pub mod stmt;
pub mod types;

pub use expr::*;
pub use item::*;
pub use lower::{LowerResult, Lowerer, SugarOrigin, lower};
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

/// A sequence of statements.
pub type Block = Vec<StmtId>;

/// Owns every HIR node category as its own arena, plus a parallel span
/// table per category. `Hir::alloc_*` is the only way to create a node
/// ID, so an ID is always paired with a span from the moment it exists.
/// Nodes synthesized by lowering carry the span of the sugar node they
/// were desugared from.
#[derive(Debug)]
pub struct Hir {
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

impl Default for Hir {
    fn default() -> Self {
        Self::new()
    }
}

impl Hir {
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
