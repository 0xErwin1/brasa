//! What the server answers, asked of the analysis directly.
//!
//! The transport is not exercised here — `lsp-server` owns the framing
//! and `lsp-types` owns the shapes, and neither is ours to test. What
//! IS ours is every answer: which byte a hover lands on, what it says
//! about a type and an error-set, and whether a file mid-edit still
//! produces any of it.

use std::path::{Path, PathBuf};

use brasa_lsp::analysis::{self, Analysis};
use brasa_module::Overlay;

/// Analyses `source` as if it were an open, unsaved buffer at a path
/// that does not exist on disk.
///
/// That is not a shortcut — it is the case an editor produces most
/// often, and doing it this way means these tests exercise the overlay
/// rather than a temp-file arrangement that would not.
fn analyze(source: &str) -> (Analysis, PathBuf) {
    let path = std::env::temp_dir().join("brasa-lsp-test/main.bras");

    let mut overlay = Overlay::new();
    overlay.insert(path.clone(), source.to_string());

    (analysis::analyze(&path, &overlay), path)
}

/// The byte offset just after `needle` starts — where a caret sits when
/// a user has just typed the name and hovers it.
fn at(source: &str, needle: &str) -> u32 {
    source.find(needle).expect("the needle is in the source") as u32
}

fn hover_at(source: &str, needle: &str) -> Option<(Option<String>, Option<String>)> {
    let (analysis, path) = analyze(source);
    let file = analysis.file_of(&path)?;
    let hover = analysis.hover(file, at(source, needle))?;

    Some((hover.ty, hover.throws))
}

const SIMPLE: &str = r#"def double(n: int): int
  n * 2
end

def main()
  let count = 21
  let doubled = double(count)
  puts doubled
end
"#;

#[test]
fn a_local_hovers_as_its_inferred_type() {
    let (ty, _) = hover_at(SIMPLE, "count = 21").expect("a hover on `count`");
    assert_eq!(ty.as_deref(), Some("int"));
}

/// The innermost node wins. Hovering the argument of a call must answer
/// about the argument, not about the call that encloses it — the two
/// spans both cover the byte, and only the nesting rule separates them.
#[test]
fn the_innermost_expression_wins_over_its_enclosing_call() {
    let source = "def main()\n  let n = 1\n  puts(n)\nend\n";

    let (ty, _) = hover_at(source, "n)").expect("a hover on the argument");
    assert_eq!(ty.as_deref(), Some("int"));
}

/// The feature the ticket is named for. A function that throws shows
/// its INFERRED set, which is the thing a user has no other way to see:
/// nothing in the source says `string.ParseError`.
#[test]
fn a_throwing_function_hovers_with_its_inferred_error_set() {
    let source = r#"def parse(text: string): int
  text.toInt()
end
"#;

    let (_, throws) = hover_at(source, "text.toInt").expect("a hover inside `parse`");
    assert_eq!(throws.as_deref(), Some("throws string.ParseError"));
}

/// And a function that cannot throw says so, rather than saying
/// nothing. "throws never" is an answer; absence would read as "the
/// server does not know".
#[test]
fn an_infallible_function_hovers_as_throws_never() {
    let (_, throws) = hover_at(SIMPLE, "n * 2").expect("a hover inside `double`");
    assert_eq!(throws.as_deref(), Some("throws never"));
}

/// A call site shows what the CALLEE throws, which is where the
/// question is usually asked: the caller wants to know what it has to
/// handle before it writes the `catch`.
#[test]
fn a_call_hovers_with_the_callees_error_set() {
    let source = r#"def parse(text: string): int
  text.toInt()
end

def main()
  let n = parse("42")
end
"#;

    let (_, throws) = hover_at(source, "parse(\"42\")").expect("a hover on the call");
    assert_eq!(throws.as_deref(), Some("throws string.ParseError"));
}

/// The property BRS-114 settled, asked of the LSP rather than of the
/// pipeline: a file that does not parse still answers. If this ever
/// fails, the editor goes blank exactly when the user is typing.
#[test]
fn a_file_mid_edit_still_answers() {
    let source = "def main()\n  let count = 21\n  let x = \nend\n";

    let (analysis, path) = analyze(source);
    let file = analysis.file_of(&path).expect("the buffer is in the graph");

    assert!(
        !analysis.diagnostics.is_empty(),
        "a file with a hole must report something"
    );

    let hover = analysis
        .hover(file, at(source, "count = 21"))
        .expect("the sound part of a broken file still hovers");
    assert_eq!(hover.ty.as_deref(), Some("int"));
}

/// An overlay decides what a file SAYS. This is the property the whole
/// editor story rests on: the buffer, not the disk, is what gets
/// analysed.
#[test]
fn the_overlay_is_what_gets_analysed() {
    let path = std::env::temp_dir().join("brasa-lsp-test/overlaid.bras");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, "def main()\n  let x = 1\nend\n").unwrap();

    let mut overlay = Overlay::new();
    overlay.insert(
        path.clone(),
        "def main()\n  let x = \"text\"\nend\n".to_string(),
    );

    let analysis = analysis::analyze(&path, &overlay);
    let file = analysis.file_of(&path).expect("the file is in the graph");

    let source = "def main()\n  let x = \"text\"\nend\n";
    let hover = analysis
        .hover(file, at(source, "x = "))
        .expect("a hover on the overlaid binding");

    assert_eq!(
        hover.ty.as_deref(),
        Some("string"),
        "the buffer says string; only disk still says int"
    );

    std::fs::remove_file(&path).ok();
}

/// A path the analysis never loaded has no `FileId`, so an editor
/// asking about an unrelated document gets nothing instead of an answer
/// drawn from the wrong file.
#[test]
fn a_file_outside_the_graph_is_not_answered_for() {
    let (analysis, _) = analyze(SIMPLE);

    assert_eq!(analysis.file_of(Path::new("/nowhere/else.bras")), None);
}

/// The bug the end-to-end drive found. A binder's span is the whole
/// `let` statement, not just the name, so a hover that preferred
/// binders over expressions answered about the binding wherever on the
/// line the cursor was — hovering the CALL gave `n`.
///
/// Smallest-span-wins is what fixes it, and this pins that the two
/// positions now give different answers.
#[test]
fn a_binder_does_not_swallow_the_expression_beside_it() {
    let source = r#"def parse(text: string): int
  text.toInt()
end

def main()
  let n = parse("42")
end
"#;

    let (analysis, path) = analyze(source);
    let file = analysis.file_of(&path).expect("the buffer is in the graph");

    let binder = analysis
        .hover(file, at(source, "n = parse"))
        .expect("a hover on the binder");
    let call = analysis
        .hover(file, at(source, "parse(\"42\")"))
        .expect("a hover on the call");

    assert_ne!(
        binder.span, call.span,
        "the binder and the call it initializes are different nodes"
    );
    assert!(
        span_len(call.span) < span_len(binder.span),
        "the call is inside the statement that binds it"
    );
}

fn span_len(span: brasa_source::Span) -> u32 {
    span.end.0 - span.start.0
}
