//! Drawing the debugger. Layout only — every decision lives in
//! [`crate::debugger`].

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap};

use crate::debugger::{Breakpoint, Debugger, Focus, Run};

pub fn draw(frame: &mut Frame, debugger: &Debugger) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(8),
            Constraint::Length(6),
            Constraint::Length(1),
        ])
        .split(frame.area());

    header(frame, rows[0], debugger);

    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(60), Constraint::Percentage(40)])
        .split(rows[1]);

    if debugger.run == Run::Failed {
        diagnostics(frame, columns[0], debugger);
    } else {
        source(frame, columns[0], debugger);
    }

    let side = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
        .split(columns[1]);

    frames(frame, side[0], debugger);
    locals(frame, side[1], debugger);

    let bottom = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(60), Constraint::Percentage(40)])
        .split(rows[2]);

    output(frame, bottom[0], debugger);
    heap(frame, bottom[1], debugger);

    footer(frame, rows[3], debugger);

    if let Some(inspect) = &debugger.inspect {
        overlay(frame, " why is this alive ", inspect);
    }
    if debugger.help {
        overlay(frame, " keys ", HELP);
    }
}

const HELP: &str = "\
  j / k, arrows   move the cursor
  b               toggle a breakpoint on the cursor's line
  r               run, or continue
  s               step into
  n               step over
  o               step out
  p               pause a running program
  R               restart from the beginning
  w               why is the selected local still alive
  tab             next panel
  ?               this help
  q               quit";

fn header(frame: &mut Frame, area: Rect, debugger: &Debugger) {
    let colour = match debugger.run {
        Run::Ready => Color::Cyan,
        Run::Paused => Color::Yellow,
        Run::Finished(_) => Color::Green,
        Run::Failed => Color::Red,
    };

    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                debugger.title.clone(),
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
            Span::styled(debugger.status(), Style::default().fg(colour)),
        ]))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" brasa debug "),
        ),
        area,
    );
}

fn source(frame: &mut Frame, area: Rect, debugger: &Debugger) {
    let height = area.height.saturating_sub(2) as usize;
    let lines = debugger.lines();

    let start = debugger.scroll.saturating_sub(1);
    let visible = lines.iter().skip(start).take(height);

    let rendered: Vec<Line> = visible
        .map(|line| {
            // The gutter carries three facts at once: whether there is
            // a breakpoint, whether it bound, and whether this is where
            // the program stopped. A glyph each, so none needs colour
            // to be read.
            let marker = match line.breakpoint {
                Breakpoint::Set => "●",
                Breakpoint::Unbound => "○",
                Breakpoint::None => " ",
            };
            let arrow = if line.current { "▶" } else { " " };

            let style = if line.current {
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Yellow)
                    .add_modifier(Modifier::BOLD)
            } else if line.number == debugger.cursor {
                Style::default().add_modifier(Modifier::REVERSED)
            } else {
                Style::default()
            };

            Line::from(vec![
                Span::styled(marker.to_string(), Style::default().fg(Color::Red)),
                Span::raw(arrow.to_string()),
                Span::styled(
                    format!("{:>4} ", line.number),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::styled(line.text.clone(), style),
            ])
        })
        .collect();

    frame.render_widget(
        Paragraph::new(rendered).block(bordered(" source ", debugger.focus == Focus::Source)),
        area,
    );
}

fn diagnostics(frame: &mut Frame, area: Rect, debugger: &Debugger) {
    let items: Vec<ListItem> = debugger
        .diagnostics
        .iter()
        .map(|entry| {
            ListItem::new(vec![
                Line::from(entry.summary()),
                Line::from(Span::styled(
                    format!("  {}", entry.at),
                    Style::default().fg(Color::DarkGray),
                )),
            ])
        })
        .collect();

    frame.render_widget(
        List::new(items).block(bordered(
            &format!(" problems ({}) ", debugger.diagnostics.len()),
            true,
        )),
        area,
    );
}

fn frames(frame: &mut Frame, area: Rect, debugger: &Debugger) {
    if debugger.frames.is_empty() {
        frame.render_widget(
            Paragraph::new("not paused")
                .block(bordered(" frames ", debugger.focus == Focus::Frames)),
            area,
        );
        return;
    }

    // Innermost first: it is where execution is, and where a reader
    // looks before anything else.
    let items: Vec<ListItem> = debugger
        .frames
        .iter()
        .rev()
        .map(|frame| ListItem::new(format!("{}  line {}", frame.name, frame.line)))
        .collect();

    let mut state = ListState::default();
    state.select(Some(
        debugger.frames.len() - 1 - debugger.selected_frame.min(debugger.frames.len() - 1),
    ));

    frame.render_stateful_widget(
        List::new(items)
            .block(bordered(" frames ", debugger.focus == Focus::Frames))
            .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
            .highlight_symbol("> "),
        area,
        &mut state,
    );
}

fn locals(frame: &mut Frame, area: Rect, debugger: &Debugger) {
    let Some(current) = debugger.frame() else {
        frame.render_widget(
            Paragraph::new("").block(bordered(" locals ", debugger.focus == Focus::Locals)),
            area,
        );
        return;
    };

    let items: Vec<ListItem> = current
        .locals
        .iter()
        .map(|local| {
            let mut lines = vec![Line::from(vec![
                Span::styled(
                    format!("slot {} ", local.slot),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::raw(local.value.clone()),
            ])];

            for (name, value) in &local.children {
                lines.push(Line::from(Span::styled(
                    format!("    {name}: {value}"),
                    Style::default().fg(Color::Cyan),
                )));
            }

            ListItem::new(lines)
        })
        .collect();

    let mut state = ListState::default();
    state.select(Some(
        debugger
            .selected_local
            .min(current.locals.len().saturating_sub(1)),
    ));

    frame.render_stateful_widget(
        List::new(items)
            .block(bordered(" locals ", debugger.focus == Focus::Locals))
            .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
            .highlight_symbol("> "),
        area,
        &mut state,
    );
}

fn output(frame: &mut Frame, area: Rect, debugger: &Debugger) {
    let height = area.height.saturating_sub(2) as usize;

    // Tail rather than head: the newest output is what a debugging
    // session is about, and scrolling to it every time would be work
    // the tool should do.
    let start = debugger.output.len().saturating_sub(height);
    let text: Vec<Line> = debugger.output[start..]
        .iter()
        .map(|line| Line::from(line.clone()))
        .collect();

    frame.render_widget(
        Paragraph::new(if text.is_empty() {
            vec![Line::from(Span::styled(
                "(no output yet)",
                Style::default().fg(Color::DarkGray),
            ))]
        } else {
            text
        })
        .block(bordered(" output ", debugger.focus == Focus::Output)),
        area,
    );
}

fn heap(frame: &mut Frame, area: Rect, debugger: &Debugger) {
    let Some(heap) = &debugger.heap else {
        frame.render_widget(
            Paragraph::new(Span::styled(
                "(not running)",
                Style::default().fg(Color::DarkGray),
            ))
            .block(bordered(" heap ", debugger.focus == Focus::Heap)),
            area,
        );
        return;
    };

    let mut lines = vec![Line::from(format!(
        "{} live, {} free, {} bytes",
        heap.live_slots, heap.free_slots, heap.live_bytes
    ))];

    for (kind, count) in &heap.by_kind {
        lines.push(Line::from(format!("{count:>5}  {kind}")));
    }

    frame.render_widget(
        Paragraph::new(lines).block(bordered(" heap ", debugger.focus == Focus::Heap)),
        area,
    );
}

fn footer(frame: &mut Frame, area: Rect, debugger: &Debugger) {
    let keys = match debugger.run {
        Run::Failed => "? help   q quit",
        Run::Ready => "b break   r run   tab panel   ? help   q quit",
        Run::Paused => {
            "r continue   s in   n over   o out   b break   w why   tab panel   ? help   q quit"
        }
        Run::Finished(_) => "R restart   tab panel   ? help   q quit",
    };

    frame.render_widget(
        Paragraph::new(format!("[{}]  {keys}", debugger.focus.label()))
            .style(Style::default().fg(Color::DarkGray)),
        area,
    );
}

/// A centred box over the rest, for answers that interrupt reading.
fn overlay(frame: &mut Frame, title: &str, body: &str) {
    let area = frame.area();
    let width = area.width.saturating_sub(20).clamp(20, 70);
    let height = (body.lines().count() as u16 + 2).min(area.height.saturating_sub(4));

    let box_area = Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height,
    };

    frame.render_widget(Clear, box_area);
    frame.render_widget(
        Paragraph::new(body).wrap(Wrap { trim: false }).block(
            Block::default()
                .borders(Borders::ALL)
                .title(title.to_string()),
        ),
        box_area,
    );
}

fn bordered(title: &str, focused: bool) -> Block<'static> {
    let style = if focused {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default()
    };

    Block::default()
        .borders(Borders::ALL)
        .border_style(style)
        .title(title.to_string())
}

#[cfg(test)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use super::*;
    use crate::debugger::{Frame as DebugFrame, Local};

    fn screen(debugger: &Debugger) -> String {
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).expect("a test terminal");
        terminal.draw(|frame| draw(frame, debugger)).expect("draws");

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

    fn debugger() -> Debugger {
        Debugger::new(
            "script.bras".to_string(),
            vec![
                "def bump(n: int): int".to_string(),
                "  n * 2".to_string(),
                "end".to_string(),
                "".to_string(),
                "def main()".to_string(),
                "  puts bump(20)".to_string(),
                "end".to_string(),
            ],
        )
    }

    /// The source is on screen with its line numbers — the anchor the
    /// whole thing hangs on.
    #[test]
    fn the_source_is_shown_with_line_numbers() {
        let screen = screen(&debugger());

        assert!(screen.contains("def bump(n: int): int"));
        assert!(screen.contains("puts bump(20)"));
        assert!(screen.contains("1 "), "line numbers are in the gutter");
    }

    /// The gutter shows a bound breakpoint and an unbound one
    /// differently, and both differently from nothing.
    #[test]
    fn the_gutter_shows_both_kinds_of_breakpoint() {
        let mut debugger = debugger();
        debugger.bound(2, true);
        debugger.bound(4, false);

        let screen = screen(&debugger);

        assert!(screen.contains('●'), "a bound breakpoint");
        assert!(screen.contains('○'), "one that did not bind");
    }

    /// The stopped line is marked distinctly from the cursor, because
    /// the two separate as soon as anyone scrolls.
    #[test]
    fn the_current_line_is_marked_apart_from_the_cursor() {
        let mut debugger = debugger();
        debugger.run = Run::Paused;
        debugger.frames = vec![DebugFrame {
            name: "bump".to_string(),
            line: 2,
            locals: Vec::new(),
        }];
        debugger.stopped_at(2);
        debugger.move_cursor(3);

        let screen = screen(&debugger);

        assert!(screen.contains('▶'), "the program's position is marked");
        assert_eq!(debugger.cursor, 5, "and the cursor moved away from it");
    }

    /// Frames and their locals are on screen together: reading a frame
    /// without its values answers half the question.
    #[test]
    fn frames_and_locals_are_shown_together() {
        let mut debugger = debugger();
        debugger.run = Run::Paused;
        debugger.frames = vec![
            DebugFrame {
                name: "main".to_string(),
                line: 6,
                locals: Vec::new(),
            },
            DebugFrame {
                name: "bump".to_string(),
                line: 2,
                locals: vec![Local {
                    slot: 0,
                    value: "20".to_string(),
                    children: Vec::new(),
                    inspectable: false,
                }],
            },
        ];
        debugger.stopped_at(2);

        let screen = screen(&debugger);

        assert!(screen.contains("bump"), "the innermost frame");
        assert!(screen.contains("main"), "and its caller");
        assert!(screen.contains("slot 0"), "with the paused values");
        assert!(screen.contains("20"));
    }

    /// The program's own output is on screen. Without it `puts`
    /// debugging is invisible, which is how most people debug.
    #[test]
    fn the_programs_output_is_visible() {
        let mut debugger = debugger();
        debugger.output = vec!["41".to_string(), "83".to_string()];

        let screen = screen(&debugger);

        assert!(screen.contains("41"));
        assert!(screen.contains("83"));
    }

    /// The footer offers the keys that apply to the current state, so
    /// a reader is never shown a binding that would do nothing.
    #[test]
    fn the_footer_offers_only_the_keys_that_apply() {
        let mut debugger = debugger();
        assert!(screen(&debugger).contains("r run"));

        debugger.run = Run::Paused;
        let paused = screen(&debugger);
        assert!(paused.contains("s in"), "stepping applies while paused");
        assert!(paused.contains("r continue"));

        debugger.run = Run::Finished("exit 0".to_string());
        let finished = screen(&debugger);
        assert!(finished.contains("R restart"));
        assert!(!finished.contains("s in"), "stepping does not apply");
    }

    /// A program that did not compile shows the problems where the
    /// source would be: there is nothing to step through, and the
    /// errors are the whole answer.
    #[test]
    fn a_failed_compile_shows_the_problems_instead_of_source() {
        let mut debugger = debugger();
        debugger.run = Run::Failed;
        debugger.diagnostics = vec![crate::model::Entry {
            severity: brasa_diagnostics::Severity::Error,
            code: "T001".to_string(),
            message: "mismatched types".to_string(),
            at: "script.bras:2:3".to_string(),
            line: "  n * 2".to_string(),
            detail: Vec::new(),
        }];

        let screen = screen(&debugger);

        assert!(screen.contains("problems (1)"));
        assert!(screen.contains("T001"));
        assert!(screen.contains("nothing to run"));
    }

    /// The help overlay lists the bindings, so nothing has to be
    /// guessed or read from a manual.
    #[test]
    fn the_help_overlay_lists_the_bindings() {
        let mut debugger = debugger();
        debugger.help = true;

        let screen = screen(&debugger);

        assert!(screen.contains("toggle a breakpoint"));
        assert!(screen.contains("step into"));
        assert!(screen.contains("why is the selected local"));
    }

    /// The retention answer arrives as an overlay, because it
    /// interrupts reading and should be dismissible.
    #[test]
    fn the_retention_answer_is_an_overlay() {
        let mut debugger = debugger();
        debugger.inspect = Some("kept alive by: main -> all -> this".to_string());

        let screen = screen(&debugger);

        assert!(screen.contains("why is this alive"));
        assert!(screen.contains("kept alive by"));
    }
}
