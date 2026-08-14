//! `brasa debug` (BRS-118), driven the way an agent and CI drive it.
//!
//! The point of this command existing before any UI is that it can be
//! regression-tested at all, so these run the real binary against real
//! scripts rather than calling into the substrate.

use std::path::{Path, PathBuf};
use std::process::Command;

fn brasa() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_brasa"))
}

fn scratch(name: &str, source: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("brasa-debug-cli");
    std::fs::create_dir_all(&dir).expect("the scratch directory is writable");

    let path = dir.join(name);
    std::fs::write(&path, source).expect("the fixture is writable");
    path
}

struct Run {
    stdout: String,
    stderr: String,
    code: i32,
}

fn debug(script: &Path, args: &[&str]) -> Run {
    let output = Command::new(brasa())
        .arg("debug")
        .arg(script)
        .args(args)
        .output()
        .expect("the binary runs");

    Run {
        stdout: String::from_utf8(output.stdout).expect("utf-8"),
        stderr: String::from_utf8(output.stderr).expect("utf-8"),
        code: output.status.code().expect("an exit code"),
    }
}

const COUNTER: &str = r#"struct Point
  x: int
  y: int
end

def bump(n: int): int
  let doubled = n * 2
  doubled + 1
end

def main()
  let p = Point { x: 3, y: 4 }
  let a = bump(20)
  let b = bump(a)
  puts b + p.x
end
"#;

/// The basic contract: stop where asked, name the function, exit 0.
#[test]
fn a_breakpoint_stops_and_reports_the_frames() {
    let script = scratch("counter.bras", COUNTER);
    let at = format!("{}:7", script.display());

    let run = debug(&script, &["--break", &at]);

    assert_eq!(run.code, 0, "stderr: {}", run.stderr);
    assert!(
        run.stdout.contains("stopped in `bump`"),
        "got: {}",
        run.stdout
    );
    assert!(run.stdout.contains("main at"), "the caller is on the stack");
}

/// `--json` is the agent's contract, so its shape is pinned: the same
/// facts as the plain form, as data.
#[test]
fn json_reports_the_same_stop_as_data() {
    let script = scratch("counter_json.bras", COUNTER);
    let at = format!("{}:7", script.display());

    let run = debug(&script, &["--break", &at, "--dump", "locals", "--json"]);
    assert_eq!(run.code, 0, "stderr: {}", run.stderr);

    let value: serde_json::Value = serde_json::from_str(&run.stdout).expect("valid JSON");

    assert_eq!(value["stopped"], serde_json::json!(true));
    assert_eq!(value["function"], serde_json::json!("bump"));
    assert_eq!(
        value["locals"][0]["value"],
        serde_json::json!("20"),
        "`main` called `bump(20)`"
    );
}

/// `--continue N` resumes past the first hit, which is the whole reason
/// a breakpoint inside a repeatedly-called function is useful.
#[test]
fn continue_reaches_a_later_hit() {
    let script = scratch("counter_continue.bras", COUNTER);
    let at = format!("{}:7", script.display());

    let first = debug(&script, &["--break", &at, "--dump", "locals", "--json"]);
    let second = debug(
        &script,
        &[
            "--break",
            &at,
            "--continue",
            "1",
            "--dump",
            "locals",
            "--json",
        ],
    );

    let first: serde_json::Value = serde_json::from_str(&first.stdout).expect("valid JSON");
    let second: serde_json::Value = serde_json::from_str(&second.stdout).expect("valid JSON");

    assert_eq!(first["locals"][0]["value"], serde_json::json!("20"));
    assert_eq!(
        second["locals"][0]["value"],
        serde_json::json!("41"),
        "the first call returned 20 * 2 + 1"
    );
}

/// A value reads one level deep, so a struct's fields are there without
/// a second command.
#[test]
fn locals_include_one_level_of_children() {
    let script = scratch("counter_children.bras", COUNTER);
    let at = format!("{}:15", script.display());

    let run = debug(&script, &["--break", &at, "--dump", "locals", "--json"]);
    let value: serde_json::Value = serde_json::from_str(&run.stdout).expect("valid JSON");

    let point = value["locals"]
        .as_array()
        .expect("locals is an array")
        .iter()
        .find(|local| local["value"] == serde_json::json!("Point"))
        .expect("`p` is a Point");

    assert_eq!(point["children"][0]["name"], serde_json::json!("x"));
    assert_eq!(point["children"][0]["value"], serde_json::json!("3"));
}

const NEVER: &str = r#"def unused(n: int): int
  n + 1
end

def main()
  puts 1
end
"#;

/// A breakpoint that never fires is its own outcome, not a clean run.
/// A caller that got 0 here would read "the program did what I asked".
#[test]
fn a_breakpoint_that_never_fires_has_its_own_exit_code() {
    let script = scratch("never.bras", NEVER);
    let at = format!("{}:2", script.display());

    let run = debug(&script, &["--break", &at]);

    assert_eq!(run.code, 3, "stdout: {}", run.stdout);
    assert!(run.stdout.contains("no breakpoint was hit"));
    assert!(
        run.stdout.contains('1'),
        "the program still ran to completion"
    );
}

/// And it says so in JSON too, so an agent does not have to parse prose
/// to tell the two apart.
#[test]
fn the_never_hit_case_is_visible_in_json() {
    let script = scratch("never_json.bras", NEVER);
    let at = format!("{}:2", script.display());

    let run = debug(&script, &["--break", &at, "--json"]);
    assert_eq!(run.code, 3);

    let body = run
        .stdout
        .split_once('{')
        .map(|(_, rest)| format!("{{{rest}"))
        .expect("the JSON object follows the program's own output");
    let value: serde_json::Value = serde_json::from_str(&body).expect("valid JSON");

    assert_eq!(value["stopped"], serde_json::json!(false));
    assert_eq!(value["at"], serde_json::Value::Null);
}

/// A position that names no loaded file is a usage error, reported
/// before anything runs. Silently never stopping would blame the
/// program for a typo in the command.
#[test]
fn an_unknown_file_is_a_usage_error() {
    let script = scratch("usage.bras", NEVER);

    let run = debug(&script, &["--break", "nowhere.bras:2"]);

    assert_eq!(run.code, 64);
    assert!(
        run.stderr.contains("is not a file this program loads"),
        "stderr: {}",
        run.stderr
    );
}

/// A line past the end of the file is the same class of mistake.
#[test]
fn a_line_out_of_range_is_a_usage_error() {
    let script = scratch("usage_line.bras", NEVER);
    let at = format!("{}:999", script.display());

    let run = debug(&script, &["--break", &at]);

    assert_eq!(run.code, 64);
    assert!(
        run.stderr.contains("has no line 999"),
        "stderr: {}",
        run.stderr
    );
}

/// With no breakpoints at all the script simply runs, which keeps
/// `brasa debug` usable as a plain runner in a script that sets
/// breakpoints conditionally.
#[test]
fn no_breakpoints_runs_the_program() {
    let script = scratch("plain.bras", NEVER);

    let run = debug(&script, &[]);

    assert_eq!(run.code, 3, "nothing was asked, so nothing was hit");
    assert!(run.stdout.contains('1'));
}
