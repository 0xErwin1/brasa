//! Top-level item nodes.
//!
//! Items have no sugar of their own; lowering copies their structure
//! with HIR IDs so the HIR is self-contained. The one shape difference
//! from the AST is [`Item::Stmt`], which holds a statement sequence.

use crate::stmt::LetStmt;
use crate::{Block, TypeExprId};

/// Import paths and `throws` clauses carry no node IDs, so the AST's
/// types are shared verbatim rather than duplicated.
pub use brasa_ast::{Import, ImportPath, Throws};

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
    pub constraint: Option<Constraint>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Param {
    SelfParam,
    Named { name: String, ty: TypeExprId },
}

/// A method signature inside an `interface` body or an inline anonymous
/// interface constraint; see `brasa_ast::IfaceMember` for why it may
/// declare `throws`.
#[derive(Debug, Clone, PartialEq)]
pub struct IfaceMember {
    pub name: String,
    pub params: Vec<Param>,
    pub ret: Option<TypeExprId>,
    pub throws: Option<Throws>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FuncDef {
    pub is_pub: bool,
    pub name: String,
    pub generics: Vec<GenericParam>,
    pub params: Vec<Param>,
    pub ret: Option<TypeExprId>,
    pub throws: Option<Throws>,
    pub body: Block,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Field {
    pub name: String,
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

#[derive(Debug, Clone, PartialEq)]
pub enum Item {
    Import(Import),
    FuncDef(FuncDef),
    StructDef(StructDef),
    EnumDef(EnumDef),
    InterfaceDef(InterfaceDef),
    TopLet(TopLet),
    /// A top-level statement. Unlike the AST's single `StmtId`, this
    /// holds a sequence: lowering one AST statement can produce several
    /// HIR statements (a compound assignment on a `Field`/`Index` target
    /// emits temp `let`s before the plain assignment).
    Stmt(Block),
}
