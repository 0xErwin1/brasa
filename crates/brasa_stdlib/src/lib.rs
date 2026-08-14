//! The stdlib declaration tables: one table per stdlib module, read by
//! every layer that used to carry its own copy of the surface (BRS-96).
//!
//! Before this crate a member had to be hand-written into three places —
//! the `brasa_bytecode` id registry, the `brasa_typeck` signature table,
//! and the `brasa_vm` implementation — and nothing but a guard test
//! stopped the three from disagreeing. A table here declares the member
//! once; each layer derives its own view from it:
//!
//! | Layer | Derives |
//! |-------|---------|
//! | `brasa_typeck` | parameter and result types, by lowering [`TyDesc`] against the receiver |
//! | `brasa_vm` | the dispatch arm, by matching the generated member enum exhaustively |
//! | `brasa_bytecode` | nothing, but its id registry is cross-checked against these tables |
//!
//! This crate DECLARES; it never implements. It has no dependencies, so
//! every layer can read it without inverting the compiler's layering —
//! in particular the VM still knows nothing about the checker, and the
//! checker still knows nothing about the bytecode containers.
//!
//! The `BuiltinId` assignment stays where it is, in `brasa_bytecode`.
//! Ids are positions in a single ordered list and appending is the only
//! compatible extension, while these tables are grouped by module and
//! share one id across receiver kinds (`len` serves string, `Vector`,
//! `Map` and `Set`), so the two orders cannot be the same list.
//!
//! `docs/spec/05-stdlib.md` remains normative and hand-written: this
//! removes duplication inside the compiler, not between the compiler
//! and the spec.
//!
//! # Three table shapes
//!
//! A receiver type ([`method_table!`], `Vector<T>`), a free module
//! ([`module_table!`], `std::fs`) and a record ([`record_table!`],
//! `Output`) do NOT share a shape, because almost nothing about their
//! columns overlaps:
//!
//! | | receiver method | free module member | record member |
//! |---|---|---|---|
//! | receiver | a type with 0, 1 or 2 arguments, named by [`RecvShape`] | none | one concrete type |
//! | result | can depend on an argument ([`RetDesc`]) | a fixed type, or the checker's ([`ModuleKind::Custom`]) | always a fixed type |
//! | parameters | all required types | optional tail, and may be a rule ([`ParamDesc::Command`]) | none, or all required types |
//! | reached by | always a call | a call, or a read ([`ModuleKind::Constant`]) | a call, or a read ([`RecordKind::Field`]) |
//! | errors raised | part of the contract ([`MethodDecl::throws`]) | part of the contract ([`ModuleDecl::throws`]) | none |
//! | registry name | the bare name, shared across receivers | `module.name`, unique | the bare name, shared |
//!
//! One shape covering all three would carry an `elem`/`key`/`value`
//! case free modules and records can never use, an optional column
//! neither of the others fills, and a `Constant` case a method cannot
//! be — columns no test could reach. What they DO share is the type
//! language ([`TyDesc`] and [`ty!`]), which is where the real
//! duplication would have been.
//!
//! A name shared across receiver kinds holds ONE `brasa_bytecode` id
//! and is declared once per receiver that carries it, with the
//! signature that receiver gives it. `remove` answers a bool on a
//! `Set` and the removed value on a `Map`; the VM dispatches on the
//! runtime kind, so the two rows describe different receivers rather
//! than contradicting each other.
//!
//! # What is deliberately not data
//!
//! Two escape hatches exist, and both are narrow on purpose.
//! [`RetDesc::Custom`] and [`ModuleKind::Custom`] hand a member's
//! signature to the checker, which is right only when the signature is
//! not expressible here at all: `Vector.sort` exists only for orderable
//! elements, `math.abs` must answer in the kind it was given,
//! `rand.choice` is generic over an element a free module has no
//! receiver to name. A module member states its reason in the table,
//! and a guard keeps the delegated set from growing quietly.
//!
//! A record declares only its own members. The universal derived
//! `toString` is layered on by the checker for every type, so a row
//! spelling it would be a second answer to a question already
//! answered — [`RECORDS`] is guarded against exactly that.

/// A type in a declaration, written in the table's small type language
/// and lowered to the checker's `Type` by `brasa_typeck`.
///
/// The receiver-derived names ([`TyDesc::Elem`], [`TyDesc::Key`],
/// [`TyDesc::Value`]) are what let one row serve every instantiation of
/// a generic receiver. Which of them a row may use is the receiver's
/// [`RecvShape`], not the row's choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TyDesc {
    Int,
    Float,
    String,
    Bool,
    Unit,
    /// A range value, which `rand.int` draws from.
    Range,
    /// The checker's `Unknown`: unifies with everything, used where a
    /// member's type is decided by the call site rather than the table.
    Unknown,
    /// A character, which `string.chars` yields.
    Char,
    /// The receiver's element type — `T` in a `Vector<T>` or `Set<T>`.
    ///
    /// One of the three receiver-derived names. Which of them a row may
    /// mention is decided by the receiver: a `Vector` has an element
    /// and no key, a `Map` has a key and a value and no element, and a
    /// free module or a concrete receiver has none of the three. Naming
    /// one the receiver does not provide is a declaration bug, rejected
    /// by a guard rather than discovered when a user calls the member.
    Elem,
    /// The receiver's key type — `K` in a `Map<K, V>`.
    Key,
    /// The receiver's value type — `V` in a `Map<K, V>`.
    Value,
    /// The `Walk` record `fs.tryWalk` yields (BRS-66).
    Walk,
    /// The `Json` tree `json.parse` yields (BRS-34).
    Json,
    /// The `Output` record every `std::proc` runner yields (BRS-32).
    ProcOutput,
    /// The `Response` record `http.get`/`http.post` yield (BRS-113).
    HttpResponse,
    /// The `Args` record `cli.parse` yields (BRS-112).
    CliArgs,
    Vector(&'static TyDesc),
    Option(&'static TyDesc),
    Map(&'static TyDesc, &'static TyDesc),
    Set(&'static TyDesc),
    Tuple(&'static [TyDesc]),
    Fn(&'static [TyDesc], &'static TyDesc),
}

/// How a member's result type is decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetDesc {
    /// The common case: a type the table states outright.
    Ty(TyDesc),
    /// `Vector<T>.map((T) -> U) -> Vector<U>`: the element comes from
    /// the function argument, so it is known only after that argument is
    /// checked.
    VectorOfFnRet,
    /// The escape hatch: the checker owns both this member's result type
    /// AND whether it exists at all for a given receiver, because
    /// neither is expressible as data (`Vector.sort` exists only for
    /// orderable elements, `Vector.flatten` only for nested vectors).
    ///
    /// The table still declares that the member exists, so no layer can
    /// disagree about the surface; only the signature is delegated.
    Custom,
}

/// One declared member: the surface name plus the signature the checker
/// derives from it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MethodDecl {
    /// The surface name, which is also the `brasa_bytecode` registry
    /// name the id is minted under.
    pub name: &'static str,
    /// Parameter types, receiver excluded.
    pub params: &'static [TyDesc],
    pub ret: RetDesc,
    /// The stdlib-native errors this method raises, exactly as
    /// [`ModuleDecl::throws`], and for the same reason.
    ///
    /// A receiver method CAN throw: `string.toInt` raises
    /// `string.ParseError` and the four regex methods raise
    /// `string.RegexError`. That list used to live in `brasa_errorset`,
    /// a table away from the signature it belongs to — the arrangement
    /// whose failure mode is a method added to one and forgotten in the
    /// other, which makes `throws never` verifiable over a body that
    /// throws.
    pub throws: &'static [&'static str],
}

/// What type names a receiver's rows may mention.
///
/// A row can only name a type its receiver actually has, and the
/// receiver is the table's, not the row's — so this is declared once
/// per table and guarded once for all its rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecvShape {
    /// No type arguments: `string`, `int`, `float`, `Json`. Rows are
    /// concrete throughout, exactly like a free module's.
    Plain,
    /// One type argument, named `elem`: `Vector<T>`, `Set<T>`.
    Elem,
    /// Two, named `key` and `value`: `Map<K, V>`. The reason `elem` is
    /// not enough on its own — a map has two type arguments and
    /// neither of them is "the element".
    KeyValue,
}

impl RecvShape {
    /// Whether a row on this receiver may mention `desc`'s
    /// receiver-derived name.
    pub fn provides(self, desc: TyDesc) -> bool {
        match desc {
            TyDesc::Elem => matches!(self, RecvShape::Elem),
            TyDesc::Key | TyDesc::Value => matches!(self, RecvShape::KeyValue),
            _ => true,
        }
    }
}

/// One declared member of a free stdlib module (`fs.read(path)`): the
/// surface name, the signature, and the errors the member raises.
///
/// The error column lives here rather than in a table of its own
/// because the two rot apart otherwise: before BRS-96 the error-set
/// pass carried its own copy of this knowledge, and a throwing member
/// added to the signature table but forgotten in the error table made
/// `throws never` verifiable over a body that throws.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModuleDecl {
    /// The surface name. The `brasa_bytecode` registry mints the id
    /// under the qualified `module.name` instead, since a free member's
    /// bare name is not unique across modules (`fs.read`, `io.readAll`).
    pub name: &'static str,
    pub kind: ModuleKind,
    /// The stdlib-native errors this member raises, by canonical
    /// qualified name (`fs.NotFound`) — the member's contribution to
    /// its caller's inferred error-set.
    ///
    /// Outside [`ModuleKind::Call`] this is always empty: a constant is
    /// a value that is simply there, and no delegated member throws
    /// today. It stays on the outside so that stops being true by
    /// someone writing it down.
    pub throws: &'static [&'static str],
}

/// What a free module member is.
///
/// The three are variants rather than columns because each carries
/// what the others cannot: a constant has no parameters to declare, a
/// delegated member has no signature to declare at all, and only a call
/// has an optional tail. As columns, two thirds of every row would be
/// fields no test could reach.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModuleKind {
    /// The common case: called as `fs.read(path)`.
    Call {
        /// The parameters every call must pass.
        required: &'static [ParamDesc],
        /// Trailing parameters a call may omit, in order.
        optional: &'static [ParamDesc],
        /// The result, which is always one type — unlike a parameter,
        /// which may be a rule ([`ParamDesc`]).
        ret: TyDesc,
    },
    /// Read without a call: `math.pi`. Writing `math.pi()` is a call on
    /// a plain value, and the checker says so rather than reporting an
    /// unknown member.
    ///
    /// The same field/method distinction [`RecordKind`] draws, for the
    /// same reason: it is the surface's, not an implementation detail.
    Constant(TyDesc),
    /// The escape hatch: the checker owns the whole signature, because
    /// it is not expressible as data. `math.abs`/`min`/`max` are
    /// polymorphic over `int` and `float` and must answer in the kind
    /// they were given; `rand.choice`/`shuffle` are generic over the
    /// element of the vector they are passed.
    ///
    /// The table still declares that the member EXISTS, so no layer can
    /// disagree about the surface; only the signature is delegated.
    /// This is [`RetDesc::Custom`]'s counterpart for free modules, and
    /// it is deliberately the last resort — a member that merely has an
    /// awkward type belongs in [`ModuleKind::Call`].
    ///
    /// The payload is why this member cannot be data, stated per row.
    /// An escape hatch with no stated reason is how an escape hatch
    /// becomes the ordinary way to add a member.
    Custom(&'static str),
}

/// What a free module's parameter accepts.
///
/// Almost every parameter is one type, and [`ParamDesc::Ty`] says so.
/// The exception exists because a parameter can be a rule that no
/// single type expresses, while a result never can — which is why this
/// enum wraps [`TyDesc`] in parameter position instead of becoming a
/// case inside it. Every `TyDesc` lowers to exactly one of the
/// checker's types, and `ModuleDecl::ret` depends on that.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParamDesc {
    /// The ordinary case: the argument must be this type.
    Ty(TyDesc),
    /// A `std::proc` command: `Vector<string>` (the argv form) or
    /// `string` (the whitespace-split sugar for a command an author
    /// typed literally) — `docs/spec/05-stdlib.md`.
    ///
    /// The checker owns both the acceptance test and its wording, since
    /// naming one expected type would be a lie about the other.
    Command,
}

/// One type in the table's type language, as a [`TyDesc`].
///
/// Every type is exactly one token tree, which is what keeps the table
/// grammar trivial: named types are bare words (`int`, `float`,
/// `string`, `bool`, `unit`, `range`, `unknown`, `walk`, `json`,
/// `procOutput`, `response`, `args`), the receiver's element type is
/// `elem`, and every composite type is bracketed — `[Vector<elem>]`,
/// `[Option<elem>]`, `[Map<string, string>]`, `[Tuple<elem, unknown>]`,
/// `[fn(elem) -> bool]`.
#[macro_export]
macro_rules! ty {
    (int) => {
        $crate::TyDesc::Int
    };
    (float) => {
        $crate::TyDesc::Float
    };
    (range) => {
        $crate::TyDesc::Range
    };
    (string) => {
        $crate::TyDesc::String
    };
    (bool) => {
        $crate::TyDesc::Bool
    };
    (unit) => {
        $crate::TyDesc::Unit
    };
    (unknown) => {
        $crate::TyDesc::Unknown
    };
    (char) => {
        $crate::TyDesc::Char
    };
    (elem) => {
        $crate::TyDesc::Elem
    };
    (key) => {
        $crate::TyDesc::Key
    };
    (value) => {
        $crate::TyDesc::Value
    };
    (walk) => {
        $crate::TyDesc::Walk
    };
    (procOutput) => {
        $crate::TyDesc::ProcOutput
    };
    (response) => {
        $crate::TyDesc::HttpResponse
    };
    (args) => {
        $crate::TyDesc::CliArgs
    };
    (json) => {
        $crate::TyDesc::Json
    };
    ([Vector<$inner:tt>]) => {
        $crate::TyDesc::Vector(&$crate::ty!($inner))
    };
    ([Option<$inner:tt>]) => {
        $crate::TyDesc::Option(&$crate::ty!($inner))
    };
    ([Map<$key:tt, $value:tt>]) => {
        $crate::TyDesc::Map(&$crate::ty!($key), &$crate::ty!($value))
    };
    ([Set<$inner:tt>]) => {
        $crate::TyDesc::Set(&$crate::ty!($inner))
    };
    ([Tuple<$($item:tt),+>]) => {
        $crate::TyDesc::Tuple(&[$($crate::ty!($item)),+])
    };
    ([fn($($param:tt),*) -> $ret:tt]) => {
        $crate::TyDesc::Fn(&[$($crate::ty!($param)),*], &$crate::ty!($ret))
    };
}

/// How a record member is reached.
///
/// The distinction is the surface's, not an implementation detail:
/// `output.stdout` is a read and `response.header("x")` is a call, and
/// writing either the other way is an error the checker reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordKind {
    /// Read without a call: `output.stdout`.
    Field,
    /// Called with these parameters: `response.header("x")`.
    ///
    /// A record method exists where a field would have to promise
    /// something it cannot — `header` is case-insensitive and total, so
    /// it takes the name being looked up.
    Method(&'static [TyDesc]),
}

/// One declared member of a stdlib record (`Output`, `Response`,
/// `Args`, `Walk`): the surface name, how it is reached, and its type.
///
/// There is no `throws` column because no record member throws: a
/// record is a value the runtime already built, and reading it cannot
/// fail. A member that could would not belong on one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecordDecl {
    /// The surface name, which is also the `brasa_bytecode` registry
    /// name — records use bare names, shared across receiver kinds, the
    /// way receiver methods do.
    pub name: &'static str,
    pub kind: RecordKind,
    pub ret: TyDesc,
}

/// One parameter of a free module member, as a [`ParamDesc`].
///
/// Every token the [`ty!`] language accepts means the same thing here,
/// wrapped as [`ParamDesc::Ty`]; `command` is the one token that is a
/// rule rather than a type. Keeping the wrapping in the macro is what
/// let this arrive without touching a single existing row.
#[macro_export]
macro_rules! param {
    (command) => {
        $crate::ParamDesc::Command
    };
    ($ty:tt) => {
        $crate::ParamDesc::Ty($crate::ty!($ty))
    };
}

/// A member's result rule: a type in the [`ty!`] language, or one of the
/// two rules that are not a type — `fnRetVector` and `custom`.
#[macro_export]
macro_rules! ret {
    (fnRetVector) => {
        $crate::RetDesc::VectorOfFnRet
    };
    (custom) => {
        $crate::RetDesc::Custom
    };
    ($ty:tt) => {
        $crate::RetDesc::Ty($crate::ty!($ty))
    };
}

/// Declares one stdlib module's method surface.
///
/// The table is the single declaration of these members: it expands to
/// the member enum the VM matches exhaustively (so a new row breaks the
/// VM's build until it is implemented) and to the [`MethodDecl`] table
/// the checker reads its signatures from.
///
/// ```ignore
/// brasa_stdlib::method_table! {
///     /// The `Vector<T>` methods.
///     VectorMember => VECTOR_METHODS {
///         Len  "len"  ()                 -> int;
///         Push "push" (elem)             -> unit;
///         Map  "map"  ([fn(elem) -> unknown]) -> fnRetVector;
///     }
/// }
/// ```
#[macro_export]
macro_rules! method_table {
    (@throws) => { &[] };
    (@throws $throws:expr) => { $throws };
    (
        $(#[$table_meta:meta])*
        $member:ident => $table:ident, receiver $recv:literal $shape:ident {
            $(
                $(#[$row_meta:meta])*
                $variant:ident $name:literal ( $($param:tt),* ) -> $ret:tt
                    $( throws $throws:expr )? ;
            )*
        }
    ) => {
        $(#[$table_meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum $member {
            $(
                $(#[$row_meta])*
                $variant,
            )*
        }

        impl $member {
            /// The receiver these methods are reached through, as the
            /// checker displays it.
            pub const RECEIVER: &'static str = $recv;

            /// What receiver-derived type names this table's rows may
            /// mention.
            pub const SHAPE: $crate::RecvShape = $crate::RecvShape::$shape;

            /// The member a surface name selects, or `None` when the
            /// name is not part of this receiver's surface.
            pub fn from_name(name: &str) -> Option<Self> {
                match name {
                    $($name => Some(Self::$variant),)*
                    _ => None,
                }
            }

            /// This member's declaration.
            pub const fn decl(self) -> &'static $crate::MethodDecl {
                &$table[self as usize]
            }
        }

        $(#[$table_meta])*
        pub const $table: &[$crate::MethodDecl] = &[
            $(
                $crate::MethodDecl {
                    name: $name,
                    params: &[$($crate::ty!($param)),*],
                    ret: $crate::ret!($ret),
                    throws: $crate::method_table!(@throws $($throws)?),
                },
            )*
        ];
    };
}

/// Declares one free stdlib module's member surface — the modules
/// called as `module.member(...)` rather than through a receiver.
///
/// Like [`method_table!`] it expands to the member enum the VM matches
/// exhaustively and to the declaration table the checker reads, and it
/// adds the two things a free member has and a method does not:
/// optional trailing parameters, written as a second parenthesized
/// group after a `?`, and the errors the member raises, written after
/// `throws`.
///
/// Parameters are written in the [`param!`] language, which is the
/// [`ty!`] language plus `command`; results are written in [`ty!`]
/// alone, since a result is always one type.
///
/// A row takes one of the three forms of [`ModuleKind`]. A parameter
/// list means a call; `constant` means a member read without one; and
/// `custom` means the checker owns the signature:
///
/// ```ignore
/// brasa_stdlib::module_table! {
///     /// The `std::fs` members.
///     FsMember => FS_MEMBERS, module "fs" {
///         Read "read" (string)                     -> string           throws ALL_ERRORS;
///         Base "base" (string)                     -> string;
///         Walk "walk" (string) ?([Vector<string>]) -> [Vector<string>] throws ALL_ERRORS;
///     }
/// }
///
/// brasa_stdlib::module_table! {
///     /// The `std::proc` members: `command` is the one parameter that
///     /// is a rule rather than a type.
///     ProcMember => PROC_MEMBERS, module "proc" {
///         Run "run" (command) ?(string) -> procOutput throws STRICT_ERRORS;
///     }
/// }
///
/// brasa_stdlib::module_table! {
///     /// The `std::math` members: all three row forms at once.
///     MathMember => MATH_MEMBERS, module "math" {
///         Sqrt "sqrt" (float) -> float;                    // called
///         Pi   "pi"   constant float;                      // read, never called
///         Abs  "abs"  custom "polymorphic over int/float";  // delegated
///     }
/// }
/// ```
#[macro_export]
macro_rules! module_table {
    (@throws) => { &[] };
    (@throws $throws:expr) => { $throws };

    // One row's kind. Exactly one of the outer optional groups is
    // present, so exactly one of these matches.
    (@kind ( $($req:tt),* ) -> $ret:tt) => {
        $crate::ModuleKind::Call {
            required: &[$($crate::param!($req)),*],
            optional: &[],
            ret: $crate::ty!($ret),
        }
    };
    (@kind ( $($req:tt),* ) ? ( $($opt:tt),* ) -> $ret:tt) => {
        $crate::ModuleKind::Call {
            required: &[$($crate::param!($req)),*],
            optional: &[$($crate::param!($opt)),*],
            ret: $crate::ty!($ret),
        }
    };
    (@kind constant $ret:tt) => {
        $crate::ModuleKind::Constant($crate::ty!($ret))
    };
    (@kind custom $reason:literal) => {
        $crate::ModuleKind::Custom($reason)
    };

    (
        $(#[$table_meta:meta])*
        $member:ident => $table:ident, module $module:literal {
            $(
                $(#[$row_meta:meta])*
                $variant:ident $name:literal
                    $( ( $($req:tt),* ) $( ? ( $($opt:tt),* ) )? -> $ret:tt )?
                    $( constant $cret:tt )?
                    $( custom $reason:literal )?
                    $( throws $throws:expr )? ;
            )*
        }
    ) => {
        $(#[$table_meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum $member {
            $(
                $(#[$row_meta])*
                $variant,
            )*
        }

        impl $member {
            /// The module these members are reached through, which is
            /// also the prefix of their `brasa_bytecode` registry names.
            pub const MODULE: &'static str = $module;

            /// The member a surface name selects, or `None` when the
            /// name is not part of this module's surface.
            pub fn from_name(name: &str) -> Option<Self> {
                match name {
                    $($name => Some(Self::$variant),)*
                    _ => None,
                }
            }

            /// This member's declaration.
            pub const fn decl(self) -> &'static $crate::ModuleDecl {
                &$table[self as usize]
            }
        }

        $(#[$table_meta])*
        pub const $table: &[$crate::ModuleDecl] = &[
            $(
                $crate::ModuleDecl {
                    name: $name,
                    kind: $crate::module_table!(@kind
                        $( ( $($req),* ) $( ? ( $($opt),* ) )? -> $ret )?
                        $( constant $cret )?
                        $( custom $reason )?
                    ),
                    throws: $crate::module_table!(@throws $($throws)?),
                },
            )*
        ];
    };
}

/// Declares one stdlib record's member surface.
///
/// Like the other two table macros it expands to the member enum the VM
/// matches exhaustively and to the declaration table the checker reads.
/// A row is a field when it has no parameter list and a method when it
/// has one, which is the same distinction the surface draws:
///
/// ```ignore
/// brasa_stdlib::record_table! {
///     /// The `Response` record's members.
///     ResponseMember => RESPONSE_MEMBERS, record "Response" {
///         Status "status"          -> int;
///         Body   "body"            -> string;
///         Header "header" (string) -> [Option<string>];
///     }
/// }
/// ```
#[macro_export]
macro_rules! record_table {
    (@kind) => { $crate::RecordKind::Field };
    (@kind $($param:tt),*) => {
        $crate::RecordKind::Method(&[$($crate::ty!($param)),*])
    };
    (
        $(#[$table_meta:meta])*
        $member:ident => $table:ident, record $record:literal {
            $(
                $(#[$row_meta:meta])*
                $variant:ident $name:literal $( ( $($param:tt),* ) )? -> $ret:tt ;
            )*
        }
    ) => {
        $(#[$table_meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum $member {
            $(
                $(#[$row_meta])*
                $variant,
            )*
        }

        impl $member {
            /// The record's surface name, as the checker displays it.
            pub const RECORD: &'static str = $record;

            /// The member a surface name selects, or `None` when the
            /// name is not part of this record's surface.
            pub fn from_name(name: &str) -> Option<Self> {
                match name {
                    $($name => Some(Self::$variant),)*
                    _ => None,
                }
            }

            /// This member's declaration.
            pub const fn decl(self) -> &'static $crate::RecordDecl {
                &$table[self as usize]
            }
        }

        $(#[$table_meta])*
        pub const $table: &[$crate::RecordDecl] = &[
            $(
                $crate::RecordDecl {
                    name: $name,
                    kind: $crate::record_table!(@kind $($($param),*)?),
                    ret: $crate::ty!($ret),
                },
            )*
        ];
    };
}

pub mod cli;
pub mod env;
pub mod fs;
pub mod http;
pub mod io;
pub mod json;
pub mod map;
pub mod math;
pub mod num;
pub mod proc;
pub mod rand;
pub mod set;
pub mod string;
pub mod time;
pub mod vector;

pub use map::{MAP_METHODS, MapMember};
pub use num::{FLOAT_METHODS, FloatMember, INT_METHODS, IntMember};
pub use set::{SET_METHODS, SetMember};
pub use string::{STRING_METHODS, StringMember};

pub use cli::{ARGS_MEMBERS, ArgsMember, CLI_MEMBERS, CliMember};
pub use env::{ENV_MEMBERS, EnvMember};
pub use fs::{FS_MEMBERS, FsMember, WALK_MEMBERS, WalkMember};
pub use http::{HTTP_MEMBERS, HttpMember, RESPONSE_MEMBERS, ResponseMember};
pub use io::{IO_MEMBERS, IoMember};
pub use json::{JSON_ACCESSORS, JSON_MEMBERS, JsonAccessor, JsonMember};
pub use math::{MATH_MEMBERS, MathMember};
pub use proc::{OUTPUT_MEMBERS, OutputMember, PROC_MEMBERS, ProcMember};
pub use rand::{RAND_MEMBERS, RandMember};
pub use time::{TIME_MEMBERS, TimeMember};
pub use vector::{VECTOR_METHODS, VectorMember};

/// Every free module that has been converted to a table, by module
/// name. The layers that cover the whole free surface at once — the
/// checker's `module.name` lookup and the bytecode registry's
/// both-directions cross-check — walk this instead of naming each
/// module, so converting the next one does not mean editing them.
///
/// Every free stdlib module is now here (BRS-96). The list stays
/// because the layers that cover the whole free surface walk it rather
/// than naming each module — so a module added to the language later is
/// covered by joining one list.
pub const FREE_MODULES: &[(&str, &[ModuleDecl])] = &[
    (FsMember::MODULE, FS_MEMBERS),
    (JsonMember::MODULE, JSON_MEMBERS),
    (IoMember::MODULE, IO_MEMBERS),
    (EnvMember::MODULE, ENV_MEMBERS),
    (ProcMember::MODULE, PROC_MEMBERS),
    (HttpMember::MODULE, HTTP_MEMBERS),
    (CliMember::MODULE, CLI_MEMBERS),
    (MathMember::MODULE, MATH_MEMBERS),
    (TimeMember::MODULE, TIME_MEMBERS),
    (RandMember::MODULE, RAND_MEMBERS),
];

/// The declaration of `module.name`, or `None` when the module has no
/// table yet or the name is not one of its members.
///
/// The two answers are deliberately the same: a caller that resolves a
/// member wants the declaration or nothing, and one that wants to know
/// whether a module is converted asks [`is_free_module`].
pub fn free_member(module: &str, name: &str) -> Option<&'static ModuleDecl> {
    let (_, members) = FREE_MODULES.iter().find(|(m, _)| *m == module)?;
    members.iter().find(|decl| decl.name == name)
}

/// Whether this module's surface is declared by a table here — which
/// is what makes "not a member of it" a final answer rather than a
/// reason to look in a layer's own list.
pub fn is_free_module(module: &str) -> bool {
    FREE_MODULES.iter().any(|(m, _)| *m == module)
}

/// Every receiver type whose methods are declared here, with the shape
/// that says which receiver-derived type names its rows may mention.
///
/// Walked by the layers that cover the whole method surface at once —
/// the bytecode registry's cross-check and the table guards — for the
/// same reason [`FREE_MODULES`] and [`RECORDS`] are.
///
/// The checker maps its own `Type` to one of these tables, since it is
/// the only layer that knows what a `Type` is. That map is also where
/// `Option<Json>` flattens onto the `Json` table: which table a
/// receiver selects is a question about types, not about rows.
pub const RECEIVERS: &[(&str, RecvShape, &[MethodDecl])] = &[
    (StringMember::RECEIVER, StringMember::SHAPE, STRING_METHODS),
    (IntMember::RECEIVER, IntMember::SHAPE, INT_METHODS),
    (FloatMember::RECEIVER, FloatMember::SHAPE, FLOAT_METHODS),
    (VectorMember::RECEIVER, VectorMember::SHAPE, VECTOR_METHODS),
    (MapMember::RECEIVER, MapMember::SHAPE, MAP_METHODS),
    (SetMember::RECEIVER, SetMember::SHAPE, SET_METHODS),
    (JsonAccessor::RECEIVER, JsonAccessor::SHAPE, JSON_ACCESSORS),
];

/// Every stdlib record, by the name the checker displays.
///
/// The layers that cover all four at once walk this rather than naming
/// each — the bytecode registry's cross-check and the table guards —
/// for the same reason [`FREE_MODULES`] exists. The checker maps its
/// own `Type` to one of these tables, since it is the only layer that
/// knows what a `Type` is.
pub const RECORDS: &[(&str, &[RecordDecl])] = &[
    (OutputMember::RECORD, OUTPUT_MEMBERS),
    (ResponseMember::RECORD, RESPONSE_MEMBERS),
    (ArgsMember::RECORD, ARGS_MEMBERS),
    (WalkMember::RECORD, WALK_MEMBERS),
];

#[cfg(test)]
mod tests {
    use super::{
        FREE_MODULES, ModuleDecl, ModuleKind, ParamDesc, RECEIVERS, RECORDS, RecordKind, RetDesc,
        TyDesc,
    };

    /// Every type a free module row mentions, whatever row form it is.
    ///
    /// A constant has one and a delegated member has none, so a guard
    /// that reached for `required`/`ret` directly would either miss
    /// rows or not compile. Going through the kind is what keeps the
    /// guards total as the row forms grow.
    fn module_row_types(decl: &'static ModuleDecl) -> Vec<&'static TyDesc> {
        match &decl.kind {
            ModuleKind::Call {
                required,
                optional,
                ret,
            } => required
                .iter()
                .chain(*optional)
                .filter_map(|param| match param {
                    ParamDesc::Ty(desc) => Some(desc),
                    ParamDesc::Command => None,
                })
                .chain(std::iter::once(ret))
                .collect(),
            ModuleKind::Constant(ret) => vec![ret],
            ModuleKind::Custom(_) => Vec::new(),
        }
    }

    /// A free module has no receiver, so a row mentioning the
    /// receiver's element type would have nothing to lower against and
    /// would surface only when a user called that member — as a panic
    /// inside the checker.
    ///
    /// One test over [`FREE_MODULES`] rather than one per module: a
    /// module converted later is covered the moment it joins the list,
    /// which is the same reason the bytecode registry's cross-check
    /// walks it.
    #[test]
    fn no_free_module_row_names_a_receiver_type() {
        for (module, members) in FREE_MODULES {
            for decl in *members {
                let mut named = Vec::new();
                for desc in module_row_types(decl) {
                    named_types(desc, &mut named);
                }

                assert!(
                    named.is_empty(),
                    "`{module}.{}` names {named:?}, but a free module has no receiver",
                    decl.name
                );
            }
        }
    }

    /// Every receiver-derived name a row uses, at any depth.
    fn named_types(desc: &'static TyDesc, found: &mut Vec<TyDesc>) {
        match desc {
            TyDesc::Elem | TyDesc::Key | TyDesc::Value => found.push(*desc),
            TyDesc::Vector(inner) | TyDesc::Option(inner) | TyDesc::Set(inner) => {
                named_types(inner, found)
            }
            TyDesc::Map(key, value) => {
                named_types(key, found);
                named_types(value, found);
            }
            TyDesc::Tuple(items) => items.iter().for_each(|item| named_types(item, found)),
            TyDesc::Fn(params, ret) => {
                params.iter().for_each(|param| named_types(param, found));
                named_types(ret, found);
            }
            _ => {}
        }
    }

    /// A row may only name a type its receiver actually has: `elem` on
    /// a `Vector` or `Set`, `key`/`value` on a `Map`, none of the three
    /// on `string` or `Json`.
    ///
    /// Without this the failure is a panic inside the checker the first
    /// time a user calls that member — the declaration is wrong from
    /// the moment it is written, but nothing looks at it until then.
    /// `Map` is why the check has to be per receiver rather than one
    /// global "no `elem`": a map's two type arguments are named, and
    /// naming the wrong one of them typechecks for every map whose key
    /// and value coincide.
    #[test]
    fn no_row_names_a_type_its_receiver_lacks() {
        for (receiver, shape, members) in RECEIVERS {
            for decl in *members {
                let mut named = Vec::new();

                for desc in decl.params {
                    named_types(desc, &mut named);
                }
                if let RetDesc::Ty(ret) = &decl.ret {
                    named_types(ret, &mut named);
                }

                for desc in named {
                    assert!(
                        shape.provides(desc),
                        "`{receiver}.{}` names `{desc:?}`, which a {shape:?} receiver does not have",
                        decl.name
                    );
                }
            }
        }
    }

    /// Only the receiver that declares a delegated row may have one.
    /// `RetDesc::Custom` hands the signature to a checker function that
    /// knows one receiver's rules, so a second table using it would
    /// reach code written for the first.
    #[test]
    fn only_vector_delegates_a_method_signature() {
        for (receiver, _, members) in RECEIVERS {
            for decl in *members {
                if matches!(decl.ret, RetDesc::Custom | RetDesc::VectorOfFnRet) {
                    assert_eq!(
                        *receiver, "Vector",
                        "`{receiver}.{}` delegates its signature, but only Vector's rules exist",
                        decl.name
                    );
                }
            }
        }
    }

    /// Throwing methods are rare and deliberate. Written out rather
    /// than derived, because what `throws` decides is whether the
    /// checker accepts a caller's `throws` clause.
    #[test]
    fn exactly_six_builtin_methods_throw() {
        let throwing: Vec<_> = RECEIVERS
            .iter()
            .flat_map(|(receiver, _, members)| {
                members
                    .iter()
                    .filter(|decl| !decl.throws.is_empty())
                    .map(move |decl| format!("{receiver}.{}", decl.name))
            })
            .collect();

        assert_eq!(
            throwing,
            [
                "string.toInt",
                "string.toFloat",
                "string.match?",
                "string.captures",
                "string.replaceRe",
                "string.scan",
            ],
            "the set of throwing builtin methods changed"
        );
    }

    /// Every declared error is one the resolver knows as native, for
    /// receivers as well as modules: an unlisted name is one no `catch`
    /// arm can match, so the method would be uncatchable rather than
    /// merely misnamed.
    #[test]
    fn every_method_error_is_spelled_like_the_modules() {
        for (receiver, _, members) in RECEIVERS {
            for decl in *members {
                for error in decl.throws {
                    assert!(
                        error.contains('.'),
                        "`{receiver}.{}` raises `{error}`, which is not a qualified name",
                        decl.name
                    );
                }
            }
        }
    }

    /// The escape hatch stays an escape hatch. Every delegated member
    /// states why it cannot be data, and the reason has to be a
    /// sentence rather than a placeholder — an empty one would make
    /// `custom` the cheapest way to add a member instead of the most
    /// expensive.
    #[test]
    fn every_delegated_member_states_why() {
        for (module, members) in FREE_MODULES {
            for decl in *members {
                let ModuleKind::Custom(reason) = decl.kind else {
                    continue;
                };

                assert!(
                    reason.len() > 20,
                    "`{module}.{}` delegates its signature without saying why",
                    decl.name
                );
            }
        }
    }

    /// Delegation is rare on purpose. This is a ratchet, not a law: if
    /// a genuine sixth arrives, raising the number is a deliberate edit
    /// with a reviewer looking at it, which is exactly the point.
    #[test]
    fn delegation_stays_rare() {
        let delegated: Vec<_> = FREE_MODULES
            .iter()
            .flat_map(|(module, members)| {
                members
                    .iter()
                    .filter(|decl| matches!(decl.kind, ModuleKind::Custom(_)))
                    .map(move |decl| format!("{module}.{}", decl.name))
            })
            .collect();

        assert_eq!(
            delegated,
            [
                "math.abs",
                "math.min",
                "math.max",
                "rand.choice",
                "rand.shuffle"
            ],
            "the set of members the checker owns changed"
        );
    }

    /// Only a member that is never called may be a constant, and a
    /// constant never throws: there is no call for an error to escape
    /// from. The same holds for a delegated member today, and the
    /// column lives outside the kind so that stops being true by
    /// someone writing it down rather than by accident.
    #[test]
    fn only_a_called_member_throws() {
        for (module, members) in FREE_MODULES {
            for decl in *members {
                if matches!(decl.kind, ModuleKind::Call { .. }) {
                    continue;
                }

                assert!(
                    decl.throws.is_empty(),
                    "`{module}.{}` is not called, so nothing can throw out of it",
                    decl.name
                );
            }
        }
    }

    /// A record is a CONCRETE receiver: it has no type arguments, so
    /// none of the three names is available to it. Unlike a free
    /// module a record is a receiver at all, which is what makes the
    /// mistake available to make and worth refusing.
    #[test]
    fn no_record_row_names_a_receiver_type() {
        for (record, members) in RECORDS {
            for decl in *members {
                let mut named = Vec::new();

                if let RecordKind::Method(params) = decl.kind {
                    for param in params {
                        named_types(param, &mut named);
                    }
                }
                named_types(&decl.ret, &mut named);

                assert!(
                    named.is_empty(),
                    "`{record}.{}` names {named:?}, but a record has no type arguments",
                    decl.name
                );
            }
        }
    }

    /// No record declares `toString`. The checker layers the universal
    /// derived one onto every type, so a row spelling it would be a
    /// second answer to a question already answered — and whichever
    /// answer the lookup happened to reach first would win silently.
    #[test]
    fn no_record_redeclares_the_universal_to_string() {
        for (record, members) in RECORDS {
            for decl in *members {
                assert_ne!(
                    decl.name, "toString",
                    "`{record}` redeclares the universal derived toString"
                );
            }
        }
    }

    /// Within one record a name means one thing. Names ARE shared
    /// across records the way they are across receiver kinds (both
    /// `Output` and `Response` would answer a `body`-shaped read), so
    /// this is deliberately per-record rather than global.
    #[test]
    fn record_member_names_are_unique_within_their_record() {
        for (record, members) in RECORDS {
            for (ix, decl) in members.iter().enumerate() {
                let first = members
                    .iter()
                    .position(|other| other.name == decl.name)
                    .expect("the row finds itself");

                assert_eq!(
                    first, ix,
                    "`{record}` declares `{}` twice; the second row is unreachable",
                    decl.name
                );
            }
        }
    }

    /// Every free member resolves through its own module and no other.
    /// Bare names repeat across modules (`fs.read` and `io.readAll`
    /// both end in a `read`-ish name; `env.get` and `Map.get` share
    /// one outright), so a lookup that ignored the module would answer
    /// with a signature from the wrong surface.
    #[test]
    fn free_members_resolve_only_through_their_own_module() {
        for (module, members) in FREE_MODULES {
            for decl in *members {
                let found = super::free_member(module, decl.name)
                    .unwrap_or_else(|| panic!("`{module}.{}` resolves", decl.name));
                assert_eq!(found.name, decl.name);

                for (other, _) in FREE_MODULES {
                    if other == module {
                        continue;
                    }

                    if let Some(shared) = super::free_member(other, decl.name) {
                        assert!(
                            !std::ptr::eq(shared, found),
                            "`{module}.{}` and `{other}.{}` are the same row",
                            decl.name,
                            decl.name
                        );
                    }
                }
            }
        }
    }
}
