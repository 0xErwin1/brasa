//! Pattern nodes, used by `match` arms and `for` bindings.

use crate::PatternId;

/// A literal pattern. `docs/spec/02-grammar.md`'s `pattern` production
/// references an undefined `literal` nonterminal; this covers the
/// lexical literal kinds (`INT`, `FLOAT`, `true`/`false`, `CHAR`,
/// `STRING`). See BRS-9 open questions for whether a `STRING` pattern may
/// contain interpolation (the spec does not say).
#[derive(Debug, Clone, PartialEq)]
pub enum Literal {
    Int(i64),
    Float(f64),
    Bool(bool),
    Char(char),
    Str(String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Pattern {
    Wildcard,
    Literal(Literal),
    Binding(String),
    /// `TYPE_IDENT` alone or `TYPE_IDENT(pattern, ...)`. `args` is empty
    /// for a bare constructor reference (e.g. matching a unit variant),
    /// since patterns have no separate "called with zero args" form worth
    /// distinguishing.
    Ctor {
        name: String,
        args: Vec<PatternId>,
    },
    Tuple(Vec<PatternId>),
}
