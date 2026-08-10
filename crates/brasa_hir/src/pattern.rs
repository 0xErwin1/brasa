//! Pattern nodes, used by `match` arms and `for` bindings.
//!
//! Patterns have no sugar; lowering copies them structurally so the HIR
//! is self-contained. The shapes mirror `brasa_ast::Pattern` with HIR
//! IDs.

use crate::PatternId;

/// Literal patterns carry no node IDs, so the AST's enum is shared
/// verbatim rather than duplicated.
pub use brasa_ast::Literal;

#[derive(Debug, Clone, PartialEq)]
pub enum Pattern {
    Wildcard,
    Literal(Literal),
    Binding(String),
    Ctor { name: String, args: Vec<PatternId> },
    Tuple(Vec<PatternId>),
}
