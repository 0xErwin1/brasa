//! `brasa test` at the command line (BRS-110): what it runs, what it
//! reports, and what it exits with.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

fn brasa(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_brasa"))
        .args(args)
        .env_remove("BRASA_PATH")
        .stdin(Stdio::null())
        .output()
        .expect("failed to spawn brasa")
}

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("brasa-test-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("failed to create temp dir");
    dir
}

fn write(dir: &Path, name: &str, text: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, text).expect("failed to write fixture");
    path
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// The base case: passing and failing tests in one file, one line each,
/// a non-zero exit, and the count at the end.
#[test]
fn tests_run_one_at_a_time_and_a_failure_does_not_stop_the_rest() {
    let dir = temp_dir("basic");
    let script = write(
        &dir,
        "suite.bras",
        concat!(
            "def double(x: int): int\n  x * 2\nend\n\n",
            "test \"double doubles\"\n  assertEq double(21), 42\nend\n\n",
            "test \"this one fails\"\n  assertEq double(2), 5\nend\n\n",
            "test \"and this one still runs\"\n  assert double(2) > 3\nend\n",
        ),
    );

    let output = brasa(&["test", script.to_str().expect("utf-8 path")]);

    assert_eq!(
        output.status.code(),
        Some(1),
        "a failure must exit non-zero"
    );
    let out = stdout(&output);
    assert!(out.contains("ok   double doubles"), "got: {out}");
    assert!(out.contains("FAIL this one fails"), "got: {out}");
    assert!(
        out.contains("ok   and this one still runs"),
        "one failed test must not stop the rest, got: {out}"
    );
    assert!(out.contains("2 passed, 1 failed"), "got: {out}");
}

#[test]
fn a_suite_that_all_passes_exits_zero() {
    let dir = temp_dir("green");
    let script = write(
        &dir,
        "suite.bras",
        "test \"trivially true\"\n  assert true\nend\n",
    );

    let output = brasa(&["test", script.to_str().expect("utf-8 path")]);

    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    assert!(stdout(&output).contains("1 passed, 0 failed"));
}

/// The failure has to say where it was, not just that it happened.
#[test]
fn a_failure_names_the_test_it_came_from() {
    let dir = temp_dir("named");
    let script = write(
        &dir,
        "suite.bras",
        "test \"a name worth finding\"\n  assert false\nend\n",
    );

    let output = brasa(&["test", script.to_str().expect("utf-8 path")]);

    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr(&output).contains("a name worth finding"),
        "the stack trace must name the test, got: {}",
        stderr(&output)
    );
}

/// The top level runs ONCE, before the tests, exactly as it does for a
/// program. A runner that re-ran it per test would be testing something
/// the program never does.
#[test]
fn the_top_level_runs_once_before_the_tests() {
    let dir = temp_dir("setup");
    let script = write(
        &dir,
        "suite.bras",
        concat!(
            "puts \"setup\"\n\n",
            "test \"one\"\n  assert true\nend\n\n",
            "test \"two\"\n  assert true\nend\n",
        ),
    );

    let output = brasa(&["test", script.to_str().expect("utf-8 path")]);

    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    assert_eq!(
        stdout(&output).matches("setup").count(),
        1,
        "the top level must not re-run per test"
    );
}

/// A test may use the module it is written in, including its imports —
/// that is most of the point of testing in the language rather than
/// from outside it.
#[test]
fn a_test_can_call_across_a_module_boundary() {
    let dir = temp_dir("imports");
    write(
        &dir,
        "util.bras",
        "pub def slugify(s: string): string\n  s.trim().toLower().replace(\" \", \"-\")\nend\n",
    );
    let script = write(
        &dir,
        "suite.bras",
        concat!(
            "import \"util.bras\"\n\n",
            "test \"slugify hyphenates\"\n",
            "  assertEq util.slugify(\"Hola Mundo\"), \"hola-mundo\"\n",
            "end\n",
        ),
    );

    let output = brasa(&["test", script.to_str().expect("utf-8 path")]);

    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    assert!(stdout(&output).contains("1 passed, 0 failed"));
}

/// An imported module's tests are not the entry file's. A library's
/// suite running as a side effect of importing it would make
/// `brasa test` mean something different depending on dependencies.
#[test]
fn an_imported_modules_tests_are_not_run() {
    let dir = temp_dir("imported-tests");
    write(
        &dir,
        "util.bras",
        concat!(
            "pub def one(): int\n  1\nend\n\n",
            "test \"the library's own test\"\n  assert false\nend\n",
        ),
    );
    let script = write(
        &dir,
        "suite.bras",
        concat!(
            "import \"util.bras\"\n\n",
            "test \"the entry file's test\"\n  assertEq util.one(), 1\nend\n",
        ),
    );

    let output = brasa(&["test", script.to_str().expect("utf-8 path")]);

    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    let out = stdout(&output);
    assert!(
        !out.contains("the library's own test"),
        "an imported module's tests must not run, got: {out}"
    );
    assert!(out.contains("1 passed, 0 failed"), "got: {out}");
}

#[test]
fn a_file_with_no_tests_says_so_and_exits_zero() {
    let dir = temp_dir("empty");
    let script = write(&dir, "plain.bras", "puts \"nothing to test\"\n");

    let output = brasa(&["test", script.to_str().expect("utf-8 path")]);

    assert_eq!(output.status.code(), Some(0));
    assert!(stdout(&output).contains("no tests"));
}

/// A top level that fails means no test ran, and saying so is the
/// difference between a diagnosable failure and a confusing one.
#[test]
fn a_failing_top_level_reports_that_no_test_ran() {
    let dir = temp_dir("bad-setup");
    let script = write(
        &dir,
        "suite.bras",
        concat!(
            "let boom = [1][9]\n\n",
            "test \"never reached\"\n  assert true\nend\n",
        ),
    );

    let output = brasa(&["test", script.to_str().expect("utf-8 path")]);

    assert_eq!(output.status.code(), Some(70));
    assert!(
        stderr(&output).contains("no test ran"),
        "got: {}",
        stderr(&output)
    );
}

/// `assert`/`assertEq` are prelude functions, not test-only syntax: an
/// assertion is useful in a script too, and making them test-only would
/// be a second vocabulary for one idea.
#[test]
fn assertions_work_outside_a_test_too() {
    let dir = temp_dir("outside");
    let script = write(&dir, "plain.bras", "assert 1 < 2\nputs \"held\"\n");

    let output = brasa(&[script.to_str().expect("utf-8 path")]);

    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    assert_eq!(stdout(&output), "held\n");
}

#[test]
fn a_failing_assertion_outside_a_test_is_an_ordinary_panic() {
    let dir = temp_dir("outside-fail");
    let script = write(&dir, "plain.bras", "assert 2 < 1\nputs \"unreachable\"\n");

    let output = brasa(&[script.to_str().expect("utf-8 path")]);

    assert_eq!(output.status.code(), Some(70));
    assert!(
        stderr(&output).contains("panics.AssertionFailed"),
        "got: {}",
        stderr(&output)
    );
    assert!(stdout(&output).is_empty(), "nothing after the failure");
}

/// `assertEq` compares two values of the SAME type, the rule `==`
/// follows, because that is the comparison it performs.
#[test]
fn assert_eq_rejects_mismatched_types_at_compile_time() {
    let dir = temp_dir("mismatch");
    let script = write(&dir, "plain.bras", "assertEq 1, \"one\"\n");

    let output = brasa(&["--check", script.to_str().expect("utf-8 path")]);

    assert_eq!(output.status.code(), Some(65));
    assert!(stderr(&output).contains("T001"), "got: {}", stderr(&output));
}

#[test]
fn assert_rejects_a_non_bool_at_compile_time() {
    let dir = temp_dir("nonbool");
    let script = write(&dir, "plain.bras", "assert 1\n");

    let output = brasa(&["--check", script.to_str().expect("utf-8 path")]);

    assert_eq!(output.status.code(), Some(65));
    assert!(stderr(&output).contains("T001"), "got: {}", stderr(&output));
}

// --- `std::cli` at the command line (BRS-112) ------------------------

/// The bug `std::cli` exposed: without it the interpreter claimed the
/// script's own flags and a script could never see one.
#[test]
fn a_script_receives_its_own_flags() {
    let dir = temp_dir("script-flags");
    let script = write(
        &dir,
        "tool.bras",
        concat!(
            "import std::cli\nimport std::env\n\n",
            "let spec = [[\"option\", \"top\", \"t\", \"rows\"], [\"flag\", \"help\", \"h\", \"usage\"]]\n\n",
            "def main()\n",
            "  let args = cli.parse(env.args(), spec) catch! (e)\n",
            "    cli.UsageError => cli.parse([], spec)\n",
            "  end\n",
            "  puts args.option(\"top\") ?? \"none\"\n",
            "  puts args.flag(\"help\")\n",
            "end\n",
        ),
    );

    let output = brasa(&[script.to_str().expect("utf-8 path"), "--top", "9", "--help"]);

    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    assert_eq!(
        stdout(&output),
        "9\ntrue\n",
        "the interpreter must not claim `--top` or `--help`"
    );
}

/// `brasa --help` still belongs to the interpreter: the split is at the
/// script path, so a flag BEFORE it is ours.
#[test]
fn the_interpreters_own_help_still_works() {
    let output = brasa(&["--help"]);

    assert_eq!(output.status.code(), Some(0));
    assert!(
        stdout(&output).contains("The Brasa programming language"),
        "got: {}",
        stdout(&output)
    );
}

#[test]
fn a_subcommand_keeps_its_own_flags() {
    let output = brasa(&["fmt", "--help"]);

    assert_eq!(output.status.code(), Some(0));
    assert!(
        stdout(&output).contains("Format Brasa source files"),
        "got: {}",
        stdout(&output)
    );
}
