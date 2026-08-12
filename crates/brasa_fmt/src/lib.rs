//! `brasa fmt`: the canonical formatter for Brasa source, implemented in
//! BRS-91.
//!
//! # Architecture
//!
//! The formatter prints the AST, not the token stream: [`format`] parses
//! the source and walks the resulting `brasa_ast::Ast`, so the output's
//! shape is decided by the tree rather than by whatever spacing the
//! author happened to use. Two things the AST alone cannot answer are
//! read back from the original source through node spans:
//!
//! - **Leaf spelling.** Literals are printed verbatim from their span,
//!   so `0xFF` does not become `255`, `1.50` does not become `1.5`, and
//!   a string keeps its own escapes. The same trick recovers the
//!   spellings the AST normalizes away: `and` vs `&&`, `not` vs `!`, the
//!   inline `if ... then` form, `puts x` vs `puts(x)`, and a trailing
//!   `do ... end` block.
//! - **Author intent that is not a tree property.** One blank line
//!   between two statements is preserved (more than one is collapsed),
//!   and a method chain the author split across lines stays split even
//!   when it would fit on one.
//!
//! Comments are not in the AST at all; [`comments`] recovers their spans
//! from the lexer and the printer attaches them to the construct they
//! precede, trail, or sit inside. See [`comments::Comments::take_range`]
//! for what happens to a comment written in a position the printer has no
//! line for.
//!
//! # What it normalizes
//!
//! Indentation, spacing, one statement per line, and runs of blank lines
//! (collapsed to one). Two normalizations go further and are deliberate:
//!
//! - **Parentheses are re-derived.** `brasa_ast` has no node for
//!   grouping, so the output carries exactly the parentheses the
//!   precedence table requires — a redundant pair disappears. The
//!   safety net below is what makes that safe to do.
//! - **A comment with no line of its own is hoisted.** One written
//!   inside a construct that the formatter prints on a single line (in
//!   the middle of an argument list, say) moves onto its own line just
//!   above that construct. Nothing is ever dropped; a comment between
//!   statements, at the end of a block, or trailing a line keeps its
//!   place exactly.
//!
//! # Line breaking
//!
//! A construct is printed on one line when it fits in [`MAX_WIDTH`]
//! columns, and broken otherwise. Only constructs that *can* be broken
//! are: newlines are statement separators in Brasa, and the sole
//! continuation rules are bracket nesting and a line starting with `|>`,
//! `.` or `?.` (`docs/spec/02-grammar.md`). So argument lists, collection
//! literals, method chains and pipes break, and a long binary expression
//! does not — breaking one would change what the program parses as.
//!
//! # Safety net
//!
//! [`format`] reparses its own output and compares the span-free AST dump
//! against the input's. A formatter that changed the tree returns
//! [`FormatError::Unstable`] instead of silently handing back rewritten
//! semantics.

mod comments;
mod exprs;
mod items;
mod stmts;

use brasa_ast::Ast;
use brasa_diagnostics::{Diagnostic, Severity};
use brasa_source::{FileId, Span};

use comments::Comments;

/// The column the formatter tries to keep every line within.
pub const MAX_WIDTH: usize = 100;

/// One indentation level, in spaces.
pub const INDENT: usize = 2;

#[derive(Debug)]
pub enum FormatError {
    /// The source does not parse. Formatting a file with syntax errors
    /// would mean guessing at what the author meant, so it is refused and
    /// the diagnostics are handed back for the caller to render.
    Parse(Vec<Diagnostic>),
    /// The formatter produced output that does not parse back into the
    /// same tree. Always a bug in this crate; reported rather than
    /// returned as output, so no caller can overwrite a file with it.
    Unstable(String),
}

/// Formats one file's `source`, returning the formatted text (always
/// newline-terminated).
pub fn format(source: &str, file: FileId) -> Result<String, FormatError> {
    let parsed = brasa_parser::parse(source, file);
    if parsed
        .diagnostics
        .iter()
        .any(|diag| diag.severity == Severity::Error)
    {
        return Err(FormatError::Parse(parsed.diagnostics));
    }

    let mut printer = Printer {
        src: source,
        ast: &parsed.ast,
        comments: Comments::new(source, file),
    };
    let formatted = printer.program(&parsed.roots);

    let before = brasa_parser::dump::dump(&parsed.ast, &parsed.roots);
    let reparsed = brasa_parser::parse(&formatted, file);
    if let Some(diag) = reparsed
        .diagnostics
        .iter()
        .find(|diag| diag.severity == Severity::Error)
    {
        return Err(FormatError::Unstable(format!(
            "formatted output does not parse: {} at byte {}",
            diag.message, diag.primary_span.start.0
        )));
    }

    let after = brasa_parser::dump::dump(&reparsed.ast, &reparsed.roots);
    if before != after {
        return Err(FormatError::Unstable(
            "formatted output parses into a different tree".to_string(),
        ));
    }

    verify_comments_survived(source, &formatted, file)?;

    Ok(formatted)
}

/// Checks that the output carries exactly the comments the input did.
///
/// The tree comparison above cannot do this: the dump is comment-free by
/// construction, so a comment the printer forgot to flush — or flushed
/// twice — reparses into an identical tree and passes unnoticed. Compared
/// as a sorted multiset rather than in order, because hoisting a comment
/// that has no line of its own is allowed to move it; losing one or
/// inventing one is not.
///
/// What this deliberately does not catch is a comment that survives with
/// its text intact but lands against the wrong construct. Nothing cheap
/// can: the "right" construct for a comment is a judgement about intent,
/// not a property of the text. That class is held by tests instead.
fn verify_comments_survived(
    source: &str,
    formatted: &str,
    file: FileId,
) -> Result<(), FormatError> {
    let before = comment_texts(source, file);
    let after = comment_texts(formatted, file);

    if before == after {
        return Ok(());
    }

    Err(FormatError::Unstable(format!(
        "formatted output does not carry the same comments: {} in, {} out",
        before.len(),
        after.len()
    )))
}

fn comment_texts(source: &str, file: FileId) -> Vec<&str> {
    let mut texts: Vec<&str> = brasa_lexer::comment_spans(source, file)
        .iter()
        .map(|span| source[span.start.0 as usize..span.end.0 as usize].trim_end())
        .collect();

    texts.sort_unstable();
    texts
}

/// Whether `source` is already formatted, without producing the output.
pub fn is_formatted(source: &str, file: FileId) -> Result<bool, FormatError> {
    Ok(format(source, file)? == source)
}

pub(crate) struct Printer<'a> {
    src: &'a str,
    ast: &'a Ast,
    comments: Comments,
}

/// An output buffer that thinks in lines: it collapses runs of blank
/// lines, never opens with one, and can staple a trailing comment onto
/// whatever line was written last.
pub(crate) struct Lines {
    out: String,
    pending_blank: bool,
}

impl Lines {
    pub(crate) fn new() -> Self {
        Self {
            out: String::new(),
            pending_blank: false,
        }
    }

    pub(crate) fn push(&mut self, text: &str) {
        if !self.out.is_empty() {
            self.out.push('\n');
            if self.pending_blank {
                self.out.push('\n');
            }
        }
        self.pending_blank = false;
        self.out.push_str(text);
    }

    /// Requests a blank separator line before the next line written.
    /// Ignored at the start of the buffer, and idempotent, so blank runs
    /// collapse to one and a block never opens or closes with one.
    pub(crate) fn blank(&mut self) {
        if self.out.is_empty() {
            return;
        }
        self.pending_blank = true;
    }

    /// Appends a trailing comment to the last written line.
    pub(crate) fn trail(&mut self, text: &str) {
        if self.out.is_empty() {
            self.push(text);
            return;
        }
        self.out.push_str("  ");
        self.out.push_str(text);
    }

    pub(crate) fn finish(self) -> String {
        self.out
    }
}

pub(crate) fn indent_of(level: usize) -> String {
    " ".repeat(level)
}

/// Whether `text` still fits when it starts at column `col`. Only the
/// first line is measured: a construct that already contains a newline
/// (a raw string, an inner block) has committed to being multi-line, and
/// what matters is whether its opening line still lands inside the limit.
pub(crate) fn fits(col: usize, text: &str) -> bool {
    let first = text.split('\n').next().unwrap_or(text);
    col + first.chars().count() <= MAX_WIDTH
}

impl<'a> Printer<'a> {
    pub(crate) fn slice(&self, span: Span) -> &'a str {
        &self.src[span.start.0 as usize..span.end.0 as usize]
    }

    pub(crate) fn text(&self, start: u32, end: u32) -> &'a str {
        &self.src[start as usize..end as usize]
    }

    /// Whether the line above the one `pos` sits on is blank.
    ///
    /// Phrased in terms of lines rather than of the gap since the
    /// previous construct: not every construct in the AST records where
    /// it starts (a struct method's span begins at its *name*, after the
    /// `def`), and every one of them does start on its own line.
    pub(crate) fn blank_before(&self, pos: u32) -> bool {
        let head = &self.src[..pos as usize];

        let line_start = head.rfind('\n').map_or(0, |index| index + 1);
        if line_start == 0 {
            return false;
        }

        let above = &head[..line_start - 1];
        let above_start = above.rfind('\n').map_or(0, |index| index + 1);
        above[above_start..].trim().is_empty()
    }

    /// Emits every pending comment that starts before `upto` at `level`,
    /// preserving the blank lines around them.
    pub(crate) fn emit_comments_before(&mut self, lines: &mut Lines, level: usize, upto: u32) {
        while let Some(comment) = self.comments.next_before(upto) {
            if self.blank_before(comment.start) {
                lines.blank();
            }
            lines.push(&format!("{}{}", indent_of(level), comment.text));
        }
    }

    /// Emits the comments a construct swallowed: ones that fell inside
    /// its span with no line of their own. They are hoisted onto their
    /// own lines just above it, which keeps every comment in the output
    /// even when its exact column cannot be reproduced.
    pub(crate) fn emit_hoisted(&mut self, lines: &mut Lines, level: usize, span: Span) {
        for comment in self.comments.take_range(span.start.0, span.end.0) {
            lines.push(&format!("{}{}", indent_of(level), comment.text));
        }
    }

    /// Appends the trailing comment on the same line as `after`, if there
    /// is one.
    pub(crate) fn emit_trailing(&mut self, lines: &mut Lines, after: u32) {
        if let Some(comment) = self.comments.take_same_line(self.src, after) {
            lines.trail(&comment.text);
        }
    }

    /// The byte offset where the `end` keyword closing a construct
    /// starts, given the construct's own span. Every `end`-terminated
    /// node's span stops at that keyword, so comments belonging inside
    /// the body are exactly the ones before this offset.
    pub(crate) fn body_region_end(&self, span: Span) -> u32 {
        span.end.0.saturating_sub(3)
    }

    /// Whether the source at `pos` (after any whitespace) starts with
    /// `word` as a whole keyword rather than as an identifier prefix.
    pub(crate) fn keyword_follows(&self, pos: u32, word: &str) -> bool {
        let rest = self.src[pos as usize..].trim_start();
        let Some(after) = rest.strip_prefix(word) else {
            return false;
        };

        !after
            .chars()
            .next()
            .is_some_and(|c| c.is_alphanumeric() || c == '_')
    }
}

#[cfg(test)]
mod tests;
