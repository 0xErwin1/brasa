//! Pretty terminal rendering of [`Diagnostic`]s via `ariadne`.
//!
//! `ariadne` needs two things this crate doesn't otherwise provide: a
//! [`ariadne::Cache`] that resolves a [`FileId`] to source text, and a
//! [`ariadne::Span`] carrying that same ID. [`SourceMapCache`] below
//! bridges [`SourceMap`] into the former; spans are handed to `ariadne` as
//! plain byte ranges by switching its report [`Config`] to
//! [`IndexType::Byte`], so no byte-to-character offset translation is
//! needed even though `ariadne`'s default indexing is character-based.

use std::collections::HashMap;
use std::fmt;
use std::io::{self, Write};

use ariadne::{
    Cache, Config, IndexType, Label as AriadneLabel, Report, ReportKind, Source,
    Span as AriadneSpan,
};
use brasa_source::{FileId, SourceMap};

use crate::{Diagnostic, Severity};

/// A [`brasa_source::Span`], reinterpreted as an `ariadne` byte-offset span
/// over the file it belongs to.
#[derive(Clone, Copy, PartialEq, Eq)]
struct ByteSpan {
    file: FileId,
    start: usize,
    end: usize,
}

impl From<brasa_source::Span> for ByteSpan {
    fn from(span: brasa_source::Span) -> Self {
        Self {
            file: span.file,
            start: span.start.0 as usize,
            end: span.end.0 as usize,
        }
    }
}

impl AriadneSpan for ByteSpan {
    type SourceId = FileId;

    fn source(&self) -> &FileId {
        &self.file
    }

    fn start(&self) -> usize {
        self.start
    }

    fn end(&self) -> usize {
        self.end
    }
}

/// Replaces raw C0 control bytes (`0x00`-`0x1F`) and DEL (`0x7F`) in `text`
/// with a single visible ASCII placeholder, so that terminal control
/// sequences embedded in a source file (ANSI escapes, BEL, etc.) are never
/// forwarded verbatim into a rendered diagnostic and interpreted by the
/// user's terminal. `\n` and `\t` are left untouched: `ariadne` relies on
/// both for line splitting and column alignment.
///
/// Used both for the source snippet text ariadne quotes (where preserving
/// byte offsets matters, see below) and for diagnostic messages, label
/// messages, and notes (which may themselves embed a raw offending
/// character, e.g. a lexer's "unexpected character `<char>`" message, and
/// carry no span to keep aligned).
///
/// The placeholder is a single ASCII byte (`?`) rather than a Unicode
/// replacement character such as U+FFFD (3 bytes in UTF-8) so the
/// sanitized text keeps the exact same byte length and byte offsets as the
/// original: every [`brasa_source::Span`] computed against the original
/// source stays valid against the sanitized text handed to `ariadne`.
///
/// C0 bytes and DEL never occur as part of a multi-byte UTF-8 sequence
/// (UTF-8 continuation bytes are always `>= 0x80`), so rewriting them
/// byte-for-byte cannot produce invalid UTF-8.
///
/// C1 control codes (`U+0080`-`U+009F`) are intentionally left untouched:
/// they are encoded as 2 bytes in UTF-8, so replacing one with a 1-byte
/// placeholder would shift every following byte offset and break span
/// alignment. Handling them would require remapping spans, which is out of
/// scope for this fix.
fn sanitize_control_bytes(text: &str) -> String {
    let mut bytes = text.as_bytes().to_vec();

    for byte in &mut bytes {
        if matches!(*byte, 0x00..=0x08 | 0x0B..=0x1F | 0x7F) {
            *byte = b'?';
        }
    }

    String::from_utf8(bytes).expect("replacing ASCII control bytes cannot produce invalid UTF-8")
}

/// Adapts a [`SourceMap`] to `ariadne`'s [`Cache`] trait, lazily building
/// and memoizing one [`ariadne::Source`] per file it is asked to fetch.
struct SourceMapCache<'a> {
    sources: &'a SourceMap,
    built: HashMap<FileId, Source<String>>,
}

impl<'a> SourceMapCache<'a> {
    fn new(sources: &'a SourceMap) -> Self {
        Self {
            sources,
            built: HashMap::new(),
        }
    }
}

impl Cache<FileId> for SourceMapCache<'_> {
    type Storage = String;

    fn fetch(&mut self, id: &FileId) -> Result<&Source<String>, impl fmt::Debug> {
        if !self.built.contains_key(id) {
            let file = self.sources.get(id);
            self.built
                .insert(*id, Source::from(sanitize_control_bytes(&file.text)));
        }

        Ok::<_, ()>(&self.built[id])
    }

    fn display<'b>(&self, id: &'b FileId) -> Option<impl fmt::Display + 'b> {
        Some(self.sources.get(id).path.display().to_string())
    }
}

fn report_kind(severity: &Severity) -> ReportKind<'static> {
    match severity {
        Severity::Error => ReportKind::Error,
        Severity::Warning => ReportKind::Warning,
        Severity::Info | Severity::Hint => ReportKind::Advice,
    }
}

/// Renders `diag` as a pretty, human-readable report into `out`.
///
/// `sources` must contain every file referenced by `diag`'s spans. Pass
/// `color = false` for deterministic, ANSI-free output (used by golden
/// tests and non-tty destinations).
pub fn render(
    diag: &Diagnostic,
    sources: &SourceMap,
    out: &mut impl Write,
    color: bool,
) -> io::Result<()> {
    let config = Config::new()
        .with_index_type(IndexType::Byte)
        .with_color(color);

    let mut builder = Report::build(
        report_kind(&diag.severity),
        ByteSpan::from(diag.primary_span),
    )
    .with_code(&diag.error_code)
    .with_message(sanitize_control_bytes(&diag.message))
    .with_config(config);

    for label in &diag.labels {
        builder = builder.with_label(
            AriadneLabel::new(ByteSpan::from(label.span))
                .with_message(sanitize_control_bytes(&label.message)),
        );
    }

    for note in &diag.notes {
        builder = builder.with_note(sanitize_control_bytes(note));
    }

    let report = builder.finish();
    let mut cache = SourceMapCache::new(sources);

    report.write(&mut cache, out)
}

#[cfg(test)]
mod tests {
    use super::render;
    use crate::{Diagnostic, Severity};
    use brasa_source::SourceMap;
    use brasa_source::{BytePosition, Span};

    fn render_to_string(diag: &Diagnostic, sources: &SourceMap) -> String {
        let mut out = Vec::new();
        render(diag, sources, &mut out, false).expect("render should not fail");
        String::from_utf8(out).expect("output should be valid utf-8")
    }

    #[test]
    fn renders_a_diagnostic_with_two_labels() {
        let mut sources = SourceMap::new();
        let file = sources.add_virtual("two_labels", "let x = 1\nlet y = x + z\n".to_string());

        let primary = Span::new(file, BytePosition(23), BytePosition(24));
        let related = Span::new(file, BytePosition(4), BytePosition(5));

        let diagnostic = Diagnostic::new(
            Severity::Error,
            "undefined variable `z`".to_string(),
            "BRS-E001".to_string(),
            primary,
        )
        .with_label(primary, "used here".to_string())
        .with_label(related, "did you mean this?".to_string());

        insta::assert_snapshot!(render_to_string(&diagnostic, &sources));
    }

    #[test]
    fn renders_a_multiline_span() {
        let mut sources = SourceMap::new();
        let file = sources.add_virtual(
            "multiline",
            "def broken(\n  x: int,\n  y: int\n): int\nend\n".to_string(),
        );

        let span = Span::new(file, BytePosition(0), BytePosition(38));

        let diagnostic = Diagnostic::new(
            Severity::Error,
            "unclosed parameter list".to_string(),
            "BRS-E002".to_string(),
            span,
        )
        .with_label(span, "starts here".to_string());

        insta::assert_snapshot!(render_to_string(&diagnostic, &sources));
    }

    #[test]
    fn renders_multibyte_lines_with_correct_alignment() {
        let mut sources = SourceMap::new();
        let file = sources.add_virtual(
            "multibyte",
            "let niño = 1\nlet 世界 = niño + 1\n".to_string(),
        );

        // Points at `niño` on the second line, after the multibyte `世界`.
        let span = Span::new(file, BytePosition(27), BytePosition(32));

        let diagnostic = Diagnostic::new(
            Severity::Warning,
            "shadowed identifier".to_string(),
            "BRS-W001".to_string(),
            span,
        )
        .with_label(span, "shadows the outer `niño`".to_string());

        insta::assert_snapshot!(render_to_string(&diagnostic, &sources));
    }

    #[test]
    fn renders_source_with_tabs_and_correct_columns() {
        let mut sources = SourceMap::new();
        let file = sources.add_virtual("tabs", "let x = 1\n\tlet y = z\n".to_string());

        // Points at `z`, past a leading tab on line 2.
        let span = Span::new(file, BytePosition(19), BytePosition(20));

        let diagnostic = Diagnostic::new(
            Severity::Error,
            "undefined variable `z`".to_string(),
            "BRS-E001".to_string(),
            span,
        )
        .with_label(span, "used here".to_string());

        insta::assert_snapshot!(render_to_string(&diagnostic, &sources));
    }

    #[test]
    fn strips_raw_control_bytes_from_rendered_output() {
        let mut sources = SourceMap::new();
        let file = sources.add_virtual(
            "evil",
            "let x = \x1b[8mHIDDEN\x1b[0m @\x07\x7f\n".to_string(),
        );

        let span = Span::new(file, BytePosition(0), BytePosition(3));

        let diagnostic = Diagnostic::new(
            Severity::Error,
            "unexpected character".to_string(),
            "BRS-E003".to_string(),
            span,
        )
        .with_label(span, "here".to_string());

        let mut out = Vec::new();
        render(&diagnostic, &sources, &mut out, false).expect("render should not fail");

        assert!(
            !out.contains(&0x1B),
            "output must not contain a raw ESC byte"
        );
        assert!(
            !out.contains(&0x07),
            "output must not contain a raw BEL byte"
        );
        assert!(
            !out.contains(&0x7F),
            "output must not contain a raw DEL byte"
        );
    }
}
