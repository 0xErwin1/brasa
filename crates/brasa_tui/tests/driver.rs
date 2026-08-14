//! The debugger TUI driven against a real VM session.
//!
//! The view tests check what a screen shows given a model; these check
//! that the model is what a session actually said. Between them the
//! only untested part left is the terminal itself.

use std::path::PathBuf;

use brasa_bytecode::Module;
use brasa_source::{FileId, SourceMap};
use brasa_tui::capture::Capture;
use brasa_tui::debugger::{Breakpoint, Debugger, Run, Toggle};
use brasa_tui::driver;
use brasa_vm::debug::Session;

const SCRIPT: &str = r#"def bump(n: int): int
  let doubled = n * 2
  doubled + 1
end

def main()
  let a = bump(20)
  puts a
end
"#;

fn compile(source: &str) -> (Module, SourceMap, FileId) {
    let mut sources = SourceMap::new();
    let file = sources.add_file(PathBuf::from("tui.bras"), source.to_string());

    let parsed = brasa_parser::parse(source, file);
    assert!(parsed.diagnostics.is_empty(), "the fixture must parse");

    let lowered = brasa_hir::lower(&parsed.ast, &parsed.roots);
    let resolved = brasa_resolver::resolve(&lowered.hir, &lowered.roots);
    let checked = brasa_typeck::check(
        &lowered.hir,
        &lowered.roots,
        &resolved.resolutions,
        &lowered.sugar_origins,
    );
    let inferred = brasa_errorset::infer(
        &lowered.hir,
        &lowered.roots,
        &resolved.resolutions,
        &checked.types,
    );
    assert!(inferred.diagnostics.is_empty(), "the fixture must check");

    let compiled = brasa_codegen::compile_program(
        &lowered.hir,
        &lowered.roots,
        &lowered.roots,
        &resolved.resolutions,
        &checked.types,
    );
    assert!(compiled.diagnostics.is_empty(), "the fixture must compile");

    (compiled.module, sources, file)
}

fn debugger() -> Debugger {
    Debugger::new(
        "tui.bras".to_string(),
        SCRIPT.lines().map(str::to_string).collect(),
    )
}

fn line_range(sources: &SourceMap, file: FileId, line: usize) -> (u32, u32) {
    let source = sources.get(&file);
    let start = source.line_starts[line - 1].0;
    let end = source
        .line_starts
        .get(line)
        .map(|next| next.0)
        .unwrap_or_else(|| source.len_bytes());

    (start, end)
}

/// Setting a breakpoint on a real line binds it, and the gutter says
/// so. This is the whole interaction the TUI exists for.
#[test]
fn a_breakpoint_set_on_the_cursor_binds_and_shows_in_the_gutter() {
    let (module, sources, file) = compile(SCRIPT);
    let capture = Capture::new();
    let (mut out, mut err, mut input) = (capture.clone(), std::io::sink(), std::io::empty());

    let mut session = Session::new(
        &module,
        brasa_runtime::Streams {
            out: &mut out,
            err: &mut err,
            input: &mut input,
        },
        &[],
    );

    let mut debugger = debugger();
    debugger.cursor = 2;

    assert_eq!(debugger.toggle_breakpoint(), Toggle::Added(2));

    let (start, end) = line_range(&sources, file, 2);
    let bound = session.resolve_range(file, start, end);
    assert!(bound.is_some(), "line 2 has code");

    let (func, ip) = bound.expect("bound");
    session.set_breakpoint(func, ip);
    debugger.bound(2, true);

    assert_eq!(debugger.lines()[1].breakpoint, Breakpoint::Set);
}

/// A line with no code comes back unbound rather than silently doing
/// nothing, which is how a user concludes the debugger is broken.
#[test]
fn a_line_without_code_is_marked_unbound() {
    let (module, sources, file) = compile(SCRIPT);
    let capture = Capture::new();
    let (mut out, mut err, mut input) = (capture.clone(), std::io::sink(), std::io::empty());

    let session = Session::new(
        &module,
        brasa_runtime::Streams {
            out: &mut out,
            err: &mut err,
            input: &mut input,
        },
        &[],
    );

    let mut debugger = debugger();
    debugger.cursor = 5; // the blank line

    debugger.toggle_breakpoint();
    let (start, end) = line_range(&sources, file, 5);
    debugger.bound(5, session.resolve_range(file, start, end).is_some());

    assert_eq!(debugger.lines()[4].breakpoint, Breakpoint::Unbound);
}

/// Running to a breakpoint fills the model with what the session says:
/// the state, the stopped line, the frames and their locals.
#[test]
fn running_to_a_breakpoint_fills_the_model_from_the_session() {
    let (module, sources, file) = compile(SCRIPT);
    let capture = Capture::new();
    let (mut out, mut err, mut input) = (capture.clone(), std::io::sink(), std::io::empty());

    let mut session = Session::new(
        &module,
        brasa_runtime::Streams {
            out: &mut out,
            err: &mut err,
            input: &mut input,
        },
        &[],
    );

    let (start, end) = line_range(&sources, file, 2);
    let (func, ip) = session.resolve_range(file, start, end).expect("line 2");
    session.set_breakpoint(func, ip);

    let mut debugger = debugger();
    let stop = session.resume();
    driver::apply(&mut debugger, &session, &sources, &stop);

    assert_eq!(debugger.run, Run::Paused);
    assert_eq!(debugger.current_line, Some(2));

    assert_eq!(debugger.frames.len(), 2, "paused in `bump`, from `main`");
    assert_eq!(debugger.frames[1].name, "bump");
    assert_eq!(debugger.frames[0].name, "main");

    let bump = &debugger.frames[1];
    assert_eq!(bump.locals[0].value, "20", "`main` called `bump(20)`");

    assert_eq!(
        debugger.selected_frame, 1,
        "the innermost frame is selected: it is where execution is"
    );
}

/// The heap is filled at every stop, so the panel is never stale.
#[test]
fn every_stop_refreshes_the_heap() {
    let (module, sources, file) = compile(SCRIPT);
    let capture = Capture::new();
    let (mut out, mut err, mut input) = (capture.clone(), std::io::sink(), std::io::empty());

    let mut session = Session::new(
        &module,
        brasa_runtime::Streams {
            out: &mut out,
            err: &mut err,
            input: &mut input,
        },
        &[],
    );

    let (start, end) = line_range(&sources, file, 2);
    let (func, ip) = session.resolve_range(file, start, end).expect("line 2");
    session.set_breakpoint(func, ip);

    let mut debugger = debugger();
    let stop = session.resume();
    driver::apply(&mut debugger, &session, &sources, &stop);

    assert!(
        debugger.heap.is_some(),
        "the heap panel has something to show"
    );
}

/// A finished run says so, clears the frames, and stops offering the
/// keys that would do nothing.
#[test]
fn a_finished_run_clears_the_paused_state() {
    let (module, sources, _file) = compile(SCRIPT);
    let capture = Capture::new();
    let (mut out, mut err, mut input) = (capture.clone(), std::io::sink(), std::io::empty());

    let mut session = Session::new(
        &module,
        brasa_runtime::Streams {
            out: &mut out,
            err: &mut err,
            input: &mut input,
        },
        &[],
    );

    let mut debugger = debugger();
    let stop = session.resume();
    driver::apply(&mut debugger, &session, &sources, &stop);

    assert!(matches!(debugger.run, Run::Finished(_)));
    assert!(debugger.frames.is_empty());
    assert_eq!(debugger.current_line, None);
    assert!(!debugger.can_run(), "nothing left to resume");
}

/// The program's own output reaches the model. Without this `puts`
/// debugging is invisible in the one tool built for debugging.
#[test]
fn the_programs_output_is_captured_while_it_runs() {
    let (module, _sources, _file) = compile(SCRIPT);
    let capture = Capture::new();
    let (mut out, mut err, mut input) = (capture.clone(), std::io::sink(), std::io::empty());

    {
        let mut session = Session::new(
            &module,
            brasa_runtime::Streams {
                out: &mut out,
                err: &mut err,
                input: &mut input,
            },
            &[],
        );
        session.resume();
    }

    // bump(20) = 41.
    assert_eq!(capture.lines(), vec!["41"]);
}

/// Stepping moves the stop, and the model follows it.
#[test]
fn stepping_moves_where_the_model_says_it_is() {
    let (module, sources, file) = compile(SCRIPT);
    let capture = Capture::new();
    let (mut out, mut err, mut input) = (capture.clone(), std::io::sink(), std::io::empty());

    let mut session = Session::new(
        &module,
        brasa_runtime::Streams {
            out: &mut out,
            err: &mut err,
            input: &mut input,
        },
        &[],
    );

    let (start, end) = line_range(&sources, file, 2);
    let (func, ip) = session.resolve_range(file, start, end).expect("line 2");
    session.set_breakpoint(func, ip);

    let mut debugger = debugger();
    let stop = session.resume();
    driver::apply(&mut debugger, &session, &sources, &stop);
    let first = debugger.current_line;

    session.clear_breakpoint(func, ip);
    let stop = session.step_out();
    driver::apply(&mut debugger, &session, &sources, &stop);

    assert_ne!(debugger.current_line, first, "the stop moved");
    assert_eq!(
        debugger.frames.len(),
        1,
        "stepping out left `bump`: {:?}",
        debugger.frames
    );
}
