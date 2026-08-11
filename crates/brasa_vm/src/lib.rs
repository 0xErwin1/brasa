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
//! - **Heap layer**: heap value kinds live behind the thin handle
//!   aliases in [`value`] (`Rc` / `Rc<RefCell<...>>` for now); the GC
//!   unit swaps the alias, not the call sites.
//! - **Reentrancy**: the dispatch loop is iterative (compiled calls
//!   push frames, never Rust frames), but native builtins that invoke
//!   user code (`map`, `filter`, `sortBy`, user `toString` during
//!   rendering) run a nested bounded loop — mirroring the walker's own
//!   recursion at exactly the same points, bounded by the same
//!   call-depth guard.

mod builtins;
mod display;
mod value;
mod vm;

pub use brasa_interp::Outcome;
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

/// Runs `<toplevel>` (functions[0]) and then the module's `main` if the
/// file defines one, writing program output to `out`.
pub fn run<W: Write + Send>(module: &Module, out: &mut W) -> Outcome {
    run_with_depth(module, out, DEFAULT_MAX_CALL_DEPTH)
}

/// [`run`] with an explicit call-depth limit; exceeding it raises a
/// `panics.StackOverflow` panic instead of overflowing the Rust stack.
pub fn run_with_depth<W: Write + Send>(module: &Module, out: &mut W, max_depth: usize) -> Outcome {
    std::thread::scope(|scope| {
        let handle = std::thread::Builder::new()
            .name("brasa-vm".to_string())
            .stack_size(VM_STACK_SIZE)
            .spawn_scoped(scope, move || vm::Vm::new(module, out, max_depth).run())
            .expect("failed to spawn the VM thread");

        handle
            .join()
            .expect("the VM thread never panics: failures are Outcome values")
    })
}
