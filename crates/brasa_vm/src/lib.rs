//! The bytecode stack VM (BRS-28, M3).
//!
//! Executes a compiled [`brasa_bytecode::Module`] with an iterative
//! dispatch loop over one contiguous value stack, per the normative
//! design in `docs/spec/07-bytecode.md`. The observable-behavior oracle
//! is the reference tree-walker (`brasa_interp`): outputs, error and
//! panic messages, stacktraces, and exit semantics must match it byte
//! for byte — where they disagree, this crate has a bug.
//!
//! Decisions this unit fixes:
//!
//! - **Outcome sharing**: [`Outcome`] is re-exported from
//!   `brasa_interp`. One type means the CLI maps exit codes through a
//!   single `match` and the parity suite compares outcomes with plain
//!   equality — no conversion layer to drift. The walker stays in-tree
//!   as the reference interpreter, so the dependency is permanent by
//!   design.
//! - **Heap layer**: the mutable, cycle-capable value kinds (`Vector`,
//!   `Map`, `Set`, `Struct`) live in a precise mark-and-sweep arena
//!   ([`heap`]); the immutable kinds stay behind `Rc` handles, which
//!   are provably cycle-free (see the heap module docs). String
//!   constants are interned once at module load.
//! - **Reentrancy**: the dispatch loop is iterative (compiled calls
//!   push frames, never Rust frames), but native builtins that invoke
//!   user code (`map`, `filter`, `sortBy`, user `toString` during
//!   rendering) run a nested bounded loop — mirroring the walker's own
//!   recursion at exactly the same points, bounded by the same
//!   call-depth guard.

mod builtins;
mod display;
mod heap;
mod value;
mod vm;

pub use brasa_interp::Outcome;
pub use heap::{DEFAULT_GC_THRESHOLD, GcRef};
pub use value::Value;

use std::io::Write;

use brasa_bytecode::Module;

/// Default call-depth limit, identical to the walker's
/// (`brasa_interp::DEFAULT_MAX_CALL_DEPTH`).
pub const DEFAULT_MAX_CALL_DEPTH: usize = brasa_interp::DEFAULT_MAX_CALL_DEPTH;

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
    /// Distinct interned strings (the module's string constants).
    pub interned_strings: usize,
    /// Intern lookups served by an existing allocation.
    pub intern_hits: u64,
}

/// Runs `<toplevel>` (functions[0]) and then the module's `main` if the
/// file defines one, writing program output to `out`.
pub fn run<W: Write + Send>(module: &Module, out: &mut W) -> Outcome {
    run_with_depth(module, out, DEFAULT_MAX_CALL_DEPTH)
}

/// [`run`] with an explicit call-depth limit; exceeding it raises a
/// `panics.StackOverflow` panic instead of overflowing the Rust stack.
pub fn run_with_depth<W: Write + Send>(module: &Module, out: &mut W, max_depth: usize) -> Outcome {
    run_configured(module, out, max_depth, DEFAULT_GC_THRESHOLD).0
}

/// [`run`] with an explicit GC allocation threshold: the collector arms
/// after this many live arena objects. Exists for the GC test suite and
/// BRS-30's benchmarks; programs use the default.
pub fn run_with_gc_threshold<W: Write + Send>(
    module: &Module,
    out: &mut W,
    gc_threshold: usize,
) -> (Outcome, RunStats) {
    run_configured(module, out, DEFAULT_MAX_CALL_DEPTH, gc_threshold)
}

fn run_configured<W: Write + Send>(
    module: &Module,
    out: &mut W,
    max_depth: usize,
    gc_threshold: usize,
) -> (Outcome, RunStats) {
    std::thread::scope(|scope| {
        let handle = std::thread::Builder::new()
            .name("brasa-vm".to_string())
            .stack_size(VM_STACK_SIZE)
            .spawn_scoped(scope, move || {
                let mut vm = vm::Vm::new(module, out, max_depth, gc_threshold);
                let outcome = vm.run();
                (outcome, vm.run_stats())
            })
            .expect("failed to spawn the VM thread");

        handle
            .join()
            .expect("the VM thread never panics: failures are Outcome values")
    })
}
