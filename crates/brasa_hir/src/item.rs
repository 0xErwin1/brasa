//! Top-level item nodes.
//!
//! Items have no sugar of their own; lowering copies their structure
//! with HIR IDs so the HIR is self-contained. The one shape difference
//! from the AST is [`Item::Stmt`], which holds a statement sequence.

use brasa_source::Span;

use crate::stmt::LetStmt;
use crate::{Block, TypeExprId};

/// Import paths and `throws` clauses carry no node IDs, so the AST's
/// types are shared verbatim rather than duplicated.
pub use brasa_ast::{Import, ImportPath, Throws, ThrowsType};

/// A generic parameter's constraint.
#[derive(Debug, Clone, PartialEq)]
pub enum Constraint {
    /// A named interface, e.g. `T: Comparable`.
    Named(String),
    /// An inline anonymous interface, e.g. `T: { toString(): string }`.
    Inline(Vec<IfaceMember>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct GenericParam {
    pub name: String,
    /// The span of the parameter name itself, copied from the AST, so
    /// diagnostics about the generic point at the name rather than the
    /// whole declaring item (spec: 06 — Diagnósticos).
    pub name_span: Span,
    pub constraint: Option<Constraint>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Param {
    SelfParam {
        span: Span,
    },
    Named {
        name: String,
        /// The span of the parameter name itself; see
        /// [`GenericParam::name_span`].
        name_span: Span,
        ty: TypeExprId,
    },
}

/// A method signature inside an `interface` body or an inline anonymous
/// interface constraint; see `brasa_ast::IfaceMember` for why it may
/// declare `throws`.
#[derive(Debug, Clone, PartialEq)]
pub struct IfaceMember {
    pub name: String,
    /// The span of the member name itself; see
    /// [`GenericParam::name_span`].
    pub name_span: Span,
    pub params: Vec<Param>,
    pub ret: Option<TypeExprId>,
    pub throws: Option<Throws>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FuncDef {
    pub is_pub: bool,
    pub name: String,
    /// The span of the function name itself; see
    /// [`GenericParam::name_span`].
    pub name_span: Span,
    pub generics: Vec<GenericParam>,
    pub params: Vec<Param>,
    pub ret: Option<TypeExprId>,
    pub throws: Option<Throws>,
    pub body: Block,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Field {
    pub name: String,
    /// The span of the field name itself; see
    /// [`GenericParam::name_span`].
    pub name_span: Span,
    pub ty: TypeExprId,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StructDef {
    pub is_pub: bool,
    pub name: String,
    pub generics: Vec<GenericParam>,
    pub fields: Vec<Field>,
    pub methods: Vec<FuncDef>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Variant {
    pub name: String,
    /// The span of the variant name itself; see
    /// [`GenericParam::name_span`].
    pub name_span: Span,
    pub fields: Vec<Field>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EnumDef {
    pub is_pub: bool,
    pub name: String,
    pub generics: Vec<GenericParam>,
    pub variants: Vec<Variant>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct InterfaceDef {
    pub is_pub: bool,
    pub name: String,
    pub generics: Vec<GenericParam>,
    pub methods: Vec<IfaceMember>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TopLet {
    pub is_pub: bool,
    pub let_stmt: LetStmt,
}

/// A `test "name" ... end` item.
///
/// A test is a body with a human-readable name and nothing else: no
/// parameters, no return type, no `throws` clause. It is not a function
/// — nothing can call it — and it is compiled only by `brasa test`, so a
/// normal run never pays for one.
#[derive(Debug, Clone, PartialEq)]
pub struct TestDef {
    pub name: String,
    /// The span of the name literal, so a failure points at the test
    /// rather than at the whole item.
    pub name_span: Span,
    pub body: Block,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Item {
    Import(Import),
    FuncDef(FuncDef),
    StructDef(StructDef),
    EnumDef(EnumDef),
    InterfaceDef(InterfaceDef),
    TopLet(TopLet),
    TestDef(TestDef),
    /// A top-level statement. Unlike the AST's single `StmtId`, this
    /// holds a sequence: lowering one AST statement can produce several
    /// HIR statements (a compound assignment on a `Field`/`Index` target
    /// emits temp `let`s before the plain assignment).
    Stmt(Block),
}
