//! Reference tree-walking interpreter over HIR (BRS-20, M1).
//!
//! Executes the checked core HIR directly: this walker is the
//! provisional execution engine of `docs/spec/00-vision.md`'s roadmap
//! (the bytecode VM lands in M3, and the walker stays as the reference
//! implementation afterwards), so it favors simplicity and spec
//! fidelity over speed. Shared heap semantics use `Rc<RefCell<...>>`
//! instead of a GC.
//!
//! [`run`] consumes the outputs of the earlier phases — the lowered
//! [`Hir`], its roots, the resolver's [`Resolutions`], and the checker's
//! [`TypeTables`] (whose `wrap_decisions` drive `?.` flattening) — and
//! reports one of three outcomes so the CLI can map exit codes: clean
//! success, an uncaught thrown error, or a panic
//! (`docs/spec/04-errors.md`: both failure classes exit non-zero, a
//! panic additionally carries the call chain).

mod builtins;
pub mod fs_glue;
mod interp;
pub mod io_glue;
pub mod json_glue;
pub mod proc_env;
pub mod rand_glue;
pub mod table;
pub mod time_glue;
mod value;

pub use io_glue::Streams;
pub use value::Value;

use std::io::{BufReader, Write};

use brasa_hir::{Hir, ItemId};
use brasa_resolver::Resolutions;
use brasa_typeck::TypeTables;

use interp::{Interp, Signal};

/// Default call-depth limit: deep enough for real scripts, shallow
/// enough that the guarded Rust stack never overflows first.
pub const DEFAULT_MAX_CALL_DEPTH: usize = 4096;

/// Stack size of the dedicated interpreter thread. Each Brasa call
/// frame costs a handful of Rust frames, so the walker runs on its own
/// generously-sized stack and the call-depth guard — not the host
/// stack — is what bounds recursion.
const INTERP_STACK_SIZE: usize = 256 * 1024 * 1024;

/// How one program run ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Success,
    /// An uncaught thrown error (`docs/spec/04-errors.md`: message and
    /// exit code, no stacktrace).
    Error {
        message: String,
    },
    /// An uncaught panic (`docs/spec/04-errors.md`: message plus the
    /// call chain, innermost first).
    Panic {
        message: String,
    },
    /// The output stream's read end closed mid-write (`EPIPE`, e.g.
    /// `brasa script.brs | head`). Standard Unix tools treat this as a
    /// silent, successful exit, so the CLI reports nothing and exits 0.
    BrokenPipe,
}

/// Runs the program rooted at `roots`, writing its output to `out`.
/// `io.eprint` and the stdin readers reach the real process streams;
/// [`run_with_streams`] wires them elsewhere. `args` are the script's
/// trailing CLI arguments, served by `env.args()`
/// (`docs/spec/05-stdlib.md`, BRS-32).
pub fn run<W: Write + Send>(
    hir: &Hir,
    roots: &[ItemId],
    resolutions: &Resolutions,
    types: &TypeTables,
    out: &mut W,
    args: &[String],
) -> Outcome {
    run_with_depth(
        hir,
        roots,
        resolutions,
        types,
        out,
        DEFAULT_MAX_CALL_DEPTH,
        args,
    )
}

/// [`run`] with an explicit call-depth limit; exceeding it raises a
/// `panics.StackOverflow` panic instead of overflowing the Rust stack.
pub fn run_with_depth<W: Write + Send>(
    hir: &Hir,
    roots: &[ItemId],
    resolutions: &Resolutions,
    types: &TypeTables,
    out: &mut W,
    max_depth: usize,
    args: &[String],
) -> Outcome {
    let mut err = std::io::stderr();
    let mut input = BufReader::new(std::io::stdin());

    run_with_streams(
        hir,
        roots,
        resolutions,
        types,
        Streams {
            out,
            err: &mut err,
            input: &mut input,
        },
        max_depth,
        args,
    )
}

/// [`run_with_depth`] with every stream wired explicitly: the one entry
/// point through which `io.eprint` and `io.readLine`/`io.readAll` are
/// observable to a caller (the parity harness runs both backends this
/// way).
pub fn run_with_streams<'a>(
    hir: &'a Hir,
    roots: &[ItemId],
    resolutions: &'a Resolutions,
    types: &'a TypeTables,
    streams: Streams<'a>,
    max_depth: usize,
    args: &[String],
) -> Outcome {
    std::thread::scope(|scope| {
        let handle = std::thread::Builder::new()
            .name("brasa-interp".to_string())
            .stack_size(INTERP_STACK_SIZE)
            .spawn_scoped(scope, move || {
                let mut interp = Interp::new(hir, resolutions, types, streams, max_depth, args);
                let result = interp.run_program(roots);
                finish(&mut interp, result)
            })
            .expect("failed to spawn the interpreter thread");

        handle
            .join()
            .expect("the interpreter thread never panics: failures are Outcome values")
    })
}

fn finish(interp: &mut Interp<'_>, result: Result<(), Signal>) -> Outcome {
    match result {
        Ok(()) => Outcome::Success,
        Err(Signal::Error(value)) => {
            let tag = interp.nominal_tag(&value);
            let rendered = interp
                .display(&value)
                .unwrap_or_else(|_| "<toString failed>".to_string());
            Outcome::Error {
                message: format!("error: {tag}: {rendered}"),
            }
        }
        Err(Signal::Panic(panic)) => {
            let mut message = format!("panic: {}: {}", panic.kind.name(), panic.detail);
            for frame in &panic.stack {
                message.push_str("\n  in ");
                message.push_str(frame);
            }
            Outcome::Panic { message }
        }
        Err(Signal::Fatal(message)) => Outcome::Error { message },
        Err(Signal::BrokenPipe) => Outcome::BrokenPipe,
        // `return`/`break`/`continue` escaping to the top level would be
        // a checker bug; surface it instead of hiding it.
        Err(Signal::Return(_) | Signal::Break | Signal::Continue) => Outcome::Error {
            message: "brasa: control-flow signal escaped to the top level".to_string(),
        },
    }
}
