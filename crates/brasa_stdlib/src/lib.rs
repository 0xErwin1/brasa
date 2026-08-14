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
//! | receiver | one generic type, whose element type is a type ([`TyDesc::Elem`]) | none | one concrete type |
//! | result | can depend on an argument ([`RetDesc`]) | a fixed type, or the checker's ([`ModuleKind::Custom`]) | always a fixed type |
//! | parameters | all required types | optional tail, and may be a rule ([`ParamDesc::Command`]) | none, or all required types |
//! | reached by | always a call | a call, or a read ([`ModuleKind::Constant`]) | a call, or a read ([`RecordKind::Field`]) |
//! | errors raised | none | part of the contract ([`ModuleDecl::throws`]) | none |
//! | registry name | the bare name, shared across receivers | `module.name`, unique | the bare name, shared |
//!
//! One shape covering all three would carry an `Elem` case free modules
//! can never use, an optional/`throws` column receivers never fill, and
//! a `Constant` case a method cannot be — columns no test could reach.
//! What they DO share is the type language ([`TyDesc`] and [`ty!`]),
//! which is where the real duplication would have been.
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
/// Receiver-derived types ([`TyDesc::Elem`]) are what let one row serve
/// every instantiation of a generic receiver.
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
    /// The receiver's element type — `T` in a `Vector<T>` receiver.
    /// Meaningless in a free module's table, which has no receiver.
    Elem,
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
    (elem) => {
        $crate::TyDesc::Elem
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
    (
        $(#[$table_meta:meta])*
        $member:ident => $table:ident {
            $(
                $(#[$row_meta:meta])*
                $variant:ident $name:literal ( $($param:tt),* ) -> $ret:tt ;
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
            /// The member a surface name selects, or `None` when the
            /// name is not part of this module's surface.
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
pub mod math;
pub mod proc;
pub mod rand;
pub mod time;
pub mod vector;

pub use cli::{ARGS_MEMBERS, ArgsMember, CLI_MEMBERS, CliMember};
pub use env::{ENV_MEMBERS, EnvMember};
pub use fs::{FS_MEMBERS, FsMember, WALK_MEMBERS, WalkMember};
pub use http::{HTTP_MEMBERS, HttpMember, RESPONSE_MEMBERS, ResponseMember};
pub use io::{IO_MEMBERS, IoMember};
pub use json::{JSON_MEMBERS, JsonMember};
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
    use super::{FREE_MODULES, ModuleDecl, ModuleKind, ParamDesc, RECORDS, RecordKind, TyDesc};

    /// Whether a declared type reaches [`TyDesc::Elem`] anywhere.
    ///
    /// Written as one exhaustive match rather than a wildcard so that a
    /// new composite type cannot be added without deciding whether it
    /// can contain another type. The `Map` arm is why this is worth
    /// saying: while each module carried its own copy of the guard
    /// below, the copies predated `Map` and none of them looked inside
    /// one, so the check silently stopped being total.
    fn mentions_elem(desc: &TyDesc) -> bool {
        match desc {
            TyDesc::Elem => true,
            TyDesc::Vector(inner) | TyDesc::Option(inner) => mentions_elem(inner),
            TyDesc::Map(key, value) => mentions_elem(key) || mentions_elem(value),
            TyDesc::Tuple(items) => items.iter().any(mentions_elem),
            TyDesc::Fn(params, ret) => params.iter().any(mentions_elem) || mentions_elem(ret),
            TyDesc::Int
            | TyDesc::Float
            | TyDesc::String
            | TyDesc::Bool
            | TyDesc::Unit
            | TyDesc::Range
            | TyDesc::Unknown
            | TyDesc::Walk
            | TyDesc::Json
            | TyDesc::ProcOutput
            | TyDesc::HttpResponse
            | TyDesc::CliArgs => false,
        }
    }

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
    fn no_free_module_row_mentions_the_receiver_element_type() {
        for (module, members) in FREE_MODULES {
            for decl in *members {
                for desc in module_row_types(decl) {
                    assert!(
                        !mentions_elem(desc),
                        "`{module}.{}` mentions `elem`, but a free module has no receiver",
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

    /// A record has no receiver element type either, and unlike a free
    /// module it is a receiver — so the mistake is available to make
    /// and worth refusing.
    #[test]
    fn no_record_row_mentions_the_receiver_element_type() {
        for (record, members) in RECORDS {
            for decl in *members {
                if let RecordKind::Method(params) = decl.kind {
                    for param in params {
                        assert!(
                            !mentions_elem(param),
                            "`{record}.{}` takes `elem`, but a record has no element type",
                            decl.name
                        );
                    }
                }

                assert!(
                    !mentions_elem(&decl.ret),
                    "`{record}.{}` returns `elem`, but a record has no element type",
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
