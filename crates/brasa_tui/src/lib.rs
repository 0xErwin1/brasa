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

/// The terminal events a driver needs, re-exported so a caller does not
/// take its own `crossterm` dependency — the point of confining the UI
/// to this crate is that nothing else grows one.
pub mod input {
    pub use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, poll, read};
}

pub mod capture;
pub mod debug_view;
pub mod debugger;
pub mod driver;
pub mod model;

use std::io::Stdout;

use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

/// Puts the terminal into the mode a full-screen UI needs.
///
/// Public because the debugger drives its own event loop: it owns a VM
/// session that cannot be handed to a generic runner, so it needs the
/// same setup and teardown.
pub fn enter() -> std::io::Result<Terminal<CrosstermBackend<Stdout>>> {
    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    crossterm::execute!(stdout, EnterAlternateScreen)?;

    Terminal::new(CrosstermBackend::new(stdout))
}

/// Restores it. Called on every path out, including a panic unwinding
/// through: a tool that leaves a shell in raw mode is worse than one
/// that never drew anything.
pub fn leave(terminal: &mut Terminal<CrosstermBackend<Stdout>>) -> std::io::Result<()> {
    disable_raw_mode()?;
    crossterm::execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()
}
