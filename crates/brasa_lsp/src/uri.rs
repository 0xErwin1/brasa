//! Between `file:` URIs and paths.
//!
//! `lsp-types` 0.97 models a URI as a parsed, opaque
//! `fluent_uri::Uri` — it will tell you the scheme and hand back the
//! string, and deliberately says nothing about filesystems. The `url`
//! crate's `from_file_path`/`to_file_path` do not exist here, so the
//! conversion is ours.
//!
//! It is worth doing carefully rather than by `format!`. A path with a
//! space in it is the common case that a naive encoder gets wrong, and
//! the failure is silent: the client sends `file:///tmp/my%20file.bras`
//! and a server that does not decode looks for a file called
//! `my%20file.bras`, finds nothing, and reports no diagnostics at all.
//!
//! POSIX paths only. Brasa's toolchain targets Linux (`AGENTS.md`), and
//! a Windows path in a URI has a drive-letter form this does not try to
//! guess at.

use std::path::{Path, PathBuf};

use lsp_types::Uri;

/// The path a `file:` URI names, or `None` for any other scheme —
/// which this server has nothing to say about.
pub fn to_path(uri: &Uri) -> Option<PathBuf> {
    let text = uri.as_str();
    let rest = text.strip_prefix("file://")?;

    // `file:///a/b` has an empty authority; `file://host/a/b` names
    // another machine, and a language server has no business guessing
    // what that means locally.
    let path = match rest.find('/') {
        Some(0) => rest,
        _ => return None,
    };

    Some(PathBuf::from(percent_decode(path)))
}

/// The `file:` URI for a path, or `None` when the path is relative —
/// a URI has no way to express one, and inventing a base would make
/// the server report against a file the client cannot match.
pub fn from_path(path: &Path) -> Option<Uri> {
    if !path.is_absolute() {
        return None;
    }

    let text = format!("file://{}", percent_encode(path.to_str()?));
    text.parse().ok()
}

/// Decodes `%XX` escapes. An escape that is not two hex digits is left
/// as written: it is not a valid encoding, and mangling it further
/// would lose the only evidence of what the client actually sent.
fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;

    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[index + 1..index + 3]).ok();
            if let Some(byte) = hex.and_then(|hex| u8::from_str_radix(hex, 16).ok()) {
                out.push(byte);
                index += 3;
                continue;
            }
        }
        out.push(bytes[index]);
        index += 1;
    }

    String::from_utf8_lossy(&out).into_owned()
}

/// Escapes everything a path segment may not carry literally.
///
/// The unreserved set of RFC 3986 plus `/`, which separates segments
/// and must survive. Encoding conservatively is safe; under-encoding
/// is what produces a URI a client parses differently than we wrote it.
fn percent_encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());

    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_path_round_trips() {
        let path = Path::new("/home/user/script.bras");
        let uri = from_path(path).expect("an absolute path has a URI");

        assert_eq!(uri.as_str(), "file:///home/user/script.bras");
        assert_eq!(to_path(&uri).as_deref(), Some(path));
    }

    /// The case a naive `format!` gets wrong, and gets wrong silently:
    /// the server would look for a file whose name contains a literal
    /// `%20` and report nothing at all.
    #[test]
    fn a_path_with_a_space_round_trips() {
        let path = Path::new("/tmp/my file.bras");
        let uri = from_path(path).expect("an absolute path has a URI");

        assert_eq!(uri.as_str(), "file:///tmp/my%20file.bras");
        assert_eq!(to_path(&uri).as_deref(), Some(path));
    }

    /// Non-ASCII is percent-encoded per UTF-8 byte, not per character.
    #[test]
    fn a_non_ascii_path_round_trips() {
        let path = Path::new("/tmp/café/año.bras");
        let uri = from_path(path).expect("an absolute path has a URI");

        assert!(uri.as_str().starts_with("file:///tmp/caf%C3%A9/"));
        assert_eq!(to_path(&uri).as_deref(), Some(path));
    }

    /// A scheme this server cannot read a file from gets nothing,
    /// rather than a path invented from the rest of the URI.
    #[test]
    fn a_non_file_scheme_has_no_path() {
        let uri: Uri = "untitled:Untitled-1".parse().unwrap();
        assert_eq!(to_path(&uri), None);

        let uri: Uri = "https://example.com/a.bras".parse().unwrap();
        assert_eq!(to_path(&uri), None);
    }

    /// A URI naming another host is not a local file, and guessing that
    /// it is would make the server read the wrong thing.
    #[test]
    fn a_remote_authority_has_no_local_path() {
        let uri: Uri = "file://otherhost/tmp/a.bras".parse().unwrap();
        assert_eq!(to_path(&uri), None);
    }

    /// A relative path cannot be expressed, and must not be guessed at.
    #[test]
    fn a_relative_path_has_no_uri() {
        assert!(from_path(Path::new("script.bras")).is_none());
    }

    /// A malformed escape is left alone rather than half-decoded.
    #[test]
    fn a_malformed_escape_survives() {
        assert_eq!(percent_decode("/tmp/100%done"), "/tmp/100%done");
        assert_eq!(percent_decode("/tmp/a%2"), "/tmp/a%2");
    }
}
