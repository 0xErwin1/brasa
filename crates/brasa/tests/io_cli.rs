//! CLI-level `std::io` tests (BRS-34, `docs/spec/05-stdlib.md`).
//!
//! `io.readLine`/`io.readAll` consume the REAL process stdin and
//! `io.eprint` writes to the real stderr, so the library-level parity
//! harness (which only injects a stdout sink) cannot exercise them.
//! These tests run the built `brasa` binary with piped stdin on BOTH
//! backends and assert identical stdout, stderr, and exit status —
//! the CLI is the parity oracle here.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

fn run_backend(script: &Path, backend: &str, stdin: &[u8]) -> std::process::Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_brasa"))
        .arg("--backend")
        .arg(backend)
        .arg(script)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn brasa");

    child
        .stdin
        .take()
        .expect("stdin is piped")
        .write_all(stdin)
        .expect("failed to write stdin");

    child.wait_with_output().expect("failed to collect output")
}

/// Runs `source` with `stdin` on both backends, asserts they agree,
/// and returns the shared `(stdout, stderr, code)`.
fn run_both(tag: &str, source: &str, stdin: &[u8]) -> (String, String, Option<i32>) {
    let dir = std::env::temp_dir().join(format!("brasa-io-cli-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("failed to create temp dir");
    let script = dir.join("io.brs");
    std::fs::write(&script, source).expect("failed to write script");

    let walker = run_backend(&script, "walker", stdin);
    let vm = run_backend(&script, "vm", stdin);

    let walker_stdout = String::from_utf8_lossy(&walker.stdout).into_owned();
    let vm_stdout = String::from_utf8_lossy(&vm.stdout).into_owned();
    let walker_stderr = String::from_utf8_lossy(&walker.stderr).into_owned();
    let vm_stderr = String::from_utf8_lossy(&vm.stderr).into_owned();

    assert_eq!(walker_stdout, vm_stdout, "stdout parity failed");
    assert_eq!(walker_stderr, vm_stderr, "stderr parity failed");
    assert_eq!(
        walker.status.code(),
        vm.status.code(),
        "exit-code parity failed"
    );

    std::fs::remove_dir_all(&dir).ok();
    (walker_stdout, walker_stderr, walker.status.code())
}

#[test]
fn read_line_strips_newlines_and_reports_eof_as_none() {
    let (stdout, stderr, code) = run_both(
        "readline",
        r#"
import std::io

let first = io.readLine() ?? "<eof>"
let second = io.readLine() ?? "<eof>"
let third = io.readLine() ?? "<eof>"
let fourth = io.readLine() ?? "<eof>"
puts first
puts second
puts third
puts fourth
"#,
        b"alpha\r\nbeta\nlast without newline",
    );

    assert_eq!(stdout, "alpha\nbeta\nlast without newline\n<eof>\n");
    assert_eq!(stderr, "");
    assert_eq!(code, Some(0));
}

#[test]
fn read_all_takes_the_remaining_stdin_verbatim() {
    let (stdout, stderr, code) = run_both(
        "readall",
        r#"
import std::io

let first = io.readLine() ?? "<eof>"
let rest = io.readAll()
puts "first: #{first}"
puts "rest: #{rest}"
"#,
        b"one\ntwo\nthree\n",
    );

    assert_eq!(stdout, "first: one\nrest: two\nthree\n\n");
    assert_eq!(stderr, "");
    assert_eq!(code, Some(0));
}

#[test]
fn eprint_writes_to_stderr_and_io_printers_mirror_the_prelude() {
    let (stdout, stderr, code) = run_both(
        "eprint",
        r#"
import std::io

io.puts("to stdout")
io.print("no newline")
io.eprint("to stderr")
io.eprint(42)
"#,
        b"",
    );

    assert_eq!(stdout, "to stdout\nno newline");
    assert_eq!(stderr, "to stderr42");
    assert_eq!(code, Some(0));
}

#[test]
fn empty_stdin_reads_cleanly() {
    let (stdout, stderr, code) = run_both(
        "empty",
        r#"
import std::io

puts io.readLine() ?? "<eof>"
puts "all: #{io.readAll()}"
"#,
        b"",
    );

    assert_eq!(stdout, "<eof>\nall: \n");
    assert_eq!(stderr, "");
    assert_eq!(code, Some(0));
}

#[test]
fn invalid_utf8_stdin_decodes_lossily() {
    let (stdout, stderr, code) = run_both(
        "lossy",
        r#"
import std::io

puts io.readAll()
"#,
        b"ok \xff\xfe end\n",
    );

    assert_eq!(stdout, "ok \u{fffd}\u{fffd} end\n\n");
    assert_eq!(stderr, "");
    assert_eq!(code, Some(0));
}
