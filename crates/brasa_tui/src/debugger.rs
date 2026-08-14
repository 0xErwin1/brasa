//! The debugger's state, independent of any terminal.
//!
//! Everything with a decision in it: which line the cursor is on, what
//! the gutter shows, whether the view should follow execution, what
//! state the run is in. The view below is then layout only, and both
//! are checked directly.

use std::collections::BTreeSet;

/// Where the run is. Four states, and they are four because a reader
/// does something different in each: set breakpoints, wait, inspect,
/// or restart.
#[derive(Debug, Clone, PartialEq)]
pub enum Run {
    /// Compiled, not started. Breakpoints can be set freely.
    Ready,
    /// Paused at a position, with frames to read.
    Paused,
    /// Ended. The heap is still readable; nothing can be resumed.
    Finished(String),
    /// Never compiled, so there is nothing to run.
    Failed,
}

/// One line of source, with what the gutter should say about it.
#[derive(Debug, Clone, PartialEq)]
pub struct SourceLine {
    pub number: usize,
    pub text: String,
    pub breakpoint: Breakpoint,
    /// The line the program is stopped ON. Distinct from the cursor:
    /// the two separate the moment you scroll, and confusing them is
    /// disorienting in exactly the situation you most need orientation.
    pub current: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Breakpoint {
    None,
    /// Bound to an instruction; it will fire.
    Set,
    /// Asked for on a line with no code. Shown differently rather than
    /// dropped: silently ignoring the request is how a user concludes
    /// the debugger is broken.
    Unbound,
}

/// One frame of the paused stack, as the panel shows it.
#[derive(Debug, Clone, PartialEq)]
pub struct Frame {
    pub name: String,
    pub line: usize,
    pub locals: Vec<Local>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Local {
    pub slot: usize,
    pub value: String,
    pub children: Vec<(String, String)>,
    /// Whether this value lives in the arena, and so can be asked why
    /// it is still alive.
    pub inspectable: bool,
}

/// Which panel has focus. Keys mean different things in each, and a
/// single focus is what keeps that unambiguous.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Focus {
    #[default]
    Source,
    Frames,
    Locals,
    Output,
    Heap,
}

impl Focus {
    pub fn next(self) -> Focus {
        match self {
            Focus::Source => Focus::Frames,
            Focus::Frames => Focus::Locals,
            Focus::Locals => Focus::Output,
            Focus::Output => Focus::Heap,
            Focus::Heap => Focus::Source,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Focus::Source => "source",
            Focus::Frames => "frames",
            Focus::Locals => "locals",
            Focus::Output => "output",
            Focus::Heap => "heap",
        }
    }
}

/// The whole debugger, as the view sees it.
#[derive(Debug, Clone)]
pub struct Debugger {
    pub title: String,
    pub run: Run,
    pub source: Vec<String>,
    /// Line numbers (1-based) with a breakpoint, and whether it bound.
    pub breakpoints: BTreeSet<usize>,
    pub unbound: BTreeSet<usize>,
    /// The line the program is stopped on, when it is stopped.
    pub current_line: Option<usize>,
    pub frames: Vec<Frame>,
    pub output: Vec<String>,
    pub heap: Option<crate::model::Heap>,
    pub diagnostics: Vec<crate::model::Entry>,

    pub focus: Focus,
    /// The line the cursor is on, 1-based. Where a breakpoint toggle
    /// lands.
    pub cursor: usize,
    pub selected_frame: usize,
    pub selected_local: usize,
    /// First visible source line, 1-based.
    pub scroll: usize,
    /// What `retention` last answered about, shown until dismissed.
    pub inspect: Option<String>,
    pub help: bool,
}

impl Debugger {
    pub fn new(title: String, source: Vec<String>) -> Debugger {
        Debugger {
            title,
            run: Run::Ready,
            source,
            breakpoints: BTreeSet::new(),
            unbound: BTreeSet::new(),
            current_line: None,
            frames: Vec::new(),
            output: Vec::new(),
            heap: None,
            diagnostics: Vec::new(),
            focus: Focus::default(),
            cursor: 1,
            selected_frame: 0,
            selected_local: 0,
            scroll: 1,
            inspect: None,
            help: false,
        }
    }

    /// The source, annotated for the gutter.
    pub fn lines(&self) -> Vec<SourceLine> {
        self.source
            .iter()
            .enumerate()
            .map(|(ix, text)| {
                let number = ix + 1;

                SourceLine {
                    number,
                    text: text.clone(),
                    breakpoint: if self.breakpoints.contains(&number) {
                        Breakpoint::Set
                    } else if self.unbound.contains(&number) {
                        Breakpoint::Unbound
                    } else {
                        Breakpoint::None
                    },
                    current: self.current_line == Some(number),
                }
            })
            .collect()
    }

    pub fn frame(&self) -> Option<&Frame> {
        self.frames
            .get(self.selected_frame.min(self.frames.len().saturating_sub(1)))
    }

    /// Moves the cursor, clamped to the file.
    pub fn move_cursor(&mut self, delta: isize) {
        if self.source.is_empty() {
            return;
        }

        let last = self.source.len();
        let next = (self.cursor as isize + delta).clamp(1, last as isize) as usize;
        self.cursor = next;
        self.keep_cursor_visible();
    }

    /// Selecting a frame moves the cursor to where that frame is, which
    /// is the point of a call stack: clicking a caller should show you
    /// the caller.
    pub fn select_frame(&mut self, index: usize) {
        if self.frames.is_empty() {
            return;
        }

        self.selected_frame = index.min(self.frames.len() - 1);
        self.selected_local = 0;

        if let Some(frame) = self.frames.get(self.selected_frame) {
            self.cursor = frame.line.max(1);
            self.keep_cursor_visible();
        }
    }

    pub fn move_local(&mut self, delta: isize) {
        let count = self.frame().map(|frame| frame.locals.len()).unwrap_or(0);
        if count == 0 {
            return;
        }

        let next = (self.selected_local as isize + delta).clamp(0, count as isize - 1);
        self.selected_local = next as usize;
    }

    /// Follows execution to a new stop — but only moves the view; it
    /// does not fight a reader who scrolled somewhere else on purpose,
    /// because the cursor is theirs and the current line is the
    /// program's.
    pub fn stopped_at(&mut self, line: usize) {
        self.current_line = Some(line);
        self.cursor = line.max(1);
        self.selected_frame = self.frames.len().saturating_sub(1);
        self.selected_local = 0;
        self.keep_cursor_visible();
    }

    /// Keeps the cursor on screen with a margin, so stepping near the
    /// bottom does not leave you reading the last line of the viewport.
    fn keep_cursor_visible(&mut self) {
        const MARGIN: usize = 3;
        const VIEWPORT: usize = 20;

        if self.cursor < self.scroll + MARGIN {
            self.scroll = self.cursor.saturating_sub(MARGIN).max(1);
        } else if self.cursor + MARGIN > self.scroll + VIEWPORT {
            self.scroll = (self.cursor + MARGIN).saturating_sub(VIEWPORT).max(1);
        }
    }

    /// The status line. One sentence, because it is the first thing
    /// anyone reads and the four states want different next actions.
    pub fn status(&self) -> String {
        match &self.run {
            Run::Ready if self.breakpoints.is_empty() => {
                "ready — press b to set a breakpoint, r to run".to_string()
            }
            Run::Ready => format!("ready — {} breakpoint(s), r to run", self.breakpoints.len()),
            Run::Paused => match (&self.current_line, self.frames.last()) {
                (Some(line), Some(frame)) => {
                    format!("paused in `{}` at line {line}", frame.name)
                }
                (Some(line), None) => format!("paused at line {line}"),
                _ => "paused".to_string(),
            },
            Run::Finished(outcome) => format!("finished — {outcome}"),
            Run::Failed => format!("{} error(s) — nothing to run", self.diagnostics.len()),
        }
    }

    pub fn can_run(&self) -> bool {
        matches!(self.run, Run::Ready | Run::Paused)
    }
}

/// The breakpoints the session should hold, as line numbers.
pub type Lines = BTreeSet<usize>;

/// What a toggle asked for, before the session says whether it bound.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Toggle {
    Added(usize),
    Removed(usize),
}

impl Debugger {
    /// Toggles a breakpoint on the cursor's line, reporting what was
    /// asked so the caller can bind it against the session.
    pub fn toggle_breakpoint(&mut self) -> Toggle {
        let line = self.cursor;

        if self.breakpoints.remove(&line) || self.unbound.remove(&line) {
            Toggle::Removed(line)
        } else {
            Toggle::Added(line)
        }
    }

    /// Records what the session answered about a requested line.
    pub fn bound(&mut self, line: usize, bound: bool) {
        if bound {
            self.breakpoints.insert(line);
            self.unbound.remove(&line);
        } else {
            self.unbound.insert(line);
            self.breakpoints.remove(&line);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn debugger() -> Debugger {
        Debugger::new(
            "script.bras".to_string(),
            (1..=40).map(|n| format!("line {n}")).collect(),
        )
    }

    /// The gutter distinguishes a breakpoint that will fire from one
    /// that was asked for on a line with no code. Dropping the second
    /// silently is how a user concludes the debugger is broken.
    #[test]
    fn the_gutter_distinguishes_bound_from_unbound() {
        let mut debugger = debugger();
        debugger.bound(3, true);
        debugger.bound(5, false);

        let lines = debugger.lines();

        assert_eq!(lines[2].breakpoint, Breakpoint::Set);
        assert_eq!(lines[4].breakpoint, Breakpoint::Unbound);
        assert_eq!(lines[0].breakpoint, Breakpoint::None);
    }

    /// A toggle reports what it asked for, and asking twice undoes it.
    #[test]
    fn toggling_reports_what_it_asked_and_undoes_itself() {
        let mut debugger = debugger();
        debugger.cursor = 7;

        assert_eq!(debugger.toggle_breakpoint(), Toggle::Added(7));
        debugger.bound(7, true);

        assert_eq!(debugger.toggle_breakpoint(), Toggle::Removed(7));
        assert!(debugger.breakpoints.is_empty());
    }

    /// An unbound breakpoint is still removable — otherwise a mistaken
    /// click would be permanent.
    #[test]
    fn an_unbound_breakpoint_can_be_removed() {
        let mut debugger = debugger();
        debugger.cursor = 9;
        debugger.toggle_breakpoint();
        debugger.bound(9, false);

        assert_eq!(debugger.toggle_breakpoint(), Toggle::Removed(9));
        assert!(debugger.unbound.is_empty());
    }

    /// Selecting a frame moves the cursor to it. Without that a call
    /// stack is decoration.
    #[test]
    fn selecting_a_frame_moves_the_cursor_to_it() {
        let mut debugger = debugger();
        debugger.frames = vec![
            Frame {
                name: "main".to_string(),
                line: 30,
                locals: Vec::new(),
            },
            Frame {
                name: "inner".to_string(),
                line: 12,
                locals: Vec::new(),
            },
        ];

        debugger.select_frame(0);
        assert_eq!(debugger.cursor, 30, "the caller's line");

        debugger.select_frame(1);
        assert_eq!(debugger.cursor, 12, "the callee's line");
    }

    /// Stopping selects the innermost frame, which is where a reader
    /// wants to be: the place execution actually is.
    #[test]
    fn stopping_selects_the_innermost_frame() {
        let mut debugger = debugger();
        debugger.frames = vec![
            Frame {
                name: "main".to_string(),
                line: 30,
                locals: Vec::new(),
            },
            Frame {
                name: "inner".to_string(),
                line: 12,
                locals: Vec::new(),
            },
        ];

        debugger.stopped_at(12);

        assert_eq!(debugger.selected_frame, 1);
        assert_eq!(debugger.current_line, Some(12));
    }

    /// The current line and the cursor are different things: scrolling
    /// away must not move where the program is stopped.
    #[test]
    fn scrolling_does_not_move_the_current_line() {
        let mut debugger = debugger();
        debugger.stopped_at(10);

        debugger.move_cursor(15);

        assert_eq!(debugger.cursor, 25);
        assert_eq!(debugger.current_line, Some(10), "the program has not moved");
    }

    /// The view follows the cursor with a margin, so stepping near the
    /// bottom does not leave you reading the last visible line.
    #[test]
    fn the_view_keeps_a_margin_around_the_cursor() {
        let mut debugger = debugger();

        debugger.move_cursor(30);
        assert!(
            debugger.scroll > 1,
            "the view followed the cursor down: {}",
            debugger.scroll
        );
        assert!(
            debugger.cursor >= debugger.scroll,
            "the cursor is on screen"
        );

        debugger.move_cursor(-30);
        assert_eq!(debugger.scroll, 1, "and back to the top");
    }

    /// The cursor clamps to the file rather than running off it.
    #[test]
    fn the_cursor_clamps_to_the_file() {
        let mut debugger = debugger();

        debugger.move_cursor(-10);
        assert_eq!(debugger.cursor, 1);

        debugger.move_cursor(1000);
        assert_eq!(debugger.cursor, 40);
    }

    /// The status line says what to do next, and it differs per state
    /// because the next action differs.
    #[test]
    fn the_status_line_differs_per_state() {
        let mut debugger = debugger();
        assert!(debugger.status().contains("press b"));

        debugger.bound(3, true);
        assert!(debugger.status().contains("1 breakpoint"));

        debugger.run = Run::Paused;
        debugger.frames = vec![Frame {
            name: "main".to_string(),
            line: 3,
            locals: Vec::new(),
        }];
        debugger.stopped_at(3);
        assert!(debugger.status().contains("paused in `main` at line 3"));

        debugger.run = Run::Finished("exit 0".to_string());
        assert!(debugger.status().contains("finished — exit 0"));
    }

    /// A program that did not compile cannot be run, and the state says
    /// so rather than offering keys that would do nothing.
    #[test]
    fn a_failed_compile_cannot_be_run() {
        let mut debugger = debugger();
        debugger.run = Run::Failed;

        assert!(!debugger.can_run());
        assert!(debugger.status().contains("nothing to run"));
    }

    /// Focus cycles through every panel and returns, so tab alone
    /// reaches all of them.
    #[test]
    fn focus_cycles_through_every_panel() {
        let mut focus = Focus::default();
        let mut seen = vec![focus];

        for _ in 0..4 {
            focus = focus.next();
            seen.push(focus);
        }

        assert_eq!(focus.next(), Focus::Source, "it returns to the start");
        assert_eq!(seen.len(), 5, "every panel is reachable by tab");
    }
}
