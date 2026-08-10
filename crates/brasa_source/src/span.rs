//! [`Span`]: a byte range inside one specific source file.

use crate::{BytePosition, file::FileId};

/// A byte range `[start, end)` inside the file identified by `file`.
///
/// Spans are stored in side tables keyed by node ID rather than inside AST
/// nodes, so nodes stay plain, immutable data; see `brasa_ast`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Span {
    pub start: BytePosition,
    pub end: BytePosition,
    pub file: FileId,
}

impl Span {
    /// Creates a new span with validation.
    ///
    /// # Panics
    /// Panics in debug mode if `start > end`.
    pub fn new(file: FileId, start: BytePosition, end: BytePosition) -> Self {
        debug_assert!(
            start <= end,
            "Span::new() called with invalid range: start {} > end {}",
            start,
            end
        );
        Self { file, start, end }
    }

    /// Smallest span covering both `a` and `b`.
    ///
    /// # Panics
    /// Panics in debug mode if `a` and `b` belong to different files.
    pub fn merge(a: &Self, b: &Self) -> Self {
        debug_assert_eq!(a.file, b.file, "Cannot merge spans from different files");
        Self {
            file: a.file,
            start: a.start.min(b.start),
            end: a.end.max(b.end),
        }
    }

    /// Returns the length of the span in bytes.
    ///
    /// # Note
    /// Returns 0 if `start > end` (invalid span) to avoid underflow.
    pub fn len(&self) -> usize {
        if self.end.0 >= self.start.0 {
            (self.end.0 - self.start.0) as usize
        } else {
            0
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::Span;
    use crate::BytePosition;
    use crate::file::FileId;

    #[test]
    fn merge_covers_both_spans_within_the_same_file() {
        let file = FileId::new(0);
        let left = Span::new(file, BytePosition(2), BytePosition(4));
        let right = Span::new(file, BytePosition(6), BytePosition(9));

        let merged = Span::merge(&left, &right);

        assert_eq!(merged, Span::new(file, BytePosition(2), BytePosition(9)));
    }

    #[test]
    fn merge_is_order_independent() {
        let file = FileId::new(0);
        let left = Span::new(file, BytePosition(6), BytePosition(9));
        let right = Span::new(file, BytePosition(2), BytePosition(4));

        assert_eq!(Span::merge(&left, &right), Span::merge(&right, &left));
    }

    #[test]
    fn len_and_is_empty_report_byte_length() {
        let file = FileId::new(0);
        let span = Span::new(file, BytePosition(2), BytePosition(5));
        let empty = Span::new(file, BytePosition(7), BytePosition(7));

        assert_eq!(span.len(), 3);
        assert!(!span.is_empty());
        assert!(empty.is_empty());
    }
}
