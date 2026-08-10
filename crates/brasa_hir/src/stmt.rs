//! Core statement nodes.

use crate::{Block, ExprId, PatternId, TypeExprId};

#[derive(Debug, Clone, PartialEq)]
pub struct LetStmt {
    pub mutable: bool,
    pub name: String,
    pub ty: Option<TypeExprId>,
    pub value: ExprId,
}

/// The shared shape of `if`/`elsif`/`else`, used by both `Stmt::If` and
/// `Expr::If`, mirroring the AST's single-node treatment of the two
/// surface forms (`docs/spec/02-grammar.md`).
#[derive(Debug, Clone, PartialEq)]
pub struct IfNode {
    /// `if`, then each `elsif`, in source order.
    pub branches: Vec<(ExprId, Block)>,
    pub else_: Option<Block>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Stmt {
    Let(LetStmt),
    /// Plain assignment only: there is no compound-assignment operator in
    /// HIR. Lowering rewrote `x += e` into `x = x + e` (binding `Field`/
    /// `Index` receivers to temps first so they evaluate once), so no
    /// `AssignOp` enum exists at this level. As in the AST, later phases
    /// validate that `target` has lvalue shape.
    Assign {
        target: ExprId,
        value: ExprId,
    },
    Return(Option<ExprId>),
    Break,
    Continue,
    Throw(ExprId),
    If(IfNode),
    While {
        cond: ExprId,
        body: Block,
    },
    /// `for` stays a core node rather than desugaring into calls: per
    /// `docs/spec/03-types.md` ("`for` only iterates built-in types in
    /// v1": Vector, Map, Set, ranges, strings), there is no user-level
    /// iteration protocol to lower into, so this node itself is the
    /// iteration hook for the tree-walker and VM. Ranges stay lazy —
    /// iterating one never materializes a vector.
    For {
        pattern: PatternId,
        iterable: ExprId,
        body: Block,
    },
    Expr(ExprId),
}
