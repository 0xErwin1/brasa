//! Golden `.brs` program suite for the M1 tree-walker (BRS-20).
//!
//! Success programs under `tests/programs/` pin their exact stdout in a
//! sibling `.out` file; failure programs assert the exit code and
//! stderr substrings. The runnable repository examples are pinned here
//! too, so a semantic regression in any layer shows up as a diff.

use std::path::PathBuf;
use std::process::{Command, Output};

fn run(path: &PathBuf) -> Output {
    Command::new(env!("CARGO_BIN_EXE_brasa"))
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

/// Runs a golden program and compares its stdout byte-for-byte against
/// the sibling `.out` file; the run must succeed with empty stderr.
fn assert_golden(name: &str) {
    let output = run(&program_path(&format!("{name}.brs")));
    let expected = std::fs::read_to_string(program_path(&format!("{name}.out")))
        .expect("missing expected-output file");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(0), "stderr: {stderr}");
    assert!(stderr.is_empty(), "expected empty stderr, got: {stderr}");
    assert_eq!(String::from_utf8_lossy(&output.stdout), expected);
}

/// Runs an example and compares its stdout against the expectation
/// pinned inline; the run must succeed with empty stderr.
fn assert_example(name: &str, expected: &str) {
    let output = run(&example_path(name));

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(0), "stderr: {stderr}");
    assert!(stderr.is_empty(), "expected empty stderr, got: {stderr}");
    assert_eq!(String::from_utf8_lossy(&output.stdout), expected);
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
    let output = run(&program_path("throw_uncaught.brs"));

    assert_eq!(output.status.code(), Some(70));
    assert_eq!(String::from_utf8_lossy(&output.stdout), "before\n");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("error:"), "stderr: {stderr}");
    assert!(stderr.contains("BoomError"), "stderr: {stderr}");
}

#[test]
fn uncaught_panic_exits_70_with_type_and_call_chain() {
    let output = run(&program_path("panic_uncaught.brs"));

    assert_eq!(output.status.code(), Some(70));
    assert_eq!(String::from_utf8_lossy(&output.stdout), "start\n");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("panic"), "stderr: {stderr}");
    assert!(
        stderr.contains("panics.IndexOutOfBounds"),
        "stderr: {stderr}"
    );
    assert!(stderr.contains("in inner"), "stderr: {stderr}");
    assert!(stderr.contains("in outer"), "stderr: {stderr}");
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
