//! Golden `.brs` program suite for the M1 tree-walker (BRS-20) and,
//! since M3, the bytecode VM backend (BRS-28).
//!
//! Success programs under `tests/programs/` pin their exact stdout in a
//! sibling `.out` file; failure programs assert the exit code and
//! stderr substrings. The runnable repository examples are pinned here
//! too, so a semantic regression in any layer shows up as a diff.
//! Every golden and example runs on BOTH backends against the same
//! pinned expectations, so the VM must match the walker byte for byte.

use std::path::PathBuf;
use std::process::{Command, Output};

/// The two execution backends behind `--backend`; every golden runs on
/// both against the same pinned output.
const BACKENDS: &[&str] = &["walker", "vm"];

fn run_with_backend(path: &PathBuf, backend: &str) -> Output {
    Command::new(env!("CARGO_BIN_EXE_brasa"))
        .arg(format!("--backend={backend}"))
        .arg(path)
        .output()
        .expect("failed to run brasa")
}

fn program_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/programs")
        .join(name)
}

fn example_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples")
        .join(name)
}

/// Runs a golden program on every backend and compares its stdout
/// byte-for-byte against the sibling `.out` file; each run must
/// succeed with empty stderr.
fn assert_golden(name: &str) {
    let expected = std::fs::read_to_string(program_path(&format!("{name}.out")))
        .expect("missing expected-output file");

    for backend in BACKENDS {
        let output = run_with_backend(&program_path(&format!("{name}.brs")), backend);

        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(
            output.status.code(),
            Some(0),
            "[{backend}] stderr: {stderr}"
        );
        assert!(
            stderr.is_empty(),
            "[{backend}] expected empty stderr, got: {stderr}"
        );
        assert_eq!(
            String::from_utf8_lossy(&output.stdout),
            expected,
            "[{backend}] stdout mismatch"
        );
    }
}

/// Runs an example on every backend and compares its stdout against
/// the expectation pinned inline; each run must succeed with empty
/// stderr.
fn assert_example(name: &str, expected: &str) {
    for backend in BACKENDS {
        let output = run_with_backend(&example_path(name), backend);

        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(
            output.status.code(),
            Some(0),
            "[{backend}] stderr: {stderr}"
        );
        assert!(
            stderr.is_empty(),
            "[{backend}] expected empty stderr, got: {stderr}"
        );
        assert_eq!(
            String::from_utf8_lossy(&output.stdout),
            expected,
            "[{backend}] stdout mismatch"
        );
    }
}

#[test]
fn golden_basics() {
    assert_golden("basics");
}

#[test]
fn golden_structs_enums() {
    assert_golden("structs_enums");
}

#[test]
fn golden_collections() {
    assert_golden("collections");
}

#[test]
fn golden_errors() {
    assert_golden("errors");
}

#[test]
fn uncaught_throw_exits_70_with_the_error_message() {
    for backend in BACKENDS {
        let output = run_with_backend(&program_path("throw_uncaught.brs"), backend);

        assert_eq!(output.status.code(), Some(70), "[{backend}]");
        assert_eq!(String::from_utf8_lossy(&output.stdout), "before\n");

        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("error:"), "[{backend}] stderr: {stderr}");
        assert!(stderr.contains("BoomError"), "[{backend}] stderr: {stderr}");
    }
}

#[test]
fn uncaught_panic_exits_70_with_type_and_call_chain() {
    for backend in BACKENDS {
        let output = run_with_backend(&program_path("panic_uncaught.brs"), backend);

        assert_eq!(output.status.code(), Some(70), "[{backend}]");
        assert_eq!(String::from_utf8_lossy(&output.stdout), "start\n");

        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("panic"), "[{backend}] stderr: {stderr}");
        assert!(
            stderr.contains("panics.IndexOutOfBounds"),
            "[{backend}] stderr: {stderr}"
        );
        assert!(stderr.contains("in inner"), "[{backend}] stderr: {stderr}");
        assert!(stderr.contains("in outer"), "[{backend}] stderr: {stderr}");
    }
}

#[test]
fn check_flag_stops_after_typeck_without_output() {
    let output = Command::new(env!("CARGO_BIN_EXE_brasa"))
        .arg("--check")
        .arg(example_path("hello.brs"))
        .output()
        .expect("failed to run brasa");

    assert_eq!(output.status.code(), Some(0));
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
}

#[test]
fn dump_bytecode_compiles_a_golden_program() {
    let output = Command::new(env!("CARGO_BIN_EXE_brasa"))
        .arg("--dump-bytecode")
        .arg(program_path("basics.brs"))
        .output()
        .expect("failed to run brasa");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(0), "stderr: {stderr}");
    assert!(stderr.is_empty(), "expected empty stderr, got: {stderr}");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("== fn0 <toplevel>"),
        "expected a disassembly, got: {stdout}"
    );
}

#[test]
fn example_hello() {
    assert_example("hello.brs", "hello, world!\n1 + 1 = 2\n");
}

#[test]
fn example_fib() {
    let expected = "\
fib(0) = 0
fib(1) = 1
fib(2) = 1
fib(3) = 2
fib(4) = 3
fib(5) = 5
fib(6) = 8
fib(7) = 13
fib(8) = 21
fib(9) = 34
fib(10) = 55
iterative fib(40) = 102334155
";
    assert_example("fib.brs", expected);
}

#[test]
fn example_fizzbuzz() {
    let expected = "\
1
2
Fizz
4
Buzz
Fizz
7
8
Fizz
Buzz
11
Fizz
13
14
FizzBuzz
16
17
Fizz
19
Buzz
";
    assert_example("fizzbuzz.brs", expected);
}

#[test]
fn example_pipeline() {
    let expected = "\
ignis
dbflux
brasa: 1
unknown: 0
brasa is warm
ignis is hot
dbflux is warm
";
    assert_example("pipeline.brs", expected);
}

#[test]
fn example_strings() {
    let expected = "\
Hello, Brasa World
HELLO, BRASA WORLD
Hello, Brasa Script
words: 3
starts? true
joined: <a>, <b>, <c>

line one \\n stays literal
project: brasa

ñ
a
n
d
ú
parsed: -1
";
    assert_example("strings.brs", expected);
}

#[test]
fn example_errors() {
    let expected = "\
recovered: timeout
parsed: 42
out of range gives 0
";
    assert_example("errors.brs", expected);
}

#[test]
fn example_shapes() {
    let expected = "\
circle: area 12.56636
square: area 9.0
dot: area 0.0
biggest is a circle
distance: 5.0
";
    assert_example("shapes.brs", expected);
}
