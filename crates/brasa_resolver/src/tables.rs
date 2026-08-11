//! Resolution tables: what every name in the HIR resolved to.
//!
//! Every reference the resolver settles is queryable by its HIR node ID,
//! so the type checker never re-walks scopes. References are recorded in
//! `HashMap`s keyed by the arena IDs (which are `Hash`) rather than dense
//! vectors: only a minority of nodes carry a resolution (an `Ident` does,
//! a `Binary` does not), so dense per-arena tables would be mostly empty.
//! Binding sites get a dense [`LocalId`] index into [`Resolutions::locals`]
//! because every local is looked up by the later phases.

use std::collections::HashMap;

use brasa_hir::{ExprId, ItemId, PatternId, StmtId, TypeExprId};
use brasa_source::Span;

/// Index of one value binding site (parameter, `let`, lambda parameter,
/// pattern binding, or `catch` binding) into [`Resolutions::locals`].
///
/// Top-level `let`s are items, not locals: references to them resolve to
/// [`Res::Item`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LocalId(pub u32);

/// What kind of binding site introduced a local.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BinderKind {
    Param,
    Let,
    LambdaParam,
    PatternBinding,
    CatchBinding,
}

/// Side information about one local binding.
///
/// `mutable` is only ever `true` for `let mut` bindings; the resolver
/// records it but does not enforce assignment rules (that is the type
/// checker's job). `ty` is the declared annotation when the binding site
/// carries one (`let x: int`, `x: int` parameters, annotated lambda
/// parameters); pattern and `catch` bindings never have one.
///
/// `span` is the most precise span the HIR offers for the binding site:
/// the parameter's own name for function/method and lambda parameters,
/// the `let` statement for lets, the pattern node for pattern bindings,
/// and the enclosing `catch` expression for `catch` bindings (the one
/// binder with no name node of its own).
#[derive(Debug, Clone)]
pub struct LocalInfo {
    pub name: String,
    pub mutable: bool,
    pub span: Span,
    pub kind: BinderKind,
    pub ty: Option<TypeExprId>,
}

/// A definition that owns parameters and/or generic parameters: either a
/// top-level item (function, struct, enum, interface) or a struct method
/// (which has no `ItemId` of its own — it is addressed as the owning
/// struct item plus its index in `StructDef::methods`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DefRef {
    Item(ItemId),
    Method { owner: ItemId, index: usize },
}

/// Prelude values, predeclared in the outermost scope
/// (`docs/spec/05-stdlib.md`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BuiltinValue {
    Puts,
    Print,
}

impl BuiltinValue {
    pub fn name(self) -> &'static str {
        match self {
            BuiltinValue::Puts => "puts",
            BuiltinValue::Print => "print",
        }
    }
}

/// Prelude types and interfaces, predeclared in the outermost scope:
/// primitives and core containers per `docs/spec/05-stdlib.md`, stdlib
/// interfaces per `docs/spec/03-types.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BuiltinType {
    Int,
    Float,
    Bool,
    String,
    Char,
    Unit,
    Option,
    Vector,
    Map,
    Set,
    Range,
    Comparable,
    Printable,
    Hashable,
}

impl BuiltinType {
    pub fn name(self) -> &'static str {
        match self {
            BuiltinType::Int => "int",
            BuiltinType::Float => "float",
            BuiltinType::Bool => "bool",
            BuiltinType::String => "string",
            BuiltinType::Char => "char",
            BuiltinType::Unit => "unit",
            BuiltinType::Option => "Option",
            BuiltinType::Vector => "Vector",
            BuiltinType::Map => "Map",
            BuiltinType::Set => "Set",
            BuiltinType::Range => "Range",
            BuiltinType::Comparable => "Comparable",
            BuiltinType::Printable => "Printable",
            BuiltinType::Hashable => "Hashable",
        }
    }

    /// Whether this builtin lives in the interface subset of the type
    /// namespace (usable as a generic constraint).
    pub fn is_interface(self) -> bool {
        matches!(
            self,
            BuiltinType::Comparable | BuiltinType::Printable | BuiltinType::Hashable
        )
    }
}

/// What a value-namespace reference resolved to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Res {
    Local(LocalId),
    /// A module-level `FuncDef` or `TopLet` item.
    Item(ItemId),
    /// The module handle bound by an `Item::Import`; member access stays
    /// unresolved until the type checker (`docs/spec/01-syntax.md`, no
    /// selective import — all access is qualified).
    Module(ItemId),
    Builtin(BuiltinValue),
    /// `self` inside a method whose parameter list contains
    /// `Param::SelfParam` (`docs/spec/01-syntax.md`).
    SelfParam,
}

/// What a type-namespace reference resolved to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypeRes {
    /// A `StructDef`, `EnumDef`, or `InterfaceDef` item.
    Item(ItemId),
    Builtin(BuiltinType),
    GenericParam {
        owner: DefRef,
        index: usize,
    },
    /// `Self` inside an interface body or inline interface constraint
    /// (`docs/spec/03-types.md`).
    SelfType,
}

/// What a constructor reference (`Expr::EnumCtor` or `Pattern::Ctor`)
/// resolved to. Candidates are `Some`/`None`, the builtin `Set`
/// constructor (expression position only), plus the variants of every
/// enum in scope; the resolver requires the name to be unambiguous
/// (the type checker may refine this with expected-type context later).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CtorRes {
    OptionSome,
    OptionNone,
    /// The builtin `Set(vector)` constructor
    /// (`docs/spec/01-syntax.md`, collection literals). Never appears
    /// in `ctor_pattern_res`: `Set(...)` is not a valid pattern.
    SetCtor,
    EnumVariant {
        enum_item: ItemId,
        variant_index: usize,
    },
}

/// Every table produced by [`crate::resolve`]. See the module docs for
/// the keying strategy.
#[derive(Debug, Default)]
pub struct Resolutions {
    /// All value binding sites, in resolution (source) order.
    pub locals: Vec<LocalInfo>,
    /// `Expr::Ident` and `Expr::SelfExpr` references.
    pub expr_res: HashMap<ExprId, Res>,
    /// `Expr::EnumCtor` references.
    pub ctor_expr_res: HashMap<ExprId, CtorRes>,
    /// `Pattern::Ctor` references.
    pub ctor_pattern_res: HashMap<PatternId, CtorRes>,
    /// `Pattern::Binding` sites.
    pub pattern_locals: HashMap<PatternId, LocalId>,
    /// `Stmt::Let` sites (including lowering temps).
    pub stmt_locals: HashMap<StmtId, LocalId>,
    /// `Expr::Lambda` parameter lists, aligned with `Lambda::params`.
    pub lambda_params: HashMap<ExprId, Vec<LocalId>>,
    /// `Expr::Catch` binding sites.
    pub catch_bindings: HashMap<ExprId, LocalId>,
    /// Resolved bare `CatchType::Named` arm types. `CatchType` lives
    /// inline in its arm and has no arena id, so the key is positional:
    /// (catch expr, arm index, index within the arm's `|` group). Dotted
    /// names (`panics.X`, stdlib errors) are absent — they resolve in M4
    /// (BRS-24).
    pub catch_arm_types: HashMap<(ExprId, usize, usize), TypeRes>,
    /// Function/method parameter lists, aligned with `FuncDef::params`;
    /// `None` marks a `Param::SelfParam` slot (`self` is not a local).
    pub func_params: HashMap<DefRef, Vec<Option<LocalId>>>,
    /// Resolved `Constraint::Named` targets, keyed by owner and generic
    /// parameter index. Always an interface.
    pub constraint_res: HashMap<(DefRef, usize), TypeRes>,
    /// `Expr::StructLit` type names (resolved in the type namespace).
    pub struct_lit_res: HashMap<ExprId, TypeRes>,
    /// `TypeExpr::Named` references (`Tuple`/`Fn` nodes are structural
    /// and carry no name of their own).
    pub type_res: HashMap<TypeExprId, TypeRes>,
}

impl Resolutions {
    pub fn local(&self, id: LocalId) -> &LocalInfo {
        &self.locals[id.0 as usize]
    }
}
