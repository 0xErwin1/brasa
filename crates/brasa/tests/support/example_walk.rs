//! The `examples/` directory walk, shared by the two completeness
//! guards that must agree on what an example is:
//! `every_example_is_pinned` in `crates/brasa/tests/programs.rs` and
//! `every_example_is_snapshotted` in
//! `crates/brasa_parser/tests/examples.rs`.
//!
//! It lives in one file because it took four revisions to settle —
//! following symlinks, then following neither, then following only at a
//! leaf, then skipping hidden entries — and two copies drifting apart
//! would leave one guard watching a different set than the other, with
//! nothing failing to say so.
//!
//! `brasa_parser` reaches it with `#[path = "../../brasa/..."]`, so its
//! test target no longer builds from its own crate directory alone.
//! That is a real cost: a `cargo package` verification or a partial
//! checkout of that crate stops compiling this test rather than merely
//! skipping a guard. It is accepted here because the alternative is a
//! second copy of exactly the logic that needed four corrections, and
//! because the guard is worth nothing if the two halves disagree.

use std::path::Path;

/// Every `.brs` under `root`, recursively, as paths relative to it.
///
/// Four rules, each of which was a defect before it was a rule:
///
/// - Hidden entries are skipped. They are never examples, and they are
///   where tools keep their state — the symlink rule below fired on
///   `real/.devenv/profile` the first time it ran.
/// - A leaf goes through [`Path::is_file`], which follows symlinks, so
///   an example added as a symlink still counts. Refusing to follow
///   here would leave an unpinned example one `ln -s` away from
///   invisible, which is the condition these guards exist to prevent.
/// - A `.brs` symlink that resolves to nothing is refused too. Skipping
///   it would leave a name that looks like an example in `ls` and is
///   exercised by nothing, which is the invisibility the rule below was
///   made loud to prevent.
/// - A symlinked directory is refused rather than followed or skipped.
///   Following one can walk out of `examples/` entirely (a link to an
///   ancestor enumerates the whole repository) and can cycle; skipping
///   one hides every example beneath it. Refusing says so, which is the
///   only one of the three a reader can act on.
pub fn collect_examples(root: &Path) -> Vec<String> {
    let mut found = Vec::new();
    walk(root, "", &mut found);
    found.sort();

    found
}

fn walk(dir: &Path, prefix: &str, found: &mut Vec<String>) {
    let entries =
        std::fs::read_dir(dir).unwrap_or_else(|e| panic!("cannot read {}: {e}", dir.display()));

    for entry in entries {
        let entry =
            entry.unwrap_or_else(|e| panic!("cannot read an entry of {}: {e}", dir.display()));

        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') {
            continue;
        }

        let kind = entry
            .file_type()
            .unwrap_or_else(|e| panic!("cannot stat {prefix}{name}: {e}"));

        assert!(
            !(kind.is_symlink() && entry.path().is_dir()),
            "{prefix}{name} is a symlinked directory; this walk does not follow those, \
             so every example under it would be invisible to the guard. Make it a real \
             directory, or move it out of examples/."
        );

        if kind.is_dir() {
            walk(&entry.path(), &format!("{prefix}{name}/"), found);
        } else if name.ends_with(".brs") {
            assert!(
                entry.path().is_file(),
                "{prefix}{name} does not resolve to a file; it would look like an example \
                 and be exercised by nothing. Fix the link, or remove it."
            );
            found.push(format!("{prefix}{name}"));
        }
    }
}
