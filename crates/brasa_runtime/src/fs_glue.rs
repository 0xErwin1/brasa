//! Backend-agnostic OS glue for `std::fs` plus `env.cwd`/`env.cd`
//! (BRS-33, `docs/spec/05-stdlib.md`), shared by the walker and the VM
//! so filesystem behavior and every observable message can never drift
//! between backends. Value construction stays in each backend's own
//! builtin table, like `proc_env`.
//!
//! Decisions recorded here (mirrored in the spec):
//!
//! - **Error mapping**: `std::io::ErrorKind::NotFound` raises
//!   `fs.NotFound`, `PermissionDenied` raises `fs.Denied`, and every
//!   other kind raises `fs.IoError` carrying the OS message.
//! - `read` requires valid UTF-8 and raises `fs.IoError` otherwise —
//!   silently replacing bytes would corrupt data on a write-back.
//! - `write`/`append` never create parent directories (`mkdirAll`
//!   exists for that); `append` creates the file itself when missing,
//!   like `>>` in a shell.
//! - The predicates follow symlinks (`std::fs::metadata`), so a
//!   dangling symlink reports `exists?` false; they never throw. The
//!   exception is `isSymlink?`, whose whole job is to answer about the
//!   path rather than its target, so it stats without following.
//! - `abs` is lexical and `resolve` is not: `abs` normalizes `.`/`..`
//!   without touching an inode, `resolve` follows every link and
//!   requires the path to exist. A containment check needs `resolve`;
//!   one written on `abs` can be walked out of its root through a
//!   link.
//! - `ls` returns entry NAMES (not paths), sorted bytewise, without
//!   `.`/`..`. `walk` returns PATHS (the argument joined with each
//!   relative path) of every non-directory entry, recursively, sorted
//!   bytewise; symlinks are reported as leaf entries and never
//!   followed. `walk` takes an optional list of directory NAMES to
//!   prune, skipping each matching directory and everything beneath
//!   it — a real tree has a `.git` and a `target` in it, and a walk
//!   that cannot skip them is a walk of the object store. `glob`
//!   returns the matched paths sorted bytewise; an invalid pattern
//!   raises `fs.IoError`.
//! - `rm` removes a file, a symlink, or an EMPTY directory; `rmAll`
//!   removes a whole tree (or a single file) recursively. Both raise
//!   `fs.NotFound` on a missing path.
//! - `cp` copies one file (directory sources raise `fs.IoError`); `mv`
//!   renames, falling back to copy-plus-delete for a FILE crossing
//!   filesystems.
//! - The path helpers are pure lexical string operations with Rust
//!   `std::path` semantics — except `abs`, which absolutizes a
//!   relative path against the current directory and lexically
//!   normalizes `.`/`..` without touching the filesystem: no symlink
//!   resolution, no existence requirement.
//! - `cd` calls `std::env::set_current_dir`: it moves the REAL host
//!   process cwd (relative paths everywhere, child processes, `abs`
//!   all follow). Acceptable single-threaded scripting semantics; an
//!   overlay cwd was rejected as complexity without a consumer.

use std::path::{Component, Path, PathBuf};

use brasa_resolver::{FS_DENIED, FS_IO_ERROR, FS_NOT_FOUND};

/// One failed filesystem operation: the qualified native-error name
/// (`fs.NotFound`, `fs.Denied`, or `fs.IoError`) and its message.
pub struct FsError {
    pub name: &'static str,
    pub message: String,
}

pub type FsResult<T> = Result<T, FsError>;

/// The BRS-33 `ErrorKind` mapping (module docs).
fn error_name(kind: std::io::ErrorKind) -> &'static str {
    match kind {
        std::io::ErrorKind::NotFound => FS_NOT_FOUND,
        std::io::ErrorKind::PermissionDenied => FS_DENIED,
        _ => FS_IO_ERROR,
    }
}

fn fs_err(action: &str, path: &str, err: std::io::Error) -> FsError {
    FsError {
        name: error_name(err.kind()),
        message: format!("cannot {action} `{path}`: {err}"),
    }
}

fn lossy(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

pub fn read(path: &str) -> FsResult<String> {
    let bytes = std::fs::read(path).map_err(|err| fs_err("read", path, err))?;

    String::from_utf8(bytes).map_err(|_| FsError {
        name: FS_IO_ERROR,
        message: format!("cannot read `{path}`: contents are not valid UTF-8"),
    })
}

pub fn write(path: &str, contents: &str) -> FsResult<()> {
    std::fs::write(path, contents).map_err(|err| fs_err("write", path, err))
}

pub fn append(path: &str, contents: &str) -> FsResult<()> {
    use std::io::Write;

    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(path)
        .map_err(|err| fs_err("append to", path, err))?;

    file.write_all(contents.as_bytes())
        .map_err(|err| fs_err("append to", path, err))
}

pub fn exists(path: &str) -> bool {
    std::fs::metadata(path).is_ok()
}

pub fn is_file(path: &str) -> bool {
    std::fs::metadata(path).is_ok_and(|meta| meta.is_file())
}

pub fn is_dir(path: &str) -> bool {
    std::fs::metadata(path).is_ok_and(|meta| meta.is_dir())
}

/// Whether the path ITSELF is a symlink, following nothing — the one
/// predicate that must not follow, since following one answers about
/// its target instead. Like the others it never throws: a path the OS
/// refuses to stat is simply `false`.
pub fn is_symlink(path: &str) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_symlink())
}

pub fn ls(path: &str) -> FsResult<Vec<String>> {
    let entries = std::fs::read_dir(path).map_err(|err| fs_err("list", path, err))?;

    let mut names = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|err| fs_err("list", path, err))?;
        names.push(entry.file_name());
    }

    names.sort_by(|a, b| a.as_os_str().cmp(b.as_os_str()));
    Ok(names
        .iter()
        .map(|name| name.to_string_lossy().into_owned())
        .collect())
}

pub fn glob(pattern: &str) -> FsResult<Vec<String>> {
    let matches = glob::glob(pattern).map_err(|err| FsError {
        name: FS_IO_ERROR,
        message: format!("invalid glob pattern `{pattern}`: {err}"),
    })?;

    let mut paths = Vec::new();
    for matched in matches {
        let matched = matched.map_err(|err| {
            let path = lossy(err.path());
            fs_err("glob", &path, err.into())
        })?;
        paths.push(lossy(&matched));
    }

    paths.sort();
    Ok(paths)
}

/// Every non-directory entry under `path`, with any directory whose
/// NAME appears in `prune` skipped along with everything beneath it.
///
/// Pruning is by entry name, the vocabulary `ls` already speaks, and
/// matching is exact — `glob` is the member for patterns. It applies
/// to directories only: a pruned name that is a file is still
/// returned, since pruning is about subtrees and a file has none. The
/// root is never pruned, even when its own base name is listed:
/// pruning the argument you just passed is a mistake, not a request.
pub fn walk(path: &str, prune: &[String]) -> FsResult<Vec<String>> {
    let mut paths = Vec::new();
    walk_into(Path::new(path), prune, &mut paths)?;

    // Ordered by the encoded bytes, as the spec says and as `tryWalk`
    // does. Sorting the rendered strings instead would order two names
    // differing only in bytes that are not valid UTF-8 by their
    // replacement characters, and the two members would disagree on a
    // tree holding such a name.
    paths.sort_by(|a, b| a.as_os_str().cmp(b.as_os_str()));

    Ok(paths.iter().map(|path| lossy(path)).collect())
}

/// Depth-first collection under `dir`: non-directory entries (files
/// and symlinks — `DirEntry::file_type` never follows links) are
/// leaves, directories recurse unless pruned. Sorting happens once at
/// the end.
fn walk_into(dir: &Path, prune: &[String], paths: &mut Vec<PathBuf>) -> FsResult<()> {
    let shown = lossy(dir);
    let entries = std::fs::read_dir(dir).map_err(|err| fs_err("walk", &shown, err))?;

    for entry in entries {
        let entry = entry.map_err(|err| fs_err("walk", &shown, err))?;
        let file_type = entry
            .file_type()
            .map_err(|err| fs_err("walk", &lossy(&entry.path()), err))?;

        if file_type.is_dir() {
            let name = entry.file_name();
            if prune.iter().any(|skip| skip.as_str() == name) {
                continue;
            }
            walk_into(&entry.path(), prune, paths)?;
        } else {
            paths.push(entry.path());
        }
    }

    Ok(())
}

/// `walk`, but a directory below the root that cannot be read is
/// recorded instead of aborting the whole traversal.
///
/// Returns what it reached and what it could not read, both sorted.
/// Never silently: the point of the pair is that a best-effort caller
/// can still say what it missed, which `walk` returning a short list
/// could not. The root itself is NOT tolerated — a root that is not
/// there, or not readable, is the caller asking for the wrong thing,
/// and `tryWalk` throws it exactly as `walk` would.
///
/// That rule is about OPENING the root. Once it is open, an entry
/// inside it that cannot be enumerated carries no path of its own, so
/// it is named by its parent — which is the root. Seeing the root in
/// `unreadable` therefore means "some entries directly under it could
/// not be listed", not "the root could not be opened": that second
/// case raised instead of returning.
pub fn try_walk(path: &str, prune: &[String]) -> FsResult<(Vec<String>, Vec<String>)> {
    let root = Path::new(path);

    // The root is read ONCE, here, and the iterator is handed to the
    // descent. Reading it a second time inside the tolerant walk would
    // put a root that became unreadable between the two opens into
    // `unreadable` instead of raising — and a tree changing under the
    // walk is the very thing this member exists to survive.
    let entries = std::fs::read_dir(root).map_err(|err| fs_err("tryWalk", &lossy(root), err))?;

    let mut paths = Vec::new();
    let mut unreadable = Vec::new();
    try_walk_entries(entries, root, prune, &mut paths, &mut unreadable);

    // Both halves are collected as paths and ordered by their encoded
    // bytes, then rendered. Neither of the obvious alternatives is the
    // bytewise order the spec promises and `walk` produces: sorting the
    // lossy strings orders two names differing in invalid UTF-8 by
    // their replacement characters, and `PathBuf`'s own `Ord` compares
    // component by component, which puts `a/b.txt` BEFORE `a.txt`
    // because "a" < "a.txt" — a walk and a tryWalk of the same tree
    // would then disagree.
    let by_bytes = |a: &PathBuf, b: &PathBuf| a.as_os_str().cmp(b.as_os_str());

    paths.sort_by(by_bytes);

    // A directory names its whole skipped subtree, so it is worth
    // saying once. Without this, one whose entries individually failed
    // to enumerate would appear once per failure, and a caller counting
    // `unreadable.len()` as "places I could not reach" would be wrong.
    //
    // Deduplicated as paths, not as the strings they render to: two
    // directories whose names differ only in bytes that are not valid
    // UTF-8 render the same and would collapse into one, undercounting
    // the very thing this field is for.
    unreadable.sort_by(by_bytes);

    // Deduplicated by the same relation it was sorted by. `PathBuf`'s
    // own `PartialEq` compares components, so `a//b` equals `a/b` while
    // sorting apart — `dedup` needs equal elements adjacent, and only
    // matching the two keeps that true if a path ever arrives
    // unnormalized.
    unreadable.dedup_by(|a, b| a.as_os_str() == b.as_os_str());

    let rendered = |paths: Vec<PathBuf>| paths.iter().map(|path| lossy(path)).collect();

    Ok((rendered(paths), rendered(unreadable)))
}

/// The tolerant half of [`try_walk`]. A directory it cannot read, or an
/// entry it cannot stat, is recorded and the traversal continues;
/// nothing below such a directory is reachable, so the directory stands
/// for its whole subtree.
fn try_walk_into(
    dir: &Path,
    prune: &[String],
    paths: &mut Vec<PathBuf>,
    unreadable: &mut Vec<PathBuf>,
) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        unreadable.push(dir.to_path_buf());
        return;
    };

    try_walk_entries(entries, dir, prune, paths, unreadable);
}

/// The loop body, split out so the root can be walked from an iterator
/// its caller already opened (see [`try_walk`]).
fn try_walk_entries(
    entries: std::fs::ReadDir,
    dir: &Path,
    prune: &[String],
    paths: &mut Vec<PathBuf>,
    unreadable: &mut Vec<PathBuf>,
) {
    for entry in entries {
        // A failed entry carries no path of its own, so the directory
        // holding it is what gets named. `try_walk` deduplicates, so a
        // directory with several such entries is still one report.
        let Ok(entry) = entry else {
            unreadable.push(dir.to_path_buf());
            continue;
        };
        // Recorded before the prune list is consulted, and it has to
        // be: pruning applies to directories, and an entry whose kind
        // cannot be read is not known to be one. Reporting a pruned
        // name that failed to stat is the lesser wrong — the caller
        // learns something it can check, rather than having a failure
        // hidden under a rule that may not even apply.
        let Ok(file_type) = entry.file_type() else {
            unreadable.push(entry.path());
            continue;
        };

        if file_type.is_dir() {
            let name = entry.file_name();
            if prune.iter().any(|skip| skip.as_str() == name) {
                continue;
            }
            try_walk_into(&entry.path(), prune, paths, unreadable);
        } else {
            paths.push(entry.path());
        }
    }
}

pub fn mkdir(path: &str) -> FsResult<()> {
    std::fs::create_dir(path).map_err(|err| fs_err("create directory", path, err))
}

pub fn mkdir_all(path: &str) -> FsResult<()> {
    std::fs::create_dir_all(path).map_err(|err| fs_err("create directory", path, err))
}

pub fn rm(path: &str) -> FsResult<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::IsADirectory => {
            std::fs::remove_dir(path).map_err(|err| fs_err("remove", path, err))
        }
        Err(err) => Err(fs_err("remove", path, err)),
    }
}

pub fn rm_all(path: &str) -> FsResult<()> {
    let meta = std::fs::symlink_metadata(path).map_err(|err| fs_err("remove", path, err))?;

    if meta.is_dir() {
        std::fs::remove_dir_all(path).map_err(|err| fs_err("remove", path, err))
    } else {
        std::fs::remove_file(path).map_err(|err| fs_err("remove", path, err))
    }
}

pub fn cp(from: &str, to: &str) -> FsResult<()> {
    std::fs::copy(from, to)
        .map(|_| ())
        .map_err(|err| two_path_err("copy", from, to, err))
}

pub fn mv(from: &str, to: &str) -> FsResult<()> {
    match std::fs::rename(from, to) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::CrossesDevices && is_file(from) => {
            cp(from, to)?;
            rm(from)
        }
        Err(err) => Err(two_path_err("move", from, to, err)),
    }
}

fn two_path_err(action: &str, from: &str, to: &str, err: std::io::Error) -> FsError {
    FsError {
        name: error_name(err.kind()),
        message: format!("cannot {action} `{from}` to `{to}`: {err}"),
    }
}

pub fn join(base: &str, part: &str) -> String {
    lossy(&Path::new(base).join(part))
}

pub fn base(path: &str) -> String {
    Path::new(path)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

pub fn dir(path: &str) -> String {
    Path::new(path).parent().map(lossy).unwrap_or_default()
}

pub fn ext(path: &str) -> String {
    Path::new(path)
        .extension()
        .map(|ext| ext.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// The real path: absolute AND with every symlink followed, so it
/// answers where a path actually leads rather than where it reads as
/// leading. [`abs`] is lexical and cannot do this — it never touches an
/// inode — which is why a containment check written on `abs` can be
/// walked straight out of its root through a link.
///
/// Requires the path to exist, since a link with no target has no real
/// path to report. A symlink loop surfaces as `fs.IoError`, the OS's
/// own answer.
pub fn resolve(path: &str) -> FsResult<String> {
    let real = std::fs::canonicalize(path).map_err(|err| fs_err("resolve", path, err))?;
    Ok(lossy(&real))
}

pub fn abs(path: &str) -> FsResult<String> {
    let path = Path::new(path);

    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else {
        current_dir()?.join(path)
    };

    Ok(lossy(&normalize(&joined)))
}

/// Lexical normalization: drops `.`, resolves `..` against the
/// preceding component (staying put at the root), touches no inode.
fn normalize(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();

    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !matches!(
                    normalized.components().next_back(),
                    None | Some(Component::RootDir | Component::Prefix(_))
                ) {
                    normalized.pop();
                }
            }
            other => normalized.push(other),
        }
    }

    normalized
}

pub fn cwd() -> FsResult<String> {
    current_dir().map(|dir| lossy(&dir))
}

fn current_dir() -> FsResult<PathBuf> {
    std::env::current_dir().map_err(|err| FsError {
        name: FS_IO_ERROR,
        message: format!("cannot get the current directory: {err}"),
    })
}

pub fn cd(path: &str) -> FsResult<()> {
    std::env::set_current_dir(path).map_err(|err| fs_err("cd to", path, err))
}
