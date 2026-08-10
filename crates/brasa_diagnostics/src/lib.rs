//! Diagnostic types for the Brasa compiler.
//!
//! Every phase reports errors as a [`Diagnostic`] built against a
//! [`brasa_source::Span`]; only the CLI decides how to render them.
//! Rendering (via `ariadne` or otherwise) is out of scope here — see
//! BRS-12.

use brasa_source::Span;

#[derive(Debug, Clone, PartialEq)]
pub enum Severity {
    Info,
    Warning,
    Error,
    Hint,
}

#[derive(Debug, Clone)]
pub struct Label {
    pub span: Span,
    pub message: String,
}

#[derive(Debug, Clone)]
pub struct Diagnostic {
    pub severity: Severity,
    pub message: String,
    pub error_code: String,
    pub primary_span: Span,
    pub labels: Vec<Label>,
    pub notes: Vec<String>,
}

impl Diagnostic {
    pub fn new(
        severity: Severity,
        message: String,
        error_code: String,
        primary_span: Span,
    ) -> Self {
        Self {
            severity,
            message,
            error_code,
            primary_span,
            labels: Vec::new(),
            notes: Vec::new(),
        }
    }

    pub fn with_label(mut self, span: Span, message: String) -> Self {
        self.labels.push(Label { span, message });
        self
    }

    pub fn with_note(mut self, note: String) -> Self {
        self.notes.push(note);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::{Diagnostic, Severity};
    use brasa_source::{BytePosition, FileId, Span};

    #[test]
    fn builder_accumulates_labels_and_notes() {
        let file = FileId::new(0);
        let span = Span::new(file, BytePosition(0), BytePosition(3));

        let diagnostic = Diagnostic::new(
            Severity::Error,
            "unexpected token".to_string(),
            "E0001".to_string(),
            span,
        )
        .with_label(span, "here".to_string())
        .with_note("check the grammar".to_string());

        assert_eq!(diagnostic.labels.len(), 1);
        assert_eq!(diagnostic.notes, vec!["check the grammar".to_string()]);
    }
}
