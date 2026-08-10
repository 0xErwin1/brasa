//! Source file management for the Brasa compiler.
//!
//! Owns [`Span`] (a file-qualified byte range) and the [`SourceMap`]/
//! [`SourceFile`] pair that every phase uses to turn a byte offset back
//! into a human-readable line/column. Rendering diagnostics against this
//! data (via `ariadne` or otherwise) is out of scope here; see
//! `brasa_diagnostics`.

pub mod file;
pub mod span;

pub use file::{FileId, SourceFile, SourceMap};
pub use span::Span;

/// A byte offset into a single source file's text.
#[derive(Default, Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BytePosition(pub u32);

impl std::fmt::Display for BytePosition {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}
