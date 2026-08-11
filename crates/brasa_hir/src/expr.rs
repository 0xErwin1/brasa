//! Core expression nodes.
//!
//! The AST's `Pipe`, `Coalesce`, `SafeNav`, and interpolated `StringLit`
//! do not exist here: lowering desugars each of them exactly once (see
//! `docs/spec/00-vision.md`'s HIR row and `crate::lower`). Two nodes are
//! HIR-only: [`Expr::OptionWrap`] and [`Expr::ToString`], both introduced
//! by lowering to carry type-directed behavior the syntax alone cannot
//! resolve.

use brasa_source::Span;

use crate::stmt::IfNode;
use crate::{Block, ExprId, PatternId, TypeExprId};

/// Operator enums carry no node IDs, so they are shared with the AST
/// verbatim rather than duplicated.
pub use brasa_ast::{BinaryOp, CatchType, UnaryOp};

#[derive(Debug, Clone, PartialEq)]
pub struct LambdaParam {
    pub name: String,
    /// The span of the parameter name itself, copied from the AST, so
    /// diagnostics about the parameter point at the name rather than the
    /// whole lambda (`docs/spec/06-diagnostics.md`).
    pub name_span: Span,
    pub ty: Option<TypeExprId>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum LambdaBody {
    Expr(ExprId),
    Block(Block),
}

/// The shared `( expr | NL block )` shape of a match/catch arm.
#[derive(Debug, Clone, PartialEq)]
pub enum ArmBody {
    Expr(ExprId),
    Block(Block),
}

#[derive(Debug, Clone, PartialEq)]
pub struct MatchArm {
    pub pattern: PatternId,
    pub guard: Option<ExprId>,
    pub body: ArmBody,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CatchArm {
    pub types: Vec<CatchType>,
    pub guard: Option<ExprId>,
    pub body: ArmBody,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Int(i64),
    Float(f64),
    Bool(bool),
    Char(char),
    Unit,
    /// A plain string literal. Interpolated AST string literals are gone
    /// by this point: lowering turned them into `+` concatenation over
    /// `Str` and [`Expr::ToString`] pieces. The text is already
    /// unescaped, and the raw/cooked source distinction is irrelevant
    /// post-lowering.
    Str(String),
    Ident(String),
    SelfExpr,
    Call {
        callee: ExprId,
        args: Vec<ExprId>,
    },
    Field {
        recv: ExprId,
        name: String,
    },
    Index {
        recv: ExprId,
        index: ExprId,
    },
    Unary {
        op: UnaryOp,
        operand: ExprId,
    },
    Binary {
        op: BinaryOp,
        lhs: ExprId,
        rhs: ExprId,
    },
    /// "Wrap in `Some` unless the operand is already an `Option`."
    ///
    /// Produced only by `?.` lowering for the member access in the `Some`
    /// arm. Per `docs/spec/03-types.md`'s operator table, `?.` flattens:
    /// the result is never a nested `Option`, so when the member itself
    /// yields `Option<R>` the whole expression is `Option<R>`, not
    /// `Option<Option<R>>`. Which of the two cases applies is
    /// type-directed and cannot be decided syntactically; the type
    /// checker resolves it (recording the answer in a side table), and
    /// the tree-walker/codegen consume that resolved form.
    OptionWrap(ExprId),
    /// Implicit `toString` conversion of the operand.
    ///
    /// Produced only by string-interpolation lowering for each `#{expr}`
    /// piece. Per `docs/spec/03-types.md`, every type has an
    /// automatically derived `toString`; the type checker later makes
    /// this a no-op for operands that are already strings.
    ToString(ExprId),
    Lambda {
        params: Vec<LambdaParam>,
        body: LambdaBody,
    },
    /// Shares `IfNode` with `Stmt::If`, mirroring the AST.
    If(IfNode),
    Match {
        scrutinee: ExprId,
        arms: Vec<MatchArm>,
    },
    VectorLit(Vec<ExprId>),
    MapLit(Vec<(ExprId, ExprId)>),
    /// `(a, b)`; always at least one element, mirroring the AST node.
    TupleLit(Vec<ExprId>),
    StructLit {
        type_name: String,
        fields: Vec<(String, ExprId)>,
    },
    Range {
        lo: ExprId,
        hi: ExprId,
        inclusive: bool,
    },
    /// Postfix `catch`/`catch_all` on `subject`; semantics in
    /// `docs/spec/04-errors.md`. Not sugar — error handling is core.
    Catch {
        subject: ExprId,
        exhaustive: bool,
        binding: String,
        arms: Vec<CatchArm>,
    },
    EnumCtor {
        name: String,
        args: Vec<ExprId>,
    },
}
