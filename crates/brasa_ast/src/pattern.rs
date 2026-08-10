//! Pattern nodes, used by `match` arms and `for` bindings.

use crate::PatternId;

/// A literal pattern. `docs/spec/02-grammar.md`'s `pattern` production
/// references an undefined `literal` nonterminal; this covers the
/// lexical literal kinds (`INT`, `FLOAT`, `true`/`false`, `CHAR`,
/// `STRING`). A `STRING` pattern never contains interpolation: the parser
/// rejects `#{` inside a pattern's string literal (see
/// `Parser::parse_plain_string`).
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
