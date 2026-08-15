//! Manifest import aliases through the loader (`load_with`).
//!
//! An alias claims a `::` import's FIRST segment for one directory. The
//! contract worth pinning is the miss: an alias never falls through to
//! the search path, because a module that quietly resolved somewhere
//! else would mean something different in and out of the project. And
//! an alias only touches `::` imports — a relative `import "..."` with
//! the same name keeps meaning the file beside the importer.

use std::path::{Path, PathBuf};

use brasa_module::LoadOptions;
use brasa_source::SourceMap;

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "brasa-module-aliases-{name}-{}",
        std::process::id()
    ));

    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("the scratch directory is writable");

    dir
}

fn write(dir: &Path, name: &str, text: &str) -> PathBuf {
    let path = dir.join(name);

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("the fixture directory is writable");
    }
    std::fs::write(&path, text).expect("the fixture is writable");

    path
}

fn aliased(name: &str, dir: &Path) -> LoadOptions {
    LoadOptions {
        aliases: vec![(name.to_string(), dir.to_path_buf())],
    }
}

#[test]
fn an_aliased_import_resolves_under_the_aliased_directory() {
    let dir = scratch("hit");
    let entry = write(
        &dir,
        "main.bras",
        "import util::helpers\nputs helpers.greet()\n",
    );
    write(
        &dir,
        "src/util/helpers.bras",
        "pub def greet(): string\n  \"hi\"\nend\n",
    );

    let mut sources = SourceMap::new();
    let program = brasa_module::load_with(
        &entry,
        &mut sources,
        &aliased("util", &dir.join("src/util")),
    );

    assert!(
        program.diagnostics.is_empty(),
        "the alias resolves cleanly, got: {:?}",
        program.diagnostics
    );
    assert_eq!(program.modules.len(), 2, "the aliased module was loaded");

    let _ = std::fs::remove_dir_all(&dir);
}

/// A miss under an alias is a failure that names the alias and the one
/// directory it tried; it must NOT fall through to the search path,
/// even when the search path could answer.
#[test]
fn an_alias_miss_fails_loudly_instead_of_falling_through() {
    let dir = scratch("miss");
    let entry = write(&dir, "main.bras", "import util::helpers\nputs 1\n");

    // The search path's `lib` beside the entry COULD answer; the alias
    // must keep it from being consulted.
    write(
        &dir,
        "lib/util/helpers.bras",
        "pub def greet(): string\n  \"hi\"\nend\n",
    );

    let alias_dir = dir.join("src/util");
    std::fs::create_dir_all(&alias_dir).unwrap();

    let mut sources = SourceMap::new();
    let program = brasa_module::load_with(&entry, &mut sources, &aliased("util", &alias_dir));

    assert_eq!(program.modules.len(), 1, "only the entry was loaded");

    let diagnostic = program
        .diagnostics
        .iter()
        .find(|diag| diag.message.contains("`util` alias"))
        .expect("the failure names the alias");
    assert!(
        diagnostic
            .notes
            .iter()
            .any(|note| note.contains(&alias_dir.display().to_string())),
        "the failure names the directory that was tried, got: {:?}",
        diagnostic.notes
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// An alias claims `::` imports only. A relative `import "util.bras"`
/// keeps meaning the file beside the importer, whatever the manifest
/// says about the name `util`.
#[test]
fn an_alias_does_not_shadow_a_relative_import() {
    let dir = scratch("relative");
    let entry = write(
        &dir,
        "main.bras",
        "import \"util.bras\"\nputs util.greet()\n",
    );
    write(
        &dir,
        "util.bras",
        "pub def greet(): string\n  \"local\"\nend\n",
    );

    // The alias points somewhere else entirely, and must not be asked.
    let elsewhere = dir.join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();

    let mut sources = SourceMap::new();
    let program = brasa_module::load_with(&entry, &mut sources, &aliased("util", &elsewhere));

    assert!(
        program.diagnostics.is_empty(),
        "the relative import resolves as always, got: {:?}",
        program.diagnostics
    );

    let loaded = program.module(0);
    assert_eq!(
        loaded.path,
        std::fs::canonicalize(dir.join("util.bras")).unwrap(),
        "the file beside the importer won"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
