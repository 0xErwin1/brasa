//! Type-expression nodes (the `type` grammar production).

use crate::TypeExprId;

#[derive(Debug, Clone, PartialEq)]
pub enum TypeExpr {
    /// `Vector<int>`, `Map<string, T>`, or a bare name with no generic
    /// arguments.
    Named {
        /// The name as written, which may be a qualified path
        /// (`lib.Point`) naming a type exported by an imported file
        /// module (spec: 01 — Sintaxis, modules).
        ///
        /// The qualifier lives in this `String` rather than in a field
        /// of its own, the way `CatchType::Named` already carries dotted
        /// names: a type name can never contain a `.`, so splitting on
        /// the first one is unambiguous, and a separate `Option<String>`
        /// would grow every `TypeExpr` — and, through the same change on
        /// `Expr`, the arena of every program — by half again.
        name: String,
        args: Vec<TypeExprId>,
    },
    /// `(int, string)`.
    Tuple(Vec<TypeExprId>),
    /// `(int, int) -> int`.
    Fn {
        params: Vec<TypeExprId>,
        ret: TypeExprId,
    },
}
