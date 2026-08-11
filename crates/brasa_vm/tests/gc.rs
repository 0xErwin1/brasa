//! End-to-end GC tests (BRS-29): programs run on the VM with an
//! artificially low allocation threshold, forcing many mark-and-sweep
//! collections mid-program. Output is pinned — a collector that frees
//! live data or misses roots corrupts it — and the run statistics
//! prove collections actually happened and garbage was reclaimed.

use brasa_interp::Outcome;
use brasa_vm::{RunStats, run_with_gc_threshold};

/// Threshold low enough that every test triggers collections while the
/// program is still running.
const TINY_GC_THRESHOLD: usize = 8;

/// Compiles `source` through the whole frontend (it must be clean) and
/// runs it on the VM with a tiny GC threshold, asserting the expected
/// stdout; returns the run statistics.
fn run_hot_gc(source: &str, expected_stdout: &str) -> RunStats {
    let mut sources = brasa_source::SourceMap::new();
    let file = sources.add_file("gc.brs", source.to_string());

    let parsed = brasa_parser::parse(source, file);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);

    let lowered = brasa_hir::lower(&parsed.ast, &parsed.roots);
    assert!(lowered.diagnostics.is_empty(), "{:?}", lowered.diagnostics);

    let resolved = brasa_resolver::resolve(&lowered.hir, &lowered.roots);
    assert!(
        resolved.diagnostics.is_empty(),
        "{:?}",
        resolved.diagnostics
    );

    let checked = brasa_typeck::check(
        &lowered.hir,
        &lowered.roots,
        &resolved.resolutions,
        &lowered.sugar_origins,
    );
    assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);

    let compiled = brasa_codegen::compile(
        &lowered.hir,
        &lowered.roots,
        &resolved.resolutions,
        &checked.types,
    );
    assert!(
        compiled.diagnostics.is_empty(),
        "{:?}",
        compiled.diagnostics
    );
    let module = compiled.module;

    let mut out = Vec::new();
    let (outcome, stats) = run_with_gc_threshold(&module, &mut out, TINY_GC_THRESHOLD);
    let stdout = String::from_utf8(out).expect("VM output is UTF-8");

    assert_eq!(outcome, Outcome::Success, "stdout: {stdout:?}");
    assert_eq!(stdout, expected_stdout);
    stats
}

#[test]
fn garbage_stress_loop_collects_and_output_survives() {
    let stats = run_hot_gc(
        r##"
let mut count = 0
for i in 0..1000
  let garbage = [i, i + 1, i + 2]
  count = count + garbage.len()
end
puts count
"##,
        "3000\n",
    );

    assert!(stats.gc_collections > 0, "expected collections: {stats:?}");
    assert!(
        stats.live_heap_objects < stats.heap_allocations as usize,
        "garbage was never reclaimed: {stats:?}"
    );
}

#[test]
fn live_data_survives_collections_mid_program() {
    let stats = run_hot_gc(
        r##"
let keeper = [1, 2, 3]
let table = { "a": 1 }

for i in 0..500
  let garbage = [[i], [i]]
end

keeper.push(4)
table.insert("b", 2)
puts keeper
puts table["b"] ?? -1
"##,
        "[1, 2, 3, 4]\n2\n",
    );

    assert!(stats.gc_collections > 0, "expected collections: {stats:?}");
}

#[test]
fn cyclic_garbage_is_reclaimed() {
    // Every iteration closes a struct-vector cycle and drops it: plain
    // reference counting would retain all 800 pairs; the sweeper must
    // keep the live count bounded near the threshold instead.
    let stats = run_hot_gc(
        r##"
struct Node
  items: Vector<Node>
end

for i in 0..400
  let node = Node { items: [] }
  node.items.push(node)
end
puts "done"
"##,
        "done\n",
    );

    assert!(stats.gc_collections > 0, "expected collections: {stats:?}");
    assert!(
        stats.live_heap_objects <= 4 * TINY_GC_THRESHOLD,
        "cyclic garbage leaked: {stats:?}"
    );
}

#[test]
fn iterator_snapshot_is_rooted_across_collections() {
    let stats = run_hot_gc(
        r##"
let source = [[10], [20], [30]]
let mut total = 0
for item in source
  for i in 0..200
    let garbage = [i]
  end
  total = total + item[0]
end
puts total
"##,
        "60\n",
    );

    assert!(stats.gc_collections > 0, "expected collections: {stats:?}");
}

#[test]
fn closures_and_captures_survive_collections() {
    let stats = run_hot_gc(
        r##"
def make_reader(): () -> int
  let cell = [41]
  || cell.len() + (cell.first() ?? 0)
end

let reader = make_reader()
let mut last = 0
for i in 0..300
  let garbage = [i, i]
  last = reader()
end
puts last
"##,
        "42\n",
    );

    assert!(stats.gc_collections > 0, "expected collections: {stats:?}");
}

#[test]
fn string_constants_are_interned_once() {
    let stats = run_hot_gc(
        r##"
let a = "shared"
let b = "shared"
let c = "shared"
puts a == b
puts c
"##,
        "true\nshared\n",
    );

    // The pool deduplicates identical literals, so one intern entry
    // serves every `const` push; hits count the pool-level reuse.
    assert!(stats.interned_strings > 0, "nothing interned: {stats:?}");
}
