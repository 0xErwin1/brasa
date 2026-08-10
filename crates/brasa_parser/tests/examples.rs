//! Every file under `examples/` (BRS-10/11 acceptance criterion) must
//! parse with zero diagnostics; the dump of each is snapshotted so a
//! future grammar change that silently reshapes the tree is caught.

use std::path::Path;

use brasa_source::SourceMap;

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

macro_rules! example_test {
    ($test_name:ident, $file:literal) => {
        #[test]
        fn $test_name() {
            let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../../examples/", $file);
            let (result, _map) = parse_example(Path::new(root));
            assert_zero_diagnostics($file, &result);
            let dump = brasa_parser::dump::dump(&result.ast, &result.roots);
            insta::assert_snapshot!(stringify!($test_name), dump);
        }
    };
}

example_test!(example_errors, "errors.brs");
example_test!(example_fib, "fib.brs");
example_test!(example_fizzbuzz, "fizzbuzz.brs");
example_test!(example_hello, "hello.brs");
example_test!(example_pipeline, "pipeline.brs");
example_test!(example_shapes, "shapes.brs");
example_test!(example_stars, "stars.brs");
example_test!(example_strings, "strings.brs");
example_test!(example_modules_main, "modules/main.brs");
example_test!(example_modules_utils, "modules/utils.brs");
