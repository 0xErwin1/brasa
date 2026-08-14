//! Every file under `examples/` (BRS-10/11 acceptance criterion) must
//! parse with zero diagnostics; the dump of each is snapshotted so a
//! future grammar change that silently reshapes the tree is caught.

use std::path::Path;

use brasa_source::SourceMap;

// The walk both completeness guards share; see its own docs for why it
// is one file rather than a copy per crate.
#[path = "../../brasa/tests/support/example_walk.rs"]
mod example_walk;

fn parse_example(path: &Path) -> (brasa_parser::ParseResult, SourceMap) {
    let text = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()));
    let mut source_map = SourceMap::new();
    let file = source_map.add_file(path.to_path_buf(), text.clone());
    let result = brasa_parser::parse(&text, file);
    (result, source_map)
}

fn assert_zero_diagnostics(name: &str, result: &brasa_parser::ParseResult) {
    assert!(
        result.diagnostics.is_empty(),
        "{name} expected zero diagnostics, got: {:#?}",
        result.diagnostics
    );
}

/// Declares one `#[test]` per example AND the list the completeness
/// guard checks, from one declaration. The list still exists, but it is
/// not editable on its own: the same literal that puts a name into it
/// also emits the test that must parse the file and match a snapshot,
/// so a name cannot be added without the coverage arriving with it.
macro_rules! example_tests {
    ($($test_name:ident => $file:literal),* $(,)?) => {
        $(
            #[test]
            fn $test_name() {
                let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../../examples/", $file);
                let (result, _map) = parse_example(Path::new(root));
                assert_zero_diagnostics($file, &result);
                let dump = brasa_parser::dump::dump(&result.ast, &result.roots);
                insta::assert_snapshot!(stringify!($test_name), dump);
            }
        )*

        const SNAPSHOTTED: &[&str] = &[$($file),*];
    };
}

example_tests! {
    example_destructure => "destructure.bras",
    example_errors => "errors.bras",
    example_fib => "fib.bras",
    example_fizzbuzz => "fizzbuzz.bras",
    example_hello => "hello.bras",
    example_modules_main => "modules/main.bras",
    example_modules_utils => "modules/utils.bras",
    example_pipeline => "pipeline.bras",
    example_real_gitreport => "real/gitreport.bras",
    example_real_lockaudit => "real/lockaudit.bras",
    example_real_logstat => "real/logstat.bras",
    example_real_tally => "real/tally.bras",
    example_shapes => "shapes.bras",
    example_stars => "stars.bras",
    example_strings => "strings.bras",
}

/// The list above claims to be every example, so it is checked rather
/// than trusted: the three `real/` scripts were missing from it, which
/// is the same way `stars.bras` was left uncompiled for a whole
/// milestone (BRS-63).
#[test]
fn every_example_is_snapshotted() {
    let found = example_walk::collect_examples(Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../examples"
    )));

    let mut snapshotted: Vec<String> = SNAPSHOTTED.iter().map(|s| s.to_string()).collect();
    snapshotted.sort();

    // Reported as two directions rather than as a set mismatch: they
    // have different remedies, and the walk sees untracked files, so
    // the first is as likely to mean "a scratch file wandered in" as
    // "an example was added without a test".
    let missing: Vec<&String> = found.iter().filter(|f| !snapshotted.contains(f)).collect();
    assert!(
        missing.is_empty(),
        "these examples have no parse snapshot: {missing:?}\n\
         Either add them to example_tests!, or move them out of examples/ \
         if they are scratch files — this walks the working directory, so \
         untracked files count."
    );

    let stale: Vec<&String> = snapshotted.iter().filter(|s| !found.contains(s)).collect();
    assert!(
        stale.is_empty(),
        "example_tests! names files that no longer exist: {stale:?}"
    );
}
