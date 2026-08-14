//! CLI-level `std::io` tests (BRS-34, spec: 05 — Stdlib de scripting).
//!
//! The library-level conformance harness injects all three streams, so
//! it pins the `std::io` semantics themselves. What only the CLI can
//! pin is the WIRING: that `brasa` hands the real process stdin,
//! stdout, and stderr to the run. These tests drive the built binary
//! with piped stdin and assert stdout, stderr, and exit status.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

fn run_script(script: &Path, stdin: &[u8]) -> std::process::Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_brasa"))
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

/// Runs `source` with `stdin` through the built binary and returns
/// `(stdout, stderr, code)`.
fn run_cli(tag: &str, source: &str, stdin: &[u8]) -> (String, String, Option<i32>) {
    let dir = std::env::temp_dir().join(format!("brasa-io-cli-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("failed to create temp dir");
    let script = dir.join("io.bras");
    std::fs::write(&script, source).expect("failed to write script");

    let run = run_script(&script, stdin);

    let stdout = String::from_utf8_lossy(&run.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&run.stderr).into_owned();

    std::fs::remove_dir_all(&dir).ok();
    (stdout, stderr, run.status.code())
}

#[test]
fn read_line_strips_newlines_and_reports_eof_as_none() {
    let (stdout, stderr, code) = run_cli(
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
    let (stdout, stderr, code) = run_cli(
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
    let (stdout, stderr, code) = run_cli(
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
    let (stdout, stderr, code) = run_cli(
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
    let (stdout, stderr, code) = run_cli(
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
