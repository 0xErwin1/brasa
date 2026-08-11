//! End-to-end interpreter tests: full frontend pipeline into [`run`],
//! capturing program output through the injectable writer.

use brasa_interp::{Outcome, run_with_depth};

/// Compiles `source` through the whole frontend (it must be clean) and
/// runs it with the given call-depth limit, capturing stdout.
fn execute(source: &str, max_depth: usize) -> (Outcome, String) {
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

    let mut out = Vec::new();
    let outcome = run_with_depth(
        &lowered.hir,
        &lowered.roots,
        &resolved.resolutions,
        &checked.types,
        &mut out,
        max_depth,
    );
    (outcome, String::from_utf8(out).expect("output is UTF-8"))
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
