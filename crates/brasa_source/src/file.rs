//! [`SourceFile`] and [`SourceMap`]: file contents plus byte-offset ->
//! line/column lookup.

use std::collections::HashMap;
use std::path::PathBuf;

use brasa_arena::{Id, Store};

use crate::BytePosition;

pub type FileId = Id<SourceFile>;

/// The text of one source file, with precomputed line-start offsets so
/// byte-offset -> line/column lookups don't rescan the text.
#[derive(Default, Clone, Eq, PartialEq, Hash, Debug)]
pub struct SourceFile {
    pub path: PathBuf,
    pub text: String,
    pub line_starts: Vec<BytePosition>,
}

impl SourceFile {
    pub fn new(path: PathBuf, text: String) -> Self {
        let line_starts = compute_line_starts(&text);
        Self {
            path,
            text,
            line_starts,
        }
    }

    #[inline]
    pub fn len_bytes(&self) -> u32 {
        self.text.len() as u32
    }
}

/// Interns source files by path, handing out a stable [`FileId`] for each
/// one.
#[derive(Default)]
pub struct SourceMap {
    files: Store<SourceFile>,
    by_path: HashMap<PathBuf, FileId>,
}

impl SourceMap {
    pub fn new() -> Self {
        Self {
            files: Store::new(),
            by_path: HashMap::new(),
        }
    }

    /// Adds a file, or returns the existing [`FileId`] if this path was
    /// already interned.
    pub fn add_file<P: Into<PathBuf>>(&mut self, path: P, text: String) -> FileId {
        let path = path.into();

        if let Some(id) = self.by_path.get(&path) {
            return *id;
        }

        let id = self.files.alloc(SourceFile::new(path.clone(), text));
        self.by_path.insert(path, id);
        id
    }

    /// Adds a file with no on-disk path (e.g. an in-memory test fixture),
    /// labeled `<label>` for display purposes. Never interned by path.
    pub fn add_virtual(&mut self, label: &str, text: String) -> FileId {
        let path = PathBuf::from(format!("<{label}>"));
        self.files.alloc(SourceFile::new(path, text))
    }

    #[inline]
    pub fn get(&self, id: &FileId) -> &SourceFile {
        self.files.get(id)
    }

    /// Looks up a file by its path. Returns `None` if the file is not in
    /// the map.
    pub fn lookup_by_path<P: AsRef<std::path::Path>>(&self, path: P) -> Option<FileId> {
        self.by_path.get(path.as_ref()).copied()
    }

    /// Converts a byte offset into a 1-based `(line, column)` pair, where
    /// the column counts Unicode scalar values rather than bytes so it
    /// matches what a text editor shows for multibyte UTF-8 content.
    pub fn display_line_col(&self, file: &FileId, pos: BytePosition) -> (u32, u32) {
        let f = self.get(file);
        let line = upper_bound_line(&f.line_starts, pos);
        let line_start = f.line_starts[line].0 as usize;
        let slice = &f.text.as_bytes()[line_start..pos.0 as usize];
        let col = unicode_column(slice);

        ((line as u32) + 1, (col as u32) + 1)
    }
}

fn compute_line_starts(text: &str) -> Vec<BytePosition> {
    let bytes = text.as_bytes();
    let mut v = Vec::with_capacity(128);
    v.push(BytePosition(0));

    for (i, b) in bytes.iter().enumerate() {
        if *b == b'\n' {
            v.push(BytePosition((i + 1) as u32));
        }
    }
    v
}

fn upper_bound_line(starts: &[BytePosition], pos: BytePosition) -> usize {
    let mut lo = 0usize;
    let mut hi = starts.len();
    while lo + 1 < hi {
        let mid = (lo + hi) / 2;
        if starts[mid].0 <= pos.0 {
            lo = mid
        } else {
            hi = mid
        }
    }
    lo
}

fn unicode_column(slice: &[u8]) -> usize {
    std::str::from_utf8(slice)
        .map(|s| s.chars().count())
        .unwrap_or(slice.len())
}

#[cfg(test)]
mod tests {
    use super::SourceMap;
    use crate::BytePosition;

    #[test]
    fn display_line_col_uses_unicode_columns_for_multibyte_text() {
        let mut source_map = SourceMap::new();
        let file = source_map.add_virtual("utf8", "aéb".to_string());

        assert_eq!(source_map.display_line_col(&file, BytePosition(0)), (1, 1));
        // 'é' is 2 bytes (offsets 1..3) but a single column.
        assert_eq!(source_map.display_line_col(&file, BytePosition(1)), (1, 2));
        assert_eq!(source_map.display_line_col(&file, BytePosition(3)), (1, 3));
    }

    #[test]
    fn display_line_col_counts_wide_cjk_scalar_as_single_column() {
        let mut source_map = SourceMap::new();
        // '世' is three UTF-8 bytes (offsets 1..4) but a single column.
        let file = source_map.add_virtual("wide", "a世b".to_string());

        assert_eq!(source_map.display_line_col(&file, BytePosition(1)), (1, 2));
        assert_eq!(source_map.display_line_col(&file, BytePosition(4)), (1, 3));
    }

    #[test]
    fn display_line_col_tracks_lines_after_newlines() {
        let mut source_map = SourceMap::new();
        let file = source_map.add_virtual("multi", "alpha\nbeta\ngamma".to_string());

        assert_eq!(source_map.display_line_col(&file, BytePosition(0)), (1, 1));
        assert_eq!(source_map.display_line_col(&file, BytePosition(6)), (2, 1));
        assert_eq!(source_map.display_line_col(&file, BytePosition(11)), (3, 1));
    }

    #[test]
    fn add_file_interns_by_path() {
        let mut source_map = SourceMap::new();

        let first = source_map.add_file("a.brs", "let x = 1".to_string());
        let second = source_map.add_file("a.brs", "let x = 1".to_string());

        assert_eq!(first, second);
        assert_eq!(source_map.lookup_by_path("a.brs"), Some(first));
    }
}
