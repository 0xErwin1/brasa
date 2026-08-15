//! Execution glue shared by every backend: the stdlib's contact with
//! the outside world (filesystem, processes, environment, clock,
//! randomness, JSON), the ordered collections behind `Map` and `Set`,
//! and the types describing how a run ended.
//!
//! Nothing here knows how a program is executed. These modules hold the
//! semantics a backend must not restate — the error mapping of
//! spec: 05 — Stdlib de scripting, the insertion-order guarantees of
//! spec: 03 — Sistema de tipos — so that a second implementation cannot
//! quietly disagree with the first about what a member does.

pub mod cli_glue;
pub mod fs_glue;
pub mod http_glue;
pub mod io_glue;
pub mod json_glue;
pub mod num_glue;
pub mod offload;
pub mod proc_env;
pub mod rand_glue;
pub mod table;
pub mod time_glue;

pub use io_glue::Streams;

/// Default call-depth limit: deep enough for real scripts, shallow
/// enough that a guarded host stack never overflows first.
pub const DEFAULT_MAX_CALL_DEPTH: usize = 4096;

/// How one program run ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Success,
    /// An uncaught thrown error (spec: 04 — Sistema de errores: message and
    /// exit code, no stacktrace).
    Error {
        message: String,
    },
    /// An uncaught panic (spec: 04 — Sistema de errores: message plus the
    /// call chain, innermost first).
    Panic {
        message: String,
    },
    /// The output stream's read end closed mid-write (`EPIPE`, e.g.
    /// `brasa script.bras | head`). Standard Unix tools treat this as a
    /// silent, successful exit, so the CLI reports nothing and exits 0.
    BrokenPipe,
    /// `env.exit(code)`: the script chose its own status. The CLI
    /// prints nothing — a chosen exit is not a failure to report.
    Exit {
        code: i32,
    },
}
