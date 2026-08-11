use brasa_source::FileId;

use crate::lex;

/// Renders the token stream as `Kind "slice"` lines, one per token, for
/// readable insta snapshots. `Eof`'s slice is always empty.
fn render(source: &str) -> String {
    let (tokens, errors) = lex(source, FileId::new(0));
    let mut out = String::new();

    for token in &tokens {
        let slice = &source[token.span.start.0 as usize..token.span.end.0 as usize];
        out.push_str(&format!("{:?} {:?}\n", token.kind, slice));
    }

    if !errors.is_empty() {
        out.push_str("--- errors ---\n");
        for error in &errors {
            out.push_str(&format!(
                "{}..{}: {}\n",
                error.span.start.0, error.span.end.0, error.message
            ));
        }
    }

    out
}

macro_rules! snapshot {
    ($name:ident, $source:expr) => {
        #[test]
        fn $name() {
            insta::assert_snapshot!(render($source));
        }
    };
}

// --- operators & punctuation ---

snapshot!(op_arithmetic, "+ - * / % **");
snapshot!(op_comparison, "== != < <= > >=");
snapshot!(op_logical, "&& || !");
snapshot!(op_assignment, "= += -= *= /= %=");
snapshot!(op_pipe_nav_range_arrow, "|> ?. ?? .. ..= => -> ::");
snapshot!(punctuation, "( ) [ ] { } , : . | _");

// --- ambiguous pairs: longest match must win ---

snapshot!(ambiguous_star, "* **");
snapshot!(ambiguous_dot, ". .. ..=");
snapshot!(ambiguous_pipe, "| || |>");
snapshot!(ambiguous_colon, ": ::");
snapshot!(ambiguous_eq, "= == =>");
snapshot!(ambiguous_question, "?? ?.");
snapshot!(lambda_no_params, "|_|");

// --- keywords vs identifiers ---

snapshot!(keyword_if_vs_ident, "if iffy");
snapshot!(keyword_do_vs_ident, "do door");
snapshot!(predicate_ident, "isDir?");
snapshot!(bang_ident, "sort!");
snapshot!(type_ident, "Vector");
snapshot!(bare_underscore, "_");
snapshot!(safe_nav_after_ident, "user.nickname?.len()");
snapshot!(not_eq_after_bang_ident, "sort!=x");
snapshot!(predicate_ident_not_keyword, "empty? save!");

// `catch!` is the one keyword with an `IDENT` suffix, so keyword lookup
// has to see the absorbed suffix without letting it swallow `!=` or a
// spaced `!`.
snapshot!(keyword_catch_bang, "catch!");
snapshot!(keyword_catch_plain, "catch");
snapshot!(keyword_catch_not_eq, "catch!=x");
snapshot!(keyword_catch_spaced_bang, "catch !");
snapshot!(catch_all_is_not_a_keyword, "catch_all");

// --- numerics ---

snapshot!(int_zero, "0");
snapshot!(int_plain, "42");
snapshot!(int_underscored, "1_000_000");
snapshot!(int_hex, "0xFF_AB");
snapshot!(int_binary, "0b10_10");
snapshot!(float_plain, "3.14");
snapshot!(float_exp, "1.0e-9");

#[test]
fn parse_int_and_float_helpers_match_lexed_values() {
    assert_eq!(brasa_token::parse_int("1_000_000"), Ok(1_000_000));
    assert_eq!(brasa_token::parse_int("0xFF_AB"), Ok(0xFF_AB));
    assert_eq!(brasa_token::parse_int("0b10_10"), Ok(0b10_10));
    assert_eq!(brasa_token::parse_float("1.0e-9"), Some(1.0e-9));
    assert_eq!(
        brasa_token::parse_int("99999999999999999999"),
        Err(brasa_token::IntParseError::Overflow)
    );
}

// --- newlines & comments ---

snapshot!(newline_emission, "a\nb");
snapshot!(crlf_newline_is_one_token, "a\r\nb");
snapshot!(comment_skipped, "a # trailing comment\nb");
snapshot!(comment_then_newline, "# just a comment\n");

// --- char literals ---

snapshot!(char_ascii, "'a'");
snapshot!(char_multibyte, "'\u{f1}'");

// --- strings ---

snapshot!(string_empty, "\"\"");
snapshot!(string_plain, "\"hello\"");
snapshot!(string_escapes, "\"a\\nb\\tc\\\"d\\\\e\\#f\"");
snapshot!(string_simple_interp, "\"a#{x + 1}b\"");
snapshot!(string_interp_map_literal, "\"#{ {\"a\": 1} }\"");
snapshot!(string_nested_interp, "\"x#{ \"inner #{y}\" }z\"");
snapshot!(
    raw_string_multiline_interp,
    "\"\"\"line one\nline #{2}\ndone\"\"\""
);

// --- errors & recovery ---

snapshot!(unterminated_string, "\"abc");
snapshot!(unterminated_interp, "\"a#{1 + 2");
snapshot!(unknown_char, "a @ b");

#[test]
fn lexing_continues_after_an_error() {
    let (tokens, errors) = lex("a @ b", FileId::new(0));
    assert_eq!(errors.len(), 1);
    // Ident("a"), Error("@"), Ident("b"), Eof
    assert_eq!(tokens.len(), 4);
    assert_eq!(tokens[0].kind, brasa_token::TokenKind::Ident);
    assert_eq!(tokens[1].kind, brasa_token::TokenKind::Error);
    assert_eq!(tokens[2].kind, brasa_token::TokenKind::Ident);
    assert_eq!(tokens[3].kind, brasa_token::TokenKind::Eof);
}

// --- fix3: a leading UTF-8 BOM is skipped, spans stay absolute ---

#[test]
fn fix3_leading_bom_is_skipped_and_spans_index_into_the_original_source() {
    let source = "\u{FEFF}let x = 1\n";
    let (tokens, errors) = lex(source, FileId::new(0));

    assert!(errors.is_empty(), "unexpected lex errors: {errors:#?}");

    // The BOM is 3 bytes; `let` must start right after it, and every span
    // must still index correctly into the original (BOM-prefixed) source.
    let let_token = tokens[0];
    assert_eq!(let_token.kind, brasa_token::TokenKind::Let);
    assert_eq!(let_token.span.start.0, 3);
    assert_eq!(
        &source[let_token.span.start.0 as usize..let_token.span.end.0 as usize],
        "let"
    );
}

// --- integration: the full example program from docs/spec/01-syntax.md ---

snapshot!(
    example_program,
    r##"import std::fs
import std::json

struct Repo
  name: string
  stars: int
end

def topRepos(path: string, min: int): Vector<Repo>
  let data = json.parse(fs.read(path))
  data.repos
    |> filter(|r| r.stars >= min)
    |> sortBy(|r| -r.stars)
end

let repos = topRepos("repos.json", 100) catch (e)
  fs.NotFound => []
end

for repo in repos
  puts "#{repo.name}: #{repo.stars}"
end
"##
);
