//! Type-expression nodes (the `type` grammar production).

use crate::TypeExprId;

#[derive(Debug, Clone, PartialEq)]
pub enum TypeExpr {
    /// `Vector<int>`, `Map<string, T>`, or a bare name with no generic
    /// arguments.
    Named { name: String, args: Vec<TypeExprId> },
    /// `(int, string)`.
    Tuple(Vec<TypeExprId>),
    /// `(int, int) -> int`.
    Fn {
        params: Vec<TypeExprId>,
        ret: TypeExprId,
    },
}
