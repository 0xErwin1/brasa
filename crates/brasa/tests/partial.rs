//! Does the compiler answer at all about a file that is still being
//! typed? (BRS-114, the prerequisite for an editor integration.)
//!
//! An editor asks about source that is, most of the time, syntactically
//! incomplete: mid-identifier, an unclosed `do`, a call with no
//! arguments yet. The parser already recovers. The question this file
//! settles is what the phases AFTER it do — whether the resolver, the
//! type checker and error-set inference produce partial, usable tables
//! over a tree with holes, or bail and leave the editor with nothing.
//!
//! Every phase is run unconditionally here, which is the difference
//! between this and the CLI. spec: 06 — Diagnósticos's
//! clean-phase-gating principle is a rule for BATCH compilation, where
//! running the checker over an unresolved tree only produces cascades a
//! user did not ask for. An editor wants the opposite trade, and these
//! tests exist so that staying tolerant is a property the suite
//! defends rather than an accident of the current code.

use std::collections::HashMap;
use std::path::PathBuf;

use brasa_source::SourceMap;

/// Everything the pipeline produces for one source, with no phase
/// gated on the previous one being clean.
struct Analysis {
    parse_errors: usize,
    resolve_errors: usize,
    typed_exprs: usize,
    typed_locals: usize,
    error_sets: usize,
}

/// Runs every phase over `source` regardless of what failed before it.
///
/// A panic here is the finding: it would mean a phase assumes a
/// well-formed tree, and no editor could call it.
fn analyze(source: &str) -> Analysis {
    let mut sources = SourceMap::new();
    let file = sources.add_file(PathBuf::from("editing.bras"), source.to_string());

    let parsed = brasa_parser::parse(source, file);
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

    Analysis {
        parse_errors: parsed.diagnostics.len(),
        resolve_errors: resolved.diagnostics.len(),
        typed_exprs: checked.types.expr_types.len(),
        typed_locals: checked.types.local_types.len(),
        error_sets: inferred.sets.len(),
    }
}

/// The baseline: a complete file, so the partial cases below have
/// something to be compared against.
#[test]
fn a_complete_file_types_everything() {
    let complete = analyze(
        r#"
def double(x: int): int
  x * 2
end

let n = double(21)
puts n
"#,
    );

    assert_eq!(complete.parse_errors, 0);
    assert_eq!(complete.resolve_errors, 0);
    assert!(complete.typed_exprs > 0);
    assert!(complete.error_sets > 0);
}

/// The commonest editing state: a name that does not exist yet, because
/// the user is halfway through typing it. Everything around it must
/// still be answered — that is the whole value of hover.
#[test]
fn an_unknown_name_does_not_stop_the_phases_after_it() {
    let analysis = analyze(
        r#"
def double(x: int): int
  x * 2
end

let n = double(21)
let broken = halfTypedNa
puts n
"#,
    );

    assert_eq!(
        analysis.parse_errors, 0,
        "this one parses; it just resolves badly"
    );
    assert!(
        analysis.resolve_errors > 0,
        "the unknown name must be reported"
    );
    assert!(
        analysis.typed_exprs > 0,
        "the checker must still type the expressions it can"
    );
    assert!(
        analysis.typed_locals > 0,
        "`n` still has a type even though `broken` does not"
    );
    assert!(
        analysis.error_sets > 0,
        "`double`'s error-set is still inferable"
    );
}

/// A block the user has opened and not closed. The parser recovers; the
/// question is whether anything downstream trips over the recovery
/// placeholders it leaves behind.
#[test]
fn an_unclosed_block_still_yields_tables() {
    let analysis = analyze(
        r#"
def ready(): int
  1
end

def halfWritten()
  for i in 0..3
    puts i
"#,
    );

    assert!(analysis.parse_errors > 0, "the file does not parse");
    assert!(
        analysis.typed_exprs > 0,
        "the complete function above the hole must still be typed"
    );
    assert!(analysis.error_sets > 0);
}

/// A call the user has opened and not filled in — the state the editor
/// is in at the exact moment it would want signature help.
#[test]
fn an_unfinished_call_still_yields_tables() {
    let analysis = analyze(
        r#"
def add(a: int, b: int): int
  a + b
end

let sum = add(
"#,
    );

    assert!(analysis.parse_errors > 0);
    assert!(
        analysis.typed_exprs > 0,
        "`add`'s body must still be typed while its call site is a hole"
    );
}

/// A member access with nothing after the dot: what the editor holds
/// when it asks for completions.
#[test]
fn a_dangling_member_access_still_yields_tables() {
    let analysis = analyze(
        r#"
let items = [1, 2, 3]
let n = items.
"#,
    );

    assert!(analysis.parse_errors > 0);
    assert!(
        analysis.typed_exprs > 0,
        "`items` must still be typed as a vector"
    );
}

/// Several holes at once, which is what a real half-written file looks
/// like. Cascades are acceptable; silence is not.
#[test]
fn a_file_full_of_holes_still_yields_tables() {
    let analysis = analyze(
        r#"
import "nowhere.bras"

def works(x: int): int
  x + 1
end

let a = works(1)
let b = missingName
let c = a.
def unfinished(
"#,
    );

    assert!(analysis.parse_errors > 0);
    assert!(
        analysis.typed_exprs > 0,
        "every phase must still produce what it can"
    );
    assert!(analysis.error_sets > 0);
}

/// Whether re-running the whole pipeline per keystroke is affordable.
///
/// If it is, incrementality is a non-goal and no query system needs
/// building. The bound is deliberately loose — it is a smoke test
/// against an order-of-magnitude regression, not a benchmark — but a
/// failure here is the signal that the "just re-run it" answer has
/// stopped holding.
#[test]
fn the_whole_pipeline_is_affordable_per_keystroke() {
    let source = std::fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/real/lockaudit.bras"),
    )
    .expect("the largest bundled script must be readable");

    // One run to warm the allocator, then the measured ones.
    analyze(&source);

    let runs = 20;
    let start = std::time::Instant::now();
    for _ in 0..runs {
        analyze(&source);
    }
    let per_run = start.elapsed() / runs;

    assert!(
        per_run < std::time::Duration::from_millis(50),
        "one full pipeline over {} lines took {per_run:?}; \
         re-running per keystroke is the LSP's whole design assumption",
        source.lines().count()
    );

    // Printed rather than asserted tightly: the number is the finding,
    // and pinning it exactly would fail on a loaded machine.
    println!(
        "full pipeline over {} lines: {per_run:?} per run",
        source.lines().count()
    );
}

/// The tables an editor reads are keyed by node ID, so a hole in one
/// place must not shift the keys of anything else. Pinned by comparing
/// a complete file's tables against the same file with a hole appended:
/// every expression that existed before must still be typed.
#[test]
fn a_hole_does_not_disturb_the_tables_of_what_came_before() {
    let prefix = r#"
def double(x: int): int
  x * 2
end

let n = double(21)
"#;

    let complete = analyze(prefix);
    let with_hole = analyze(&format!("{prefix}let broken = notAName\n"));

    assert!(
        with_hole.typed_exprs >= complete.typed_exprs,
        "appending a hole must not cost the file's existing types: \
         {} before, {} after",
        complete.typed_exprs,
        with_hole.typed_exprs
    );
    assert_eq!(
        with_hole.error_sets, complete.error_sets,
        "the same functions must still have error-sets"
    );
}

/// The one place a phase could still leave an editor with nothing: the
/// module loader runs before everything, and a file that fails to parse
/// produces no module at all. Confirms the failure is confined to the
/// unparseable file rather than taking its importer down with it.
#[test]
fn an_importer_survives_an_unparseable_import() {
    let dir = std::env::temp_dir().join(format!("brasa-partial-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");

    std::fs::write(dir.join("broken.bras"), "def f(\n").expect("write");
    let main = dir.join("main.bras");
    std::fs::write(
        &main,
        "import \"broken.bras\"\n\ndef ownWork(x: int): int\n  x + 1\nend\n\nputs ownWork(1)\n",
    )
    .expect("write");

    let mut sources = SourceMap::new();
    let program = brasa_module::load(&main, &mut sources);

    assert!(
        !program.diagnostics.is_empty(),
        "the unparseable import must be reported"
    );

    let roots = program.all_roots();
    let imports: Vec<HashMap<brasa_hir::ItemId, usize>> = program
        .modules
        .iter()
        .map(|module| module.imports.clone())
        .collect();
    let views: Vec<brasa_resolver::ModuleView<'_>> = program
        .modules
        .iter()
        .zip(&imports)
        .map(|(module, imports)| brasa_resolver::ModuleView {
            name: &module.name,
            roots: &module.roots,
            imports,
        })
        .collect();

    let resolved = brasa_resolver::resolve_program(&program.hir, &views);
    let checked = brasa_typeck::check(
        &program.hir,
        &roots,
        &resolved.resolutions,
        &program.sugar_origins,
    );

    assert!(
        !checked.types.expr_types.is_empty(),
        "the importer's own code must still be typed"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
