//! Top-level item nodes.

use brasa_source::Span;

use crate::stmt::LetStmt;
use crate::{Block, StmtId, TypeExprId};

#[derive(Debug, Clone, PartialEq)]
pub enum ImportPath {
    /// A `::` path, in source order: `import std::fs` is
    /// `["std", "fs"]`, `import lib::helpers` is `["lib", "helpers"]`.
    ///
    /// The root decides what it names. `std` is reserved for the
    /// standard library and is never looked for on disk; every other
    /// root is a module resolved against the search path
    /// (`docs/spec/01-syntax.md`, modules).
    Path(Vec<String>),
    /// `import "utils.bras"` / `import "./sub/helpers.bras"`, resolved
    /// relative to the importing file.
    File(String),
}

impl ImportPath {
    /// The std module this import names, or `None` for anything else.
    ///
    /// Shared rather than re-derived per phase because the answer is not
    /// "is it a `::` path" — `lib::fs` is a `::` path naming a file
    /// module, and treating it as `std::fs` would silently bind the
    /// standard library's `fs` to someone else's code.
    pub fn std_module(&self) -> Option<&str> {
        let ImportPath::Path(segments) = self else {
            return None;
        };

        match segments.as_slice() {
            [root, module] if root == "std" => Some(module),
            _ => None,
        }
    }
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
    /// The span of the parameter name itself, so diagnostics about the
    /// generic point at the name rather than the whole declaring item
    /// (`docs/spec/06-diagnostics.md`).
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

/// One name in a `throws` list, with the span of the name token so
/// later phases can point diagnostics at it — like `CatchType::Named`,
/// it lives inline in its declaration and has no arena id (and therefore
/// no side-table span) of its own.
#[derive(Debug, Clone, PartialEq)]
pub struct ThrowsType {
    pub name: String,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Throws {
    Never,
    Types(Vec<ThrowsType>),
}

/// A method signature inside an `interface` body or an inline anonymous
/// interface constraint. Never carries generics, but MAY declare `throws`:
/// interfaces are contracts and error contracts are not inferred (see
/// `docs/spec/04-errors.md`), so a throwing interface method must state
/// its error set here.
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
    /// The span of the variant name itself; see
    /// [`GenericParam::name_span`].
    pub name_span: Span,
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
    Stmt(StmtId),
}
