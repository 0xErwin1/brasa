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
    Elem,
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

/// One type in the table's type language, as a [`TyDesc`].
///
/// Every type is exactly one token tree, which is what keeps the table
/// grammar trivial: primitives are bare words (`int`, `string`, `bool`,
/// `unit`, `unknown`), the receiver's element type is `elem`, and every
/// composite type is bracketed — `[Vector<elem>]`, `[Option<elem>]`,
/// `[Tuple<elem, unknown>]`, `[fn(elem) -> bool]`.
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

pub mod vector;

pub use vector::{VECTOR_METHODS, VectorMember};
