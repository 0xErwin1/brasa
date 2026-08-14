//! The program's own output, readable while it is still running.
//!
//! `brasa_runtime::Streams` takes `&mut dyn Write`, and a debug session
//! owns the VM that holds that borrow — so a TUI cannot read what the
//! program printed while the session is alive. Which is to say: without
//! this, `puts` is invisible in the debugger, and `puts` is how most
//! people debug.
//!
//! A shared buffer solves it. `Arc<Mutex<_>>` rather than `Rc<RefCell<_>>`
//! because `Streams` requires `Send`; the lock is never contended in
//! practice, since the VM and the UI take turns rather than run at once.

use std::io::Write;
use std::sync::{Arc, Mutex};

/// A writer whose contents can be read while it is still being written.
#[derive(Clone, Default)]
pub struct Capture {
    buffer: Arc<Mutex<Vec<u8>>>,
}

impl Capture {
    pub fn new() -> Capture {
        Capture::default()
    }

    /// Everything written so far, as lines.
    ///
    /// Lossy on purpose: a program can print any bytes it likes, and a
    /// debugger that refused to show output because it was not UTF-8
    /// would be withholding the evidence.
    pub fn lines(&self) -> Vec<String> {
        let buffer = self
            .buffer
            .lock()
            .expect("the capture lock is never poisoned");

        String::from_utf8_lossy(&buffer)
            .lines()
            .map(str::to_string)
            .collect()
    }

    pub fn is_empty(&self) -> bool {
        self.buffer
            .lock()
            .expect("the capture lock is never poisoned")
            .is_empty()
    }

    /// Forgets everything written, for a restart.
    pub fn clear(&self) {
        self.buffer
            .lock()
            .expect("the capture lock is never poisoned")
            .clear();
    }
}

impl Write for Capture {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        self.buffer
            .lock()
            .expect("the capture lock is never poisoned")
            .extend_from_slice(data);

        Ok(data.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The point of the type: what was written is readable through
    /// another handle, without the writer being given back.
    #[test]
    fn a_clone_reads_what_the_original_wrote() {
        let mut writer = Capture::new();
        let reader = writer.clone();

        assert!(reader.is_empty());
        writeln!(writer, "hello").expect("writes");
        writeln!(writer, "world").expect("writes");

        assert_eq!(reader.lines(), vec!["hello", "world"]);
    }

    /// Non-UTF-8 output is shown rather than withheld: a debugger that
    /// hid the evidence because it was malformed would be useless
    /// exactly when it matters.
    #[test]
    fn invalid_utf8_is_shown_lossily() {
        let mut writer = Capture::new();
        writer
            .write_all(&[b'a', 0xff, b'b', b'\n'])
            .expect("writes");

        let lines = writer.lines();
        assert_eq!(lines.len(), 1);
        assert!(lines[0].starts_with('a') && lines[0].ends_with('b'));
    }

    #[test]
    fn clearing_forgets_everything_for_a_restart() {
        let mut writer = Capture::new();
        writeln!(writer, "before").expect("writes");

        writer.clear();
        assert!(writer.is_empty());
        assert!(writer.lines().is_empty());
    }
}
