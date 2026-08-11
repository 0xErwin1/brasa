//! Backend-agnostic stdin glue for `std::io` (BRS-34,
//! `docs/spec/05-stdlib.md`), shared by the walker and the VM.
//!
//! Decisions recorded here (mirrored in the spec):
//!
//! - Both backends read the REAL process stdin through the same OS
//!   handle; there is no per-run stdin injection (the library-level
//!   parity harness cannot pipe stdin, so `readLine`/`readAll` are
//!   pinned by CLI-level tests instead).
//! - Input decodes as lossy UTF-8 (invalid bytes become U+FFFD),
//!   consistent with `std::proc`'s output capture — a Unix filter must
//!   never die on a stray byte.
//! - `readLine` strips one trailing `\n` (and a preceding `\r`), so a
//!   final line without a newline still yields its content; end of
//!   input is `None`.
//! - `std::io` has no error namespace in v1: an OS-level read failure
//!   is treated as end of input (`readLine` yields `None`, `readAll`
//!   yields what was readable). Inventing an error type for a
//!   condition scripts cannot meaningfully handle was ruled out.

use std::io::{BufRead, Read};

/// One stdin line without its trailing newline; `None` at end of
/// input.
pub fn read_line() -> Option<String> {
    let mut bytes = Vec::new();
    let _ = std::io::stdin().lock().read_until(b'\n', &mut bytes);

    if bytes.is_empty() {
        return None;
    }

    if bytes.last() == Some(&b'\n') {
        bytes.pop();
        if bytes.last() == Some(&b'\r') {
            bytes.pop();
        }
    }

    Some(String::from_utf8_lossy(&bytes).into_owned())
}

/// The whole remaining stdin, newlines intact.
pub fn read_all() -> String {
    let mut bytes = Vec::new();
    let _ = std::io::stdin().lock().read_to_end(&mut bytes);

    String::from_utf8_lossy(&bytes).into_owned()
}
