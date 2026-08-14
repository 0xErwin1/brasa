//! The bytecode stack VM (BRS-28, M3).
//!
//! Executes a compiled [`brasa_bytecode::Module`] with an iterative
//! dispatch loop over one contiguous value stack, per the normative
//! design in spec: 07 — Diseño del bytecode. The observable-behavior oracle
//! is the conformance corpus (`tests/conformance.rs`): outputs, error
//! and panic messages, stacktraces, and exit semantics must match what
//! is pinned there byte for byte — where they disagree, this crate has
//! a bug. That role belonged to the reference tree-walker until
//! BRS-108.
//!
//! Decisions this unit fixes:
//!
//! - **Outcome sharing**: [`Outcome`] is re-exported from
//!   `brasa_runtime`. One type means the CLI maps exit codes through a
//!   single `match` and the corpus compares outcomes with plain
//!   equality — no conversion layer to drift.
//! - **Heap layer**: the mutable, cycle-capable value kinds (`Vector`,
//!   `Map`, `Set`, `Struct`) live in a precise mark-and-sweep arena
//!   ([`heap`]); the immutable kinds stay behind `Rc` handles, which
//!   are provably cycle-free (see the heap module docs). String
//!   constants are interned once at module load.
//! - **Reentrancy**: the dispatch loop is iterative (compiled calls
//!   push frames, never Rust frames), but native builtins that invoke
//!   user code (`map`, `filter`, `sortBy`, user `toString` during
//!   rendering) run a nested bounded loop, bounded by the same
//!   call-depth guard as compiled calls.

mod builtins;
mod display;
mod heap;
mod value;
mod vm;

pub use brasa_runtime::{Outcome, Streams};
pub mod debug;

pub use heap::{DEFAULT_GC_BUDGET_BYTES, GcRef};
pub use value::Value;

use std::io::{BufReader, Write};

use brasa_bytecode::Module;

/// Default call-depth limit, identical to the walker's
/// (`brasa_runtime::DEFAULT_MAX_CALL_DEPTH`).
pub const DEFAULT_MAX_CALL_DEPTH: usize = brasa_runtime::DEFAULT_MAX_CALL_DEPTH;

/// Stack size of the dedicated VM thread. The dispatch loop itself is
/// iterative, but builtins that call back into user code nest Rust
/// frames up to the call-depth guard, so the VM runs on the same
/// generously-sized stack as the walker.
const VM_STACK_SIZE: usize = 256 * 1024 * 1024;

/// Heap and interner counters observable after a run: the measurement
/// hook for BRS-30's benchmarks and the GC test suite.
#[derive(Debug, Clone, Copy)]
pub struct RunStats {
    /// Total arena allocations over the whole run.
    pub heap_allocations: u64,
    /// Mark-and-sweep collections performed.
    pub gc_collections: u64,
    /// Arena objects still live when the program ended.
    pub live_heap_objects: usize,
    /// Most arena objects ever live at once. Exact, not an upper bound:
    /// allocation only grows the arena when the free list is empty, and
    /// an empty free list means every existing slot is live, so each
    /// growth records a moment when the live count equalled the arena's
    /// size.
    pub peak_heap_objects: usize,
    /// Bytes the live arena objects retained when the program ended.
    pub live_heap_bytes: usize,
    /// High-water mark of the retained bytes (BRS-100), which is the
    /// figure the heap budget actually bounds. An upper bound, not an
    /// exact one: between two collections it still counts objects that
    /// have already become unreachable.
    pub peak_heap_bytes: usize,
    /// Distinct interned strings (the module's string constants).
    pub interned_strings: usize,
    /// Intern lookups served by an existing allocation.
    pub intern_hits: u64,
}

/// Runs `<toplevel>` (functions[0]) and then the module's `main` if the
/// file defines one, writing program output to `out`. `io.eprint` and
/// the stdin readers reach the real process streams;
/// [`run_with_streams`] wires them elsewhere. `args` are the script's
/// trailing CLI arguments, served by `env.args()`
/// (spec: 05 — Stdlib de scripting, BRS-32).
pub fn run<W: Write + Send>(module: &Module, out: &mut W, args: &[String]) -> Outcome {
    run_with_depth(module, out, DEFAULT_MAX_CALL_DEPTH, args)
}

/// [`run`] with an explicit call-depth limit; exceeding it raises a
/// `panics.StackOverflow` panic instead of overflowing the Rust stack.
pub fn run_with_depth<W: Write + Send>(
    module: &Module,
    out: &mut W,
    max_depth: usize,
    args: &[String],
) -> Outcome {
    let mut err = std::io::stderr();
    let mut input = BufReader::new(std::io::stdin());

    run_configured(
        module,
        Streams {
            out,
            err: &mut err,
            input: &mut input,
        },
        max_depth,
        DEFAULT_GC_BUDGET_BYTES,
        args,
    )
    .0
}

/// [`run_with_depth`] with every stream wired explicitly: the one entry
/// point through which `io.eprint` and `io.readLine`/`io.readAll` are
/// observable to a caller (the parity harness runs both backends this
/// way, including its hot-GC leg — hence the explicit budget).
pub fn run_with_streams<'a>(
    module: &'a Module,
    streams: Streams<'a>,
    max_depth: usize,
    gc_budget_bytes: usize,
    args: &[String],
) -> (Outcome, RunStats) {
    run_configured(module, streams, max_depth, gc_budget_bytes, args)
}

/// [`run`] with an explicit GC heap budget: the collector arms once the
/// live arena objects retain this many bytes. A budget of a few bytes
/// therefore collects at essentially every allocation, which is how the
/// GC test suite and the conformance corpus's hot leg force pathological
/// collection pressure; programs use the default.
pub fn run_with_gc_budget<W: Write + Send>(
    module: &Module,
    out: &mut W,
    gc_budget_bytes: usize,
) -> (Outcome, RunStats) {
    let mut err = std::io::stderr();
    let mut input = BufReader::new(std::io::stdin());

    run_configured(
        module,
        Streams {
            out,
            err: &mut err,
            input: &mut input,
        },
        DEFAULT_MAX_CALL_DEPTH,
        gc_budget_bytes,
        &[],
    )
}

/// Runs a module's `test` items, one at a time, reporting how each
/// ended alongside the outcome of the shared top-level setup.
///
/// A non-`Success` setup outcome means no test ran: the module never
/// finished initializing, so every result would be about that instead.
pub fn run_tests<W: Write + Send>(
    module: &Module,
    out: &mut W,
    args: &[String],
) -> (Outcome, Vec<(String, Outcome)>) {
    let mut err = std::io::stderr();
    let mut input = BufReader::new(std::io::stdin());

    let streams = Streams {
        out,
        err: &mut err,
        input: &mut input,
    };

    std::thread::scope(|scope| {
        let handle = std::thread::Builder::new()
            .name("brasa-vm".to_string())
            .stack_size(VM_STACK_SIZE)
            .spawn_scoped(scope, move || {
                let mut vm = vm::Vm::new(
                    module,
                    streams,
                    DEFAULT_MAX_CALL_DEPTH,
                    DEFAULT_GC_BUDGET_BYTES,
                    args,
                );
                vm.run_tests()
            })
            .expect("failed to spawn the VM thread");

        handle.join().expect("the VM thread panicked")
    })
}

fn run_configured<'a>(
    module: &'a Module,
    streams: Streams<'a>,
    max_depth: usize,
    gc_budget_bytes: usize,
    args: &[String],
) -> (Outcome, RunStats) {
    std::thread::scope(|scope| {
        let handle = std::thread::Builder::new()
            .name("brasa-vm".to_string())
            .stack_size(VM_STACK_SIZE)
            .spawn_scoped(scope, move || {
                let mut vm = vm::Vm::new(module, streams, max_depth, gc_budget_bytes, args);
                let outcome = vm.run();
                (outcome, vm.run_stats())
            })
            .expect("failed to spawn the VM thread");

        handle
            .join()
            .expect("the VM thread never panics: failures are Outcome values")
    })
}
