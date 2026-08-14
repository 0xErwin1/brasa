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

/// Everything one run produced, ready to draw.
#[derive(Debug, Clone, Default)]
pub struct Report {
    pub title: String,
    pub entries: Vec<Entry>,
    /// `None` when the program never ran because it did not compile.
    pub outcome: Option<String>,
    pub heap: Option<Heap>,
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

impl Report {
    pub fn errors(&self) -> usize {
        self.entries
            .iter()
            .filter(|entry| entry.severity == Severity::Error)
            .count()
    }

    /// The status line's text. Says what happened in one sentence,
    /// because that is the first thing anyone reads.
    pub fn status(&self) -> String {
        let errors = self.errors();
        let others = self.entries.len() - errors;

        match (&self.outcome, errors) {
            (_, 0) if self.entries.is_empty() => match &self.outcome {
                Some(outcome) => format!("compiled cleanly — {outcome}"),
                None => "compiled cleanly".to_string(),
            },
            (Some(outcome), 0) => format!("compiled with {others} warning(s) — {outcome}"),
            (None, 0) => format!("{others} warning(s)"),
            (_, count) => format!("{count} error(s), {others} other(s) — did not run"),
        }
    }

    /// The whole report as plain text, for when there is no terminal.
    ///
    /// Not a lesser fallback: a pipe and CI are exactly where someone
    /// wants this output most, and a tool that only works when a human
    /// is watching is the failure mode the rest of this toolchain was
    /// built to avoid.
    pub fn to_text(&self) -> String {
        let mut out = format!("{}  —  {}\n", self.title, self.status());

        for entry in &self.entries {
            out.push_str(&format!("\n{} {}\n", entry.summary(), entry.at));
            if !entry.line.trim().is_empty() {
                out.push_str(&format!("  {}\n", entry.line.trim_end()));
            }
            for note in &entry.detail {
                out.push_str(&format!("  {note}\n"));
            }
        }

        if let Some(heap) = &self.heap {
            out.push_str(&format!(
                "\nheap: {} live slots, {} free — {} bytes live, {} peak\n      {} allocations over {} collections\n",
                heap.live_slots,
                heap.free_slots,
                heap.live_bytes,
                heap.peak_bytes,
                heap.allocations,
                heap.collections,
            ));

            for (kind, count) in &heap.by_kind {
                out.push_str(&format!("      {count:>6}  {kind}\n"));
            }
        }

        out.trim_end().to_string()
    }
}

/// Which pane has focus. Two panes, because a diagnostic list and a
/// heap census are different questions and a single scrolling wall
/// would answer neither.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Pane {
    #[default]
    Diagnostics,
    Heap,
}

/// The interactive state: what is selected, and where focus is.
#[derive(Debug, Clone, Default)]
pub struct State {
    pub pane: Pane,
    pub selected: usize,
}

impl State {
    /// Moves the selection, clamped. Wrapping was deliberately not
    /// chosen: in a list of compiler errors, "past the last one" means
    /// you are at the last one, and silently jumping back to the first
    /// reads as a fresh list.
    pub fn select_next(&mut self, len: usize) {
        if len == 0 {
            return;
        }
        self.selected = (self.selected + 1).min(len - 1);
    }

    pub fn select_previous(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    pub fn toggle_pane(&mut self) {
        self.pane = match self.pane {
            Pane::Diagnostics => Pane::Heap,
            Pane::Heap => Pane::Diagnostics,
        };
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

    /// The status line distinguishes the three cases a reader acts on
    /// differently: it ran, it ran with warnings, it never ran.
    #[test]
    fn the_status_line_says_what_happened() {
        let clean = Report {
            outcome: Some("exit 0".to_string()),
            ..Report::default()
        };
        assert_eq!(clean.status(), "compiled cleanly — exit 0");

        let warned = Report {
            entries: vec![entry(Severity::Warning)],
            outcome: Some("exit 0".to_string()),
            ..Report::default()
        };
        assert_eq!(warned.status(), "compiled with 1 warning(s) — exit 0");

        let broken = Report {
            entries: vec![entry(Severity::Error), entry(Severity::Warning)],
            outcome: None,
            ..Report::default()
        };
        assert_eq!(broken.status(), "1 error(s), 1 other(s) — did not run");
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

    /// Selection clamps rather than wraps: past the last error means
    /// you are at the last error.
    #[test]
    fn selection_clamps_at_both_ends() {
        let mut state = State::default();

        state.select_previous();
        assert_eq!(state.selected, 0, "there is nothing before the first");

        state.select_next(2);
        state.select_next(2);
        state.select_next(2);
        assert_eq!(state.selected, 1, "there is nothing after the last");
    }

    /// An empty list has nothing to select, and moving must not put the
    /// selection somewhere the renderer would index.
    #[test]
    fn an_empty_list_has_no_selection_to_move() {
        let mut state = State::default();
        state.select_next(0);

        assert_eq!(state.selected, 0);
    }

    /// The text form carries the same facts as the screen. It is what
    /// a pipe and CI get, which is where this output is wanted most.
    #[test]
    fn the_text_form_carries_the_same_facts() {
        let report = Report {
            title: "script.bras".to_string(),
            entries: vec![entry(Severity::Error)],
            outcome: None,
            heap: Some(Heap {
                by_kind: vec![("Vector".to_string(), 3)],
                live_slots: 3,
                free_slots: 0,
                live_bytes: 100,
                peak_bytes: 120,
                allocations: 3,
                collections: 0,
            }),
        };

        let text = report.to_text();

        assert!(text.contains("script.bras"));
        assert!(text.contains("1 error(s)"));
        assert!(text.contains("T001"));
        assert!(text.contains("mismatched types"));
        assert!(text.contains("3 live slots"));
        assert!(text.contains("Vector"));
    }

    #[test]
    fn focus_alternates_between_the_two_panes() {
        let mut state = State::default();
        assert_eq!(state.pane, Pane::Diagnostics);

        state.toggle_pane();
        assert_eq!(state.pane, Pane::Heap);

        state.toggle_pane();
        assert_eq!(state.pane, Pane::Diagnostics);
    }
}
