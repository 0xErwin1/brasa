//! Statement nodes.

use crate::{Block, ExprId, PatternId, TypeExprId};

#[derive(Debug, Clone, PartialEq)]
pub struct LetStmt {
    pub mutable: bool,
    pub name: String,
    pub ty: Option<TypeExprId>,
    pub value: ExprId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssignOp {
    Assign,
    AddAssign,
    SubAssign,
    MulAssign,
    DivAssign,
    RemAssign,
}

/// The shared shape of `if`/`elsif`/`else`, used by both `Stmt::If` and
/// `Expr::If` since the grammar treats them as one construct ("if
/// expresión vs sentencia: mismo nodo", `docs/spec/02-gramatica.md`). The
/// inline `then`-form's single-expression branches are normalized into
/// one-statement blocks when the AST is built, so both surface syntaxes
/// (`if ... NL block ... end` and `if ... then expr ... end`) share this
/// one representation; there is no separate "inline if" node.
#[derive(Debug, Clone, PartialEq)]
pub struct IfNode {
    /// `if`, then each `elsif`, in source order.
    pub branches: Vec<(ExprId, Block)>,
    pub else_: Option<Block>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Stmt {
    Let(LetStmt),
    /// `target` is built from `Expr::Ident`/`Expr::Field`/`Expr::Index`
    /// nodes matching the `lvalue` grammar production (`IDENT ( "." IDENT
    /// | "[" expr "]" )*`); later phases validate that shape.
    Assign {
        target: ExprId,
        op: AssignOp,
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
    For {
        pattern: PatternId,
        iterable: ExprId,
        body: Block,
    },
    Expr(ExprId),
}
