//! The terminal UI (BRS-120): a compilation report and the heap view.
//!
//! Confined to this crate, which nothing in the compiler depends on —
//! `ratatui` and `crossterm` never reach the pipeline that builds a
//! program.
//!
//! Structured so it is testable rather than merely looked at:
//! [`model`] holds everything with a decision in it and is checked
//! directly, and [`view`] is thin enough that rendering into ratatui's
//! `TestBackend` is a fair check of the rest.

pub mod model;
pub mod view;

use std::io::{IsTerminal, Stdout};

use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

use model::{Pane, Report, State};

/// Shows `report` until the user quits.
///
/// The terminal is restored on every path out, including a panic
/// unwinding through here: a tool that leaves a shell in raw mode is
/// worse than one that never drew anything.
pub fn show(report: Report) -> std::io::Result<()> {
    // No terminal means a pipe or CI, which is where this output is
    // wanted most. Printing the same report as text is the answer; a
    // raw-mode failure there would be an opaque errno for a correct
    // thing to have done.
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        println!("{}", report.to_text());
        return Ok(());
    }

    let mut terminal = enter()?;
    let result = run(&mut terminal, report);
    leave(&mut terminal)?;

    result
}

fn enter() -> std::io::Result<Terminal<CrosstermBackend<Stdout>>> {
    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    crossterm::execute!(stdout, EnterAlternateScreen)?;

    Terminal::new(CrosstermBackend::new(stdout))
}

fn leave(terminal: &mut Terminal<CrosstermBackend<Stdout>>) -> std::io::Result<()> {
    disable_raw_mode()?;
    crossterm::execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()
}

fn run(terminal: &mut Terminal<CrosstermBackend<Stdout>>, report: Report) -> std::io::Result<()> {
    let mut state = State::default();

    loop {
        terminal.draw(|frame| view::draw(frame, &report, &state))?;

        let Event::Key(key) = event::read()? else {
            continue;
        };
        // Only presses: a terminal that reports releases would
        // otherwise move the selection twice per keystroke.
        if key.kind != KeyEventKind::Press {
            continue;
        }

        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
            KeyCode::Tab => state.toggle_pane(),
            KeyCode::Char('j') | KeyCode::Down => {
                if state.pane == Pane::Diagnostics {
                    state.select_next(report.entries.len());
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                if state.pane == Pane::Diagnostics {
                    state.select_previous();
                }
            }
            _ => {}
        }
    }
}
