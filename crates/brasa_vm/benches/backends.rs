//! Walker-vs-VM criterion harness (BRS-30, the M3 acceptance gate).
//!
//! Every program compiles through the full frontend ONCE, outside the
//! measured loop; each iteration then executes the prebuilt artifacts
//! on one backend into a sink writer, so the measurement isolates pure
//! execution. The acceptance criterion (`docs/spec/07-bytecode.md`) is
//! a statistically significant VM speedup on every program, and the
//! `catch_overhead_vm` group additionally demonstrates that a
//! never-throwing `catch` is free under handler tables by comparing
//! the same loop with and without the `catch`.
//!
//! `cold_start` measures the full pipeline (parse through execute) for
//! a small script on each backend, plus the frontend alone, so startup
//! overhead stays visible.

use criterion::{Criterion, criterion_group, criterion_main};

/// Every frontend artifact a backend needs, built once per program.
struct Compiled {
    lowered: brasa_hir::LowerResult,
    resolved: brasa_resolver::ResolveResult,
    checked: brasa_typeck::TypeckResult,
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
    let file = sources.add_file("bench.brs", source.to_string());

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

/// Frontend plus codegen, then a sanity run on BOTH backends: a broken
/// benchmark program must fail loudly instead of being measured as a
/// fast panic path.
fn compile(source: &str) -> Compiled {
    let (lowered, resolved, checked) = frontend(source);
    let module = brasa_codegen::compile(
        &lowered.hir,
        &lowered.roots,
        &resolved.resolutions,
        &checked.types,
    );
    let compiled = Compiled {
        lowered,
        resolved,
        checked,
        module,
    };

    assert_eq!(run_walker(&compiled), brasa_vm::Outcome::Success);
    assert_eq!(run_vm(&compiled), brasa_vm::Outcome::Success);

    compiled
}

fn run_walker(compiled: &Compiled) -> brasa_vm::Outcome {
    let mut out = std::io::sink();
    brasa_interp::run(
        &compiled.lowered.hir,
        &compiled.lowered.roots,
        &compiled.resolved.resolutions,
        &compiled.checked.types,
        &mut out,
        &[],
    )
}

fn run_vm(compiled: &Compiled) -> brasa_vm::Outcome {
    let mut out = std::io::sink();
    brasa_vm::run(&compiled.module, &mut out, &[])
}

/// The shared acceptance set (`docs/spec/07-bytecode.md`): arithmetic
/// loops, collection traversal, closure-heavy code, catch on the happy
/// path, call-heavy recursion, and string building.
const PROGRAMS: &[(&str, &str)] = &[
    ("arith_loop", include_str!("programs/arith.brs")),
    ("collections", include_str!("programs/collections.brs")),
    ("closures", include_str!("programs/closures.brs")),
    ("catch_happy", include_str!("programs/catch_happy.brs")),
    ("fib", include_str!("programs/fib.brs")),
    ("strings", include_str!("programs/strings.brs")),
];

fn backend_benches(criterion: &mut Criterion) {
    for (name, source) in PROGRAMS {
        let compiled = compile(source);

        let mut group = criterion.benchmark_group(*name);
        group.bench_function("walker", |b| b.iter(|| run_walker(&compiled)));
        group.bench_function("vm", |b| b.iter(|| run_vm(&compiled)));
        group.finish();
    }
}

/// Handler tables must make a never-taken `catch` free: the same loop
/// with and without the `catch`, both on the VM.
fn catch_overhead(criterion: &mut Criterion) {
    let with_catch = compile(include_str!("programs/catch_happy.brs"));
    let without_catch = compile(include_str!("programs/catch_free.brs"));

    let mut group = criterion.benchmark_group("catch_overhead_vm");
    group.bench_function("catch", |b| b.iter(|| run_vm(&with_catch)));
    group.bench_function("no_catch", |b| b.iter(|| run_vm(&without_catch)));
    group.finish();
}

fn cold_start(criterion: &mut Criterion) {
    let source = include_str!("programs/cold_start.brs");

    let mut group = criterion.benchmark_group("cold_start");
    group.bench_function("frontend_only", |b| b.iter(|| frontend(source)));
    group.bench_function("walker", |b| {
        b.iter(|| {
            let (lowered, resolved, checked) = frontend(source);
            let mut out = std::io::sink();
            brasa_interp::run(
                &lowered.hir,
                &lowered.roots,
                &resolved.resolutions,
                &checked.types,
                &mut out,
                &[],
            )
        })
    });
    group.bench_function("vm", |b| {
        b.iter(|| {
            let (lowered, resolved, checked) = frontend(source);
            let module = brasa_codegen::compile(
                &lowered.hir,
                &lowered.roots,
                &resolved.resolutions,
                &checked.types,
            );
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
    targets = backend_benches, catch_overhead, cold_start
}
criterion_main!(benches);
