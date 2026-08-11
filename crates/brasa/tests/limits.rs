//! Bytecode-limit diagnostics (`docs/spec/06-diagnostics.md`, the `C`
//! family) driven through the CLI on both backends.
//!
//! Each program here breaks a limit that is inherent to the instruction
//! set in `docs/spec/07-bytecode.md`. What is pinned is the diagnostic:
//! before these limits were checked, the same programs aborted the
//! process with a raw Rust panic (`TryFromIntError`) instead. Both
//! backends must report the same thing, because the limits belong to the
//! program, not to an execution strategy.
//!
//! The programs are generated here rather than committed: the smallest
//! of them is a few hundred kilobytes of source.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BACKENDS: &[&str] = &["walker", "vm"];

fn run_backend(script: &Path, backend: &str) -> Output {
    Command::new(env!("CARGO_BIN_EXE_brasa"))
        .arg(format!("--backend={backend}"))
        .arg(script)
        .output()
        .expect("failed to run brasa")
}

fn write_script(name: &str, source: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("brasa-limits-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("failed to create temp dir");

    let script = dir.join(format!("{name}.brs"));
    std::fs::write(&script, source).expect("failed to write script");
    script
}

/// Runs `source` on both backends and asserts each one rejected it with
/// the same exit code and the same diagnostic codes and messages.
fn assert_rejected_by_both(name: &str, source: &str, expected: &[&str]) {
    let script = write_script(name, source);

    let mut seen: Option<String> = None;
    for backend in BACKENDS {
        let output = run_backend(&script, backend);
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();

        assert_eq!(
            output.status.code(),
            Some(65),
            "[{backend}] expected a compile-time rejection, got stderr: {stderr}"
        );
        for fragment in expected {
            assert!(
                stderr.contains(fragment),
                "[{backend}] expected {fragment:?} in stderr, got: {stderr}"
            );
        }

        match &seen {
            None => seen = Some(stderr),
            Some(first) => assert_eq!(first, &stderr, "backends disagree on the diagnostics"),
        }
    }

    let _ = std::fs::remove_file(&script);
}

#[test]
fn too_many_arguments_and_parameters_are_reported() {
    let params: Vec<String> = (0..300).map(|i| format!("a{i}: int")).collect();
    let args = vec!["1"; 300].join(", ");
    let source = format!(
        "def wide({}): int\n  a0\nend\n\nputs wide({args})\n",
        params.join(", ")
    );

    assert_rejected_by_both(
        "wide_call",
        &source,
        &[
            "[C001]",
            "call takes 300 arguments, but the limit is 255",
            "[C002]",
            "`wide` takes 300 parameters, but the limit is 255",
        ],
    );
}

#[test]
fn too_many_vector_elements_are_reported() {
    let source = format!("let v = [{}]\nputs v.len()\n", vec!["1"; 65_536].join(", "));

    assert_rejected_by_both(
        "wide_vector",
        &source,
        &[
            "[C003]",
            "vector literal has 65536 elements, but the limit is 65535",
        ],
    );
}

#[test]
fn too_many_map_entries_are_reported() {
    let entries: Vec<String> = (0..65_536).map(|i| format!("{i}: 1")).collect();
    let source = format!("let m = {{{}}}\nputs m.len()\n", entries.join(", "));

    assert_rejected_by_both(
        "wide_map",
        &source,
        &[
            "[C003]",
            "map literal has 65536 entries, but the limit is 65535",
        ],
    );
}

#[test]
fn too_many_tuple_elements_are_reported() {
    let source = format!("let t = ({},)\nputs t\n", vec!["1"; 65_536].join(", "));

    assert_rejected_by_both(
        "wide_tuple",
        &source,
        &["[C003]", "tuple has 65536 elements, but the limit is 65535"],
    );
}

#[test]
fn too_many_struct_fields_are_reported() {
    let fields: Vec<String> = (0..65_536).map(|i| format!("  f{i}: int")).collect();
    let source = format!("struct Wide\n{}\nend\n\nputs 1\n", fields.join("\n"));

    assert_rejected_by_both(
        "wide_struct",
        &source,
        &[
            "[C004]",
            "struct `Wide` has 65536 fields, but the limit is 65535",
        ],
    );
}

#[test]
fn too_many_local_slots_are_reported() {
    let lets: Vec<String> = (0..65_536).map(|i| format!("  let a{i} = {i}")).collect();
    let source = format!("def f(): int\n{}\n  1\nend\n\nputs f()\n", lets.join("\n"));

    assert_rejected_by_both(
        "many_locals",
        &source,
        &["[C005]", "`f` needs more than 65535 local slots"],
    );
}

/// No single literal or call limit bounds the operand stack: a value
/// already on the stack plus a legal-sized literal beside it can need
/// more slots than a frame can reserve.
#[test]
fn an_over_deep_operand_stack_is_reported() {
    let source = format!("let t = (1, [{}])\nputs t\n", vec!["1"; 65_535].join(", "));

    assert_rejected_by_both(
        "deep_stack",
        &source,
        &[
            "[C006]",
            "needs 65536 operand-stack slots, but the limit is 65535",
        ],
    );
}

/// The limits are exact, not approximate: the largest program that fits
/// must still compile and run on both backends.
#[test]
fn the_largest_fitting_call_still_runs() {
    let params: Vec<String> = (0..255).map(|i| format!("a{i}: int")).collect();
    let args = vec!["1"; 255].join(", ");
    let source = format!(
        "def wide({}): int\n  a0\nend\n\nputs wide({args})\n",
        params.join(", ")
    );
    let script = write_script("fitting_call", &source);

    for backend in BACKENDS {
        let output = run_backend(&script, backend);
        let stderr = String::from_utf8_lossy(&output.stderr);

        assert_eq!(
            output.status.code(),
            Some(0),
            "[{backend}] stderr: {stderr}"
        );
        assert_eq!(String::from_utf8_lossy(&output.stdout), "1\n");
    }

    let _ = std::fs::remove_file(&script);
}
