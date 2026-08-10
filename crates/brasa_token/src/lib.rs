//! Token definitions for Brasa.
//!
//! The token set mirrors the lexical grammar in `docs/spec/02-gramatica.md`.
//! Kept separate from the lexer so the parser depends on token *types*
//! without depending on how they are produced (BRS-8 fills this in).

use brasa_diagnostics::Span;

/// The kind of a lexical token.
///
/// Fieldless by design: a token carries no payload beyond its [`Span`],
/// so consumers slice the source text themselves (via `parse_int`,
/// `parse_float`, `unescape_string_text` below, or a raw substring for
/// identifiers). This keeps `TokenKind` `Copy` and avoids duplicating
/// ownership of source bytes.
///
/// String literals with interpolation are not a single token: they are
/// lexed as a sequence (`StringStart`, `StringText`, `InterpStart`, ...,
/// `InterpEnd`, `StringText`, `StringEnd`) so that arbitrary expressions,
/// including nested strings, can appear inside `#{...}`. See `brasa_lexer`
/// for the driver that produces this sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TokenKind {
    // Keywords
    Def,
    End,
    If,
    Then,
    Elsif,
    Else,
    While,
    For,
    In,
    Match,
    Enum,
    Struct,
    Interface,
    Import,
    Pub,
    Let,
    Mut,
    Return,
    Break,
    Continue,
    Throw,
    Throws,
    Catch,
    CatchAll,
    Never,
    True,
    False,
    SelfKw,
    Unit,
    And,
    Or,
    Not,
    Spawn,

    // Operators
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    StarStar,
    EqEq,
    NotEq,
    Lt,
    LtEq,
    Gt,
    GtEq,
    AndAnd,
    OrOr,
    Bang,
    Eq,
    PlusEq,
    MinusEq,
    StarEq,
    SlashEq,
    PercentEq,
    PipeGt,
    QuestionDot,
    QuestionQuestion,
    DotDot,
    DotDotEq,
    FatArrow,
    Arrow,
    ColonColon,

    // Punctuation
    LParen,
    RParen,
    LBracket,
    RBracket,
    LBrace,
    RBrace,
    Comma,
    Colon,
    Dot,
    Pipe,
    Underscore,

    // Literals
    Int,
    Float,
    Char,

    /// Opens a string literal. `is_raw` is not stored here (kinds are
    /// fieldless); a raw string is instead distinguished by a dedicated
    /// `RawStringStart` variant, see below.
    StringStart,
    /// Opens a `"""..."""` raw (multiline) string literal.
    RawStringStart,
    /// Literal text inside a string, verbatim from the source including
    /// escape sequences. Use `unescape_string_text` to decode it.
    StringText,
    /// Opens a `#{` interpolation inside a string; the lexer switches back
    /// to main-mode token scanning until the matching `}`.
    InterpStart,
    /// Closes an interpolation (the `}` matching its `InterpStart`).
    InterpEnd,
    /// Closes a string literal (the matching `"` or `"""`).
    StringEnd,

    Ident,
    /// An identifier starting with an uppercase letter (types, enum
    /// variants/constructors). Never carries a `?`/`!` suffix.
    TypeIdent,

    /// One or more consecutive newlines are still one token each; the
    /// parser collapses runs of them into a single statement separator.
    Newline,

    Eof,
    /// Emitted for any lexical error (unexpected char, unterminated
    /// string/interpolation) so the lexer can keep producing tokens after
    /// a failure. Details live in the parallel `LexError` list.
    Error,
}

/// A single lexical token: its kind plus the source span it covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
}

impl Token {
    pub fn new(kind: TokenKind, span: Span) -> Self {
        Self { kind, span }
    }
}

/// Looks up whether `text` is a reserved keyword, returning its
/// [`TokenKind`] if so.
///
/// Called after an identifier has already been scanned (idents may end in
/// `?`/`!`, but keywords never do), so this is a plain string match rather
/// than something baked into the lexer's identifier pattern.
pub fn keyword(text: &str) -> Option<TokenKind> {
    Some(match text {
        "def" => TokenKind::Def,
        "end" => TokenKind::End,
        "if" => TokenKind::If,
        "then" => TokenKind::Then,
        "elsif" => TokenKind::Elsif,
        "else" => TokenKind::Else,
        "while" => TokenKind::While,
        "for" => TokenKind::For,
        "in" => TokenKind::In,
        "match" => TokenKind::Match,
        "enum" => TokenKind::Enum,
        "struct" => TokenKind::Struct,
        "interface" => TokenKind::Interface,
        "import" => TokenKind::Import,
        "pub" => TokenKind::Pub,
        "let" => TokenKind::Let,
        "mut" => TokenKind::Mut,
        "return" => TokenKind::Return,
        "break" => TokenKind::Break,
        "continue" => TokenKind::Continue,
        "throw" => TokenKind::Throw,
        "throws" => TokenKind::Throws,
        "catch" => TokenKind::Catch,
        "catch_all" => TokenKind::CatchAll,
        "never" => TokenKind::Never,
        "true" => TokenKind::True,
        "false" => TokenKind::False,
        "self" => TokenKind::SelfKw,
        "unit" => TokenKind::Unit,
        "and" => TokenKind::And,
        "or" => TokenKind::Or,
        "not" => TokenKind::Not,
        "spawn" => TokenKind::Spawn,
        _ => return None,
    })
}

/// Parses an `INT` literal (`docs/spec/02-gramatica.md`): decimal, `0x`
/// hex, or `0b` binary, with optional `_` digit separators. No octal form
/// exists in Brasa.
///
/// Returns `None` on overflow (values must fit in `i64`) or malformed
/// input; the lexer only ever calls this with text it already matched
/// against the `INT` pattern, so `None` here always means overflow in
/// practice.
pub fn parse_int(text: &str) -> Option<i64> {
    let cleaned: String = text.chars().filter(|c| *c != '_').collect();

    if let Some(rest) = cleaned.strip_prefix("0x") {
        return i64::from_str_radix(rest, 16).ok();
    }

    if let Some(rest) = cleaned.strip_prefix("0b") {
        return i64::from_str_radix(rest, 2).ok();
    }

    cleaned.parse::<i64>().ok()
}

/// Parses a `FLOAT` literal, stripping `_` digit separators first.
pub fn parse_float(text: &str) -> Option<f64> {
    let cleaned: String = text.chars().filter(|c| *c != '_').collect();
    cleaned.parse::<f64>().ok()
}

/// Decodes the escape sequences allowed inside `STRING`/`RAWSTRING` text
/// (`\n \t \" \\ \#`), leaving any other backslash sequence untouched.
///
/// `text` is the raw source slice of a `StringText` token, so it never
/// contains a bare `"` or an unescaped `#{`.
pub fn unescape_string_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();

    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }

        match chars.peek() {
            Some('n') => {
                out.push('\n');
                chars.next();
            }
            Some('t') => {
                out.push('\t');
                chars.next();
            }
            Some('"') => {
                out.push('"');
                chars.next();
            }
            Some('\\') => {
                out.push('\\');
                chars.next();
            }
            Some('#') => {
                out.push('#');
                chars.next();
            }
            _ => out.push('\\'),
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_int_decimal() {
        assert_eq!(parse_int("0"), Some(0));
        assert_eq!(parse_int("42"), Some(42));
        assert_eq!(parse_int("1_000_000"), Some(1_000_000));
    }

    #[test]
    fn parse_int_hex_and_binary() {
        assert_eq!(parse_int("0xFF_AB"), Some(0xFF_AB));
        assert_eq!(parse_int("0b10_10"), Some(0b10_10));
    }

    #[test]
    fn parse_int_overflow_is_none() {
        assert_eq!(parse_int("99999999999999999999"), None);
        assert_eq!(parse_int("0xFFFFFFFFFFFFFFFFF"), None);
    }

    #[test]
    fn parse_float_basic() {
        assert_eq!(parse_float("2.75"), Some(2.75));
        assert_eq!(parse_float("1.0e-9"), Some(1.0e-9));
        assert_eq!(parse_float("1_000.5"), Some(1_000.5));
    }

    #[test]
    fn keyword_lookup() {
        assert_eq!(keyword("if"), Some(TokenKind::If));
        assert_eq!(keyword("iffy"), None);
        assert_eq!(keyword("catch_all"), Some(TokenKind::CatchAll));
    }

    #[test]
    fn unescape_handles_all_escapes() {
        assert_eq!(
            unescape_string_text(r#"a\nb\tc\"d\\e\#f"#),
            "a\nb\tc\"d\\e#f"
        );
        assert_eq!(unescape_string_text("plain"), "plain");
    }
}
