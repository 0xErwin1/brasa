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
//! # Two table shapes
//!
//! A receiver type ([`method_table!`], `Vector<T>`) and a free module
//! ([`module_table!`], `std::fs`) do NOT share a shape, because almost
//! nothing about their columns overlaps:
//!
//! | | receiver method | free module member |
//! |---|---|---|
//! | receiver | yes, and its element type is a type ([`TyDesc::Elem`]) | none |
//! | result | can depend on an argument ([`RetDesc`]) | always a fixed type |
//! | trailing parameters | all required | last ones may be optional |
//! | errors raised | none on the converted surface | part of the contract ([`ModuleDecl::throws`]) |
//! | registry name | the bare name, shared across receivers | `module.name`, unique |
//!
//! One shape covering both would carry an `Elem` case free modules can
//! never use and an optional/`throws` column receivers never fill —
//! columns no test could reach. What the two DO share is the type
//! language ([`TyDesc`] and [`ty!`]), which is where the real
//! duplication would have been.

/// A type in a declaration, written in the table's small type language
/// and lowered to the checker's `Type` by `brasa_typeck`.
///
/// Receiver-derived types ([`TyDesc::Elem`]) are what let one row serve
/// every instantiation of a generic receiver.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TyDesc {
    Int,
    String,
    Bool,
    Unit,
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
    Vector(&'static TyDesc),
    Option(&'static TyDesc),
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
    /// The parameters every call must pass.
    pub required: &'static [TyDesc],
    /// Trailing parameters a call may omit, in order.
    pub optional: &'static [TyDesc],
    pub ret: TyDesc,
    /// The stdlib-native errors this member raises, by canonical
    /// qualified name (`fs.NotFound`) — the member's contribution to
    /// its caller's inferred error-set.
    pub throws: &'static [&'static str],
}

/// One type in the table's type language, as a [`TyDesc`].
///
/// Every type is exactly one token tree, which is what keeps the table
/// grammar trivial: named types are bare words (`int`, `string`,
/// `bool`, `unit`, `unknown`, `walk`, `json`), the receiver's element type is
/// `elem`, and every composite type is bracketed — `[Vector<elem>]`,
/// `[Option<elem>]`, `[Tuple<elem, unknown>]`, `[fn(elem) -> bool]`.
#[macro_export]
macro_rules! ty {
    (int) => {
        $crate::TyDesc::Int
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
    (json) => {
        $crate::TyDesc::Json
    };
    ([Vector<$inner:tt>]) => {
        $crate::TyDesc::Vector(&$crate::ty!($inner))
    };
    ([Option<$inner:tt>]) => {
        $crate::TyDesc::Option(&$crate::ty!($inner))
    };
    ([Tuple<$($item:tt),+>]) => {
        $crate::TyDesc::Tuple(&[$($crate::ty!($item)),+])
    };
    ([fn($($param:tt),*) -> $ret:tt]) => {
        $crate::TyDesc::Fn(&[$($crate::ty!($param)),*], &$crate::ty!($ret))
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
/// adds the two columns a free member has and a method does not:
/// optional trailing parameters, written as a second parenthesized
/// group after a `?`, and the errors the member raises, written after
/// `throws`.
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
/// ```
#[macro_export]
macro_rules! module_table {
    (@throws) => { &[] };
    (@throws $throws:expr) => { $throws };
    (
        $(#[$table_meta:meta])*
        $member:ident => $table:ident, module $module:literal {
            $(
                $(#[$row_meta:meta])*
                $variant:ident $name:literal ( $($req:tt),* ) $( ? ( $($opt:tt),* ) )?
                    -> $ret:tt $( throws $throws:expr )? ;
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
                    required: &[$($crate::ty!($req)),*],
                    optional: &[$($($crate::ty!($opt)),*)?],
                    ret: $crate::ty!($ret),
                    throws: $crate::module_table!(@throws $($throws)?),
                },
            )*
        ];
    };
}

pub mod fs;
pub mod io;
pub mod json;
pub mod vector;

pub use fs::{FS_MEMBERS, FsMember};
pub use io::{IO_MEMBERS, IoMember};
pub use json::{JSON_MEMBERS, JsonMember};
pub use vector::{VECTOR_METHODS, VectorMember};

/// Every free module that has been converted to a table, by module
/// name. The layers that cover the whole free surface at once — the
/// checker's `module.name` lookup and the bytecode registry's
/// both-directions cross-check — walk this instead of naming each
/// module, so converting the next one does not mean editing them.
///
/// A module absent here is not undeclared, only still hand-written in
/// each layer; `proc`, `env`, `math`, `time` and `rand` are the ones
/// left.
pub const FREE_MODULES: &[(&str, &[ModuleDecl])] = &[
    (FsMember::MODULE, FS_MEMBERS),
    (JsonMember::MODULE, JSON_MEMBERS),
    (IoMember::MODULE, IO_MEMBERS),
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
