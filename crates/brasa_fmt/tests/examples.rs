//! The bundled examples are the formatter's corpus.
//!
//! They are written in the style `docs/spec/01-syntax.md` documents, so
//! `brasa fmt` must leave every one of them byte-identical. A difference
//! here is a disagreement about the canonical style, to be settled by
//! changing one side or the other on purpose — never by re-recording a
//! snapshot.

use std::path::Path;

use brasa_source::FileId;

// The walk the completeness guards share; see its own docs for why it is
// one file rather than a copy per crate.
#[path = "../../brasa/tests/support/example_walk.rs"]
mod example_walk;

#[test]
fn every_example_is_already_formatted() {
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../../examples");
    let examples = example_walk::collect_examples(Path::new(root));

    assert!(
        examples.len() >= 10,
        "expected the example corpus, found {}",
        examples.len()
    );

    for name in examples {
        let path = Path::new(root).join(&name);
        let source = std::fs::read_to_string(&path)
            .unwrap_or_else(|err| panic!("cannot read {}: {err}", path.display()));

        let formatted = brasa_fmt::format(&source, FileId::new(0))
            .unwrap_or_else(|err| panic!("cannot format {name}: {err:?}"));

        assert_eq!(formatted, source, "{name} is not formatted");
    }
}
