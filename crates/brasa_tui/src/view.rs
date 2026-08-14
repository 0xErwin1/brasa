//! Drawing. Thin over [`crate::model`] on purpose: everything with a
//! decision in it lives there, so what is left here is layout.
//!
//! Every view takes a `Frame`, so a test can render into ratatui's
//! `TestBackend` and assert on the buffer — a TUI that is checked in CI
//! rather than by looking at it.

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};

use brasa_diagnostics::Severity;

use crate::model::{Pane, Report, State};

pub fn draw(frame: &mut Frame, report: &Report, state: &State) {
    let areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(6),
            Constraint::Length(1),
        ])
        .split(frame.area());

    header(frame, areas[0], report);

    match state.pane {
        Pane::Diagnostics => diagnostics(frame, areas[1], report, state),
        Pane::Heap => heap(frame, areas[1], report),
    }

    footer(frame, areas[2], state);
}

fn header(frame: &mut Frame, area: Rect, report: &Report) {
    let colour = if report.errors() > 0 {
        Color::Red
    } else if report.entries.is_empty() {
        Color::Green
    } else {
        Color::Yellow
    };

    let text = Line::from(vec![
        Span::styled(
            report.title.clone(),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(report.status(), Style::default().fg(colour)),
    ]);

    frame.render_widget(
        Paragraph::new(text).block(Block::default().borders(Borders::ALL).title(" brasa ")),
        area,
    );
}

fn diagnostics(frame: &mut Frame, area: Rect, report: &Report, state: &State) {
    // The list and the detail share the pane: a diagnostic's message is
    // a summary and its labels are the part that says what to do, and
    // making the reader toggle between them would hide half the answer.
    let split = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(55), Constraint::Percentage(45)])
        .split(area);

    if report.entries.is_empty() {
        frame.render_widget(
            Paragraph::new("no diagnostics")
                .block(Block::default().borders(Borders::ALL).title(" problems ")),
            split[0],
        );
        frame.render_widget(
            Paragraph::new("").block(Block::default().borders(Borders::ALL).title(" detail ")),
            split[1],
        );
        return;
    }

    let items: Vec<ListItem> = report
        .entries
        .iter()
        .map(|entry| {
            ListItem::new(entry.summary()).style(Style::default().fg(colour_of(&entry.severity)))
        })
        .collect();

    let mut list_state = ListState::default();
    list_state.select(Some(state.selected.min(report.entries.len() - 1)));

    frame.render_stateful_widget(
        List::new(items)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(format!(" problems ({}) ", report.entries.len())),
            )
            .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
            .highlight_symbol("> "),
        split[0],
        &mut list_state,
    );

    let entry = &report.entries[state.selected.min(report.entries.len() - 1)];
    let mut lines = vec![
        Line::from(Span::styled(
            entry.at.clone(),
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(entry.line.clone()),
        Line::from(""),
    ];
    lines.extend(entry.detail.iter().map(|note| Line::from(note.clone())));

    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .block(Block::default().borders(Borders::ALL).title(" detail ")),
        split[1],
    );
}

fn heap(frame: &mut Frame, area: Rect, report: &Report) {
    let Some(heap) = &report.heap else {
        frame.render_widget(
            Paragraph::new("the program did not run, so there is no heap to show")
                .block(Block::default().borders(Borders::ALL).title(" heap ")),
            area,
        );
        return;
    };

    let mut lines = vec![
        Line::from(format!(
            "{} live slots, {} free",
            heap.live_slots, heap.free_slots
        )),
        Line::from(format!(
            "{} bytes live, {} peak",
            heap.live_bytes, heap.peak_bytes
        )),
        Line::from(format!(
            "{} allocations over {} collections",
            heap.allocations, heap.collections
        )),
        Line::from(""),
    ];

    if heap.by_kind.is_empty() {
        lines.push(Line::from("the arena is empty"));
    } else {
        for (kind, count) in &heap.by_kind {
            lines.push(Line::from(format!("{count:>6}  {kind}")));
        }
    }

    frame.render_widget(
        Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title(" heap ")),
        area,
    );
}

fn footer(frame: &mut Frame, area: Rect, state: &State) {
    let other = match state.pane {
        Pane::Diagnostics => "heap",
        Pane::Heap => "problems",
    };

    frame.render_widget(
        Paragraph::new(format!("j/k move   tab {other}   q quit"))
            .style(Style::default().fg(Color::DarkGray)),
        area,
    );
}

fn colour_of(severity: &Severity) -> Color {
    match severity {
        Severity::Error => Color::Red,
        Severity::Warning => Color::Yellow,
        Severity::Info => Color::Cyan,
        Severity::Hint => Color::DarkGray,
    }
}

#[cfg(test)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use super::*;
    use crate::model::{Entry, Heap};

    /// Renders and returns the screen as text, so an assertion reads
    /// like what a user would see.
    fn screen(report: &Report, state: &State) -> String {
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).expect("a test terminal");
        terminal
            .draw(|frame| draw(frame, report, state))
            .expect("draws");

        let buffer = terminal.backend().buffer().clone();
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn error(code: &str, message: &str) -> Entry {
        Entry {
            severity: Severity::Error,
            code: code.to_string(),
            message: message.to_string(),
            at: "script.bras:4:7".to_string(),
            line: "  let x: int = \"no\"".to_string(),
            detail: vec!["expected `int`".to_string()],
        }
    }

    #[test]
    fn a_clean_run_says_so_and_shows_no_problems() {
        let report = Report {
            title: "script.bras".to_string(),
            outcome: Some("exit 0".to_string()),
            ..Report::default()
        };

        let screen = screen(&report, &State::default());

        assert!(screen.contains("compiled cleanly"));
        assert!(screen.contains("no diagnostics"));
    }

    /// The selected diagnostic's detail is on screen at the same time
    /// as the list: the message summarises and the label says what to
    /// do, and a reader needs both.
    #[test]
    fn the_selected_diagnostic_shows_its_detail() {
        let report = Report {
            title: "script.bras".to_string(),
            entries: vec![
                error("T001", "mismatched types"),
                error("R001", "unknown name"),
            ],
            ..Report::default()
        };

        let screen = screen(&report, &State::default());

        assert!(screen.contains("T001"), "the list shows both");
        assert!(screen.contains("R001"));
        assert!(screen.contains("script.bras:4:7"), "the detail is present");
        assert!(screen.contains("expected `int`"));
        assert!(screen.contains("problems (2)"));
    }

    /// Moving the selection changes what the detail pane shows. Without
    /// this the list would be decoration.
    #[test]
    fn moving_the_selection_changes_the_detail() {
        let mut second = error("R001", "unknown name");
        second.detail = vec!["not found in this scope".to_string()];

        let report = Report {
            entries: vec![error("T001", "mismatched types"), second],
            ..Report::default()
        };

        let first = screen(&report, &State::default());
        assert!(!first.contains("not found in this scope"));

        let mut state = State::default();
        state.select_next(report.entries.len());

        let moved = screen(&report, &state);
        assert!(moved.contains("not found in this scope"));
    }

    /// The heap pane shows the census (BRS-120).
    #[test]
    fn the_heap_pane_shows_the_census() {
        let report = Report {
            heap: Some(Heap {
                by_kind: vec![("Vector".to_string(), 3), ("struct".to_string(), 2)],
                live_slots: 5,
                free_slots: 1,
                live_bytes: 912,
                peak_bytes: 1024,
                allocations: 6,
                collections: 0,
            }),
            outcome: Some("exit 0".to_string()),
            ..Report::default()
        };

        let mut state = State::default();
        state.toggle_pane();

        let screen = screen(&report, &state);

        assert!(screen.contains("5 live slots, 1 free"));
        assert!(screen.contains("912 bytes live"));
        assert!(screen.contains("Vector"));
        assert!(screen.contains("struct"));
    }

    /// A program that never ran has no heap, and the pane says that
    /// rather than showing zeros that read like an empty heap.
    #[test]
    fn the_heap_pane_distinguishes_did_not_run_from_empty() {
        let report = Report {
            entries: vec![error("T001", "mismatched types")],
            heap: None,
            ..Report::default()
        };

        let mut state = State::default();
        state.toggle_pane();

        let screen = screen(&report, &state);
        assert!(screen.contains("did not run"));
    }

    /// The footer names the pane the tab key goes to, so the binding is
    /// discoverable without a manual.
    #[test]
    fn the_footer_names_where_tab_goes() {
        let report = Report::default();

        assert!(screen(&report, &State::default()).contains("tab heap"));

        let mut state = State::default();
        state.toggle_pane();
        assert!(screen(&report, &state).contains("tab problems"));
    }
}
