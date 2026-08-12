//! `brasa fmt` at the command line (BRS-91): what it writes, what it
//! only reports, and what it exits with.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

fn brasa(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_brasa"))
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("failed to spawn brasa")
}

/// A fresh directory per test, named after the test so a failure leaves
/// something identifiable behind.
fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("brasa-fmt-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("failed to create temp dir");
    dir
}

fn write(path: &Path, text: &str) {
    std::fs::write(path, text).expect("failed to write fixture");
}

#[test]
fn formats_a_file_in_place() {
    let dir = temp_dir("in-place");
    let script = dir.join("messy.bras");
    write(&script, "def f(a:int):int\n a+1\nend\n");

    let output = brasa(&["fmt", script.to_str().expect("utf-8 path")]);

    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        std::fs::read_to_string(&script).expect("readable"),
        "def f(a: int): int\n  a + 1\nend\n"
    );
}

#[test]
fn check_reports_without_writing_and_exits_one() {
    let dir = temp_dir("check");
    let script = dir.join("messy.bras");
    let original = "let  x =  1\n";
    write(&script, original);

    let output = brasa(&["fmt", "--check", script.to_str().expect("utf-8 path")]);

    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        std::fs::read_to_string(&script).expect("readable"),
        original
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("messy.bras"),
        "the unformatted file must be named on stdout"
    );
}

#[test]
fn check_exits_zero_when_everything_is_formatted() {
    let dir = temp_dir("check-clean");
    write(&dir.join("clean.bras"), "let x = 1\n");

    let output = brasa(&["fmt", "--check", dir.to_str().expect("utf-8 path")]);

    assert_eq!(output.status.code(), Some(0));
}

#[test]
fn a_directory_is_walked_for_bras_files() {
    let dir = temp_dir("walk");
    std::fs::create_dir_all(dir.join("sub")).expect("failed to create subdirectory");
    write(&dir.join("sub").join("nested.bras"), "let  y =  2\n");
    write(&dir.join("ignored.txt"), "let  y =  2\n");

    let output = brasa(&["fmt", dir.to_str().expect("utf-8 path")]);

    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        std::fs::read_to_string(dir.join("sub").join("nested.bras")).expect("readable"),
        "let y = 2\n"
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("ignored.txt")).expect("readable"),
        "let  y =  2\n",
        "a walked directory only yields .bras files"
    );
}

#[test]
fn stdout_leaves_the_file_alone() {
    let dir = temp_dir("stdout");
    let script = dir.join("messy.bras");
    let original = "let  x =  1\n";
    write(&script, original);

    let output = brasa(&["fmt", "--stdout", script.to_str().expect("utf-8 path")]);

    assert_eq!(output.status.code(), Some(0));
    assert_eq!(String::from_utf8_lossy(&output.stdout), "let x = 1\n");
    assert_eq!(
        std::fs::read_to_string(&script).expect("readable"),
        original
    );
}

#[test]
fn a_file_that_does_not_parse_is_refused_and_left_alone() {
    let dir = temp_dir("broken");
    let script = dir.join("broken.bras");
    let original = "def f(\n";
    write(&script, original);

    let output = brasa(&["fmt", script.to_str().expect("utf-8 path")]);

    assert_eq!(output.status.code(), Some(65));
    assert_eq!(
        std::fs::read_to_string(&script).expect("readable"),
        original
    );
    assert!(
        !String::from_utf8_lossy(&output.stderr).is_empty(),
        "the parse diagnostics must be rendered"
    );
}

#[test]
fn running_a_script_still_works_alongside_the_subcommand() {
    let dir = temp_dir("run");
    let script = dir.join("hello.bras");
    write(&script, "puts \"hi\"\n");

    let output = brasa(&[script.to_str().expect("utf-8 path")]);

    assert_eq!(output.status.code(), Some(0));
    assert_eq!(String::from_utf8_lossy(&output.stdout), "hi\n");
}

#[test]
fn no_script_and_no_subcommand_is_a_usage_error() {
    let output = brasa(&[]);

    assert_eq!(output.status.code(), Some(64));
}
