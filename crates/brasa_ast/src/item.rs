//! Top-level item nodes.

use crate::stmt::LetStmt;
use crate::{Block, StmtId, TypeExprId};

#[derive(Debug, Clone, PartialEq)]
pub enum ImportPath {
    /// `import std::fs`: `["std", "fs"]`, in source order (the leading
    /// `std` segment is part of `std_path` itself, per the grammar).
    Std(Vec<String>),
    /// `import "utils.brs"` / `import "./sub/helpers.brs"`, resolved
    /// relative to the importing file.
    File(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Import {
    pub path: ImportPath,
}

/// A generic parameter's constraint (`gen_param`'s `constraint`
/// production).
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

#[derive(Debug, Clone, PartialEq)]
pub enum Throws {
    Never,
    Types(Vec<String>),
}

/// A method signature inside an `interface` body or an inline anonymous
/// interface constraint. Never carries generics, but MAY declare `throws`:
/// interfaces are contracts and error contracts are not inferred (see
/// `docs/spec/04-errors.md`), so a throwing interface method must state
/// its error set here.
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

/// A struct body (`struct_body = ( field NL | func_def NL )*`). Fields
/// and methods are kept in two separate lists rather than one interleaved
/// one: the relative order *between* a field and a method carries no
/// semantic meaning (name resolution does not care which was written
/// first), only the order within each group does, and that order is
/// preserved.
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
    /// Empty when the variant carries no payload (`Point` in the `Shape`
    /// example of `docs/spec/01-syntax.md`).
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
    Stmt(StmtId),
}
