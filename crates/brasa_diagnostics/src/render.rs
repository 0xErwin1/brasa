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
            self.built.insert(*id, Source::from(file.text.clone()));
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
    .with_message(&diag.message)
    .with_config(config);

    for label in &diag.labels {
        builder = builder
            .with_label(AriadneLabel::new(ByteSpan::from(label.span)).with_message(&label.message));
    }

    for note in &diag.notes {
        builder = builder.with_note(note);
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
}
