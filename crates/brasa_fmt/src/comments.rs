//! The comment side-channel the formatter needs and the AST does not have.
//!
//! Comments are trivia: the parser never sees them, so nothing in
//! `brasa_ast` records where they were. [`Comments`] holds their spans
//! (from [`brasa_lexer::comment_spans`]) and hands them out in source
//! order, marking each one consumed so a comment is emitted exactly once
//! no matter which of the printer's nested loops reaches it first.

use brasa_source::{FileId, Span};

/// One comment, with its source range and its text stripped of trailing
/// whitespace (a comment runs to the end of the line, so trailing blanks
/// are invisible padding the formatter must not reproduce).
pub(crate) struct Comment {
    pub start: u32,
    pub text: String,
}

pub(crate) struct Comments {
    spans: Vec<Span>,
    texts: Vec<String>,
    consumed: Vec<bool>,
    /// Index of the first span not yet consumed; every accessor scans
    /// forward from here, so a whole file costs one pass in total.
    cursor: usize,
}

impl Comments {
    pub(crate) fn new(source: &str, file: FileId) -> Self {
        let spans = brasa_lexer::comment_spans(source, file);
        let texts = spans
            .iter()
            .map(|span| {
                source[span.start.0 as usize..span.end.0 as usize]
                    .trim_end()
                    .to_string()
            })
            .collect();
        let consumed = vec![false; spans.len()];

        Self {
            spans,
            texts,
            consumed,
            cursor: 0,
        }
    }

    fn advance_cursor(&mut self) {
        while self.cursor < self.consumed.len() && self.consumed[self.cursor] {
            self.cursor += 1;
        }
    }

    fn take(&mut self, index: usize) -> Comment {
        self.consumed[index] = true;
        let span = self.spans[index];
        let comment = Comment {
            start: span.start.0,
            text: self.texts[index].clone(),
        };
        self.advance_cursor();
        comment
    }

    /// The next unconsumed comment that starts before `upto`, if any.
    pub(crate) fn next_before(&mut self, upto: u32) -> Option<Comment> {
        self.advance_cursor();

        let index = self.cursor;
        let span = *self.spans.get(index)?;
        if span.start.0 >= upto {
            return None;
        }

        Some(self.take(index))
    }

    /// Every unconsumed comment inside `[start, end)`, in source order.
    ///
    /// This is the printer's safety net: a comment written somewhere the
    /// printer has no line of its own for — inside a vector literal, in
    /// the middle of an argument list — is still returned here, so it can
    /// be hoisted above the construct instead of being dropped.
    pub(crate) fn take_range(&mut self, start: u32, end: u32) -> Vec<Comment> {
        self.advance_cursor();

        let mut taken = Vec::new();
        let mut index = self.cursor;

        while index < self.spans.len() && self.spans[index].start.0 < end {
            if !self.consumed[index] && self.spans[index].start.0 >= start {
                taken.push(self.take(index));
            }
            index += 1;
        }

        taken
    }

    /// The unconsumed comment sitting on the same source line as `after`,
    /// if any: a trailing `# ...` that belongs to the construct that just
    /// ended rather than to whatever comes next.
    pub(crate) fn take_same_line(&mut self, source: &str, after: u32) -> Option<Comment> {
        self.advance_cursor();

        let index = self.cursor;
        let span = *self.spans.get(index)?;
        if span.start.0 < after {
            return None;
        }

        let between = &source[after as usize..span.start.0 as usize];
        if between.contains('\n') {
            return None;
        }

        Some(self.take(index))
    }
}
