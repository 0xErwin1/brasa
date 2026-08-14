//! The debug substrate (BRS-117), asked the questions its consumers
//! will ask.
//!
//! Every front-end in the group — `brasa debug`, the DAP adapter, the
//! heap view — is a shell over this API, so what is pinned here is the
//! behaviour, not any rendering of it.

use std::path::PathBuf;

use brasa_bytecode::Module;
use brasa_source::{FileId, SourceMap};
use brasa_vm::debug::{Session, Stop};

/// Compiles `source` the way the CLI does, ungated, and hands back the
/// module plus the `FileId` a breakpoint resolves against.
fn compile(source: &str) -> (Module, FileId, SourceMap) {
    let mut sources = SourceMap::new();
    let file = sources.add_file(PathBuf::from("debug.bras"), source.to_string());

    let parsed = brasa_parser::parse(source, file);
    assert!(
        parsed.diagnostics.is_empty(),
        "the fixture must parse: {:?}",
        parsed.diagnostics
    );

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
    assert!(
        compiled.diagnostics.is_empty(),
        "the fixture must compile: {:?}",
        compiled.diagnostics
    );

    (compiled.module, file, sources)
}

fn streams<'a>(
    out: &'a mut Vec<u8>,
    err: &'a mut Vec<u8>,
    input: &'a mut &'static [u8],
) -> brasa_runtime::Streams<'a> {
    brasa_runtime::Streams { out, err, input }
}

/// The offset of `needle` in `source`.
fn at(source: &str, needle: &str) -> u32 {
    source.find(needle).expect("the needle is in the source") as u32
}

const COUNTER: &str = r#"def bump(n: int): int
  let doubled = n * 2
  doubled + 1
end

def main()
  let a = bump(20)
  let b = bump(a)
  puts b
end
"#;

/// A breakpoint resolves to an instruction, stops the run before it,
/// and leaves the frame stack intact — the property everything else
/// depends on.
#[test]
fn a_breakpoint_pauses_with_the_frames_intact() {
    let (module, file, _sources) = compile(COUNTER);
    let (mut out, mut err, mut input) = (Vec::new(), Vec::new(), &b""[..]);
    let mut session = Session::new(&module, streams(&mut out, &mut err, &mut input), &[]);

    let (func, ip) = session
        .resolve(file, at(COUNTER, "n * 2"))
        .expect("the multiplication resolves to an instruction");
    assert!(session.set_breakpoint(func, ip));

    let stop = session.resume();
    let Stop::Paused { func: at_func, .. } = stop else {
        panic!("expected a pause, got {stop:?}");
    };
    assert_eq!(at_func, func);

    let frames = session.frames();
    assert_eq!(
        frames.len(),
        2,
        "paused inside `bump`, called from `main`: {frames:?}"
    );
    assert_eq!(frames.last().expect("innermost frame").name, "bump");
}

/// The parameter is readable at the pause, with the value the caller
/// passed. A frame you cannot read is not a frame you can debug.
#[test]
fn a_paused_frame_reads_its_locals() {
    let (module, file, _sources) = compile(COUNTER);
    let (mut out, mut err, mut input) = (Vec::new(), Vec::new(), &b""[..]);
    let mut session = Session::new(&module, streams(&mut out, &mut err, &mut input), &[]);

    let (func, ip) = session
        .resolve(file, at(COUNTER, "n * 2"))
        .expect("the multiplication resolves");
    session.set_breakpoint(func, ip);
    session.resume();

    let frames = session.frames();
    let bump = frames.last().expect("innermost frame");

    let slot0 = bump.locals[0].as_ref().expect("`n` is bound on entry");
    assert_eq!(slot0.summary, "20", "`main` called `bump(20)`");
}

/// Resuming continues from the pause rather than restarting, and the
/// same breakpoint fires again on the second call.
#[test]
fn resuming_continues_and_the_breakpoint_fires_again() {
    let (module, file, _sources) = compile(COUNTER);
    let (mut out, mut err, mut input) = (Vec::new(), Vec::new(), &b""[..]);
    let mut session = Session::new(&module, streams(&mut out, &mut err, &mut input), &[]);

    let (func, ip) = session
        .resolve(file, at(COUNTER, "n * 2"))
        .expect("the multiplication resolves");
    session.set_breakpoint(func, ip);

    assert!(matches!(session.resume(), Stop::Paused { .. }));

    // Second call: `bump(a)` with a = 41.
    let stop = session.resume();
    assert!(matches!(stop, Stop::Paused { .. }), "got {stop:?}");

    let frames = session.frames();
    let n = frames.last().expect("innermost frame").locals[0]
        .as_ref()
        .expect("`n` is bound");
    assert_eq!(n.summary, "41", "the first call returned 20 * 2 + 1");

    // Third resume: no more calls, the program finishes.
    assert!(matches!(session.resume(), Stop::Finished(_)));
}

/// Clearing a breakpoint takes effect: the run reaches the end instead
/// of stopping.
#[test]
fn a_cleared_breakpoint_does_not_fire() {
    let (module, file, _sources) = compile(COUNTER);
    let (mut out, mut err, mut input) = (Vec::new(), Vec::new(), &b""[..]);
    let mut session = Session::new(&module, streams(&mut out, &mut err, &mut input), &[]);

    let (func, ip) = session
        .resolve(file, at(COUNTER, "n * 2"))
        .expect("the multiplication resolves");
    session.set_breakpoint(func, ip);
    assert!(session.clear_breakpoint(func, ip));
    assert!(session.breakpoints().is_empty());

    assert!(matches!(session.resume(), Stop::Finished(_)));
}

/// A run with no breakpoints and no stepping is an ordinary run: same
/// output, same completion.
#[test]
fn a_session_without_breakpoints_runs_the_program_normally() {
    let (module, _file, _sources) = compile(COUNTER);
    let (mut out, mut err, mut input) = (Vec::new(), Vec::new(), &b""[..]);

    {
        let mut session = Session::new(&module, streams(&mut out, &mut err, &mut input), &[]);
        assert!(matches!(session.resume(), Stop::Finished(_)));
    }

    // bump(20) = 41, bump(41) = 83.
    assert_eq!(String::from_utf8(out).expect("utf-8"), "83\n");
}

/// `step_in` advances one instruction and follows a call; `step_out`
/// runs until the callee has returned. The two are the pair every
/// front-end needs and the pair most easily got backwards.
#[test]
fn stepping_in_follows_a_call_and_stepping_out_leaves_it() {
    let (module, file, _sources) = compile(COUNTER);
    let (mut out, mut err, mut input) = (Vec::new(), Vec::new(), &b""[..]);
    let mut session = Session::new(&module, streams(&mut out, &mut err, &mut input), &[]);

    let (func, ip) = session
        .resolve(file, at(COUNTER, "n * 2"))
        .expect("the multiplication resolves");
    session.set_breakpoint(func, ip);
    session.resume();
    assert_eq!(session.frames().len(), 2);

    // One instruction, still inside `bump`.
    assert!(matches!(session.step_in(), Stop::Paused { .. }));
    assert_eq!(session.frames().len(), 2);

    // Out of `bump`, back in `main`.
    session.clear_breakpoint(func, ip);
    assert!(matches!(session.step_out(), Stop::Paused { .. }));

    let frames = session.frames();
    assert_eq!(frames.len(), 1, "back in `main`: {frames:?}");
    assert_eq!(frames.last().expect("frame").name, "main");
}

const SHAPES: &str = r#"struct Point
  x: int
  y: int
end

def main()
  let p = Point { x: 3, y: 4 }
  let xs = [10, 20, 30]
  puts p.x + xs[0]
end
"#;

/// A value reads one level deep: the fields of a struct and the
/// elements of a vector, each summarised rather than expanded.
#[test]
fn a_value_reads_one_level_deep() {
    let (module, file, _sources) = compile(SHAPES);
    let (mut out, mut err, mut input) = (Vec::new(), Vec::new(), &b""[..]);
    let mut session = Session::new(&module, streams(&mut out, &mut err, &mut input), &[]);

    let (func, ip) = session
        .resolve(file, at(SHAPES, "p.x + xs[0]"))
        .expect("the final expression resolves");
    session.set_breakpoint(func, ip);
    session.resume();

    let frames = session.frames();
    let main = frames.last().expect("frame");

    let point = main
        .locals
        .iter()
        .flatten()
        .find(|view| view.summary == "Point")
        .expect("`p` is a Point");
    assert_eq!(
        point.children,
        vec![
            ("x".to_string(), "3".to_string()),
            ("y".to_string(), "4".to_string()),
        ]
    );

    let vector = main
        .locals
        .iter()
        .flatten()
        .find(|view| view.summary.starts_with("Vector"))
        .expect("`xs` is a Vector");
    assert_eq!(vector.summary, "Vector of 3");
    assert_eq!(vector.children.len(), 3);
    assert_eq!(vector.children[0], ("0".to_string(), "10".to_string()));
}

const GRAPH: &str = r#"struct Node
  label: string
  kids: Vector<int>
end

def main()
  let a = Node { label: "root", kids: [1, 2, 3] }
  let b = Node { label: "leaf", kids: [4] }
  let all = [a, b]
  puts all.len()
end
"#;

/// The heap census counts arena slots by kind — the one view an
/// editor's debug panels have no vocabulary for (BRS-120).
#[test]
fn the_heap_census_counts_live_slots_by_kind() {
    let (module, file, _sources) = compile(GRAPH);
    let (mut out, mut err, mut input) = (Vec::new(), Vec::new(), &b""[..]);
    let mut session = Session::new(&module, streams(&mut out, &mut err, &mut input), &[]);

    let (func, ip) = session
        .resolve(file, at(GRAPH, "all.len()"))
        .expect("the final expression resolves");
    session.set_breakpoint(func, ip);
    session.resume();

    let heap = session.heap();
    let by_kind: std::collections::HashMap<_, _> = heap.by_kind.iter().cloned().collect();

    // Two structs, and three vectors: each node's `kids` plus `all`.
    assert_eq!(by_kind.get("struct"), Some(&2));
    assert_eq!(by_kind.get("Vector"), Some(&3));

    assert_eq!(heap.live_slots, 5);
    assert!(heap.live_bytes > 0);
    assert!(heap.allocations >= heap.live_slots as u64);
}

/// Free slots are reported apart from live ones: an arena that is
/// mostly holes and one that is mostly live say different things about
/// whether collection is keeping up.
#[test]
fn free_slots_are_reported_apart_from_live_ones() {
    let (module, file, _sources) = compile(GRAPH);
    let (mut out, mut err, mut input) = (Vec::new(), Vec::new(), &b""[..]);
    let mut session = Session::new(&module, streams(&mut out, &mut err, &mut input), &[]);

    let (func, ip) = session
        .resolve(file, at(GRAPH, "all.len()"))
        .expect("the final expression resolves");
    session.set_breakpoint(func, ip);
    session.resume();

    let heap = session.heap();

    // Nothing has been collected yet, so every slot is live.
    assert_eq!(heap.collections, 0);
    assert_eq!(heap.free_slots, 0);
    assert!(heap.report().contains("live slots"));
}

/// Retention answers "why is this still here" with the shortest chain
/// from a root, which is the most direct reason rather than merely a
/// true one.
#[test]
fn retention_finds_the_shortest_path_from_a_root() {
    let (module, file, _sources) = compile(GRAPH);
    let (mut out, mut err, mut input) = (Vec::new(), Vec::new(), &b""[..]);
    let mut session = Session::new(&module, streams(&mut out, &mut err, &mut input), &[]);

    let (func, ip) = session
        .resolve(file, at(GRAPH, "all.len()"))
        .expect("the final expression resolves");
    session.set_breakpoint(func, ip);
    session.resume();

    let frames = session.frames();
    let main = frames.last().expect("frame");

    // `all` is the vector holding both nodes.
    let all = main
        .locals
        .iter()
        .flatten()
        .find(|view| view.summary == "Vector of 2")
        .expect("`all` holds the two nodes");

    let cell = all.cell.expect("a Vector lives in an arena cell");
    let path = session
        .retention(cell)
        .expect("a bound local is reachable from a root");

    assert_eq!(
        path.last().copied(),
        Some(cell),
        "the path ends at what was asked about"
    );
    assert_eq!(
        path.len(),
        1,
        "`all` is itself on the stack, so the shortest reason is direct"
    );

    // A node inside it is one hop further: reachable through `all`, and
    // also directly, so the shortest path is still the direct one.
    let node = main
        .locals
        .iter()
        .flatten()
        .find(|view| view.summary == "Node")
        .expect("`a` is a Node");
    let node_cell = node.cell.expect("a struct lives in an arena cell");

    assert!(
        session.retention(node_cell).is_some(),
        "a bound struct is reachable"
    );
}
