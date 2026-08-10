//! Expression nodes.

use crate::stmt::IfNode;
use crate::{Block, ExprId, PatternId, TypeExprId};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOp {
    Neg,
    Not,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Pow,
    Eq,
    NotEq,
    Lt,
    LtEq,
    Gt,
    GtEq,
    /// `&&` and the `and` keyword are aliases with identical semantics
    /// (`docs/spec/02-grammar.md`), so both lex to this one operator.
    And,
    /// `||` and the `or` keyword alias.
    Or,
}

/// One segment of an interpolated string literal.
#[derive(Debug, Clone, PartialEq)]
pub enum StringPart {
    /// Literal text between interpolations. Already unescaped (see
    /// `brasa_token::unescape_string_text_checked`) unless `raw` is set, in
    /// which case `text` is the verbatim source (raw strings apply no
    /// escapes).
    Text { text: String, raw: bool },
    /// A `#{expr}` interpolation.
    Interp(ExprId),
}

/// The right-hand side of `|>`.
#[derive(Debug, Clone, PartialEq)]
pub enum PipeTarget {
    /// `a |> f(b, c)`: `target` is the already-parsed call `f(b, c)`;
    /// AST->HIR lowering inserts `a` as its first argument.
    Call(ExprId),
    /// `a |> .m(b)`, equivalent to `a.m(b)`.
    Method { name: String, args: Vec<ExprId> },
}

#[derive(Debug, Clone, PartialEq)]
pub struct LambdaParam {
    pub name: String,
    pub ty: Option<TypeExprId>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum LambdaBody {
    /// `|x| expr`.
    Expr(ExprId),
    /// `do |x| NL block end`.
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
pub enum CatchType {
    Named(String),
    Wildcard,
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
    StringLit {
        parts: Vec<StringPart>,
    },
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
    /// `recv?.name` (safe field access) or `recv?.name(args)` (safe
    /// method call). Per `docs/spec/02-grammar.md`'s `postfix`
    /// production, a trailing `(args)` is technically its own postfix
    /// operation and could instead be modeled as a `Call` wrapping a bare
    /// `SafeNav`; this AST folds it into `SafeNav` directly instead, so
    /// the node that must short-circuit on `None` is the same node that
    /// carries the call, rather than splitting that behavior across two
    /// nested nodes. See BRS-9 design notes.
    SafeNav {
        recv: ExprId,
        name: String,
        args: Option<Vec<ExprId>>,
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
    /// `a |> f(b)` / `a |> .m(b)`. Kept as its own node; desugaring into a
    /// plain call happens once, in AST->HIR lowering
    /// (`docs/spec/00-vision.md`), not in the parser.
    Pipe {
        lhs: ExprId,
        target: PipeTarget,
    },
    /// `a ?? b`: the null-coalescing operator. Kept raw, the same way
    /// `Pipe` and `SafeNav` are: `docs/spec/00-vision.md`'s HIR row lists
    /// `?./?? -> match over Option` as a lowering step, so desugaring
    /// into a `match` over `Option::Some`/`Option::None` happens once, in
    /// AST->HIR lowering, not in the parser.
    Coalesce {
        lhs: ExprId,
        rhs: ExprId,
    },
    Lambda {
        params: Vec<LambdaParam>,
        body: LambdaBody,
    },
    /// Shares `IfNode` with `Stmt::If`; see that type's docs for why.
    If(IfNode),
    Match {
        scrutinee: ExprId,
        arms: Vec<MatchArm>,
    },
    VectorLit(Vec<ExprId>),
    MapLit(Vec<(ExprId, ExprId)>),
    StructLit {
        type_name: String,
        fields: Vec<(String, ExprId)>,
    },
    Range {
        lo: ExprId,
        hi: ExprId,
        inclusive: bool,
    },
    /// Postfix `catch`/`catch_all` on `subject`. `exhaustive` is `true`
    /// for `catch_all`; unhandled catch types re-throw only when it is
    /// `false`; full semantics in `docs/spec/04-errors.md`.
    Catch {
        subject: ExprId,
        exhaustive: bool,
        binding: String,
        arms: Vec<CatchArm>,
    },
    /// `TYPE_IDENT` or `TYPE_IDENT(args)`. `args` is empty both for a bare
    /// reference (e.g. a unit variant like `None`) and for an explicit
    /// `()` call, mirroring `Pattern::Ctor`; the two forms carry no
    /// distinct semantics yet and can be told apart from source text via
    /// the node's span if that ever changes.
    EnumCtor {
        name: String,
        args: Vec<ExprId>,
    },
}
