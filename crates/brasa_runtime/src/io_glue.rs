//! Backend-agnostic stream glue for `std::io` (BRS-34,
//! `docs/spec/05-stdlib.md`), shared by the walker and the VM.
//!
//! Decisions recorded here (mirrored in the spec):
//!
//! - A run is wired to three streams ([`Streams`]) instead of reaching
//!   for the process handles directly, so `io.eprint` and the stdin
//!   readers are observable to the parity harness exactly like `puts`.
//!   The CLI wires the real process streams; nothing else about the
//!   surface changes.
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

use std::io::{BufRead, Write};

/// The streams one run is wired to: program output, the `io.eprint`
/// sink, and the source `io.readLine`/`io.readAll` consume.
///
/// The backends run on their own thread, so every stream must be
/// `Send`. Note that `std::io::StdinLock` is not, which is why the
/// process default buffers [`std::io::Stdin`] instead.
pub struct Streams<'a> {
    pub out: &'a mut (dyn Write + Send),
    pub err: &'a mut (dyn Write + Send),
    pub input: &'a mut (dyn BufRead + Send),
}

/// One line from `input` without its trailing newline; `None` at end of
/// input.
pub fn read_line(input: &mut dyn BufRead) -> Option<String> {
    let mut bytes = Vec::new();
    let _ = input.read_until(b'\n', &mut bytes);

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

/// The whole remaining input, newlines intact.
pub fn read_all(input: &mut dyn BufRead) -> String {
    let mut bytes = Vec::new();
    let _ = input.read_to_end(&mut bytes);

    String::from_utf8_lossy(&bytes).into_owned()
}
