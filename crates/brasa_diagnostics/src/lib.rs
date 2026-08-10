//! Source management and diagnostics for the Brasa compiler.
//!
//! Owns [`Span`], the source-file table, and rendering of compiler
//! diagnostics through `ariadne`. Every other crate reports errors through
//! the types defined here; no phase prints to stderr on its own.

/// A byte range inside one source file.
///
/// Spans are stored in side tables indexed by node ID rather than inside
/// AST nodes, so they are plain `Copy` data with no source-file back
/// reference; the file is implied by the compilation unit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub start: u32,
    pub end: u32,
}

impl Span {
    pub fn new(start: u32, end: u32) -> Self {
        Self { start, end }
    }

    /// Smallest span covering both `self` and `other`.
    pub fn to(self, other: Span) -> Span {
        Span::new(self.start.min(other.start), self.end.max(other.end))
    }
}
