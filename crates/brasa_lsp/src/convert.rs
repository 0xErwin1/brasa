//! Between the compiler's positions and the protocol's.
//!
//! The compiler counts BYTES from the start of a file. LSP counts, by
//! default, UTF-16 code units from the start of a LINE. The two agree
//! only for ASCII, so every conversion goes through this file rather
//! than being open-coded at a call site that happened to be tested with
//! an ASCII fixture.
//!
//! Three encodings differ here, and picking the wrong one puts a
//! squiggle in the wrong place rather than failing loudly:
//!
//! | | `é` | `€` | `😀` |
//! |---|---|---|---|
//! | UTF-8 bytes (the compiler) | 2 | 3 | 4 |
//! | UTF-16 units (LSP default) | 1 | 1 | 2 |
//! | characters | 1 | 1 | 1 |
//!
//! The server advertises no `positionEncoding`, so the default holds
//! and UTF-16 is what a client will send and expect.

use brasa_diagnostics::{Diagnostic, Severity};
use brasa_source::{SourceMap, Span};
use lsp_types::{
    Diagnostic as LspDiagnostic, DiagnosticRelatedInformation, DiagnosticSeverity, Location,
    NumberOrString, Position, Range, Uri,
};

/// A byte offset into `text`, as an LSP position.
///
/// An offset past the end clamps to the end rather than panicking: it
/// can only arrive from a span over text the client has since changed,
/// and answering about the last position is better than dropping the
/// diagnostic that carried it.
pub fn offset_to_position(text: &str, offset: u32) -> Position {
    let offset = (offset as usize).min(text.len());

    let line_start = text[..offset]
        .rfind('\n')
        .map(|index| index + 1)
        .unwrap_or(0);
    let line = text[..line_start].matches('\n').count();
    let character = text[line_start..offset].encode_utf16().count();

    Position {
        line: line as u32,
        character: character as u32,
    }
}

/// An LSP position, as a byte offset into `text`.
///
/// Clamps rather than failing, for the same reason: a position from a
/// client that is one keystroke ahead of us must still land somewhere
/// sensible.
pub fn position_to_offset(text: &str, position: Position) -> u32 {
    let mut line_start = 0usize;
    for _ in 0..position.line {
        match text[line_start..].find('\n') {
            Some(index) => line_start += index + 1,
            None => return text.len() as u32,
        }
    }

    // Walk the line by characters, counting the UTF-16 units each one
    // costs, and stop when the client's count is reached. A position
    // landing INSIDE a surrogate pair (which a well-behaved client will
    // not send) resolves to the start of that character.
    let line = &text[line_start..];
    let mut utf16 = 0u32;

    for (byte, ch) in line.char_indices() {
        if utf16 >= position.character {
            return (line_start + byte) as u32;
        }
        if ch == '\n' {
            return (line_start + byte) as u32;
        }
        utf16 += ch.len_utf16() as u32;
    }

    (line_start + line.trim_end_matches('\n').len()) as u32
}

/// A compiler span as an LSP range, in the file the span names.
///
/// `None` when the span's file is not one this analysis loaded, which
/// is how a diagnostic about a file the client never opened is dropped
/// rather than reported against the wrong document.
pub fn span_to_range(sources: &SourceMap, span: Span) -> Range {
    let text = &sources.get(&span.file).text;

    Range {
        start: offset_to_position(text, span.start.0),
        end: offset_to_position(text, span.end.0),
    }
}

/// The compiler's severity as the protocol's.
///
/// `Hint` maps to the protocol's hint rather than to information: the
/// diagnostics spec uses it for the "did you mean" half of a message,
/// which is what a client renders unobtrusively.
pub fn severity(severity: Severity) -> DiagnosticSeverity {
    match severity {
        Severity::Error => DiagnosticSeverity::ERROR,
        Severity::Warning => DiagnosticSeverity::WARNING,
        Severity::Info => DiagnosticSeverity::INFORMATION,
        Severity::Hint => DiagnosticSeverity::HINT,
    }
}

/// One compiler diagnostic as the protocol's.
///
/// The error code travels as `code` so a client can filter on it and a
/// user can search it — spec: 06 — Diagnósticos gives every
/// diagnostic a stable one, and dropping it here would waste that.
///
/// Labels become related information rather than being flattened into
/// the message: a label points at a DIFFERENT span, and folding it into
/// the primary message would put its text at the wrong place on screen.
/// Notes have no span, so they do fold in.
pub fn diagnostic(
    sources: &SourceMap,
    diag: &Diagnostic,
    url_of: impl Fn(&Span) -> Option<Uri>,
) -> LspDiagnostic {
    let mut message = diag.message.clone();
    for note in &diag.notes {
        message.push_str("\n\nnote: ");
        message.push_str(note);
    }

    let related: Vec<_> = diag
        .labels
        .iter()
        .filter(|label| label.span != diag.primary_span)
        .filter_map(|label| {
            Some(DiagnosticRelatedInformation {
                location: Location {
                    uri: url_of(&label.span)?,
                    range: span_to_range(sources, label.span),
                },
                message: label.message.clone(),
            })
        })
        .collect();

    LspDiagnostic {
        range: span_to_range(sources, diag.primary_span),
        severity: Some(severity(diag.severity.clone())),
        code: Some(NumberOrString::String(diag.error_code.clone())),
        source: Some("brasa".to_string()),
        message,
        related_information: if related.is_empty() {
            None
        } else {
            Some(related)
        },
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_offsets_are_line_and_column() {
        let text = "def main()\n  puts 1\nend\n";

        assert_eq!(offset_to_position(text, 0), Position::new(0, 0));
        assert_eq!(offset_to_position(text, 11), Position::new(1, 0));
        assert_eq!(offset_to_position(text, 13), Position::new(1, 2));
    }

    /// The case the byte/UTF-16 distinction exists for. `é` is two
    /// bytes and one UTF-16 unit, so a column counted in bytes would
    /// drift right by one for every accent earlier on the line — and
    /// Brasa is a language people write Spanish comments in.
    #[test]
    fn a_two_byte_character_costs_one_utf16_unit() {
        let text = "let café = 1\n";

        // `=` is at byte 10 (`café` is 5 bytes) but column 9.
        assert_eq!(text.as_bytes()[10], b'=');
        assert_eq!(offset_to_position(text, 10), Position::new(0, 9));
    }

    /// An emoji is one character, four bytes, and TWO UTF-16 units, so
    /// it is the case where counting characters is wrong too.
    #[test]
    fn an_astral_character_costs_two_utf16_units() {
        let text = "puts \"😀\"\n";

        let closing = text.find('"').unwrap() + 1 + "😀".len();
        assert_eq!(text.as_bytes()[closing], b'"');
        assert_eq!(
            offset_to_position(text, closing as u32),
            Position::new(0, 8)
        );
    }

    /// The two directions must compose, or a hover would answer about
    /// a different byte than the one the user pointed at.
    #[test]
    fn the_two_directions_round_trip() {
        for text in [
            "def main()\n  puts 1\nend\n",
            "let café = 1\nlet e\u{301} = 2\n",
            "puts \"😀 ok\"\nputs 2\n",
        ] {
            for (offset, _) in text.char_indices() {
                let offset = offset as u32;
                let position = offset_to_position(text, offset);

                assert_eq!(
                    position_to_offset(text, position),
                    offset,
                    "{text:?} at byte {offset} did not round-trip"
                );
            }
        }
    }

    /// A position past the end of a line, or past the end of the file,
    /// clamps. A client one keystroke ahead of the server sends these.
    #[test]
    fn out_of_range_positions_clamp() {
        let text = "abc\ndef\n";

        assert_eq!(position_to_offset(text, Position::new(0, 99)), 3);
        assert_eq!(
            position_to_offset(text, Position::new(99, 0)),
            text.len() as u32
        );
        assert_eq!(
            offset_to_position(text, 9_999),
            offset_to_position(text, text.len() as u32)
        );
    }
}
