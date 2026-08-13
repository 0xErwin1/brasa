//! Golden `.bras` program suite, run through the CLI.
//!
//! Success programs under `tests/programs/` pin their exact stdout in a
//! sibling `.out` file; failure programs assert the exit code and
//! stderr substrings. The runnable repository examples are pinned here
//! too, so a semantic regression in any layer shows up as a diff.

use std::path::PathBuf;
use std::process::{Command, Output};

#[path = "support/example_walk.rs"]
mod example_walk;

fn run_program(path: &PathBuf) -> Output {
    run_program_args(path, &[])
}

/// Runs a program with trailing script arguments (what `env.args()`
/// sees).
fn run_program_args(path: &PathBuf, args: &[PathBuf]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_brasa"))
        .arg(path)
        .args(args)
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
/// the sibling `.out` file; it must succeed with empty stderr.
fn assert_golden(name: &str) {
    let expected = std::fs::read_to_string(program_path(&format!("{name}.out")))
        .expect("missing expected-output file");

    let output = run_program(&program_path(&format!("{name}.bras")));

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(0), "stderr: {stderr}");
    assert!(stderr.is_empty(), "expected empty stderr, got: {stderr}");
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        expected,
        "stdout mismatch"
    );
}

/// Runs an example and compares its stdout against the expectation
/// pinned inline; it must succeed with empty stderr.
fn assert_example(name: &str, expected: &str) {
    assert_example_args(name, &[], expected);
}

/// Runs an example with trailing script arguments and compares its
/// stdout against the expectation pinned inline.
fn assert_example_args(name: &str, args: &[PathBuf], expected: &str) {
    let output = run_program_args(&example_path(name), args);

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(0), "stderr: {stderr}");
    assert!(stderr.is_empty(), "expected empty stderr, got: {stderr}");
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        expected,
        "stdout mismatch"
    );
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
fn golden_generics_interfaces() {
    assert_golden("generics_interfaces");
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
    let output = run_program(&program_path("throw_uncaught.bras"));

    assert_eq!(output.status.code(), Some(70));
    assert_eq!(String::from_utf8_lossy(&output.stdout), "before\n");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("error:"), "stderr: {stderr}");
    assert!(stderr.contains("BoomError"), "stderr: {stderr}");
}

/// The defect this closes: a CLI-shaped script had no way to signal
/// failure without the runtime printing an error banner and choosing
/// 70 for it. The status has to reach the shell, stdout written before
/// the exit has to arrive, and stderr must carry only what the script
/// itself wrote.
#[test]
fn env_exit_sets_the_status_without_a_runtime_banner() {
    let output = run_program(&program_path("exit_status.bras"));

    assert_eq!(output.status.code(), Some(4));
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "checked 3 things\n"
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "usage: exit_status <path>\n",
        "the runtime added something of its own"
    );
}

#[test]
fn uncaught_panic_exits_70_with_type_and_call_chain() {
    let output = run_program(&program_path("panic_uncaught.bras"));

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
        .arg(example_path("hello.bras"))
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
        .arg(program_path("basics.bras"))
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
    assert_example("hello.bras", "hello, world!\n1 + 1 = 2\n");
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
    assert_example("fib.bras", expected);
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
    assert_example("fizzbuzz.bras", expected);
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
    assert_example("pipeline.bras", expected);
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
    assert_example("strings.bras", expected);
}

#[test]
fn example_errors() {
    let expected = "\
recovered: timeout
parsed: 42
out of range gives 0
";
    assert_example("errors.bras", expected);
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
        "real/logstat.bras",
        &[example_path("real/data/access.log")],
        expected,
    );
}

/// Runs against a committed fixture rather than this repository's own
/// `flake.lock`: the real lock moves with every `nix flake update`, and
/// the fixture is built to hit the branches it never did (an unpinned
/// input, two inputs on one revision, a non-github/gitlab origin, a
/// `follows` edge, and a transitively-resolved node so the direct-input
/// count differs from the node count).
#[test]
fn example_real_lockaudit() {
    let expected = "\
flake.lock  (lock version 7, 4 direct inputs, 5 locked nodes, 1 follows edge)
  devtools  b7c8d9e  2025-06-10  git:https://git.example.org/infra/devtools.git
  helpers   a1b2c3d  2025-01-01  github:NixOS/nixpkgs
  nixpkgs   a1b2c3d  2025-01-01  github:NixOS/nixpkgs
  vendor    c3d4e5f  2025-07-29  gitlab:brasa-lang/vendor-pins
  unpinned inputs: localsrc
  duplicated revisions:
    a1b2c3d  helpers nixpkgs
";
    assert_example_args(
        "real/lockaudit.bras",
        &[example_path("real/data/lockfixture")],
        expected,
    );
}

/// `gitreport.bras` reports on the live repository, so its counts move
/// with every commit and cannot be pinned. What IS deterministic is the
/// shape: the script always exits 0 and produces exactly one of two
/// well-formed outputs — the report skeleton on stdout, or the refusal
/// on stderr when git is missing or the tree is not a checkout.
///
/// The report header names the checkout's directory, which a clean
/// clone is free to choose, so the expectation is derived from this
/// test's own location rather than pinned to one name.
#[test]
fn example_real_gitreport() {
    let checkout = std::fs::canonicalize(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."))
        .expect("the workspace root canonicalizes");
    let expected_header = format!(
        "release report for {}\n",
        checkout
            .file_name()
            .expect("the workspace root has a name")
            .to_string_lossy()
    );

    let output = run_program(&example_path("real/gitreport.bras"));

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert_eq!(output.status.code(), Some(0), "stderr: {stderr}");

    if stdout.is_empty() {
        assert!(
            stderr == "git is not installed\n" || stderr == "not inside a git repository\n",
            "unexpected refusal: {stderr}"
        );
        return;
    }

    assert!(stderr.is_empty(), "expected empty stderr, got: {stderr}");
    assert!(
        stdout.starts_with(&expected_header),
        "expected the header {expected_header:?} in: {stdout}"
    );

    for marker in [
        "\nrange: ",
        "\ntarget tag v0.1.0: ",
        "\ncommits: ",
        "\nby type:\n",
        "\nworktree: ",
    ] {
        assert!(stdout.contains(marker), "missing {marker:?} in: {stdout}");
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
    assert_example("shapes.bras", expected);
}

/// Pinned whole, `wc` line included — the trailing path too, since this
/// test is the one that chose it — with no accommodation for an
/// environment that lacks `wc`.
///
/// An earlier revision tolerated a missing `wc` the way
/// `example_real_gitreport` tolerates a missing `git`. That was wrong
/// twice over. It bought nothing: `crates/brasa_vm/tests/parity.rs`
/// already spawns `/bin/sh` and `cat` unconditionally
/// (`proc_run_captures_stdout_stderr_and_code`,
/// `proc_stdin_round_trips_through_cat`), so a checkout without
/// coreutils fails the suite well before reaching this test. And it
/// could not report itself: a skip announced from a passing test goes
/// into libtest's capture buffer and is discarded unless the run asked
/// for `--nocapture`, so the operator would have seen green and learned
/// nothing. `git` is genuinely optional for a language test suite;
/// `wc` is not.
#[test]
fn example_stars() {
    let expected_repos = "\
brasa: 1284
brasa-vscode: 61
unknown: 412
toolbelt: 337
4 popular repos
";

    let fixture = example_path("data/repos.json");
    let expected = format!("{expected_repos}12 lines read from {}\n", fixture.display());
    let output = run_program_args(&example_path("stars.bras"), std::slice::from_ref(&fixture));

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert_eq!(output.status.code(), Some(0), "stderr: {stderr}");
    assert!(stderr.is_empty(), "expected empty stderr, got: {stderr}");
    assert_eq!(stdout, expected, "stdout mismatch");
}

/// The container lookup is total too, and nothing else reaches it: the
/// happy fixture always has a `repos` array, and every pinned failure
/// aborts inside `fs.read` or `json.parse` before the loop header is
/// evaluated.
///
/// So what this covers, and `example_stars` does not, is the CONTAINER,
/// whose `None` has two sources: an absent `repos` key, and a `repos`
/// that is present but not an array. Both are driven, each against its
/// own small fixture rather than a document borrowed from another test
/// — a borrowed one being renamed would fail this as `stars.bras`
/// exiting 70, which reads as a stars regression.
///
/// The field-level fallbacks are already pinned by `example_stars`,
/// whose fixture has an entry with no `name`, one with no `archived`,
/// and one whose `stars` is a string.
#[test]
fn example_stars_reads_a_document_without_repos() {
    let fixture = example_path("data/no-repos.json");
    let output = run_program_args(&example_path("stars.bras"), std::slice::from_ref(&fixture));

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(0), "stderr: {stderr}");
    assert!(stderr.is_empty(), "expected empty stderr, got: {stderr}");
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        format!("0 popular repos\n3 lines read from {}\n", fixture.display()),
        "stdout mismatch"
    );

    // The other half of the container's totality: `repos` present, but
    // not an array. `asArray()` yields `None` for a wrong-kinded node
    // exactly as it does for an absent key, which is what the example's
    // own comment claims and what this fixture is for.
    let fixture = example_path("data/repos-scalar.json");
    let output = run_program_args(&example_path("stars.bras"), std::slice::from_ref(&fixture));

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(0), "stderr: {stderr}");
    assert!(stderr.is_empty(), "expected empty stderr, got: {stderr}");
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        format!("0 popular repos\n3 lines read from {}\n", fixture.display()),
        "stdout mismatch"
    );
}

/// The example refuses without an argument rather than defaulting to
/// some path in the working directory, and it lets a bad path or a
/// non-JSON file propagate rather than reporting an empty dataset.
/// Those are the paths a reader hits first, so they are pinned
/// alongside the happy one.
#[test]
fn example_stars_reports_bad_input() {
    let no_args = run_program(&example_path("stars.bras"));
    assert_eq!(no_args.status.code(), Some(2), "expected a refusal");
    assert_eq!(
        String::from_utf8_lossy(&no_args.stderr),
        "usage: stars.bras <repos.json>\n",
        "unexpected refusal"
    );
    assert!(no_args.stdout.is_empty(), "expected no stdout");

    let missing = run_program_args(
        &example_path("stars.bras"),
        std::slice::from_ref(&example_path("data/does-not-exist.json")),
    );
    assert_eq!(missing.status.code(), Some(70), "expected a failure");
    assert!(
        missing.stdout.is_empty(),
        "expected nothing before the abort"
    );
    let stderr = String::from_utf8_lossy(&missing.stderr);
    assert!(stderr.starts_with("error: fs.NotFound: "), "got: {stderr}");

    // A file that certainly exists and certainly is not JSON, so no
    // fixture has to exist only in order to be broken. Deliberately
    // not another `.bras`: `every_example_is_pinned` counts any
    // quoted example name in this file, so naming one as an INPUT
    // here would stand in for pinning it.
    let not_json = run_program_args(
        &example_path("stars.bras"),
        std::slice::from_ref(&example_path("README.md")),
    );
    assert_eq!(not_json.status.code(), Some(70), "expected a failure");
    assert!(
        not_json.stdout.is_empty(),
        "expected nothing before the abort"
    );
    let stderr = String::from_utf8_lossy(&not_json.stderr);
    assert!(
        stderr.starts_with("error: json.ParseError: "),
        "got: {stderr}"
    );
}

/// A module: it declares functions and runs nothing, so the pin is that
/// it loads clean and stays silent.
#[test]
fn example_modules_utils() {
    assert_example("modules/utils.bras", "");
}

/// The whole of BRS-97 in one example: the importer's top-level `let`
/// calls across the module boundary before `main` runs, `main` reads a
/// second exported function, and `utils`' own private helper is reached
/// only from inside `utils`.
#[test]
fn example_modules_main() {
    assert_example(
        "modules/main.bras",
        ">> BRASA MODULES <<\nhola-mundo-brasa\n",
    );
}

/// Every `.bras` under `examples/` must be exercised by a test in this
/// file.
///
/// The set of exercised examples is read out of this file's own CODE
/// rather than restated in a list. A list is satisfied by adding a name
/// to it, which is exactly what someone staring at a red test does;
/// here the name has to appear in code, so the cheapest way to satisfy
/// the guard is to write the test. It is not airtight — dead code
/// naming the file would pass — but the cheap routes are closed.
///
/// This is the finding, not the fixture: `stars.bras` stopped compiling
/// when the stdlib it previewed landed for real, three separate work
/// units reported it independently, and CI never noticed because
/// nothing ran it. An unpinned example rots silently and is read as
/// working code while it does.
#[test]
fn every_example_is_pinned() {
    let found = example_walk::collect_examples(&example_path(""));
    let code = uncommented_source();
    // The walk finding nothing would make every filter below vacuous,
    // which is the blind spot this guard exists to close, restored.
    assert!(
        !found.is_empty(),
        "the walk found no examples at all; the guard would pass vacuously"
    );

    let unpinned: Vec<&String> = found
        .iter()
        .filter(|name| !code.contains(&format!("\"{name}\"")))
        .collect();

    assert!(
        unpinned.is_empty(),
        "these examples are not exercised by any test in this file: {unpinned:?}\n\
         Either add a test for each, or — if one is a scratch file that \
         wandered into examples/ — move it out. This walks the working \
         directory, so untracked files count."
    );
}

/// This file's own source with both comment forms removed, so naming an
/// example in a comment cannot stand in for exercising it.
///
/// Both forms means both: a block, and a line comment wherever it
/// starts — a trailing `// pin "orphan.bras" later` is the cheapest
/// route of all, and dropping only whole comment lines would leave it
/// open.
fn uncommented_source() -> String {
    let source = include_str!("programs.rs");

    // `strip_comments` does not lex raw strings, and an odd number of
    // quote bytes inside one would leave it stuck mid-literal for the
    // rest of the file — every comment after that point emitted as
    // code, and the guard quietly satisfiable by a name written in one.
    // Refusing is the loud version of that, and the cheap one: this
    // file has never needed a raw string.
    //
    strip_comments(source)
}

/// Rust source with its comments removed and its string literals kept.
///
/// One pass, because the two forms nest inside each other and inside
/// literals, and a pass per form gets each of those wrong: strip blocks
/// first and a `/*` written inside a line comment opens a real one;
/// strip lines first and a `//` written inside a block truncates the
/// line that would have closed it. Tracking literals matters for the
/// same reason — a delimiter inside `"..."` is text, and the literals
/// are exactly what the caller searches.
///
/// Block comments nest in Rust, so the scan counts depth. Delimiters
/// are assembled from halves: spelled out they would appear in this
/// file, which this function is used to scan.
fn strip_comments(source: &str) -> String {
    // Byte-level throughout: every delimiter is ASCII, and slicing a
    // `&str` by a byte index lands inside a multi-byte character the
    // moment a comment holds one.
    let (open_seq, close_seq) = (concat!("/", "*").as_bytes(), concat!("*", "/").as_bytes());
    let bytes = source.as_bytes();

    let mut code: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut depth = 0usize;
    let mut in_string = false;
    let mut ix = 0usize;

    while ix < bytes.len() {
        let rest = &bytes[ix..];

        if depth > 0 {
            if rest.starts_with(open_seq) {
                depth += 1;
                ix += 2;
            } else if rest.starts_with(close_seq) {
                depth -= 1;
                ix += 2;
            } else {
                ix += 1;
            }
            continue;
        }

        if in_string {
            code.push(bytes[ix]);
            match bytes[ix] {
                b'\\' if ix + 1 < bytes.len() => {
                    code.push(bytes[ix + 1]);
                    ix += 2;
                }
                b'"' => {
                    in_string = false;
                    ix += 1;
                }
                _ => ix += 1,
            }
            continue;
        }

        if rest.starts_with(open_seq) {
            depth = 1;
            ix += 2;
        } else if rest.starts_with(b"//") {
            ix += rest.iter().position(|&b| b == b'\n').unwrap_or(rest.len());
        } else if starts_a_raw_string(rest, ix.checked_sub(1).map(|at| &bytes[at])) {
            // Only reachable at a code position, so this is a real raw
            // string and not a quote inside one. `strip_comments` does
            // not lex them, and an odd number of quote bytes inside one
            // would leave it stuck mid-literal for the rest of the
            // file: every comment after that emitted as code, and the
            // guard quietly satisfiable by a name written in one.
            panic!(
                "this file gained a raw string literal, which `strip_comments` does not \
                 lex; teach it that prefix before adding one"
            );
        } else if let Some(len) = char_literal_len(rest) {
            // A char literal can hold a quote (`b'\"'` appears twice in
            // this very function), and treating that quote as a string
            // opener inverts the parity for everything after it.
            code.extend_from_slice(&rest[..len]);
            ix += len;
        } else {
            if bytes[ix] == b'"' {
                in_string = true;
            }
            code.push(bytes[ix]);
            ix += 1;
        }
    }

    String::from_utf8(code).expect("removing whole comments leaves valid UTF-8")
}

/// Whether `rest` opens a raw string, given the byte emitted before it.
///
/// All six spellings — bare, byte (`br`) and C (`cr`), each with and
/// without hashes. The `b` and `c` are identifier characters, so a rule
/// that only refuses a non-identifier byte before the `r` would admit
/// exactly the prefixes it is there to catch. `prev` is what separates a prefix from the tail of an
/// identifier, and it comes from the caller rather than from a scan
/// because only the caller knows it is looking at code — asked inside a
/// string, this would fire on the literal `"r"`.
fn starts_a_raw_string(rest: &[u8], prev: Option<&u8>) -> bool {
    // Every raw prefix Rust has: an optional `b` or `c`, then `r`.
    let r = match (rest.first(), rest.get(1)) {
        (Some(b'b' | b'c'), Some(b'r')) => 1,
        (Some(b'r'), _) => 0,
        _ => return false,
    };

    // The hashes must lead somewhere: `r#type` is a raw IDENTIFIER,
    // which holds no quote and which `strip_comments` reads correctly.
    let hashes = rest[r + 1..].iter().take_while(|&&b| b == b'#').count();
    let opens = rest.get(r + 1 + hashes) == Some(&b'"');
    let token_start = prev.is_none_or(|b| !(b.is_ascii_alphanumeric() || *b == b'_'));

    opens && token_start
}

/// `starts_a_raw_string` is what keeps `strip_comments` from being
/// handed a literal it cannot lex, and it has been wrong twice: a rule
/// that only refused a non-identifier byte before the `r` admitted the
/// `br` forms, and the `c` forms were missing entirely. All six
/// spellings are pinned, and so are the raw identifiers that must not
/// be mistaken for one.
#[test]
fn starts_a_raw_string_sees_every_spelling() {
    // Assembled, not written: spelled out, these would be raw strings
    // in the file the guard scans.
    let (q, hash) = ('"', '#');
    let raws = [
        format!("r{q}a{q};"),
        format!("r{hash}{q}a{q}{hash};"),
        format!("br{q}a{q};"),
        format!("br{hash}{q}a{q}{hash};"),
        format!("cr{q}a{q};"),
        format!("cr{hash}{q}a{q}{hash};"),
    ];

    for rest in &raws {
        assert!(
            starts_a_raw_string(rest.as_bytes(), Some(&b' ')),
            "{rest:?} should open a raw string"
        );
    }
    assert!(
        starts_a_raw_string(raws[0].as_bytes(), None),
        "a prefix at the very start of a file should count"
    );

    // The near misses: a prefix that continues an identifier (`stderr`
    // then a quote, `subr` then a hash), and one that opens nothing.
    for (rest, prev) in [
        (format!("r{q};"), b'r'),
        (format!("r{hash};"), b'b'),
        (format!("r{q};"), b'_'),
        ("rasa = 1;".to_string(), b' '),
        ("b = 2;".to_string(), b' '),
        // Raw identifiers, which hold no quote and read correctly.
        ("r#type = 1;".to_string(), b' '),
        ("r#match(x);".to_string(), b' '),
        ("br#fn;".to_string(), b' '),
        ("cr#fn;".to_string(), b' '),
    ] {
        assert!(
            !starts_a_raw_string(rest.as_bytes(), Some(&prev)),
            "{rest:?} after {:?} should not",
            prev as char
        );
    }
}

/// The panic branch has to be reachable from the stripper, not just
/// from `starts_a_raw_string`: deleting the `else if` that calls it
/// would otherwise leave every case in this file green.
#[test]
#[should_panic(expected = "gained a raw string")]
fn strip_comments_refuses_a_raw_string() {
    let q = '"';
    strip_comments(&format!("let s = r{q}a{q};"));
}

/// The byte length of the char literal at the front of `rest`, if there
/// is one.
///
/// A leading quote is ambiguous in Rust: `'a'` is a char and `'a` is a
/// lifetime. What separates them is a closing quote in the one position
/// a char literal can put it, so that is what this looks for; anything
/// else is left to be emitted a byte at a time.
fn char_literal_len(rest: &[u8]) -> Option<usize> {
    let start = match rest {
        [b'b', b'\'', ..] => 1,
        [b'\'', ..] => 0,
        _ => return None,
    };

    match rest.get(start + 1)? {
        // `'\n'`, `'\''`, `'\\'` — one escaped byte, then the close.
        // A longer escape (`'\u{1f600}'`) is not matched, and falls
        // through to the byte-at-a-time path as any other quote would.
        b'\\' if rest.get(start + 3) == Some(&b'\'') => Some(start + 4),
        _ if rest.get(start + 2) == Some(&b'\'') => Some(start + 3),
        _ => None,
    }
}

/// The stripper is what stands between `every_example_is_pinned` and a
/// name written down instead of exercised, so its own behavior is
/// pinned rather than assumed.
#[test]
fn strip_comments_drops_comments_and_keeps_literals() {
    let block = concat!("/", "*");
    let close = concat!("*", "/");

    for (name, source, kept) in [
        ("plain code", "let x = \"sentinel.not-an-example\";", true),
        (
            "whole-line comment",
            "// \"sentinel.not-an-example\"",
            false,
        ),
        ("doc comment", "/// \"sentinel.not-an-example\"", false),
        (
            "trailing comment",
            "let y = 1; // \"sentinel.not-an-example\"",
            false,
        ),
        (
            "block comment",
            &format!("{block} \"sentinel.not-an-example\" {close}"),
            false,
        ),
        (
            "nested block comment",
            &format!("{block} {block} n {close} \"sentinel.not-an-example\" {close}"),
            false,
        ),
        (
            "block opener inside a line comment",
            &format!("// {block}\nlet x = \"sentinel.not-an-example\";"),
            true,
        ),
        (
            "line opener inside a block comment",
            &format!("{block} // {close} let x = \"sentinel.not-an-example\";"),
            true,
        ),
        (
            "delimiters inside a string literal",
            &format!("let s = \"{block} {close} //\";\nlet x = \"sentinel.not-an-example\";"),
            true,
        ),
        (
            "byte-char literal holding a quote",
            "let q = b'\\'';\nlet r = b'\"';\n// \"sentinel.not-an-example\"",
            false,
        ),
        (
            "char literal holding a quote",
            "let c = '\"';\n// \"sentinel.not-an-example\"",
            false,
        ),
        (
            "lifetime is not a char literal",
            "fn f<'a>(s: &'a str) {}\n// \"sentinel.not-an-example\"",
            false,
        ),
        (
            // One escaped quote, not two: an even number closes and
            // reopens and leaves the parity where it started, so the
            // branch could be deleted without the case noticing.
            "escaped quote inside a string literal",
            &format!(
                "let s = {q}a\\{q}b{q};\n// {q}sentinel.not-an-example{q}",
                q = '"'
            ),
            false,
        ),
        (
            "unterminated block swallows the rest",
            &format!("{block} let x = \"sentinel.not-an-example\";"),
            false,
        ),
    ] {
        let stripped = strip_comments(source);
        assert_eq!(
            stripped.contains("\"sentinel.not-an-example\""),
            kept,
            "{name}: stripped to {stripped:?}"
        );
    }
}
