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
    run_with_backend_args(path, backend, &[])
}

/// Runs a program on one backend with trailing script arguments (what
/// `env.args()` sees).
fn run_with_backend_args(path: &PathBuf, backend: &str, args: &[PathBuf]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_brasa"))
        .arg(format!("--backend={backend}"))
        .arg(path)
        .args(args)
        .output()
        .expect("failed to run brasa")
}

/// The repository root, as the scripts under `examples/real/` receive it.
fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
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
    assert_example_args(name, &[], expected);
}

/// Runs an example with trailing script arguments on every backend and
/// compares its stdout against the expectation pinned inline.
fn assert_example_args(name: &str, args: &[PathBuf], expected: &str) {
    for backend in BACKENDS {
        let output = run_with_backend_args(&example_path(name), backend, args);

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
fn example_real_logstat() {
    let expected = "\
parsed 36 requests, skipped 1 unparsable lines
bytes served: 180083

status classes:
  2xx    24  66.7%
  3xx     2  5.6%
  4xx     7  19.4%
  5xx     3  8.3%

top paths:
     7  /health
     4  /api/v1/builds
     4  /api/v1/repos
     3  /api/v1/builds/8821
     2  /api/v1/repos/brasa

top clients:
     8  192.0.2.55
     8  198.51.100.24
     8  203.0.113.7

server errors by endpoint:
     2  POST /api/v1/builds
     1  GET /health
";
    assert_example_args(
        "real/logstat.brs",
        &[example_path("real/data/access.log")],
        expected,
    );
}

#[test]
fn example_real_lockaudit() {
    let expected = "\
flake.lock  (lock version 7, 16 inputs, 16 follows edges)
  cachix         a66a440  2026-04-29  github:cachix/cachix
  crate2nix      ba5dd39  2026-02-27  github:rossng/crate2nix
  devenv         4158f6b  2026-08-09  github:cachix/devenv
  flake-compat   5edf11c  2025-12-29  github:edolstra/flake-compat
  flake-parts    f7c1a2d  2026-05-13  github:hercules-ci/flake-parts
  ghostty        88b4cd0  2026-07-21  github:ghostty-org/ghostty
  git-hooks      9f7e991  2026-07-01  github:cachix/git-hooks.nix
  nix            5940732  2026-08-09  github:cachix/nix
  nixd           a64cd33  2026-07-13  github:nix-community/nixd
  nixpkgs        80bdc1e  2026-03-04  github:NixOS/nixpkgs
  nixpkgs-src    3e41b24  2026-06-16  github:NixOS/nixpkgs
  nixpkgs-src_2  2438956  2026-08-03  github:NixOS/nixpkgs
  nixpkgs_2      12866ae  2026-06-22  github:cachix/devenv-nixpkgs
  nixpkgs_3      6d5d03d  2026-08-04  github:cachix/devenv-nixpkgs
  rust-overlay   13139ae  2026-07-01  github:oxalica/rust-overlay
  treefmt-nix    db94781  2026-05-31  github:numtide/treefmt-nix
  no duplicated revisions
";
    assert_example_args("real/lockaudit.brs", &[repo_root()], expected);
}

/// `gitreport.brs` reports on the live repository, so its counts move
/// with every commit and cannot be pinned. What IS deterministic is the
/// shape: the script always exits 0 and produces exactly one of two
/// well-formed outputs — the report skeleton on stdout, or the refusal
/// on stderr when git is missing or the tree is not a checkout.
#[test]
fn example_real_gitreport() {
    for backend in BACKENDS {
        let output = run_with_backend(&example_path("real/gitreport.brs"), backend);

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        assert_eq!(
            output.status.code(),
            Some(0),
            "[{backend}] stderr: {stderr}"
        );

        if stdout.is_empty() {
            assert!(
                stderr == "git is not installed\n" || stderr == "not inside a git repository\n",
                "[{backend}] unexpected refusal: {stderr}"
            );
            continue;
        }

        assert!(
            stderr.is_empty(),
            "[{backend}] expected empty stderr, got: {stderr}"
        );
        assert!(
            stdout.starts_with("release report for brasa\n"),
            "[{backend}] stdout: {stdout}"
        );

        for marker in [
            "\nrange: ",
            "\ntarget tag v0.1.0: ",
            "\ncommits: ",
            "\nby type:\n",
            "\nworktree: ",
        ] {
            assert!(
                stdout.contains(marker),
                "[{backend}] missing {marker:?} in: {stdout}"
            );
        }
    }
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
