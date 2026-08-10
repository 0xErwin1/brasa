//! Patterns, used by `match` arms and `for` bindings.

use brasa_ast::{Literal, Pattern, PatternId};
use brasa_source::Span;
use brasa_token::TokenKind;

use crate::Parser;

impl<'a> Parser<'a> {
    /// `pattern = "_" | literal | IDENT | TYPE_IDENT ("(" pattern ("," pattern)* ")")?
    ///          | "(" pattern ("," pattern)* ")"`.
    pub(crate) fn parse_pattern(&mut self) -> PatternId {
        if !self.enter_recursion() {
            let span = self.span();
            let bail = self.ast.alloc_pattern(Pattern::Wildcard, span);
            self.exit_recursion();
            return bail;
        }

        let result = self.parse_pattern_inner();
        self.exit_recursion();
        result
    }

    fn parse_pattern_inner(&mut self) -> PatternId {
        let start = self.span();

        match self.kind() {
            TokenKind::Underscore => {
                self.bump();
                self.ast.alloc_pattern(Pattern::Wildcard, start)
            }
            TokenKind::Int => {
                let value = brasa_token::parse_int(self.slice()).unwrap_or_default();
                self.bump();
                self.ast
                    .alloc_pattern(Pattern::Literal(Literal::Int(value)), start)
            }
            TokenKind::Float => {
                let value = brasa_token::parse_float(self.slice()).unwrap_or_default();
                self.bump();
                self.ast
                    .alloc_pattern(Pattern::Literal(Literal::Float(value)), start)
            }
            TokenKind::True | TokenKind::False => {
                let value = self.at(TokenKind::True);
                self.bump();
                self.ast
                    .alloc_pattern(Pattern::Literal(Literal::Bool(value)), start)
            }
            TokenKind::Char => {
                let (value, unknown_escape) = parse_char_literal(self.slice());
                self.report_char_unknown_escape(start, unknown_escape);
                self.bump();
                self.ast
                    .alloc_pattern(Pattern::Literal(Literal::Char(value)), start)
            }
            TokenKind::StringStart | TokenKind::RawStringStart => {
                let (text, span) = self.parse_plain_string();
                self.ast
                    .alloc_pattern(Pattern::Literal(Literal::Str(text)), span)
            }
            TokenKind::Ident => {
                let name = self.slice().to_string();
                self.bump();
                self.ast.alloc_pattern(Pattern::Binding(name), start)
            }
            TokenKind::TypeIdent => self.parse_ctor_pattern(),
            TokenKind::LParen => self.parse_tuple_pattern(),
            _ => {
                self.error_expected("a pattern");
                self.ast.alloc_pattern(Pattern::Wildcard, start)
            }
        }
    }

    fn parse_ctor_pattern(&mut self) -> PatternId {
        let start = self.span();
        let name = self.slice().to_string();
        self.bump();

        let mut args = Vec::new();
        let mut end = start;

        if self.eat(TokenKind::LParen).is_some() {
            while !self.at(TokenKind::RParen) && !self.at(TokenKind::Eof) {
                let checkpoint = self.pos;
                args.push(self.parse_pattern());
                if self.eat(TokenKind::Comma).is_none() {
                    break;
                }
                self.ensure_progress(checkpoint);
            }
            if let Some(tok) = self.expect(TokenKind::RParen, "')' to close the pattern") {
                end = tok.span;
            }
        }

        self.ast
            .alloc_pattern(Pattern::Ctor { name, args }, Span::merge(&start, &end))
    }

    fn parse_tuple_pattern(&mut self) -> PatternId {
        let start = self.span();
        self.bump(); // '('

        let mut elements = Vec::new();

        while !self.at(TokenKind::RParen) && !self.at(TokenKind::Eof) {
            let checkpoint = self.pos;
            elements.push(self.parse_pattern());
            if self.eat(TokenKind::Comma).is_none() {
                break;
            }
            self.ensure_progress(checkpoint);
        }

        let close = self.expect(TokenKind::RParen, "')' to close the tuple pattern");
        let end = close.map(|t| t.span).unwrap_or(start);

        self.ast
            .alloc_pattern(Pattern::Tuple(elements), Span::merge(&start, &end))
    }
}

/// Decodes a `CHAR` token's text (including its surrounding quotes),
/// applying the same escape set as strings (`\n \t \" \\ \#`, per
/// `docs/spec/02-grammar.md`'s ambiguity table: "unknown escapes ... in
/// both string and char literals"). Any other `\<c>` is an unknown
/// escape (an ERROR, never silently passed through), reported as
/// `Some(c)` alongside the best-effort decoded value the caller still
/// uses to keep parsing.
pub(crate) fn parse_char_literal(text: &str) -> (char, Option<char>) {
    let inner = &text[1..text.len() - 1];

    if let Some(rest) = inner.strip_prefix('\\') {
        match rest.chars().next() {
            Some('n') => ('\n', None),
            Some('t') => ('\t', None),
            Some('"') => ('"', None),
            Some('\\') => ('\\', None),
            Some('#') => ('#', None),
            Some(other) => (other, Some(other)),
            None => ('\\', None),
        }
    } else {
        (inner.chars().next().unwrap_or('\0'), None)
    }
}
