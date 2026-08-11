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
use brasa_diagnostics::{Diagnostic, Severity, codes};
use brasa_lexer::LexError;
use brasa_source::{BytePosition, FileId, Span};
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
    let too_deep = deep_tree_diagnostic(&parser);

    let mut diagnostics = parser.diagnostics;
    diagnostics.extend(too_deep);

    for err in &lex_errors {
        diagnostics.push(Diagnostic::new(
            Severity::Error,
            err.message.clone(),
            err.code.to_string(),
            err.span,
        ));
    }

    diagnostics.sort_by_key(|d| (d.primary_span.start.0, d.primary_span.end.0));
    dedup_identical_diagnostics(&mut diagnostics);

    ParseResult {
        ast: parser.ast,
        roots,
        diagnostics,
        lex_errors,
    }
}

/// Reports a tree that no later phase could walk, if the parse built one.
///
/// [`Parser::enter_recursion`] bounds how deep the *parser* descends,
/// which is not the same thing as how deep the tree it produces is: the
/// Pratt loops build left-leaning chains (`1 + 1 + 1 + ...`, `x.f().f()...`,
/// `a |> f() |> f()...`) iteratively, so an arbitrarily deep tree costs
/// the parser a constant number of frames. Every later phase walks that
/// tree with real recursion, so the depth actually built is what has to
/// be bounded, and it is checked here against the same limit.
///
/// Skipped once the parser is poisoned: the recursion guard has already
/// reported this exact problem, and the truncated tree left behind says
/// nothing useful about depth.
fn deep_tree_diagnostic(parser: &Parser<'_>) -> Option<Diagnostic> {
    if parser.poisoned {
        return None;
    }

    let (span, depth) = parser.ast.deepest_node()?;
    if depth <= MAX_RECURSION_DEPTH {
        return None;
    }

    Some(
        Diagnostic::new(
            Severity::Error,
            format!("nesting too deep (limit {MAX_RECURSION_DEPTH})"),
            codes::P_NESTING_TOO_DEEP.to_string(),
            span,
        )
        .with_label(span, format!("nested {depth} levels deep here"))
        .with_note("split the expression up, binding intermediate results with `let`".to_string()),
    )
}

/// Final backstop against diagnostic cascades: drops any diagnostic that
/// repeats the exact `(message, primary_span)` of one already kept.
///
/// The parser's own `Parser::should_report_at` guard (and the lexer's
/// malformed-character-literal recovery) handle the common cascade shapes
/// at their source; this is a cheap, purely structural safety net for
/// whatever exact duplicate still slips through (e.g. two independent
/// productions that happen to fail with the same wording at the same
/// span), never for near-duplicates with different wording.
fn dedup_identical_diagnostics(diagnostics: &mut Vec<Diagnostic>) {
    let mut seen = std::collections::HashSet::new();
    diagnostics.retain(|d| seen.insert((d.message.clone(), d.primary_span)));
}

/// Upper bound on nesting depth, enforced twice against the same number:
/// as the parser descends ([`Parser::enter_recursion`]) and against the
/// tree it ends up building ([`deep_tree_diagnostic`]). Neither check
/// subsumes the other — the parser can descend without deepening the
/// tree (`((((1))))`), and it can deepen the tree without descending
/// (`1 + 1 + 1 + ...`) — and both bound real recursion, the parser's own
/// in the first case and every later phase's in the second.
///
/// 420 rather than a rounder number like 500: each nesting level costs
/// several real Rust stack frames (`parse_pipe` -> `parse_bp` ->
/// `parse_unary` -> `parse_postfix` -> `parse_primary` -> the construct's
/// own parser -> back to `parse_expr`, for the expression cluster alone),
/// and this is unoptimized debug code with no inlining. Empirically, on
/// this workspace's default `cargo test` thread stack, unguarded parens
/// nesting starts overflowing the native stack between roughly 470 and
/// 480 levels — comfortably above 400 (which must still parse with zero
/// diagnostics, see `crates/brasa_parser/tests/hardening.rs`) but well
/// below 500. 420 keeps a solid margin under the observed crash point
/// while staying above 400.
const MAX_RECURSION_DEPTH: u32 = 420;

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
    /// Current mutual-recursion depth across expr/stmt/type/pattern entry
    /// points, guarded by [`Parser::enter_recursion`]/[`Parser::exit_recursion`].
    recursion_depth: u32,
    /// Set once [`MAX_RECURSION_DEPTH`] is exceeded. While set, the cursor
    /// has already been fast-forwarded to `Eof` and every further
    /// diagnostic is suppressed, so a single too-deep report is the only
    /// output for the rest of this parse.
    poisoned: bool,
    /// The span of the last diagnostic emitted while the cursor sat at
    /// its current position, reset to `None` every time [`Parser::bump`]
    /// actually advances the cursor.
    ///
    /// Backs the diagnostic-cascade guard in [`Parser::error_expected`]/
    /// [`Parser::error_at`]: a stuck cursor (an `expect`/`error_expected`
    /// call that leaves `pos` untouched) commonly gets re-attempted by
    /// several downstream productions in the same statement — e.g.
    /// `let mut mut a = 1` fails a parameter-name check, then a `=` check,
    /// then an expression check, all still looking at the same `mut`
    /// token — which previously stacked one near-identical diagnostic per
    /// attempt. Suppressing a second diagnostic at the same span keeps
    /// exactly the first, most specific report for that root cause.
    last_error_span: Option<Span>,
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
            recursion_depth: 0,
            poisoned: false,
            last_error_span: None,
        }
    }

    /// Marks entry into one level of expr/stmt/type/pattern mutual
    /// recursion, returning whether it is safe to actually recurse.
    ///
    /// Every call must be paired with [`Parser::exit_recursion`] on every
    /// return path, including the bail-out one: the counter always tracks
    /// real call-stack depth so it can be trusted the next time the limit
    /// is checked.
    ///
    /// On first exceeding [`MAX_RECURSION_DEPTH`], this emits exactly one
    /// diagnostic and fast-forwards the cursor to `Eof`. Fast-forwarding
    /// is what keeps the rest of this parse cheap and silent: every
    /// unwinding caller's own "did I make progress"/"is this the
    /// terminator" checks see `Eof` immediately and return without
    /// recursing or emitting further diagnostics.
    fn enter_recursion(&mut self) -> bool {
        self.recursion_depth += 1;

        if self.recursion_depth <= MAX_RECURSION_DEPTH {
            return true;
        }

        if !self.poisoned {
            self.poisoned = true;
            let span = self.span();
            self.diagnostics.push(Diagnostic::new(
                Severity::Error,
                format!("nesting too deep (limit {MAX_RECURSION_DEPTH})"),
                codes::P_NESTING_TOO_DEEP.to_string(),
                span,
            ));
            self.pos = self.tokens.len() - 1;
        }

        false
    }

    fn exit_recursion(&mut self) {
        self.recursion_depth = self.recursion_depth.saturating_sub(1);
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
        self.last_error_span = None;

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
        if self.poisoned {
            return;
        }
        let span = self.span();
        if !self.should_report_at(span) {
            return;
        }
        let found = self.describe_current();
        self.diagnostics.push(
            Diagnostic::new(
                Severity::Error,
                format!("expected {what}, found {found}"),
                codes::P_EXPECTED.to_string(),
                span,
            )
            .with_label(span, format!("expected {what} here")),
        );
    }

    /// The diagnostic-cascade guard shared by [`Parser::error_expected`]
    /// and [`Parser::error_at`]: reports whether a diagnostic at `span`
    /// should actually be pushed, given [`Parser::last_error_span`].
    ///
    /// A stuck cursor (see the field docs) makes several unrelated
    /// productions re-diagnose the exact same token in a row; only the
    /// first such diagnostic is kept; the cursor moving on via
    /// [`Parser::bump`] resets the guard so a genuinely new problem at the
    /// same span later in the file is still reported.
    fn should_report_at(&mut self, span: Span) -> bool {
        if self.last_error_span == Some(span) {
            return false;
        }
        self.last_error_span = Some(span);
        true
    }

    /// Reports one unknown escape sequence (`\<c>` outside the shared
    /// `\n \t \" \\ \#` escape set) at its exact span, per the ruling in
    /// `docs/spec/02-grammar.md`'s ambiguity table: this is always an
    /// error, never a silent drop or pass-through, in both string and
    /// char literals. Shared by `expr.rs`'s string-literal decoding, which
    /// calls `brasa_token::unescape_string_text_checked`, and
    /// `pattern.rs`'s char-literal decoding, which calls the same escape
    /// table directly via `brasa_token::decode_escape`.
    fn report_unknown_escape(&mut self, span: Span, escaped: char) {
        self.error_at(
            codes::P_UNKNOWN_ESCAPE,
            span,
            format!("unknown escape sequence `\\{escaped}`"),
        );
    }

    /// Reports a `CHAR` token's unknown escape, if any, at the exact
    /// byte range of its `\<c>` inside `char_span` (the backslash sits
    /// right after the opening `'`, at byte offset 1).
    fn report_char_unknown_escape(&mut self, char_span: Span, unknown_escape: Option<char>) {
        let Some(escaped) = unknown_escape else {
            return;
        };

        let backslash_start = char_span.start.0 + 1;
        self.report_unknown_escape(
            escape_span(char_span.file, backslash_start, escaped),
            escaped,
        );
    }

    fn error_at(&mut self, code: &'static str, span: Span, message: String) {
        if self.poisoned || !self.should_report_at(span) {
            return;
        }
        self.diagnostics.push(Diagnostic::new(
            Severity::Error,
            message,
            code.to_string(),
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

/// The span of one `\<c>` escape sequence, given the byte offset of its
/// backslash: one byte for the backslash itself plus `escaped`'s own
/// UTF-8 width. Shared by [`Parser::report_char_unknown_escape`] and
/// `expr.rs`'s `Parser::report_unknown_escapes`, both of which locate an
/// unknown escape from a byte offset relative to some enclosing span.
pub(crate) fn escape_span(file: FileId, backslash_start: u32, escaped: char) -> Span {
    let end = backslash_start + 1 + escaped.len_utf8() as u32;
    Span::new(file, BytePosition(backslash_start), BytePosition(end))
}
