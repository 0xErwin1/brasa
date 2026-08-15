//! The project manifest, `brasa.toml`: what a directory full of Brasa
//! files can say about itself.
//!
//! A manifest is strictly optional and only ever fills in what the
//! command line omitted — an explicit argument always wins, and a
//! standalone script keeps running with no project around it
//! (spec: 00 — Visión y alcance). Discovery walks up from the current
//! working directory to the filesystem root; the first `brasa.toml`
//! wins, and every path inside it resolves against the manifest's own
//! directory before anything else sees it.
//!
//! The schema rejects unknown keys on every table, so a typo'd key is a
//! parse error rather than a setting that silently never applied.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use serde::Deserialize;

const FILE_NAME: &str = "brasa.toml";

/// A parsed manifest, bound to the directory it was found in.
pub struct Manifest {
    /// Where the manifest sits; the anchor every relative path inside
    /// it resolves against.
    dir: PathBuf,
    doc: Doc,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct Doc {
    #[serde(default)]
    project: Project,
    #[serde(default)]
    build: Build,
    /// Alias name to directory. A plain map rather than a struct: both
    /// sides are the user's to choose, so there are no known keys to
    /// deny. A `BTreeMap` keeps the aliases in a deterministic order.
    #[serde(default)]
    imports: BTreeMap<String, String>,
    #[serde(default)]
    test: Test,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct Project {
    name: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct Build {
    entry: Option<String>,
    out_dir: Option<String>,
    source_dir: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct Test {
    globs: Option<Vec<String>>,
}

/// Finds and parses the nearest manifest, or reports why it could not.
///
/// `Ok(None)` — no manifest anywhere up the tree — is the ordinary
/// standalone case and costs one metadata probe per ancestor. A
/// manifest that exists but cannot be read or parsed is an error, not a
/// fall-through: a project whose configuration silently stopped
/// applying is worse than one that fails loudly.
pub fn discover() -> Result<Option<Manifest>, ExitCode> {
    let cwd = match std::env::current_dir() {
        Ok(cwd) => cwd,
        Err(err) => {
            eprintln!("brasa: cannot determine the current directory: {err}");
            return Err(ExitCode::from(70));
        }
    };

    let Some(path) = find(&cwd) else {
        return Ok(None);
    };

    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) => {
            eprintln!("brasa: {}: {err}", path.display());
            return Err(ExitCode::from(65));
        }
    };

    match parse(&text) {
        Ok(doc) => Ok(Some(Manifest {
            // `find` only answers with a path that has a parent.
            dir: path.parent().map(Path::to_path_buf).unwrap_or_default(),
            doc,
        })),
        Err(err) => {
            eprintln!("brasa: {}: {err}", path.display());
            Err(ExitCode::from(65))
        }
    }
}

/// The nearest `brasa.toml` at or above `start`; first hit wins.
fn find(start: &Path) -> Option<PathBuf> {
    start
        .ancestors()
        .map(|dir| dir.join(FILE_NAME))
        .find(|candidate| candidate.is_file())
}

fn parse(text: &str) -> Result<Doc, toml::de::Error> {
    toml::from_str(text)
}

impl Manifest {
    /// The directory the manifest was found in.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// `build.entry`, resolved against the manifest directory.
    pub fn entry(&self) -> Option<PathBuf> {
        self.doc
            .build
            .entry
            .as_deref()
            .map(|entry| self.resolve(entry))
    }

    /// `build.source_dir`, resolved against the manifest directory.
    /// `None` when the manifest does not define one — the caller keeps
    /// its own default then.
    pub fn source_dir(&self) -> Option<PathBuf> {
        self.doc
            .build
            .source_dir
            .as_deref()
            .map(|dir| self.resolve(dir))
    }

    /// Where `brasa bundle` writes when `-o` was omitted:
    /// `<out_dir>/<project.name or the entry's stem>`.
    pub fn bundle_output(&self, entry: &Path) -> PathBuf {
        let out_dir = self.resolve(self.doc.build.out_dir.as_deref().unwrap_or("build"));

        let name = self
            .doc
            .project
            .name
            .clone()
            .or_else(|| {
                entry
                    .file_stem()
                    .map(|stem| stem.to_string_lossy().into_owned())
            })
            .unwrap_or_else(|| "out".to_string());

        out_dir.join(name)
    }

    /// `[test].globs`, verbatim. Expansion happens at the call site,
    /// against [`Manifest::dir`].
    pub fn test_globs(&self) -> Option<&[String]> {
        self.doc.test.globs.as_deref()
    }

    /// What the loader needs: every `[imports]` alias with its directory
    /// made absolute, since the loader never learns where the manifest
    /// was.
    pub fn load_options(&self) -> brasa_module::LoadOptions {
        let aliases = self
            .doc
            .imports
            .iter()
            .map(|(alias, dir)| (alias.clone(), canonical(&self.resolve(dir))))
            .collect();

        brasa_module::LoadOptions { aliases }
    }

    /// A manifest-relative path, anchored; an absolute one, verbatim
    /// (which is what `join` already does).
    fn resolve(&self, relative: &str) -> PathBuf {
        self.dir.join(relative)
    }
}

/// The canonical form of a path, or the path itself when the OS cannot
/// canonicalize it — the read that follows reports the real problem.
fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch tree of its own per test, so the walks cannot see each
    /// other's files.
    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("brasa-manifest-{name}-{}", std::process::id()));

        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("the scratch directory is writable");

        dir
    }

    #[test]
    fn discovery_walks_up_and_the_first_hit_wins() {
        let root = scratch("walk");
        let nested = root.join("a/b");
        std::fs::create_dir_all(&nested).unwrap();

        std::fs::write(root.join(FILE_NAME), "").unwrap();
        std::fs::write(root.join("a").join(FILE_NAME), "").unwrap();

        let found = find(&nested).expect("an ancestor holds a manifest");
        assert_eq!(
            found,
            root.join("a").join(FILE_NAME),
            "the nearest one wins"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn discovery_reports_nothing_when_no_ancestor_has_one() {
        let root = scratch("empty");

        // The scratch tree has no manifest; the walk continues above it
        // into the real temp directory, which does not have one either.
        assert!(find(&root).is_none());

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn every_field_parses_and_resolves_against_the_manifest_dir() {
        let manifest = Manifest {
            dir: PathBuf::from("/proj"),
            doc: parse(concat!(
                "[project]\nname = \"mytool\"\n\n",
                "[build]\nentry = \"src/main.bras\"\nout_dir = \"build\"\nsource_dir = \"src\"\n\n",
                "[imports]\nutil = \"src/util\"\n\n",
                "[test]\nglobs = [\"tests/*.bras\"]\n",
            ))
            .expect("the reference manifest parses"),
        };

        assert_eq!(manifest.entry(), Some(PathBuf::from("/proj/src/main.bras")));
        assert_eq!(manifest.source_dir(), Some(PathBuf::from("/proj/src")));
        assert_eq!(
            manifest.bundle_output(&PathBuf::from("/proj/src/main.bras")),
            PathBuf::from("/proj/build/mytool")
        );
        assert_eq!(
            manifest.test_globs(),
            Some(&["tests/*.bras".to_string()][..])
        );
        assert_eq!(
            manifest.load_options().aliases,
            vec![("util".to_string(), PathBuf::from("/proj/src/util"))]
        );
    }

    #[test]
    fn an_empty_manifest_is_valid_and_answers_nothing() {
        let manifest = Manifest {
            dir: PathBuf::from("/proj"),
            doc: parse("").expect("an empty manifest is a valid one"),
        };

        assert_eq!(manifest.entry(), None);
        assert_eq!(manifest.source_dir(), None);
        assert_eq!(manifest.test_globs(), None);
        assert!(manifest.load_options().aliases.is_empty());
    }

    #[test]
    fn without_a_project_name_the_bundle_is_named_after_the_entry() {
        let manifest = Manifest {
            dir: PathBuf::from("/proj"),
            doc: parse("[build]\nentry = \"src/main.bras\"\n").unwrap(),
        };

        assert_eq!(
            manifest.bundle_output(&PathBuf::from("/proj/src/main.bras")),
            PathBuf::from("/proj/build/main")
        );
    }

    #[test]
    fn a_typoed_key_is_a_parse_error_not_a_silent_no_op() {
        assert!(parse("[build]\nentrey = \"main.bras\"\n").is_err());
        assert!(parse("[buidl]\nentry = \"main.bras\"\n").is_err());
    }
}
