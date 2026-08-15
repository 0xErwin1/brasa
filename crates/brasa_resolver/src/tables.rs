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
/// (spec: 05 — Stdlib de scripting).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BuiltinValue {
    Puts,
    Print,
    /// `assert(cond)` and `assertEq(a, b)`: prelude functions rather
    /// than test-only syntax, because an assertion is useful in a script
    /// too. Both compile to the internal `<assert-failed>` raiser
    /// (`panics.AssertionFailed`), so neither needs an instruction of
    /// its own.
    Assert,
    AssertEq,
    /// `concurrent(fn(Scope) -> T) -> T` (spec: 08 — Concurrencia
    /// estructurada, BRS-133): a prelude function like `print` rather
    /// than a module member, because it is language surface — opening a
    /// scope reads like a control structure, not like a library call.
    Concurrent,
}

impl BuiltinValue {
    pub fn name(self) -> &'static str {
        match self {
            BuiltinValue::Puts => "puts",
            BuiltinValue::Print => "print",
            BuiltinValue::Assert => "assert",
            BuiltinValue::AssertEq => "assertEq",
            BuiltinValue::Concurrent => "concurrent",
        }
    }
}

/// Prelude types and interfaces, predeclared in the outermost scope:
/// primitives and core containers per spec: 05 — Stdlib de scripting, stdlib
/// interfaces per spec: 03 — Sistema de tipos.
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
    /// The compiler-known `Json` type (spec: 05 — Stdlib de scripting, BRS-34):
    /// predeclared like `Option`, so annotations can name it; values
    /// only come from `json.parse` (importing `std::json`).
    Json,
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
            BuiltinType::Json => "Json",
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

/// The closed panic union of spec: 04 — Sistema de errores, by qualified
/// name, in spec order. This is the canonical list: the resolver
/// validates `panics.`-qualified `catch` arm names against it, and the
/// VM raises by the same names — a unit test in `brasa_vm::vm` asserts
/// the two lists stay identical.
pub const PANIC_UNION: &[&str] = &[
    "panics.IndexOutOfBounds",
    "panics.DivisionByZero",
    "panics.IntegerOverflow",
    "panics.AssertionFailed",
    "panics.StackOverflow",
];

/// The canonical qualified name of the native `string` parse error
/// (spec: 05 — Stdlib de scripting: `toInt`/`toFloat` throw it).
pub const STRING_PARSE_ERROR: &str = "string.ParseError";

/// The canonical qualified name of the native `string` regex error
/// (spec: 05 — Stdlib de scripting: the regex methods throw it when the
/// pattern argument is not a valid regex).
pub const STRING_REGEX_ERROR: &str = "string.RegexError";

/// The canonical qualified name of the native `proc` non-zero-exit
/// error (spec: 05 — Stdlib de scripting: `proc.run`/`proc.shell` throw it
/// when the child exits with a non-zero code).
pub const PROC_NON_ZERO_EXIT: &str = "proc.NonZeroExit";

/// The canonical qualified name of the native `proc` spawn error
/// (spec: 05 — Stdlib de scripting: every runner throws it when the child
/// cannot start — missing binary, permission denied, empty command).
pub const PROC_SPAWN_ERROR: &str = "proc.SpawnError";

/// The canonical qualified name of the native `fs` not-found error
/// (spec: 05 — Stdlib de scripting, BRS-33: a path that does not exist).
pub const FS_NOT_FOUND: &str = "fs.NotFound";

/// The canonical qualified name of the native `fs` permission error
/// (spec: 05 — Stdlib de scripting, BRS-33: the OS denied the operation).
pub const FS_DENIED: &str = "fs.Denied";

/// The canonical qualified name of the native `fs` catch-all I/O error
/// (spec: 05 — Stdlib de scripting, BRS-33: every other OS failure, carrying
/// the OS message).
pub const FS_IO_ERROR: &str = "fs.IoError";

/// The canonical qualified name of the native `json` parse error
/// (spec: 05 — Stdlib de scripting, BRS-34: `json.parse` throws it when the
/// input is not valid JSON).
pub const JSON_PARSE_ERROR: &str = "json.ParseError";

/// The canonical qualified name of the native `json` value error
/// (spec: 05 — Stdlib de scripting, BRS-34: `json.of` and
/// `json.stringify` throw it for a language value that has no JSON
/// representation).
pub const JSON_VALUE_ERROR: &str = "json.ValueError";

/// The canonical qualified name of the native `http` request error
/// (spec: 05 — Stdlib de scripting, BRS-113): a request that never produced a
/// response — DNS, connection, TLS, or timeout. A non-2xx status is an
/// answer, not an error, so it is not here.
pub const HTTP_REQUEST_ERROR: &str = "http.RequestError";

/// The canonical qualified name of the native structured-concurrency
/// scope error (spec: 08 — Concurrencia estructurada, BRS-133):
/// `scope.spawn` after the scope's `concurrent` block returned.
pub const CONCURRENT_SCOPE_EXITED: &str = "concurrent.ScopeExited";

/// The canonical qualified name of the native structured-concurrency
/// cancellation error (spec: 08 — Concurrencia estructurada, BRS-133):
/// what a cancelled task's suspension points raise while its scope
/// tears down. Cancellation is cooperative — code between suspension
/// points is never interrupted.
pub const CONCURRENT_CANCELLED: &str = "concurrent.Cancelled";

/// The canonical qualified name of the native `time` parse error
/// (spec: 05 — Stdlib de scripting, BRS-35): `time.parseIso` throws it for a
/// string that is not an RFC 3339 timestamp — a malformed shape, a field
/// outside the calendar, or a missing UTC offset. Reading the clock still
/// cannot fail; reading a string can.
pub const TIME_PARSE_ERROR: &str = "time.ParseError";

/// The canonical qualified name of the native `cli` usage error
/// (spec: 05 — Stdlib de scripting, BRS-112): a command line the declaration
/// does not accept. Catchable rather than a panic, because the script
/// decides its own exit status.
pub const CLI_USAGE_ERROR: &str = "cli.UsageError";

/// The closed list of stdlib-native errors whose namespaces have
/// landed, by qualified dotted name (spec: 05 — Stdlib de scripting). This is
/// the canonical list, like [`PANIC_UNION`]: the resolver validates
/// dotted `catch` arm names in these namespaces against it, the
/// error-set pass tags native throwers with these names, and the
/// interpreter raises them verbatim. Every M4 stdlib error namespace
/// sketched by the spec has landed (`json` closed with BRS-34); other
/// dotted roots stay unchecked until their modules close. Unlike
/// panics, these ARE errors: they appear in error-sets and `_` catches
/// them (spec: 04 — Sistema de errores).
pub const NATIVE_ERRORS: &[&str] = &[
    STRING_PARSE_ERROR,
    STRING_REGEX_ERROR,
    PROC_NON_ZERO_EXIT,
    PROC_SPAWN_ERROR,
    FS_NOT_FOUND,
    FS_DENIED,
    FS_IO_ERROR,
    JSON_PARSE_ERROR,
    JSON_VALUE_ERROR,
    HTTP_REQUEST_ERROR,
    CLI_USAGE_ERROR,
    CONCURRENT_SCOPE_EXITED,
    CONCURRENT_CANCELLED,
    TIME_PARSE_ERROR,
];

/// Whether a dotted `catch`-arm name lives in a native-error namespace
/// that has landed (one of the roots appearing in [`NATIVE_ERRORS`]).
/// Names in landed namespaces are validated against the closed list;
/// other dotted roots stay unchecked until their modules land.
pub fn native_error_namespace_landed(name: &str) -> bool {
    let Some((root, _)) = name.split_once('.') else {
        return false;
    };

    NATIVE_ERRORS
        .iter()
        .any(|error| error.split_once('.').is_some_and(|(ns, _)| ns == root))
}

/// What a value-namespace reference resolved to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Res {
    Local(LocalId),
    /// A module-level `FuncDef` or `TopLet` item.
    Item(ItemId),
    /// The module handle bound by an `Item::Import`; member access stays
    /// unresolved until the type checker (spec: 01 — Sintaxis, no
    /// selective import — all access is qualified).
    Module(ItemId),
    Builtin(BuiltinValue),
    /// `self` inside a method whose parameter list contains
    /// `Param::SelfParam` (spec: 01 — Sintaxis).
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
    /// (spec: 03 — Sistema de tipos).
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
    /// (spec: 01 — Sintaxis, collection literals). Never appears
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
    /// (catch expr, arm index, index within the arm's `|` group).
    /// `panics.X` names live in [`Resolutions::catch_arm_panics`],
    /// stdlib-native error names in
    /// [`Resolutions::catch_arm_native_errors`]; dotted names in
    /// namespaces that have not landed yet are absent until they
    /// close.
    pub catch_arm_types: HashMap<(ExprId, usize, usize), TypeRes>,
    /// `catch` arm names matching a member of the closed panic union
    /// ([`PANIC_UNION`]), keyed like [`Resolutions::catch_arm_types`];
    /// the value is the canonical qualified name. Kept in a separate
    /// table on purpose: panics are not error types
    /// (spec: 04 — Sistema de errores) — they never subtract from error-sets
    /// and never count toward `catch!` exhaustiveness, so the
    /// error-set checks, which only consume `catch_arm_types`, must not
    /// see them.
    pub catch_arm_panics: HashMap<(ExprId, usize, usize), &'static str>,
    /// `catch` arm names matching a member of the closed native-error
    /// list ([`NATIVE_ERRORS`]), keyed like
    /// [`Resolutions::catch_arm_types`]; the value is the canonical
    /// qualified name. A separate table from both `catch_arm_types`
    /// (native errors resolve to no `TypeRes` — they are not types in
    /// scope) and `catch_arm_panics` (native errors ARE errors: they
    /// subtract from error-sets and count toward `catch!`
    /// exhaustiveness, panics do neither).
    pub catch_arm_native_errors: HashMap<(ExprId, usize, usize), &'static str>,
    /// An interface member's declared `throws`, keyed by `(interface,
    /// member index)` and aligned with the names it lists — the same
    /// shape [`Resolutions::throws_types`] has for a function, and a
    /// separate table for the same reason `catch_arm_native_errors` is:
    /// an interface member has no [`DefRef`], since it is a signature
    /// rather than a definition.
    ///
    /// Resolved but unused until BRS-141: satisfaction is structural
    /// (spec: 03 — Sistema de tipos), so nothing checks that a method
    /// matching a member honours the contract that member states. That
    /// check needs error sets, which arrive a pass later.
    pub iface_member_throws: HashMap<(ItemId, usize), Vec<Option<TypeRes>>>,
    /// The native-error half of [`Resolutions::iface_member_throws`],
    /// keyed by `(interface, member index, name index)`.
    pub iface_member_throws_natives: HashMap<(ItemId, usize, usize), &'static str>,
    /// Resolved `throws Type | ...` declaration lists, aligned with the
    /// declaring function/method's `Throws::Types` names; `None` marks a
    /// name that resolved to no type in scope — an unknown name
    /// (reported as `R003`), a `panics.` member (rejected as `E006` by
    /// the error-set pass), or a stdlib-native error, which is recorded
    /// in [`Resolutions::throws_native_errors`] instead. Interface
    /// members declare `throws` too; their names are validated during
    /// resolution but recorded in no table — enforcing their contracts
    /// needs interface-satisfaction integration, deferred to M3+ (see
    /// `Resolver::resolve_iface_member`).
    pub throws_types: HashMap<DefRef, Vec<Option<TypeRes>>>,
    /// `throws` names matching a member of the closed native-error list
    /// ([`NATIVE_ERRORS`]), keyed by the declaring function/method and
    /// the name's index in its `Throws::Types` list; the value is the
    /// canonical qualified name. A separate table from `throws_types`
    /// for the same reason `catch_arm_native_errors` is separate from
    /// `catch_arm_types`: a native error resolves to no `TypeRes` — it
    /// is not a type in scope — yet it IS an error, so it belongs in
    /// the declared contract the error-set pass verifies.
    pub throws_native_errors: HashMap<(DefRef, usize), &'static str>,
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
