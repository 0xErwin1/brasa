//! The two loaders differ in exactly two ways, and both matter to
//! whoever is typing.
//!
//! `load` is for batch compilation: files come from disk, and one that
//! did not parse is dropped rather than lowered into cascades.
//! `load_partial` is for an editor: buffers win over files, and a file
//! that did not parse still contributes what the parser salvaged.
//!
//! These are pinned because the editor's half is invisible until it is
//! wrong — a loader that dropped the file being typed would leave the
//! LSP with nothing to say, which reads as "the server is broken"
//! rather than as "this file has a syntax error".

use std::path::PathBuf;

use brasa_module::Overlay;
use brasa_source::SourceMap;

/// A file being typed: complete work, then a hole, in one body.
const MID_EDIT: &str = "def main()\n  let count = 21\n  let x = \nend\n";

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("brasa-module-partial");
    std::fs::create_dir_all(&dir).expect("the scratch directory is writable");
    dir.join(name)
}

#[test]
fn the_batch_loader_drops_a_file_that_did_not_parse() {
    let path = scratch("batch.bras");
    std::fs::write(&path, MID_EDIT).unwrap();

    let mut sources = SourceMap::new();
    let program = brasa_module::load(&path, &mut sources);

    assert!(
        !program.diagnostics.is_empty(),
        "the parse error is still reported"
    );
    assert!(
        program.all_roots().is_empty(),
        "a batch compile lowers nothing from a file that did not parse"
    );

    std::fs::remove_file(&path).ok();
}

/// The same source, through the editor's loader, keeps what parsed.
/// Without this the LSP is blank for the file the user is editing,
/// which is most files most of the time.
#[test]
fn the_editor_loader_keeps_what_the_parser_salvaged() {
    let path = scratch("editor.bras");

    let mut overlay = Overlay::new();
    overlay.insert(path.clone(), MID_EDIT.to_string());

    let mut sources = SourceMap::new();
    let program = brasa_module::load_partial(&path, &mut sources, &overlay);

    assert!(
        !program.diagnostics.is_empty(),
        "the parse error is still reported"
    );
    assert!(
        !program.all_roots().is_empty(),
        "`main` parsed, so it must still be lowered"
    );
}

/// An overlay decides what a file SAYS. Disk is not consulted at all
/// for an overlaid path — a stale read would be worse than no read.
#[test]
fn the_overlay_wins_over_the_file() {
    let path = scratch("overlaid.bras");
    std::fs::write(&path, "def main()\nend\n").unwrap();

    let mut overlay = Overlay::new();
    overlay.insert(
        path.clone(),
        "def main()\nend\n\ndef second()\nend\n".to_string(),
    );

    let mut sources = SourceMap::new();
    let program = brasa_module::load_partial(&path, &mut sources, &overlay);

    assert_eq!(
        program.all_roots().len(),
        2,
        "the buffer has two items; only disk still has one"
    );

    std::fs::remove_file(&path).ok();
}

/// An unsaved file has no bytes on disk, and the editor's loader must
/// still answer about it — a new file is the one an editor is most
/// likely to be holding unsaved.
#[test]
fn an_overlaid_path_need_not_exist_on_disk() {
    let path = scratch("never-saved.bras");
    std::fs::remove_file(&path).ok();

    let mut overlay = Overlay::new();
    overlay.insert(path.clone(), "def main()\nend\n".to_string());

    let mut sources = SourceMap::new();
    let program = brasa_module::load_partial(&path, &mut sources, &overlay);

    assert_eq!(program.all_roots().len(), 1);
}

/// An overlay changes what a file says, never which file a name means.
/// The batch loader with an empty overlay must be `load` exactly, or
/// the editor and the compiler would answer differently about the same
/// saved tree.
#[test]
fn an_empty_overlay_is_the_batch_loader_but_tolerant() {
    let path = scratch("equivalent.bras");
    std::fs::write(&path, "def main()\n  puts 1\nend\n").unwrap();

    let mut batch_sources = SourceMap::new();
    let batch = brasa_module::load(&path, &mut batch_sources);

    let mut editor_sources = SourceMap::new();
    let editor = brasa_module::load_partial(&path, &mut editor_sources, &Overlay::new());

    assert_eq!(batch.all_roots().len(), editor.all_roots().len());
    assert_eq!(batch.modules.len(), editor.modules.len());
    assert!(batch.diagnostics.is_empty() && editor.diagnostics.is_empty());

    std::fs::remove_file(&path).ok();
}
