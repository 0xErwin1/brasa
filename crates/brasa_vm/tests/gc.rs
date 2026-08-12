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

#[test]
fn garbage_inside_a_builtin_callback_is_collected() {
    // Everything here allocates inside `each`'s callback, which never
    // reaches a top-level instruction boundary. Before BRS-62 the
    // nested dispatch loop never collected, so the arena grew to hold
    // every iteration's garbage at once.
    let stats = run_hot_gc(
        r##"
let mut total = 0
[1, 2, 3, 4, 5].each do |seed|
  let mut i = 0
  while i < 200
    let garbage = [seed, i]
    total = total + garbage.len()
    i = i + 1
  end
end
puts total
"##,
        "2000\n",
    );

    assert!(stats.gc_collections > 0, "expected collections: {stats:?}");
    assert!(
        stats.peak_heap_objects <= 4 * TINY_GC_THRESHOLD,
        "callback garbage accumulated: {stats:?}"
    );
}

#[test]
fn a_callback_emptying_the_receiver_cannot_free_the_snapshot() {
    // `each` traverses a snapshot taken before the first call, so
    // draining the receiver mid-traversal must not shorten the walk —
    // and must not let the collector reclaim the elements still to
    // come, which the receiver no longer holds.
    let stats = run_hot_gc(
        r##"
let source = [[1], [1, 2], [1, 2, 3], [1, 2, 3, 4], [1, 2, 3, 4, 5]]
let mut seen = 0
source.each do |item|
  while source.len() > 0
    source.pop()
  end
  let mut i = 0
  while i < 40
    let garbage = [i]
    i = i + 1
  end
  seen = seen + item.len()
end
puts seen
"##,
        "15\n",
    );

    assert!(stats.gc_collections > 0, "expected collections: {stats:?}");
    // Bounding the peak as well as the count: a positive collection
    // count alone is satisfied by the top-level boundaries around this
    // program, so on its own it would not notice the nested safepoint
    // going away. Everything allocated here dies in its own iteration.
    assert!(
        stats.peak_heap_objects <= 8 * TINY_GC_THRESHOLD,
        "collection did not run inside the reentrant call: {stats:?}"
    );
}

#[test]
fn a_to_string_override_cannot_free_the_fields_being_rendered() {
    // Rendering copies the fields out, then reenters user code for the
    // nested override, which here replaces the field still pending
    // render. The copy is the only thing holding it from then on.
    let stats = run_hot_gc(
        r##"
struct Inner
  tag: Vector<int>

  def toString(self): string
    board.a = Inner { tag: [0] }
    board.b = Inner { tag: [0] }
    let mut i = 0
    while i < 40
      let garbage = [i]
      i = i + 1
    end
    "inner(#{self.tag.len()})"
  end
end

struct Outer
  a: Inner
  b: Inner
end

let board = Outer { a: Inner { tag: [1] }, b: Inner { tag: [1, 2] } }
puts(board.toString())
"##,
        "Outer { a: inner(1), b: inner(2) }\n",
    );

    assert!(stats.gc_collections > 0, "expected collections: {stats:?}");
    // Bounding the peak as well as the count: a positive collection
    // count alone is satisfied by the top-level boundaries around this
    // program, so on its own it would not notice the nested safepoint
    // going away. Everything allocated here dies in its own iteration.
    assert!(
        stats.peak_heap_objects <= 8 * TINY_GC_THRESHOLD,
        "collection did not run inside the reentrant call: {stats:?}"
    );
}

#[test]
fn a_callback_captured_only_by_the_builtin_stays_rooted() {
    // The callback is a temporary: `dispatch_builtin` popped it off the
    // value stack, so the only reference to it — and to the vector it
    // captured — is a Rust local for the whole traversal. This is the
    // read-only half of the property; the callback that overwrites its
    // capture is the half that needs `call_callable` to root it.
    let stats = run_hot_gc(
        r##"
def counter(): (int) -> int
  let hidden = [7, 7, 7]
  do |x|
    let mut i = 0
    while i < 40
      let garbage = [i]
      i = i + 1
    end
    x + hidden.len()
  end
end

puts([1, 2, 3].map(counter()))
"##,
        "[4, 5, 6]\n",
    );

    assert!(stats.gc_collections > 0, "expected collections: {stats:?}");
    // Bounding the peak as well as the count: a positive collection
    // count alone is satisfied by the top-level boundaries around this
    // program, so on its own it would not notice the nested safepoint
    // going away. Everything allocated here dies in its own iteration.
    assert!(
        stats.peak_heap_objects <= 8 * TINY_GC_THRESHOLD,
        "collection did not run inside the reentrant call: {stats:?}"
    );
}

#[test]
fn sort_by_and_map_each_traverse_correctly_under_collection() {
    // `sortBy` and `Map.each` pair each element with its key or value
    // to carry both through the rooted traversal; this pins that the
    // pairing round-trips, that the sort still orders by key, and that
    // equal keys keep snapshot order.
    let stats = run_hot_gc(
        r##"
struct Row
  name: string
  rank: int
end

let rows = [
  Row { name: "d", rank: 2 },
  Row { name: "a", rank: 1 },
  Row { name: "c", rank: 2 },
  Row { name: "b", rank: 0 },
]

let sorted = rows.sortBy do |row|
  let mut i = 0
  while i < 30
    let garbage = [i]
    i = i + 1
  end
  row.rank
end

puts(sorted.map(do |row| row.name end).join("-"))

let scores = { "x": [1], "y": [1, 2], "z": [1, 2, 3] }
let mut total = 0
scores.each do |key, value|
  let mut i = 0
  while i < 30
    let garbage = [i]
    i = i + 1
  end
  total = total + key.len() + value.len()
end
puts total
"##,
        "b-a-d-c\n9\n",
    );

    assert!(stats.gc_collections > 0, "expected collections: {stats:?}");
}

#[test]
fn a_large_snapshot_does_not_make_collection_quadratic() {
    // The traversal parks the whole snapshot as roots, so marking costs
    // one visit per element. Arming on the live count alone would
    // re-trace all of it every `threshold` allocations — quadratic in
    // the receiver's length, since the elements here are ints and
    // nothing survives the callback to raise `live`.
    let stats = run_hot_gc(
        r##"
let mut src: Vector<int> = []
let mut n = 0
while n < 20000
  src.push(n)
  n = n + 1
end

let mut total = 0
src.each do |x|
  let garbage = [x]
  total = total + garbage.len()
end
puts total
"##,
        "20000\n",
    );

    // Marking is charged against the allocation that preceded it, so
    // the collection count cannot grow with the snapshot the way an
    // allocation-count-driven trigger would.
    assert!(stats.gc_collections > 0, "expected collections: {stats:?}");
    assert!(
        stats.gc_collections < 40,
        "collection count scales with the snapshot: {stats:?}"
    );

    // Floating garbage is proportional to the snapshot by design — the
    // allowance counts the parked roots — so the property worth pinning
    // is the coefficient, not the absence of growth. Every element here
    // dies in its own iteration, so a peak at the element count would
    // mean the traversal held all of it, which is the defect BRS-62
    // fixed, reintroduced through the trigger instead of the safepoint.
    const ELEMENTS: usize = 20_000;
    assert!(
        stats.peak_heap_objects < ELEMENTS / 2,
        "floating garbage exceeds half the snapshot: {stats:?}"
    );
}

#[test]
fn reduce_carries_its_accumulator_across_collections() {
    // `reduce` is the one traversal whose rooted value has no backing
    // in the receiver: the accumulator is produced by the callback and
    // lives in a single reused root slot.
    let stats = run_hot_gc(
        r##"
def churn(): int
  let mut i = 0
  while i < 30
    let garbage = [i]
    i = i + 1
  end
  i
end

let words = ["a", "bb", "ccc", "dddd"]
let lengths = words.reduce([0], |acc, word| [acc, [word.len() + churn() - 30]].flatten())
puts lengths
"##,
        "[0, 1, 2, 3, 4]\n",
    );

    assert!(stats.gc_collections > 0, "expected collections: {stats:?}");
}

#[test]
fn an_early_exit_traversal_leaves_nothing_rooted() {
    // `find`/`any?` stop mid-snapshot, taking the one exit that skips
    // the end of the loop. A mark left parked there would keep the rest
    // of that snapshot alive for the whole run, so the second traversal
    // has to reclaim as well as the first.
    let stats = run_hot_gc(
        r##"
let mut rounds = 0
while rounds < 40
  let source = [[1], [2], [3], [4], [5], [6], [7], [8]]
  let hit = source.find do |item|
    let mut i = 0
    while i < 20
      let garbage = [i]
      i = i + 1
    end
    item.len() > 0
  end
  rounds = rounds + 1
end
puts rounds
"##,
        "40\n",
    );

    assert!(stats.gc_collections > 0, "expected collections: {stats:?}");
    assert!(
        stats.peak_heap_objects <= 8 * TINY_GC_THRESHOLD,
        "early exit leaked its snapshot: {stats:?}"
    );
}

#[test]
fn an_error_raised_inside_a_traversal_leaves_nothing_rooted() {
    // The error arm of every helper unroots before propagating. If it
    // did not, each caught round would strand a snapshot on the root
    // stack and the peak would grow with the number of rounds.
    let stats = run_hot_gc(
        r##"
struct StopError
  detail: string
end

def scan(xs: Vector<Vector<int>>): int throws StopError
  xs.each do |item|
    let mut i = 0
    while i < 20
      let garbage = [i]
      i = i + 1
    end
    throw StopError { detail: "stop" }
  end
  0
end

let mut rounds = 0
while rounds < 40
  let source = [[1], [2], [3], [4], [5], [6], [7], [8]]
  let seen = scan(source) catch (e)
    StopError => -1
  end
  rounds = rounds + 1
end
puts rounds
"##,
        "40\n",
    );

    assert!(stats.gc_collections > 0, "expected collections: {stats:?}");
    assert!(
        stats.peak_heap_objects <= 8 * TINY_GC_THRESHOLD,
        "the error path leaked its snapshot: {stats:?}"
    );
}

#[test]
fn results_a_traversal_accumulates_survive_collections() {
    // `map` and `filter` build a result the receiver does not hold, so
    // early elements are reachable only from the traversal's own
    // accumulation region while the later callbacks run — and those
    // callbacks allocate. Every result here is an arena value and every
    // one is read back, so a result the collector could not see would
    // come back as a recycled slot rather than as itself.
    let stats = run_hot_gc(
        r##"
let src = [1, 2, 3, 4, 5, 6, 7, 8]

let boxed = src.map do |x|
  let mut i = 0
  while i < 20
    let garbage = [i, i]
    i = i + 1
  end
  [x, x * 10]
end

let kept = src.filter do |x|
  let mut i = 0
  while i < 20
    let garbage = [i, i]
    i = i + 1
  end
  x % 2 == 0
end

puts(boxed.map(|pair| pair[1].toString()).join("-"))
puts(kept.map(|x| x.toString()).join("-"))
"##,
        "10-20-30-40-50-60-70-80\n2-4-6-8\n",
    );

    assert!(stats.gc_collections > 0, "expected collections: {stats:?}");
}

#[test]
fn a_callback_that_overwrites_its_capture_does_not_lose_it() {
    // Entering a callback's frame copies its captures into stack slots,
    // and the callee may then overwrite one. The original is not lost —
    // the next invocation republishes it from the closure — but between
    // the store and that republication the closure is the only thing
    // holding it, and the closure was popped off the value stack before
    // the builtin ran. Every call therefore has to see the original
    // three-element capture, not a slot the sweeper recycled.
    let stats = run_hot_gc(
        r##"
def make(): (int) -> int
  let mut box = [7, 7, 7]
  do |x|
    let seen = box.len()
    box = [x]
    let mut i = 0
    while i < 30
      let garbage = [i]
      i = i + 1
    end
    seen
  end
end

puts([1, 2, 3, 4, 5].map(make()).map(|v| v.toString()).join("-"))
"##,
        "3-3-3-3-3\n",
    );

    assert!(stats.gc_collections > 0, "expected collections: {stats:?}");
    // Bounding the peak as well as the count: a positive collection
    // count alone is satisfied by the top-level boundaries around this
    // program, so on its own it would not notice the nested safepoint
    // going away. Everything allocated here dies in its own iteration.
    assert!(
        stats.peak_heap_objects <= 8 * TINY_GC_THRESHOLD,
        "collection did not run inside the reentrant call: {stats:?}"
    );
}

#[test]
fn a_failing_key_callback_leaves_nothing_rooted() {
    // `sortBy` is the only traversal that parks a second region — the
    // keys — above the snapshot. Aborting mid-way has to drop both, so
    // repeated failed sorts must not accumulate.
    let stats = run_hot_gc(
        r##"
struct SortError
  detail: string
end

def ranked(rows: Vector<Vector<int>>): Vector<Vector<int>> throws SortError
  rows.sortBy do |row|
    let mut i = 0
    while i < 20
      let garbage = [i]
      i = i + 1
    end
    if row.len() > 2
      throw SortError { detail: "unrankable" }
    end
    row.len()
  end
end

let mut rounds = 0
while rounds < 40
  let rows = [[1], [2, 2], [3, 3, 3], [4], [5]]
  let sorted = ranked(rows) catch (e)
    SortError => []
  end
  rounds = rounds + 1
end
puts rounds
"##,
        "40\n",
    );

    assert!(stats.gc_collections > 0, "expected collections: {stats:?}");
    assert!(
        stats.peak_heap_objects <= 8 * TINY_GC_THRESHOLD,
        "the failing key path leaked its regions: {stats:?}"
    );
}

#[test]
fn every_traversal_degenerates_cleanly_on_an_empty_receiver() {
    // Each helper's index arithmetic has a base == end case, where the
    // loops and the split_offs must all no-op rather than pair a key
    // with nothing or read past the region.
    run_hot_gc(
        r##"
let empty: Vector<int> = []
let none: Map<string, int> = {}

puts(empty.map(|x| x + 1).len())
puts(empty.filter(|x| x > 0).len())
puts(empty.sortBy(|x| x).len())
puts(empty.reduce(0, |acc, x| acc + x))
puts(empty.find(|x| x > 0).toString())
puts(empty.any?(|x| x > 0))
puts(empty.all?(|x| x > 0))
empty.each do |x|
  puts x
end
none.each do |k, v|
  puts k
end
puts "done"
"##,
        "0\n0\n0\n0\nNone\nfalse\ntrue\ndone\n",
    );
}

#[test]
fn rendering_a_container_of_overrides_keeps_its_elements_rooted() {
    // Two more of the display path's rooting sites: `render_all` for
    // vectors, tuples and enum payloads, and the Map arm's
    // flattened entries; and `render_cell`'s own value, which is what
    // keeps every cell recorded in the cycle path naming the cell it
    // was recorded for rather than a slot some later allocation
    // recycled. Each override allocates, so a collection lands between
    // two elements of every container here.
    let stats = run_hot_gc(
        r##"
struct Tag
  size: int

  def toString(self): string
    let mut i = 0
    while i < 20
      let garbage = [i]
      i = i + 1
    end
    "t#{self.size}"
  end
end

let items = [Tag { size: 1 }, Tag { size: 2 }, Tag { size: 3 }]
puts items
puts(Set([1, 2, 3]))
puts((Tag { size: 4 }, Tag { size: 5 }))
puts({ "a": Tag { size: 6 }, "b": Tag { size: 7 } })
puts(Some(Tag { size: 8 }))
"##,
        "[t1, t2, t3]\nSet([1, 2, 3])\n(t4, t5)\n{ \"a\": t6, \"b\": t7 }\nSome(t8)\n",
    );

    assert!(stats.gc_collections > 0, "expected collections: {stats:?}");
    // Bounding the peak as well as the count: a positive collection
    // count alone is satisfied by the top-level boundaries around this
    // program, so on its own it would not notice the nested safepoint
    // going away. Everything allocated here dies in its own iteration.
    assert!(
        stats.peak_heap_objects <= 8 * TINY_GC_THRESHOLD,
        "collection did not run inside the reentrant call: {stats:?}"
    );
}

#[test]
fn a_cycle_still_renders_as_a_cycle_under_collection() {
    // The cycle path holds bare arena references while the elements
    // around them are rendered, and rendering can now collect — which
    // needs an override on the cycle, since rendering without one
    // reenters nothing and so reaches no safepoint at all. A recycled
    // slot would either mark an unrelated value as a cycle or stop
    // marking the real one.
    let stats = run_hot_gc(
        r##"
struct Leaf
  size: int

  def toString(self): string
    let mut i = 0
    while i < 20
      let garbage = [i]
      i = i + 1
    end
    "leaf#{self.size}"
  end
end

struct Node
  leaf: Leaf
  items: Vector<Node>
end

let node = Node { leaf: Leaf { size: 7 }, items: [] }
node.items.push(node)
puts node
"##,
        "Node { leaf: leaf7, items: [<cycle>] }\n",
    );

    assert!(stats.gc_collections > 0, "expected collections: {stats:?}");
    // Bounding the peak as well as the count: a positive collection
    // count alone is satisfied by the top-level boundaries around this
    // program, so on its own it would not notice the nested safepoint
    // going away. Everything allocated here dies in its own iteration.
    assert!(
        stats.peak_heap_objects <= 8 * TINY_GC_THRESHOLD,
        "collection did not run inside the reentrant call: {stats:?}"
    );
}

#[test]
fn a_render_time_mutation_cannot_free_the_entries_being_rendered() {
    // Rendering copies a map's entries out and then reenters user code
    // per element. An override that empties the map mid-render leaves
    // the pending entries reachable only from that copy.
    let stats = run_hot_gc(
        r##"
struct Slot
  size: int

  def toString(self): string
    while board.len() > 0
      board.remove("a")
      board.remove("b")
      board.remove("c")
    end
    let mut i = 0
    while i < 20
      let garbage = [i]
      i = i + 1
    end
    "s#{self.size}"
  end
end

let board = { "a": Slot { size: 1 }, "b": Slot { size: 2 }, "c": Slot { size: 3 } }
puts board
"##,
        "{ \"a\": s1, \"b\": s2, \"c\": s3 }\n",
    );

    assert!(stats.gc_collections > 0, "expected collections: {stats:?}");
    // Bounding the peak as well as the count: a positive collection
    // count alone is satisfied by the top-level boundaries around this
    // program, so on its own it would not notice the nested safepoint
    // going away. Everything allocated here dies in its own iteration.
    assert!(
        stats.peak_heap_objects <= 8 * TINY_GC_THRESHOLD,
        "collection did not run inside the reentrant call: {stats:?}"
    );
}

#[test]
fn a_walk_record_keeps_its_two_vectors_alive_across_collections() {
    // `Walk` is the first native record whose fields are arena values,
    // so the collector reaches them only through its own trace arm.
    // Nothing else in the suite would notice that arm going away: a
    // traversal followed by a print never survives long enough to be
    // swept. Here the record is held across enough allocation to force
    // several collections, and only then are its fields read.
    let dir = std::env::temp_dir().join(format!("brasa-gc-walk-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("sub")).expect("fixture dirs");

    for name in ["c", "b", "a"] {
        std::fs::write(dir.join(format!("sub/{name}.txt")), name).expect("fixture written");
    }

    let root = dir.display();
    let stats = run_hot_gc(
        &format!(
            r##"
import std::fs

let held = fs.tryWalk("{root}")
let mut i = 0
while i < 200
  let garbage = ["#{{i}}", "#{{i}}", "#{{i}}"]
  i = i + 1
end

puts(held.paths.map(|p| fs.base(p)).join(","))
puts held.unreadable.len()
"##
        ),
        "a.txt,b.txt,c.txt\n0\n",
    );

    assert!(stats.gc_collections > 0, "expected collections: {stats:?}");

    let _ = std::fs::remove_dir_all(&dir);
}
