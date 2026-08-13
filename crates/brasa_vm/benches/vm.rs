//! VM criterion harness.
//!
//! Every program compiles through the full frontend ONCE, outside the
//! measured loop; each iteration then executes the prebuilt artifacts
//! into a sink writer, so the measurement isolates pure execution. The
//! `catch_overhead_vm` group demonstrates that a never-throwing
//! `catch` is free under handler tables, by comparing the same loop
//! with and without the `catch`.
//!
//! `cold_start` measures the full pipeline (parse through execute) for
//! a small script, plus the frontend alone, so startup overhead stays
//! visible.
//!
//! This compared the VM against the tree-walker until BRS-108 (it was
//! BRS-30's M3 acceptance gate, and the VM won it). The walker legs are
//! gone with the walker; the WORKLOADS are deliberately unchanged,
//! because they are the baseline M6 performance work is measured
//! against and rewriting them now would destroy the comparison.

use criterion::{Criterion, criterion_group, criterion_main};

/// Everything the VM needs, built once per program.
struct Compiled {
    module: brasa_bytecode::Module,
}

/// Runs the frontend on a clean source, asserting no diagnostics.
fn frontend(
    source: &str,
) -> (
    brasa_hir::LowerResult,
    brasa_resolver::ResolveResult,
    brasa_typeck::TypeckResult,
) {
    let mut sources = brasa_source::SourceMap::new();
    let file = sources.add_file("bench.bras", source.to_string());

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

    (lowered, resolved, checked)
}

/// Frontend plus codegen, then a sanity run: a broken benchmark
/// program must fail loudly instead of being measured as a fast panic
/// path.
fn compile(source: &str) -> Compiled {
    let (lowered, resolved, checked) = frontend(source);
    let compiled = brasa_codegen::compile(
        &lowered.hir,
        &lowered.roots,
        &resolved.resolutions,
        &checked.types,
    );
    assert!(
        compiled.diagnostics.is_empty(),
        "{:?}",
        compiled.diagnostics
    );
    let module = compiled.module;
    let compiled = Compiled { module };

    assert_eq!(run_vm(&compiled), brasa_vm::Outcome::Success);

    compiled
}

fn run_vm(compiled: &Compiled) -> brasa_vm::Outcome {
    let mut out = std::io::sink();
    brasa_vm::run(&compiled.module, &mut out, &[])
}

/// The shared acceptance set (`docs/spec/07-bytecode.md`): arithmetic
/// loops, collection traversal, closure-heavy code, catch on the happy
/// path, call-heavy recursion, and string building.
const PROGRAMS: &[(&str, &str)] = &[
    ("arith_loop", include_str!("programs/arith.bras")),
    ("collections", include_str!("programs/collections.bras")),
    ("closures", include_str!("programs/closures.bras")),
    ("catch_happy", include_str!("programs/catch_happy.bras")),
    ("fib", include_str!("programs/fib.bras")),
    ("strings", include_str!("programs/strings.bras")),
];

fn program_benches(criterion: &mut Criterion) {
    for (name, source) in PROGRAMS {
        let compiled = compile(source);

        let mut group = criterion.benchmark_group(*name);
        group.bench_function("vm", |b| b.iter(|| run_vm(&compiled)));
        group.finish();
    }
}

/// Handler tables must make a never-taken `catch` free: the same loop
/// with and without the `catch`, both on the VM.
fn catch_overhead(criterion: &mut Criterion) {
    let with_catch = compile(include_str!("programs/catch_happy.bras"));
    let without_catch = compile(include_str!("programs/catch_free.bras"));

    let mut group = criterion.benchmark_group("catch_overhead_vm");
    group.bench_function("catch", |b| b.iter(|| run_vm(&with_catch)));
    group.bench_function("no_catch", |b| b.iter(|| run_vm(&without_catch)));
    group.finish();
}

fn cold_start(criterion: &mut Criterion) {
    let source = include_str!("programs/cold_start.bras");

    let mut group = criterion.benchmark_group("cold_start");
    group.bench_function("frontend_only", |b| b.iter(|| frontend(source)));
    group.bench_function("vm", |b| {
        b.iter(|| {
            let (lowered, resolved, checked) = frontend(source);
            let compiled = brasa_codegen::compile(
                &lowered.hir,
                &lowered.roots,
                &resolved.resolutions,
                &checked.types,
            );
            assert!(
                compiled.diagnostics.is_empty(),
                "{:?}",
                compiled.diagnostics
            );
            let module = compiled.module;
            let mut out = std::io::sink();
            brasa_vm::run(&module, &mut out, &[])
        })
    });
    group.finish();
}

criterion_group! {
    name = benches;
    config = Criterion::default()
        .warm_up_time(std::time::Duration::from_secs(1))
        .measurement_time(std::time::Duration::from_secs(3));
    targets = program_benches, catch_overhead, cold_start
}
criterion_main!(benches);
