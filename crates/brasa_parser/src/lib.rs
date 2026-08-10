//! Parser for Brasa.
//!
//! Recursive descent for items and statements; Pratt (binding powers) for
//! expressions, following the precedence table in
//! `docs/spec/02-grammar.md`. Produces `brasa_ast` arenas plus
//! diagnostics with recovery. Implemented in BRS-10/BRS-11.
//!
//! # Architecture
//!
//! One [`Parser`] walks a flat `Vec<Token>` (already lexed by
//! `brasa_lexer`) with a single cursor `pos`, building a `brasa_ast::Ast`
//! and a list of diagnostics as it goes. There is no separate tokenizing
//! pass done here: [`parse`] calls `brasa_lexer::lex` once up front and
//! folds any [`brasa_lexer::LexError`] into the returned diagnostics.
//!
//! Newline handling follows the grammar's cross-cutting notes: a bracket
//! depth counter (`(`, `[`, `{`) makes newlines insignificant while it is
//! greater than zero, which covers argument lists, groupings, vector
//! literals, and map/struct literals in one mechanism (braces are never
//! used for anything else in this grammar). At depth zero, newlines are
//! real statement separators, except for the explicit line-continuation
//! rule: a newline run whose next token is `|>`, `.`, or `?.` is skipped
//! by [`Parser::try_continue_across_newlines`].
//!
//! Error recovery never panics or loops forever: every list/block parsing
//! loop records its cursor position before an iteration and forces
//! progress afterward if nothing was consumed, and top-level constructs
//! synchronize to the next likely restart point on a hard parse failure.

pub mod dump;

mod expr;
mod item;
mod pattern;
mod stmt;
mod type_expr;

use brasa_ast::{Ast, ItemId};
use brasa_diagnostics::{Diagnostic, Severity};
use brasa_lexer::LexError;
use brasa_source::{FileId, Span};
use brasa_token::{Token, TokenKind};

/// The output of parsing one file: the AST arenas, the top-level item IDs
/// in source order, and every diagnostic collected along the way (lexical
/// and syntactic alike).
pub struct ParseResult {
    pub ast: Ast,
    pub roots: Vec<ItemId>,
    pub diagnostics: Vec<Diagnostic>,
    pub lex_errors: Vec<LexError>,
}

/// Parses `source` (belonging to `file` in the enclosing source map) into
/// a full [`ParseResult`]. Never fails outright: lexical and syntactic
/// errors alike become diagnostics, and the returned AST always contains
/// whatever could be recovered around them.
pub fn parse(source: &str, file: FileId) -> ParseResult {
    let (tokens, lex_errors) = brasa_lexer::lex(source, file);
    let mut parser = Parser::new(tokens, source);

    let roots = parser.parse_program();
    let mut diagnostics = parser.diagnostics;

    for err in &lex_errors {
        diagnostics.push(Diagnostic::new(
            Severity::Error,
            err.message.clone(),
            "BRS-LEX".to_string(),
            err.span,
        ));
    }

    diagnostics.sort_by_key(|d| (d.primary_span.start.0, d.primary_span.end.0));

    ParseResult {
        ast: parser.ast,
        roots,
        diagnostics,
        lex_errors,
    }
}

/// The parser's cursor and accumulated output. Every `parse_*` method
/// across this crate's modules is an inherent method on `Parser`.
struct Parser<'a> {
    tokens: Vec<Token>,
    pos: usize,
    source: &'a str,
    ast: Ast,
    diagnostics: Vec<Diagnostic>,
    /// Count of unmatched `(`/`[`/`{` currently open. Greater than zero
    /// makes [`Parser::normalize`] skip newlines automatically.
    depth: u32,
}

impl<'a> Parser<'a> {
    fn new(tokens: Vec<Token>, source: &'a str) -> Self {
        Self {
            tokens,
            pos: 0,
            source,
            ast: Ast::new(),
            diagnostics: Vec::new(),
            depth: 0,
        }
    }

    fn tok(&self) -> Token {
        self.tokens[self.pos]
    }

    fn kind(&self) -> TokenKind {
        self.tok().kind
    }

    fn span(&self) -> Span {
        self.tok().span
    }

    fn slice(&self) -> &'a str {
        let span = self.span();
        &self.source[span.start.0 as usize..span.end.0 as usize]
    }

    fn at(&self, kind: TokenKind) -> bool {
        self.kind() == kind
    }

    fn at_any(&self, kinds: &[TokenKind]) -> bool {
        kinds.contains(&self.kind())
    }

    /// Skips newlines while nested inside a bracket, per the grammar's
    /// "newlines are insignificant inside `( )` and `[ ]`" rule (extended
    /// here to `{ }`, which this grammar only ever uses for map/struct
    /// literals and inline interface constraints, none of which give
    /// newlines any meaning).
    fn normalize(&mut self) {
        if self.depth > 0 {
            while self.tokens[self.pos].kind == TokenKind::Newline {
                self.pos += 1;
            }
        }
    }

    fn bump(&mut self) -> Token {
        let tok = self.tok();
        self.pos += 1;

        match tok.kind {
            TokenKind::LParen | TokenKind::LBracket | TokenKind::LBrace => self.depth += 1,
            TokenKind::RParen | TokenKind::RBracket | TokenKind::RBrace => {
                self.depth = self.depth.saturating_sub(1);
            }
            _ => {}
        }

        self.normalize();
        tok
    }

    fn eat(&mut self, kind: TokenKind) -> Option<Token> {
        if self.at(kind) {
            Some(self.bump())
        } else {
            None
        }
    }

    /// Consumes `kind`, or emits an "expected X, found Y" diagnostic and
    /// leaves the cursor untouched so a caller's own recovery can decide
    /// what to do next.
    fn expect(&mut self, kind: TokenKind, what: &str) -> Option<Token> {
        match self.eat(kind) {
            Some(tok) => Some(tok),
            None => {
                self.error_expected(what);
                None
            }
        }
    }

    fn error_expected(&mut self, what: &str) {
        let found = self.describe_current();
        let span = self.span();
        self.diagnostics.push(
            Diagnostic::new(
                Severity::Error,
                format!("expected {what}, found {found}"),
                "BRS-PARSE".to_string(),
                span,
            )
            .with_label(span, format!("expected {what} here")),
        );
    }

    fn error_at(&mut self, span: Span, message: String) {
        self.diagnostics.push(Diagnostic::new(
            Severity::Error,
            message,
            "BRS-PARSE".to_string(),
            span,
        ));
    }

    fn describe_current(&self) -> String {
        match self.kind() {
            TokenKind::Eof => "end of file".to_string(),
            TokenKind::Newline => "newline".to_string(),
            _ => format!("`{}`", self.slice()),
        }
    }

    /// Consumes a run of one or more `Newline` tokens (the statement
    /// separator), returning whether any were found.
    fn skip_stmt_seps(&mut self) -> bool {
        let mut skipped = false;

        while self.at(TokenKind::Newline) {
            self.bump();
            skipped = true;
        }

        skipped
    }

    /// The line-continuation rule: at depth zero, if the current token is
    /// a newline run followed by `|>`, `.`, or `?.`, consumes the run and
    /// returns `true` so the caller can keep parsing the same expression.
    /// A no-op (returns `false`) at nonzero depth, since newlines are
    /// already insignificant there.
    fn try_continue_across_newlines(&mut self) -> bool {
        if self.depth > 0 || !self.at(TokenKind::Newline) {
            return false;
        }

        let mut i = self.pos;
        while self.tokens[i].kind == TokenKind::Newline {
            i += 1;
        }

        match self.tokens[i].kind {
            TokenKind::PipeGt | TokenKind::Dot | TokenKind::QuestionDot => {
                self.pos = i;
                true
            }
            _ => false,
        }
    }

    /// Forces the cursor forward by one token if `checkpoint` shows a
    /// parsing step made no progress, guaranteeing every loop terminates
    /// even after an unhandled error path.
    fn ensure_progress(&mut self, checkpoint: usize) {
        if self.pos == checkpoint && !self.at(TokenKind::Eof) {
            self.bump();
        }
    }

    fn at_item_start(&self) -> bool {
        matches!(
            self.kind(),
            TokenKind::Import
                | TokenKind::Def
                | TokenKind::Struct
                | TokenKind::Enum
                | TokenKind::Interface
                | TokenKind::Pub
                | TokenKind::Let
        )
    }

    /// Skips tokens until a plausible restart point for a new item: the
    /// next statement-separating newline at depth zero, the start of
    /// another item, or end of file.
    fn synchronize_item(&mut self) {
        while !self.at(TokenKind::Eof) {
            if self.depth == 0 && self.at(TokenKind::Newline) {
                self.bump();
                return;
            }
            if self.depth == 0 && self.at_item_start() {
                return;
            }
            self.bump();
        }
    }

    /// Skips tokens until a plausible restart point inside a block: a
    /// statement-separating newline at depth zero, a block-closing
    /// keyword, or end of file.
    fn synchronize_stmt(&mut self) {
        while !self.at(TokenKind::Eof) {
            if self.depth == 0 && self.at(TokenKind::Newline) {
                self.bump();
                return;
            }
            if self.depth == 0
                && matches!(
                    self.kind(),
                    TokenKind::End | TokenKind::Elsif | TokenKind::Else
                )
            {
                return;
            }
            self.bump();
        }
    }

    /// Scans forward from the cursor to the end of the current logical
    /// line (the next depth-zero newline, or EOF), reporting whether a
    /// `=>` appears at depth zero along the way.
    ///
    /// `match`/`catch` arm bodies use `NL block` with no other delimiter
    /// marking where the block ends and the next arm begins; since `=>`
    /// never appears in an ordinary statement (Brasa lambdas use `|x|
    /// expr`, not arrows), "this line contains a top-level `=>`" is an
    /// exact test for "this line starts a new arm" and lets arm blocks
    /// reuse the same statement parser as everything else.
    fn line_has_top_level_fat_arrow(&self) -> bool {
        let mut i = self.pos;
        let mut local_depth: i32 = 0;

        loop {
            match self.tokens[i].kind {
                TokenKind::Eof => return false,
                TokenKind::Newline if local_depth <= 0 => return false,
                TokenKind::LParen | TokenKind::LBracket | TokenKind::LBrace => local_depth += 1,
                TokenKind::RParen | TokenKind::RBracket | TokenKind::RBrace => local_depth -= 1,
                TokenKind::FatArrow if local_depth <= 0 => return true,
                _ => {}
            }
            i += 1;
        }
    }

    fn parse_program(&mut self) -> Vec<ItemId> {
        let mut roots = Vec::new();

        self.skip_stmt_seps();

        while !self.at(TokenKind::Eof) {
            let checkpoint = self.pos;

            roots.push(self.parse_item());

            // Resynchronize only when the cursor is genuinely stuck: not
            // sitting at a newline, EOF, or the start of the next item.
            // A diagnostic alone (e.g. a fully-recovered-in-place one) is
            // not evidence of a bad cursor position, but a stuck cursor
            // always needs one of its own: otherwise leftover tokens
            // (e.g. `let x = puts "a"`'s trailing `"a"`) are silently
            // skipped by `synchronize_item` with no diagnostic at all.
            if self.depth == 0
                && !self.at(TokenKind::Newline)
                && !self.at(TokenKind::Eof)
                && !self.at_item_start()
            {
                self.error_expected("a newline or the start of the next item");
                self.synchronize_item();
            }

            self.skip_stmt_seps();
            self.ensure_progress(checkpoint);
        }

        roots
    }
}
