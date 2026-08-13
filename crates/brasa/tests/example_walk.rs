//! The `examples/` directory walk itself (BRS-105).
//!
//! `collect_examples` is the net under both completeness guards —
//! `every_example_is_pinned` here in `crates/brasa` and
//! `every_example_is_snapshotted` in `crates/brasa_parser` — so a
//! weakened rule inside it disables both with the whole suite still
//! green. Each of its four rules cost a revision to arrive at; these
//! cases hold them in place.
//!
//! The tree is built per test rather than pointed at `examples/`,
//! because a symlink rule cannot be exercised against a directory the
//! repository is not allowed to contain.

use std::path::{Path, PathBuf};

#[path = "support/example_walk.rs"]
mod example_walk;

use example_walk::collect_examples;

/// A fresh directory per test, named after the test so a failure leaves
/// something identifiable behind.
fn temp_dir(name: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("brasa-example-walk-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("failed to create temp dir");

    dir
}

fn write(dir: &Path, name: &str, text: &str) -> PathBuf {
    let path = dir.join(name);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("failed to create parent directory");
    }
    std::fs::write(&path, text).expect("failed to write fixture");

    path
}

#[test]
fn an_empty_root_yields_nothing() {
    let dir = temp_dir("empty");

    assert_eq!(collect_examples(&dir), Vec::<String>::new());
}

/// The base case, plus the extension filter: the walk describes what an
/// example is, and a `README.md` beside one is not an example.
#[test]
fn a_flat_bras_file_is_found_and_other_extensions_are_not() {
    let dir = temp_dir("flat");
    write(&dir, "hello.bras", "puts 1\n");
    write(&dir, "README.md", "not an example\n");
    write(&dir, "notes.txt", "neither\n");

    assert_eq!(collect_examples(&dir), vec!["hello.bras".to_string()]);
}

/// Nested examples come back with the path that names them, because the
/// guards compare these strings against pinned lists.
#[test]
fn a_nested_file_carries_its_directory_prefix() {
    let dir = temp_dir("nested");
    write(&dir, "cli/deep/tool.bras", "puts 1\n");

    assert_eq!(
        collect_examples(&dir),
        vec!["cli/deep/tool.bras".to_string()]
    );
}

/// Hidden entries are never examples and are where tools keep their
/// state, so neither a hidden directory nor a hidden file is walked.
#[test]
fn hidden_entries_are_skipped() {
    let dir = temp_dir("hidden");
    write(&dir, "visible.bras", "puts 1\n");
    write(&dir, ".devenv/state.bras", "puts 2\n");
    write(&dir, ".hidden.bras", "puts 3\n");

    assert_eq!(collect_examples(&dir), vec!["visible.bras".to_string()]);
}

/// The order is the walk's contract: `read_dir` gives no guarantee, and
/// a guard comparing against a pinned list needs one.
#[test]
fn the_result_is_sorted() {
    let dir = temp_dir("sorted");
    write(&dir, "zebra.bras", "puts 1\n");
    write(&dir, "alpha.bras", "puts 2\n");
    write(&dir, "middle/two.bras", "puts 3\n");
    write(&dir, "middle/one.bras", "puts 4\n");

    assert_eq!(
        collect_examples(&dir),
        vec![
            "alpha.bras".to_string(),
            "middle/one.bras".to_string(),
            "middle/two.bras".to_string(),
            "zebra.bras".to_string(),
        ]
    );
}

#[cfg(unix)]
mod symlinks {
    use super::{collect_examples, temp_dir, write};
    use std::os::unix::fs::symlink;

    /// A leaf is followed, so an example added as a symlink still counts.
    /// Refusing here would leave an unpinned example one `ln -s` away
    /// from invisible, which is what the guards exist to prevent.
    #[test]
    fn a_symlink_to_a_real_file_counts_as_an_example() {
        let dir = temp_dir("symlink-file");
        let real = write(&dir, "real.bras", "puts 1\n");
        symlink(&real, dir.join("linked.bras")).expect("failed to create symlink");

        assert_eq!(
            collect_examples(&dir),
            vec!["linked.bras".to_string(), "real.bras".to_string()]
        );
    }

    /// A name that looks like an example and resolves to nothing is
    /// refused rather than skipped: skipping would leave it visible in
    /// `ls` and exercised by nothing.
    #[test]
    #[should_panic(expected = "dangling.bras does not resolve to a file")]
    fn a_dangling_bras_symlink_is_refused() {
        let dir = temp_dir("symlink-dangling");
        symlink(dir.join("gone.bras"), dir.join("dangling.bras"))
            .expect("failed to create symlink");

        collect_examples(&dir);
    }

    /// A symlinked directory is refused rather than followed or skipped:
    /// following can walk out of `examples/` or cycle, and skipping hides
    /// every example beneath it.
    #[test]
    #[should_panic(expected = "linked is a symlinked directory")]
    fn a_symlinked_directory_is_refused() {
        let dir = temp_dir("symlink-dir");
        write(&dir, "real/inner.bras", "puts 1\n");
        symlink(dir.join("real"), dir.join("linked")).expect("failed to create symlink");

        collect_examples(&dir);
    }

    /// The refusal names the path from the root, so a nested one is
    /// actionable without hunting for it.
    #[test]
    #[should_panic(expected = "nested/linked is a symlinked directory")]
    fn a_nested_symlinked_directory_is_named_by_its_full_path() {
        let dir = temp_dir("symlink-dir-nested");
        write(&dir, "real/inner.bras", "puts 1\n");
        write(&dir, "nested/keep.bras", "puts 2\n");
        symlink(dir.join("real"), dir.join("nested/linked")).expect("failed to create symlink");

        collect_examples(&dir);
    }
}
