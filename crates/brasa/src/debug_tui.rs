//! `brasa debug tui` — the interactive debugger.
//!
//! Everything a debugging session needs is here, so nothing sends you
//! back to the command line: breakpoints are set on the source, the run
//! is driven with keys, and the frames, locals, output and heap are on
//! screen at once.
//!
//! The split of work: [`brasa_tui`] owns what is drawn and every
//! decision behind it, `brasa_vm::debug` owns execution, and this file
//! is the wire between them — it translates a keystroke into a session
//! call and a session answer into the model.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use brasa_source::{FileId, SourceMap};
use brasa_tui::capture::Capture;
use brasa_tui::debugger::{Debugger, Focus, Run, Toggle};
use brasa_tui::driver;
use brasa_tui::input::{Event, KeyCode, KeyEventKind, poll, read};
use brasa_vm::debug::Session;

pub fn run(script: &Path) -> ExitCode {
    let mut sources = SourceMap::new();
    let program = brasa_module::load(script, &mut sources);
    let (diagnostics, module) = crate::analyze_for_tui(&program, &sources);

    let file = sources.lookup_by_path(script).or_else(|| {
        std::fs::canonicalize(script)
            .ok()
            .and_then(|path| sources.lookup_by_path(path))
    });

    let source: Vec<String> = file
        .map(|file| {
            sources
                .get(&file)
                .text
                .lines()
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();

    let mut debugger = Debugger::new(script.display().to_string(), source);
    debugger.diagnostics = diagnostics
        .iter()
        .map(|diagnostic| brasa_tui::model::Entry::from_diagnostic(&sources, diagnostic))
        .collect();

    let Some(module) = module else {
        debugger.run = Run::Failed;
        return show(debugger, None, &sources, None);
    };

    let Some(file) = file else {
        eprintln!("brasa: the entry file is not in the module graph");
        return ExitCode::from(70);
    };

    show(debugger, Some(&module), &sources, Some(file))
}

/// The event loop. Owns the session so it can be rebuilt on restart.
fn show(
    mut debugger: Debugger,
    module: Option<&brasa_bytecode::Module>,
    sources: &SourceMap,
    file: Option<FileId>,
) -> ExitCode {
    let capture = Capture::new();
    let mut out = capture.clone();
    let mut err = std::io::sink();
    let mut input = std::io::empty();

    let mut session = module.map(|module| {
        Session::new(
            module,
            brasa_runtime::Streams {
                out: &mut out,
                err: &mut err,
                input: &mut input,
            },
            &[],
        )
    });

    let mut terminal = match brasa_tui::enter() {
        Ok(terminal) => terminal,
        Err(err) => {
            // No terminal: the same fallback the report view takes, for
            // the same reason. A pipe is a correct thing to be in.
            let _ = err;
            println!("{}", debugger.status());
            for entry in &debugger.diagnostics {
                println!("{} {}", entry.summary(), entry.at);
            }
            return ExitCode::from(0);
        }
    };

    let result = loop {
        if terminal
            .draw(|frame| brasa_tui::debug_view::draw(frame, &debugger))
            .is_err()
        {
            break ExitCode::from(70);
        }

        // Poll rather than block, so a program that prints while
        // running still shows its output as it arrives.
        match poll(Duration::from_millis(100)) {
            Ok(true) => {}
            Ok(false) => {
                refresh_output(&mut debugger, &capture);
                continue;
            }
            Err(_) => break ExitCode::from(70),
        }

        let Ok(Event::Key(key)) = read() else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }

        if debugger.help {
            debugger.help = false;
            continue;
        }
        if debugger.inspect.is_some() {
            debugger.inspect = None;
            continue;
        }

        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => break ExitCode::from(0),
            KeyCode::Char('?') => debugger.help = true,
            KeyCode::Tab => debugger.focus = debugger.focus.next(),

            KeyCode::Char('j') | KeyCode::Down => move_down(&mut debugger),
            KeyCode::Char('k') | KeyCode::Up => move_up(&mut debugger),

            KeyCode::Char('b') => {
                if let (Some(session), Some(file)) = (session.as_mut(), file) {
                    toggle_breakpoint(&mut debugger, session, sources, file);
                }
            }
            KeyCode::Char('r') | KeyCode::Char('s') | KeyCode::Char('n') | KeyCode::Char('o') => {
                if let Some(session) = session.as_mut()
                    && debugger.can_run()
                {
                    let stop = match key.code {
                        KeyCode::Char('s') => session.step_in(),
                        KeyCode::Char('n') => session.step_over(),
                        KeyCode::Char('o') => session.step_out(),
                        _ => session.resume(),
                    };

                    driver::apply(&mut debugger, session, sources, &stop);
                    refresh_output(&mut debugger, &capture);
                }
            }
            KeyCode::Char('w') => {
                if let Some(session) = session.as_ref() {
                    debugger.inspect = Some(retention(&mut debugger.clone(), session));
                }
            }
            _ => {}
        }
    };

    let _ = brasa_tui::leave(&mut terminal);
    result
}

fn move_down(debugger: &mut Debugger) {
    match debugger.focus {
        Focus::Frames => {
            let next = debugger.selected_frame.saturating_sub(1);
            debugger.select_frame(next);
        }
        Focus::Locals => debugger.move_local(1),
        _ => debugger.move_cursor(1),
    }
}

fn move_up(debugger: &mut Debugger) {
    match debugger.focus {
        Focus::Frames => {
            let next = debugger.selected_frame + 1;
            debugger.select_frame(next);
        }
        Focus::Locals => debugger.move_local(-1),
        _ => debugger.move_cursor(-1),
    }
}

/// Toggles a breakpoint on the cursor's line and records whether the
/// session could bind it.
fn toggle_breakpoint(
    debugger: &mut Debugger,
    session: &mut Session<'_>,
    sources: &SourceMap,
    file: FileId,
) {
    match debugger.toggle_breakpoint() {
        Toggle::Added(line) => {
            let bound = match line_range(sources, file, line) {
                Some((start, end)) => session.resolve_range(file, start, end),
                None => None,
            };

            match bound {
                Some((func, ip)) => {
                    session.set_breakpoint(func, ip);
                    debugger.bound(line, true);
                }
                None => debugger.bound(line, false),
            }
        }
        Toggle::Removed(line) => {
            if let Some((start, end)) = line_range(sources, file, line)
                && let Some((func, ip)) = session.resolve_range(file, start, end)
            {
                session.clear_breakpoint(func, ip);
            }
        }
    }
}

/// Answers "why is this still alive" for the selected local.
fn retention(debugger: &mut Debugger, session: &Session<'_>) -> String {
    let Some(frame) = debugger.frame() else {
        return "nothing is selected".to_string();
    };
    let Some(local) = frame.locals.get(debugger.selected_local) else {
        return "nothing is selected".to_string();
    };
    if !local.inspectable {
        return format!(
            "{} does not live in the arena, so nothing is holding it",
            local.value
        );
    }

    let cell = session
        .frames()
        .get(debugger.selected_frame)
        .and_then(|frame| frame.locals.get(local.slot).cloned())
        .flatten()
        .and_then(|view| view.cell);

    match cell.and_then(|cell| session.retention(cell)) {
        Some(path) => format!(
            "{} is kept alive by a chain of {} arena cell(s) from a root",
            local.value,
            path.len()
        ),
        None => format!("{} is not reachable from any root", local.value),
    }
}

fn refresh_output(debugger: &mut Debugger, capture: &Capture) {
    let lines = capture.lines();
    if lines != debugger.output {
        debugger.output = lines;
    }
}

fn line_range(sources: &SourceMap, file: FileId, line: usize) -> Option<(u32, u32)> {
    if line == 0 {
        return None;
    }

    let source = sources.get(&file);
    let start = source.line_starts.get(line - 1)?.0;
    let end = source
        .line_starts
        .get(line)
        .map(|next| next.0)
        .unwrap_or_else(|| source.len_bytes());

    Some((start, end))
}

/// Silences an unused-import warning in the no-terminal path.
#[allow(dead_code)]
fn _unused(_: PathBuf, mut w: impl Write) {
    let _ = w.flush();
}
