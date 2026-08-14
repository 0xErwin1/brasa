//! What the views draw, independent of any terminal.
//!
//! Kept apart from the rendering on purpose: this is the part with
//! decisions in it — which diagnostic is selected, how a severity is
//! worded, what a heap census adds up to — and it is testable without a
//! screen. The views below it are then thin enough that rendering them
//! into a `TestBackend` buffer is a fair check of the whole thing.

use brasa_diagnostics::{Diagnostic, Severity};
use brasa_source::SourceMap;

/// One diagnostic, already resolved against its source.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub severity: Severity,
    pub code: String,
    pub message: String,
    /// `path:line:col`, as every other tool in this repo spells it.
    pub at: String,
    /// The source line the span points at, for the detail pane.
    pub line: String,
    /// Notes and labels, in the order the diagnostic carries them.
    pub detail: Vec<String>,
}

impl Entry {
    pub fn from_diagnostic(sources: &SourceMap, diagnostic: &Diagnostic) -> Entry {
        let span = diagnostic.primary_span;
        let (line, column) = sources.display_line_col(&span.file, span.start);
        let source = sources.get(&span.file);

        let text = source
            .text
            .lines()
            .nth(line.saturating_sub(1) as usize)
            .unwrap_or_default()
            .to_string();

        let mut detail: Vec<String> = diagnostic
            .labels
            .iter()
            .map(|label| label.message.clone())
            .collect();
        detail.extend(diagnostic.notes.iter().cloned());

        Entry {
            severity: diagnostic.severity.clone(),
            code: diagnostic.error_code.clone(),
            message: diagnostic.message.clone(),
            at: format!("{}:{line}:{column}", source.path.display()),
            line: text,
            detail,
        }
    }

    /// The one-line form the list shows.
    pub fn summary(&self) -> String {
        format!("{} {} — {}", self.marker(), self.code, self.message)
    }

    /// A glyph rather than a colour alone: a reader who cannot see the
    /// colour still gets the severity, and a screenshot pasted into a
    /// ticket keeps it.
    pub fn marker(&self) -> &'static str {
        match self.severity {
            Severity::Error => "✗",
            Severity::Warning => "!",
            Severity::Info => "i",
            Severity::Hint => "?",
        }
    }
}

/// The heap census, flattened for display (BRS-120).
#[derive(Debug, Clone, PartialEq)]
pub struct Heap {
    pub by_kind: Vec<(String, usize)>,
    pub live_slots: usize,
    pub free_slots: usize,
    pub live_bytes: usize,
    pub peak_bytes: usize,
    pub allocations: u64,
    pub collections: u64,
}

impl From<brasa_vm::debug::HeapView> for Heap {
    fn from(view: brasa_vm::debug::HeapView) -> Heap {
        Heap {
            by_kind: view.by_kind,
            live_slots: view.live_slots,
            free_slots: view.free_slots,
            live_bytes: view.live_bytes,
            peak_bytes: view.peak_bytes,
            allocations: view.allocations,
            collections: view.collections,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(severity: Severity) -> Entry {
        Entry {
            severity,
            code: "T001".to_string(),
            message: "mismatched types".to_string(),
            at: "a.bras:1:1".to_string(),
            line: "let x = 1".to_string(),
            detail: Vec::new(),
        }
    }

    /// Severity is carried by a glyph as well as by colour, so it
    /// survives a colourless terminal and a pasted screenshot.
    #[test]
    fn severity_is_visible_without_colour() {
        assert_eq!(entry(Severity::Error).marker(), "✗");
        assert_eq!(entry(Severity::Warning).marker(), "!");
        assert_ne!(
            entry(Severity::Error).marker(),
            entry(Severity::Warning).marker()
        );
    }
}
