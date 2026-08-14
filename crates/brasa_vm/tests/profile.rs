//! The sampling profiler (BRS-121).
//!
//! Sampling is statistical, so these pin the properties that must hold
//! for any fair sample — which function dominates, that self and total
//! time differ where they should, that the shapes are well-formed —
//! and never an exact count, which would be a flaky test by
//! construction.

use std::path::PathBuf;
use std::time::Duration;

use brasa_bytecode::Module;
use brasa_source::SourceMap;

fn compile(source: &str) -> Module {
    let mut sources = SourceMap::new();
    let file = sources.add_file(PathBuf::from("profile.bras"), source.to_string());

    let parsed = brasa_parser::parse(source, file);
    assert!(parsed.diagnostics.is_empty(), "the fixture must parse");

    let lowered = brasa_hir::lower(&parsed.ast, &parsed.roots);
    let resolved = brasa_resolver::resolve(&lowered.hir, &lowered.roots);
    let checked = brasa_typeck::check(
        &lowered.hir,
        &lowered.roots,
        &resolved.resolutions,
        &lowered.sugar_origins,
    );
    let inferred = brasa_errorset::infer(
        &lowered.hir,
        &lowered.roots,
        &resolved.resolutions,
        &checked.types,
    );
    assert!(inferred.diagnostics.is_empty(), "the fixture must check");

    let compiled = brasa_codegen::compile_program(
        &lowered.hir,
        &lowered.roots,
        &lowered.roots,
        &resolved.resolutions,
        &checked.types,
    );
    assert!(compiled.diagnostics.is_empty(), "the fixture must compile");

    compiled.module
}

/// A cheap caller and an expensive one, so a fair sample has an
/// unambiguous answer about which dominates.
const LOPSIDED: &str = r#"def expensive(n: int): int
  if n < 2
    n
  else
    expensive(n - 1) + expensive(n - 2)
  end
end

def cheap(): int
  1 + 1
end

def main()
  puts cheap()
  puts expensive(21)
end
"#;

fn profile(module: &Module) -> brasa_vm::profile::Profile {
    let mut out = Vec::new();
    // A tight interval so a short fixture still yields enough samples
    // for a stable answer without the test taking a visible pause.
    let (outcome, profile) = brasa_vm::profile(module, &mut out, &[], Duration::from_micros(100));

    assert!(
        matches!(outcome, brasa_runtime::Outcome::Success),
        "the fixture must run cleanly"
    );
    profile
}

/// The dominant function is the expensive one. This is the whole point
/// of the instrument, and the only claim a sample can make strongly.
#[test]
fn the_expensive_function_dominates_self_time() {
    let module = compile(LOPSIDED);
    let profile = profile(&module);

    assert!(profile.samples > 0, "a run this long must yield samples");

    let hottest = profile
        .functions
        .first()
        .expect("at least one function was sampled");
    assert_eq!(hottest.name, "expensive");
}

/// Self time and total time are different questions, and `main` is
/// where the difference shows: it does nothing itself and is on the
/// stack for everything.
#[test]
fn main_has_no_self_time_and_all_the_total_time() {
    let module = compile(LOPSIDED);
    let profile = profile(&module);

    let main = profile
        .functions
        .iter()
        .find(|f| f.name == "main")
        .expect("`main` is on every stack");

    // Not exactly zero: a sample can land while `main` runs a native
    // call (`puts`), which executes in `main`'s own frame.
    assert!(
        main.self_samples * 10 <= profile.samples,
        "`main` calls; it does not compute ({} self samples of {})",
        main.self_samples,
        profile.samples
    );
    assert_eq!(
        main.total_samples, profile.samples,
        "`main` is on the stack for the whole run"
    );
}

/// A recursive function counts once per sample, not once per frame.
/// Without that, depth would masquerade as time and a deeply recursive
/// function would look infinitely hot.
#[test]
fn recursion_counts_once_per_sample() {
    let module = compile(LOPSIDED);
    let profile = profile(&module);

    let expensive = profile
        .functions
        .iter()
        .find(|f| f.name == "expensive")
        .expect("the recursive function was sampled");

    assert!(
        expensive.total_samples <= profile.samples,
        "{} total samples over {} taken",
        expensive.total_samples,
        profile.samples
    );
}

/// Collector time is measured apart from interpreted time, because the
/// two want different fixes.
#[test]
fn collector_time_is_reported_separately() {
    let module = compile(LOPSIDED);
    let profile = profile(&module);

    assert!(
        profile.gc <= profile.elapsed,
        "gc {:?} cannot exceed the run's {:?}",
        profile.gc,
        profile.elapsed
    );
    assert!(profile.report().contains("in the collector"));
}

/// The collapsed format is what existing flamegraph tooling eats, so
/// its shape is a contract: `a;b;c COUNT`, one stack per line.
#[test]
fn collapsed_output_is_the_flamegraph_format() {
    let module = compile(LOPSIDED);
    let profile = profile(&module);

    let collapsed = profile.collapsed();
    assert!(!collapsed.is_empty());

    for line in collapsed.lines() {
        let (stack, count) = line.rsplit_once(' ').expect("`stack COUNT`");

        assert!(!stack.is_empty(), "a stack is named");
        assert!(stack.starts_with("main") || stack.starts_with("<toplevel>"));
        count.parse::<usize>().expect("the count is a number");
    }
}

/// The collapsed output keeps raw frames even for recursion: the
/// report folds runs for readability, and folding the machine format
/// too would lie to a tool that folds its own way.
#[test]
fn collapsed_output_keeps_recursion_unfolded() {
    let module = compile(LOPSIDED);
    let profile = profile(&module);

    let collapsed = profile.collapsed();
    assert!(
        collapsed.contains("expensive;expensive"),
        "recursion stays expanded in the machine format"
    );
    assert!(
        !collapsed.contains("(x"),
        "the fold is a reporting choice, not a data one"
    );

    assert!(
        profile.report().contains("(x"),
        "the human report folds the run it would otherwise repeat"
    );
}

/// A program too short to sample says so instead of printing an empty
/// table that reads like "nothing took any time".
#[test]
fn a_run_with_no_samples_says_so() {
    let module = compile("def main()\n  puts 1\nend\n");

    let mut out = Vec::new();
    let (_, profile) = brasa_vm::profile(&module, &mut out, &[], Duration::from_secs(30));

    assert_eq!(profile.samples, 0);
    assert!(profile.report().contains("no samples"));
}
