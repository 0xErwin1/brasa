//! Lexer for Brasa, built on `logos`.
//!
//! Turns source text into `brasa_token` tokens with spans. Newlines are
//! tokens (they terminate statements); string interpolation switches the
//! lexer into a sub-mode. Implemented in BRS-8.
//!
//! # Architecture
//!
//! A single `logos::Lexer<Main>` scans the whole source. Everything
//! outside of string literals ("main mode") is produced directly by
//! `logos`. String literals are scanned by hand: on `"` or `"""` the
//! driver stops calling `logos` and walks the raw source byte-by-byte,
//! emitting `StringText`/`InterpStart`/`StringEnd` tokens itself, then
//! resumes `logos` from wherever it left off via [`logos::Lexer::bump`].
//!
//! A small mode stack (`Mode`) tracks whether we are in main-mode text, in
//! an interpolation's main-mode (which needs its own `{`/`}` depth so a
//! map literal inside `#{...}` doesn't close the interpolation early), or
//! inside a string. Pushing/popping this stack is what makes arbitrarily
//! nested interpolation (`"#{ "inner #{y}" }"`) and interpolation
//! containing braces (`"#{ {"a": 1} }"`) work.

use brasa_source::{BytePosition, FileId, Span};
use brasa_token::{Token, TokenKind, keyword};
use logos::Logos;

/// A lexical error: an unexpected character, or an unterminated string or
/// interpolation. The lexer never panics or stops on these; it records
/// them here and emits a [`TokenKind::Error`] token so the parser can
/// keep going.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LexError {
    pub message: String,
    pub span: Span,
}

/// Tokens recognized outside of string literals.
///
/// Keywords are not encoded here: identifiers are scanned generically and
/// promoted to keywords afterwards (see [`brasa_token::keyword`]), which
/// keeps this grammar free of the `if`-vs-`iffy` ambiguity that a
/// keyword-aware regex would otherwise create. `_` is likewise scanned as
/// a plain identifier and reclassified as `Underscore` by the driver,
/// avoiding a tie between an `_`-only identifier match and a dedicated
/// `_` token.
#[derive(Logos, Debug, Clone, Copy, PartialEq, Eq)]
#[logos(skip r"[ \t\r]+")]
enum Main {
    #[regex(r"[a-z_][a-zA-Z0-9_]*")]
    Ident,
    #[regex(r"[A-Z][a-zA-Z0-9_]*")]
    TypeIdent,

    #[regex(r"[0-9][0-9_]*")]
    #[regex(r"0x[0-9a-fA-F_]+")]
    #[regex(r"0b[01_]+")]
    Int,
    #[regex(r"[0-9][0-9_]*\.[0-9]+(e[+-]?[0-9]+)?")]
    Float,
    #[regex(r"'([^'\\]|\\.)'")]
    Char,

    #[regex(r"\r\n|\n")]
    Newline,
    #[regex(r"#[^\n]*", logos::skip)]
    Comment,

    #[token("\"\"\"")]
    RawQuote,
    #[token("\"")]
    Quote,

    #[token("**")]
    StarStar,
    #[token("+")]
    Plus,
    #[token("-")]
    Minus,
    #[token("*")]
    Star,
    #[token("/")]
    Slash,
    #[token("%")]
    Percent,

    #[token("==")]
    EqEq,
    #[token("!=")]
    NotEq,
    #[token("<=")]
    LtEq,
    #[token("<")]
    Lt,
    #[token(">=")]
    GtEq,
    #[token(">")]
    Gt,

    #[token("&&")]
    AndAnd,
    #[token("||")]
    OrOr,
    #[token("!")]
    Bang,

    #[token("+=")]
    PlusEq,
    #[token("-=")]
    MinusEq,
    #[token("*=")]
    StarEq,
    #[token("/=")]
    SlashEq,
    #[token("%=")]
    PercentEq,
    #[token("=")]
    Eq,

    #[token("|>")]
    PipeGt,
    #[token("|")]
    Pipe,

    #[token("?.")]
    QuestionDot,
    #[token("??")]
    QuestionQuestion,

    #[token("..=")]
    DotDotEq,
    #[token("..")]
    DotDot,
    #[token(".")]
    Dot,

    #[token("=>")]
    FatArrow,
    #[token("->")]
    Arrow,

    #[token("::")]
    ColonColon,
    #[token(":")]
    Colon,

    #[token("(")]
    LParen,
    #[token(")")]
    RParen,
    #[token("[")]
    LBracket,
    #[token("]")]
    RBracket,
    #[token("{")]
    LBrace,
    #[token("}")]
    RBrace,
    #[token(",")]
    Comma,
}

/// Maps the `Main` variants with a fixed, context-free `TokenKind` (i.e.
/// everything except `Ident`/`TypeIdent`/`Newline`/`RawQuote`/`Quote`,
/// which the driver handles specially).
fn simple_kind(main: Main) -> TokenKind {
    match main {
        Main::StarStar => TokenKind::StarStar,
        Main::Plus => TokenKind::Plus,
        Main::Minus => TokenKind::Minus,
        Main::Star => TokenKind::Star,
        Main::Slash => TokenKind::Slash,
        Main::Percent => TokenKind::Percent,
        Main::EqEq => TokenKind::EqEq,
        Main::NotEq => TokenKind::NotEq,
        Main::LtEq => TokenKind::LtEq,
        Main::Lt => TokenKind::Lt,
        Main::GtEq => TokenKind::GtEq,
        Main::Gt => TokenKind::Gt,
        Main::AndAnd => TokenKind::AndAnd,
        Main::OrOr => TokenKind::OrOr,
        Main::Bang => TokenKind::Bang,
        Main::PlusEq => TokenKind::PlusEq,
        Main::MinusEq => TokenKind::MinusEq,
        Main::StarEq => TokenKind::StarEq,
        Main::SlashEq => TokenKind::SlashEq,
        Main::PercentEq => TokenKind::PercentEq,
        Main::Eq => TokenKind::Eq,
        Main::PipeGt => TokenKind::PipeGt,
        Main::Pipe => TokenKind::Pipe,
        Main::QuestionDot => TokenKind::QuestionDot,
        Main::QuestionQuestion => TokenKind::QuestionQuestion,
        Main::DotDotEq => TokenKind::DotDotEq,
        Main::DotDot => TokenKind::DotDot,
        Main::Dot => TokenKind::Dot,
        Main::FatArrow => TokenKind::FatArrow,
        Main::Arrow => TokenKind::Arrow,
        Main::ColonColon => TokenKind::ColonColon,
        Main::Colon => TokenKind::Colon,
        Main::LParen => TokenKind::LParen,
        Main::RParen => TokenKind::RParen,
        Main::LBracket => TokenKind::LBracket,
        Main::RBracket => TokenKind::RBracket,
        Main::LBrace => TokenKind::LBrace,
        Main::RBrace => TokenKind::RBrace,
        Main::Comma => TokenKind::Comma,
        Main::Int => TokenKind::Int,
        Main::Float => TokenKind::Float,
        Main::Char => TokenKind::Char,
        Main::Ident
        | Main::TypeIdent
        | Main::Newline
        | Main::RawQuote
        | Main::Quote
        | Main::Comment => {
            unreachable!("handled by the driver before calling simple_kind")
        }
    }
}

/// Classifies a scanned `Ident` match, resolving three things the `Main`
/// regex deliberately leaves open:
///
/// - `_` alone is `Underscore`, not an identifier.
/// - A trailing `?`/`!` belongs to the identifier only when it is not
///   itself the start of the `?.` safe-navigation or `!=` operator (see
///   the ambiguity note in the crate-level docs of `brasa_lexer`'s
///   README/task write-up: the grammar allows both `IDENT "?"` and the
///   `"?."` operator, and `user.nickname?.len()` must lex as
///   field-then-safe-nav, not as an identifier literally named
///   `nickname?`).
/// - Otherwise, the matched text is promoted to a keyword if it is one.
///
/// Returns the resolved kind and how many extra bytes (0 or 1) beyond the
/// base regex match belong to the token.
fn classify_ident(source: &str, span: Span) -> (TokenKind, u32) {
    let text = &source[span.start.0 as usize..span.end.0 as usize];

    if text == "_" {
        return (TokenKind::Underscore, 0);
    }

    let after = &source[span.end.0 as usize..];
    let absorbs_suffix = (after.starts_with('?') && !after[1..].starts_with('.'))
        || (after.starts_with('!') && !after[1..].starts_with('='));

    if !absorbs_suffix && let Some(kind) = keyword(text) {
        return (kind, 0);
    }

    (TokenKind::Ident, u32::from(absorbs_suffix))
}

/// What ended a manually-scanned string text segment.
enum StringSegmentEnd {
    /// Matched the closing quote (`"` or `"""`).
    Quote,
    /// Matched `#{`, opening an interpolation.
    Interp,
    /// A regular (non-raw) string hit a literal newline before closing.
    UnterminatedNewline,
    /// Ran off the end of the source without closing.
    UnterminatedEof,
}

/// Scans `rest` (source from the current position to EOF) for the next
/// string-text boundary, honoring `\`-escapes in non-raw strings (so an
/// escaped quote or `#` does not end the segment) and treating `\` as a
/// literal character in raw strings, matching the "raw" naming
/// convention used elsewhere (see the ambiguity note returned to the
/// caller of this crate: the spec does not say whether `RAWSTRING`
/// applies escapes).
///
/// Returns the byte length of the text segment (may be 0) and what ended it.
fn scan_string_segment(rest: &str, raw: bool) -> (usize, StringSegmentEnd) {
    let mut i = 0usize;

    while i < rest.len() {
        let c = rest[i..].chars().next().expect("i < rest.len()");
        let clen = c.len_utf8();

        if !raw && c == '\\' {
            match rest[i + clen..].chars().next() {
                Some(next) => {
                    i += clen + next.len_utf8();
                    continue;
                }
                None => return (rest.len(), StringSegmentEnd::UnterminatedEof),
            }
        }

        if !raw && c == '\n' {
            return (i, StringSegmentEnd::UnterminatedNewline);
        }

        if c == '"' {
            if raw {
                if rest[i..].starts_with("\"\"\"") {
                    return (i, StringSegmentEnd::Quote);
                }
            } else {
                return (i, StringSegmentEnd::Quote);
            }
        }

        if c == '#' && rest[i..].starts_with("#{") {
            return (i, StringSegmentEnd::Interp);
        }

        i += clen;
    }

    (rest.len(), StringSegmentEnd::UnterminatedEof)
}

/// Scans forward from just after an opening `'` that failed to match the
/// `CHAR` token (its contents are not a single scalar or a single escape,
/// e.g. `'ab'`), looking for a plausible closing `'` so the whole
/// malformed literal can be reported as one diagnostic instead of one per
/// stray character reached by generic "unexpected character" recovery.
/// Stops at a newline (char literals never span lines) or EOF.
///
/// Returns the byte length up to (not including) the closing quote, and
/// whether one was found.
fn scan_malformed_char_literal(rest: &str) -> (usize, bool) {
    let mut i = 0usize;

    while i < rest.len() {
        let c = rest[i..].chars().next().expect("i < rest.len()");
        if c == '\'' {
            return (i, true);
        }
        if c == '\n' {
            return (i, false);
        }
        i += c.len_utf8();
    }

    (rest.len(), false)
}

/// What the driver is currently scanning.
#[derive(Clone, Copy)]
enum Mode {
    /// Top-level main-mode text: `{`/`}` are always ordinary tokens.
    MainTop,
    /// Main-mode text inside an interpolation. `depth` counts unmatched
    /// `{` opened since entering the interpolation, so the matching `}`
    /// (at depth 0) closes the interpolation instead of being an
    /// ordinary `RBrace`.
    MainInterp { depth: u32 },
    /// Inside a string literal, scanned by hand.
    Str { raw: bool },
}

/// Lexes `source` into a token stream, always terminated by `Eof`.
///
/// `file` identifies `source` in the enclosing [`brasa_source::SourceMap`]
/// and is stamped onto every emitted span.
///
/// Never fails: lexical errors are collected in the returned `Vec<LexError>`
/// and represented in the token stream as `TokenKind::Error`, so the parser
/// can keep consuming tokens after a bad character or an unterminated
/// string/interpolation.
pub fn lex(source: &str, file: FileId) -> (Vec<Token>, Vec<LexError>) {
    // A leading UTF-8 BOM (U+FEFF, 3 bytes) is silently skipped: some
    // editors and Windows tooling prepend it, and it carries no meaning in
    // Brasa source. The lexer itself only ever scans `content` (the text
    // after the BOM), but every span it emits is shifted back by
    // `bom_len` so it still indexes correctly into the original `source`
    // this function was called with, preserving that contract for every
    // caller of this public API.
    let bom_len = source
        .strip_prefix('\u{FEFF}')
        .map_or(0, |_| '\u{FEFF}'.len_utf8()) as u32;
    let content = &source[bom_len as usize..];

    let mut tokens = Vec::new();
    let mut errors = Vec::new();
    let mut lexer = Main::lexer(content);
    let mut modes = vec![Mode::MainTop];
    let span_at = |start: u32, end: u32| {
        Span::new(
            file,
            BytePosition(start + bom_len),
            BytePosition(end + bom_len),
        )
    };

    loop {
        let mode = *modes.last().expect("mode stack is never empty");

        match mode {
            Mode::MainTop | Mode::MainInterp { .. } => {
                let Some(result) = lexer.next() else {
                    if matches!(mode, Mode::MainInterp { .. }) {
                        let pos = content.len() as u32;
                        let span = span_at(pos, pos);
                        errors.push(LexError {
                            message: "unterminated interpolation".to_string(),
                            span,
                        });
                        tokens.push(Token::new(TokenKind::Error, span));
                    }
                    break;
                };

                let raw_span = lexer.span();
                let span = span_at(raw_span.start as u32, raw_span.end as u32);

                match result {
                    Ok(Main::Ident) => {
                        let (kind, extra) = classify_ident(source, span);
                        if extra > 0 {
                            lexer.bump(extra as usize);
                        }
                        // `span` is already absolute (shifted by `span_at`
                        // above): extend its end in place rather than
                        // reapplying `span_at`, which would shift twice.
                        let extended =
                            Span::new(span.file, span.start, BytePosition(span.end.0 + extra));
                        tokens.push(Token::new(kind, extended));
                    }
                    Ok(Main::TypeIdent) => tokens.push(Token::new(TokenKind::TypeIdent, span)),
                    Ok(Main::Newline) => tokens.push(Token::new(TokenKind::Newline, span)),
                    Ok(Main::RawQuote) => {
                        tokens.push(Token::new(TokenKind::RawStringStart, span));
                        modes.push(Mode::Str { raw: true });
                    }
                    Ok(Main::Quote) => {
                        tokens.push(Token::new(TokenKind::StringStart, span));
                        modes.push(Mode::Str { raw: false });
                    }
                    Ok(Main::LBrace) => {
                        if let Some(Mode::MainInterp { depth }) = modes.last_mut() {
                            *depth += 1;
                        }
                        tokens.push(Token::new(TokenKind::LBrace, span));
                    }
                    Ok(Main::RBrace) => match modes.last_mut() {
                        Some(Mode::MainInterp { depth: 0 }) => {
                            modes.pop();
                            tokens.push(Token::new(TokenKind::InterpEnd, span));
                        }
                        Some(Mode::MainInterp { depth }) => {
                            *depth -= 1;
                            tokens.push(Token::new(TokenKind::RBrace, span));
                        }
                        _ => tokens.push(Token::new(TokenKind::RBrace, span)),
                    },
                    Ok(other) => tokens.push(Token::new(simple_kind(other), span)),
                    Err(()) if lexer.slice() == "'" => {
                        // A lone `'` that didn't match `CHAR` at all (e.g.
                        // `'ab'`, two scalars with no escape): treat it as
                        // one malformed character literal spanning up to
                        // the next `'` (or the end of the line/file if
                        // none follows), rather than letting the generic
                        // per-character fallback below fire twice (once
                        // for the opening quote, once for the closing
                        // one), which produced two identical-looking
                        // "unexpected character" diagnostics for a single
                        // root cause.
                        let after_quote = raw_span.end;
                        let (len, closed) = scan_malformed_char_literal(&content[after_quote..]);
                        let consumed = len + usize::from(closed);
                        if consumed > 0 {
                            lexer.bump(consumed);
                        }
                        let full_span =
                            span_at(raw_span.start as u32, (after_quote + consumed) as u32);
                        errors.push(LexError {
                            message: "malformed character literal".to_string(),
                            span: full_span,
                        });
                        tokens.push(Token::new(TokenKind::Error, full_span));
                    }
                    Err(()) => {
                        let text = lexer.slice();
                        errors.push(LexError {
                            message: format!("unexpected character `{text}`"),
                            span,
                        });
                        tokens.push(Token::new(TokenKind::Error, span));
                    }
                }
            }
            Mode::Str { raw } => {
                let start = lexer.span().end;
                let rest = &content[start..];
                let (len, end) = scan_string_segment(rest, raw);

                if len > 0 {
                    let text_span = span_at(start as u32, (start + len) as u32);
                    tokens.push(Token::new(TokenKind::StringText, text_span));
                    lexer.bump(len);
                }

                let after_text = lexer.span().end;

                match end {
                    StringSegmentEnd::Quote => {
                        let qlen = if raw { 3 } else { 1 };
                        lexer.bump(qlen);
                        let qspan = span_at(after_text as u32, (after_text + qlen) as u32);
                        tokens.push(Token::new(TokenKind::StringEnd, qspan));
                        modes.pop();
                    }
                    StringSegmentEnd::Interp => {
                        lexer.bump(2);
                        let ispan = span_at(after_text as u32, (after_text + 2) as u32);
                        tokens.push(Token::new(TokenKind::InterpStart, ispan));
                        modes.push(Mode::MainInterp { depth: 0 });
                    }
                    StringSegmentEnd::UnterminatedNewline | StringSegmentEnd::UnterminatedEof => {
                        let pos = after_text as u32;
                        let span = span_at(pos, pos);
                        errors.push(LexError {
                            message: "unterminated string literal".to_string(),
                            span,
                        });
                        tokens.push(Token::new(TokenKind::Error, span));
                        modes.pop();
                    }
                }
            }
        }
    }

    let eof = content.len() as u32;
    tokens.push(Token::new(TokenKind::Eof, span_at(eof, eof)));
    (tokens, errors)
}

#[cfg(test)]
mod tests;
