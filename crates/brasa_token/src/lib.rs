//! Token definitions for Brasa.
//!
//! The token set mirrors the lexical grammar in spec: 02 — Gramática formal.
//! Kept separate from the lexer so the parser depends on token *types*
//! without depending on how they are produced; see `brasa_lexer` for the
//! scanner that turns source text into a stream of these tokens.

use brasa_source::Span;

/// The kind of a lexical token.
///
/// Fieldless by design: a token carries no payload beyond its [`Span`],
/// so consumers slice the source text themselves (via `parse_int`,
/// `parse_float`, `unescape_string_text_checked` below, or a raw substring
/// for identifiers). This keeps `TokenKind` `Copy` and avoids duplicating
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
    Do,
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
    Test,

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
    /// escape sequences. Use `unescape_string_text_checked` to decode it.
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
/// Called after an identifier has already been scanned, so this is a plain
/// string match rather than something baked into the lexer's identifier
/// pattern. `text` must include any `?`/`!` suffix the lexer absorbed into
/// the identifier, because `catch!` is a keyword spelled with one.
pub fn keyword(text: &str) -> Option<TokenKind> {
    Some(match text {
        "def" => TokenKind::Def,
        "do" => TokenKind::Do,
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
        "catch!" => TokenKind::CatchAll,
        "never" => TokenKind::Never,
        "true" => TokenKind::True,
        "false" => TokenKind::False,
        "self" => TokenKind::SelfKw,
        "unit" => TokenKind::Unit,
        "and" => TokenKind::And,
        "or" => TokenKind::Or,
        "not" => TokenKind::Not,
        "spawn" => TokenKind::Spawn,
        "test" => TokenKind::Test,
        _ => return None,
    })
}

/// Why [`parse_int`] rejected an `INT` literal's text.
///
/// The lexer's `INT` pattern (spec: 02 — Gramática formal) accepts `0x`/`0b`
/// with nothing but underscores after the prefix (`0x_`, `0b__`), per the
/// grammar's own note that underscore placement is lenient; that shape is
/// a distinct failure from a value too big for `i64`, and callers should
/// report it as such rather than a misleading "out of range".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntParseError {
    /// A `0x`/`0b` prefix with no hex/binary digits after stripping `_`.
    NoDigits,
    /// The value does not fit in `i64`.
    Overflow,
}

/// Parses an `INT` literal (spec: 02 — Gramática formal): decimal, `0x`
/// hex, or `0b` binary, with optional `_` digit separators. No octal form
/// exists in Brasa.
///
/// The lexer only ever calls this with text it already matched against
/// the `INT` pattern, so the only failure modes are the two named in
/// [`IntParseError`].
pub fn parse_int(text: &str) -> Result<i64, IntParseError> {
    let cleaned: String = text.chars().filter(|c| *c != '_').collect();

    if let Some(rest) = cleaned.strip_prefix("0x") {
        if rest.is_empty() {
            return Err(IntParseError::NoDigits);
        }
        return i64::from_str_radix(rest, 16).map_err(|_| IntParseError::Overflow);
    }

    if let Some(rest) = cleaned.strip_prefix("0b") {
        if rest.is_empty() {
            return Err(IntParseError::NoDigits);
        }
        return i64::from_str_radix(rest, 2).map_err(|_| IntParseError::Overflow);
    }

    cleaned.parse::<i64>().map_err(|_| IntParseError::Overflow)
}

/// Parses a `FLOAT` literal, stripping `_` digit separators first.
pub fn parse_float(text: &str) -> Option<f64> {
    let cleaned: String = text.chars().filter(|c| *c != '_').collect();
    cleaned.parse::<f64>().ok()
}

/// An escape sequence outside the allowed set (`\n \t \" \\ \#`), reported
/// by [`unescape_string_text_checked`] with the byte offset of its
/// backslash, relative to the start of the scanned text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnknownEscape {
    pub offset: u32,
    pub escaped: char,
}

/// The single source of truth for the escape set allowed inside `STRING`,
/// `RAWSTRING`, and `CHAR` literals (`\n \t \" \\ \#`), per
/// spec: 02 — Gramática formal's ambiguity table.
///
/// Returns `None` for any other `\<c>`, which both string and char-literal
/// decoding treat as an error rather than a silent pass-through or drop.
pub fn decode_escape(escaped: char) -> Option<char> {
    match escaped {
        'n' => Some('\n'),
        't' => Some('\t'),
        '"' => Some('"'),
        '\\' => Some('\\'),
        '#' => Some('#'),
        _ => None,
    }
}

/// Decodes the escape sequences allowed inside `STRING`/`RAWSTRING` text
/// (`\n \t \" \\ \#`, see [`decode_escape`]).
///
/// Per spec: 02 — Gramática formal's ambiguity table, any other `\<c>` is an
/// error, never a silent pass-through or drop. This validates that ruling
/// and collects every offending escape found.
///
/// `text` is the raw source slice of a `StringText` token, so it never
/// contains a bare `"` or an unescaped `#{`.
pub fn unescape_string_text_checked(text: &str) -> (String, Vec<UnknownEscape>) {
    let mut out = String::with_capacity(text.len());
    let mut unknown = Vec::new();
    let mut chars = text.char_indices().peekable();

    while let Some((offset, c)) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }

        match chars.peek().copied() {
            Some((_, other)) => {
                match decode_escape(other) {
                    Some(decoded) => out.push(decoded),
                    None => {
                        unknown.push(UnknownEscape {
                            offset: offset as u32,
                            escaped: other,
                        });
                        out.push(other);
                    }
                }
                chars.next();
            }
            None => {
                unknown.push(UnknownEscape {
                    offset: offset as u32,
                    escaped: '\\',
                });
            }
        }
    }

    (out, unknown)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_int_decimal() {
        assert_eq!(parse_int("0"), Ok(0));
        assert_eq!(parse_int("42"), Ok(42));
        assert_eq!(parse_int("1_000_000"), Ok(1_000_000));
    }

    #[test]
    fn parse_int_hex_and_binary() {
        assert_eq!(parse_int("0xFF_AB"), Ok(0xFF_AB));
        assert_eq!(parse_int("0b10_10"), Ok(0b10_10));
    }

    #[test]
    fn parse_int_overflow_is_distinguished_from_no_digits() {
        assert_eq!(
            parse_int("99999999999999999999"),
            Err(IntParseError::Overflow)
        );
        assert_eq!(
            parse_int("0xFFFFFFFFFFFFFFFFF"),
            Err(IntParseError::Overflow)
        );
        assert_eq!(parse_int("0x_"), Err(IntParseError::NoDigits));
        assert_eq!(parse_int("0b_"), Err(IntParseError::NoDigits));
        assert_eq!(parse_int("0b__"), Err(IntParseError::NoDigits));
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
        assert_eq!(keyword("catch"), Some(TokenKind::Catch));
        assert_eq!(keyword("catch!"), Some(TokenKind::CatchAll));
        assert_eq!(keyword("catch_all"), None);
        assert_eq!(keyword("do"), Some(TokenKind::Do));
        assert_eq!(keyword("door"), None);
    }

    #[test]
    fn decode_escape_handles_every_allowed_escape() {
        assert_eq!(decode_escape('n'), Some('\n'));
        assert_eq!(decode_escape('t'), Some('\t'));
        assert_eq!(decode_escape('"'), Some('"'));
        assert_eq!(decode_escape('\\'), Some('\\'));
        assert_eq!(decode_escape('#'), Some('#'));
        assert_eq!(decode_escape('q'), None);
    }

    #[test]
    fn unescape_checked_reports_unknown_escapes() {
        let (text, unknown) = unescape_string_text_checked(r#"a\qb"#);
        assert_eq!(text, "aqb");
        assert_eq!(
            unknown,
            vec![UnknownEscape {
                offset: 1,
                escaped: 'q'
            }]
        );
    }

    #[test]
    fn unescape_checked_accepts_every_valid_escape_with_no_reports() {
        let (text, unknown) = unescape_string_text_checked(r#"a\nb\tc\"d\\e\#f"#);
        assert_eq!(text, "a\nb\tc\"d\\e#f");
        assert!(unknown.is_empty());
    }
}
