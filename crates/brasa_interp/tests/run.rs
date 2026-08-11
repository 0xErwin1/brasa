//! End-to-end interpreter tests: full frontend pipeline into [`run`],
//! capturing program output through the injectable writer.

use std::io::Write;

use brasa_interp::{Outcome, run_with_depth};

/// Compiles `source` through the whole frontend (it must be clean) and
/// runs it with the given call-depth limit, writing into `out`.
fn run_into<W: Write + Send>(source: &str, out: &mut W, max_depth: usize) -> Outcome {
    let mut sources = brasa_source::SourceMap::new();
    let file = sources.add_file("test.brs", source.to_string());

    let parsed = brasa_parser::parse(source, file);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);

    let lowered = brasa_hir::lower(&parsed.ast, &parsed.roots);
    assert!(lowered.diagnostics.is_empty(), "{:?}", lowered.diagnostics);

    let resolved = brasa_resolver::resolve(&lowered.hir, &lowered.roots);
    assert!(
        resolved.diagnostics.is_empty(),
        "{:?}",
        resolved.diagnostics
    );

    let checked = brasa_typeck::check(
        &lowered.hir,
        &lowered.roots,
        &resolved.resolutions,
        &lowered.sugar_origins,
    );
    assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);

    run_with_depth(
        &lowered.hir,
        &lowered.roots,
        &resolved.resolutions,
        &checked.types,
        out,
        max_depth,
    )
}

/// [`run_into`] capturing stdout as a string.
fn execute(source: &str, max_depth: usize) -> (Outcome, String) {
    let mut out = Vec::new();
    let outcome = run_into(source, &mut out, max_depth);
    (outcome, String::from_utf8(out).expect("output is UTF-8"))
}

/// A writer whose every write fails with the given error kind.
struct FailingWriter(std::io::ErrorKind);

impl Write for FailingWriter {
    fn write(&mut self, _buf: &[u8]) -> std::io::Result<usize> {
        Err(std::io::Error::new(self.0, "injected write failure"))
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn broken_pipe_on_write_becomes_the_silent_broken_pipe_outcome() {
    let mut out = FailingWriter(std::io::ErrorKind::BrokenPipe);
    let outcome = run_into("puts \"hi\"\n", &mut out, 64);

    assert_eq!(outcome, Outcome::BrokenPipe);
}

#[test]
fn other_write_errors_stay_fatal() {
    let mut out = FailingWriter(std::io::ErrorKind::Other);
    let outcome = run_into("puts \"hi\"\n", &mut out, 64);

    let Outcome::Error { message } = outcome else {
        panic!("expected an error outcome, got {outcome:?}");
    };
    assert!(message.contains("failed to write output"), "{message}");
}

#[test]
fn output_goes_through_the_injected_writer() {
    let (outcome, output) = execute("puts \"hello\"\nprint 1\nprint 2\n", 64);

    assert_eq!(outcome, Outcome::Success);
    assert_eq!(output, "hello\n12");
}

#[test]
fn runaway_recursion_hits_the_depth_guard_not_the_rust_stack() {
    let source = "\
def spin(n: int): int
  spin(n + 1)
end

puts spin(0)
";
    let (outcome, output) = execute(source, 64);

    let Outcome::Panic { message } = outcome else {
        panic!("expected a panic outcome, got {outcome:?}");
    };
    assert!(message.contains("panics.StackOverflow"), "{message}");
    assert!(message.contains("recursion limit"), "{message}");
    assert!(message.contains("in spin"), "{message}");
    assert!(output.is_empty());
}

#[test]
fn recursion_limit_panic_is_catchable_by_its_named_arm() {
    let source = "\
def spin(n: int): int
  spin(n + 1)
end

let d = spin(0) catch (e)
  panics.StackOverflow => -1
end
puts d
";
    let (outcome, output) = execute(source, 64);

    assert_eq!(outcome, Outcome::Success);
    assert_eq!(output, "-1\n");
}

#[test]
fn parse_failure_is_caught_by_the_named_native_arm_binding_the_message() {
    let source = "\
let n = \"abc\".toInt() catch (e)
  string.ParseError => e.len()
end
puts n
";
    let (outcome, output) = execute(source, 64);

    assert_eq!(outcome, Outcome::Success);
    // The arm binds the message string, so `len` counts its scalars.
    assert_eq!(
        output,
        "cannot parse \"abc\" as int".len().to_string() + "\n"
    );
}

#[test]
fn parse_failure_is_caught_by_the_wildcard_arm() {
    let source = "\
let n = \"abc\".toInt() catch (e)
  _ => -1
end
puts n
";
    let (outcome, output) = execute(source, 64);

    assert_eq!(outcome, Outcome::Success);
    assert_eq!(output, "-1\n");
}

#[test]
fn uncaught_parse_failure_is_an_error_with_the_qualified_name() {
    let (outcome, output) = execute("puts \"abc\".toInt()\n", 64);

    let Outcome::Error { message } = outcome else {
        panic!("expected an error outcome, got {outcome:?}");
    };
    assert_eq!(
        message,
        "error: string.ParseError: cannot parse \"abc\" as int"
    );
    assert!(output.is_empty());
}

#[test]
fn to_float_failure_throws_the_same_native_error() {
    let source = "\
let x = \"1.5x\".toFloat() catch (e)
  string.ParseError => 0.0
end
let y = \" 1 \".toInt() catch (e)
  string.ParseError => -1
end
puts y
puts x
";
    let (outcome, output) = execute(source, 64);

    assert_eq!(outcome, Outcome::Success);
    // Parsing is exact (no trimming): `" 1 "` fails too.
    assert_eq!(output, "-1\n0.0\n");
}

#[test]
fn main_runs_after_the_top_level() {
    let source = "\
puts \"top\"

def main()
  puts \"main\"
end

puts \"level\"
";
    let (outcome, output) = execute(source, 64);

    assert_eq!(outcome, Outcome::Success);
    assert_eq!(output, "top\nlevel\nmain\n");
}
