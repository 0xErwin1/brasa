//! Regression tests for the review-findings hardening pass: `unit` as a
//! type, a parser recursion-depth guard, unknown-escape errors, duplicate
//! struct-literal fields, diagnostic-cascade dedup, a leading UTF-8 BOM,
//! `0x_`/`0b_` vs genuine overflow, non-empty enum/interface bodies,
//! error recovery inside parenthesized/tuple expressions, and recovery
//! that stays inside the bracket it started in.

use brasa_source::SourceMap;

fn parse(source: &str) -> brasa_parser::ParseResult {
    let mut source_map = SourceMap::new();
    let file = source_map.add_virtual("t", source.to_string());
    brasa_parser::parse(source, file)
}

fn assert_clean(source: &str) {
    let result = parse(source);
    assert!(
        result.diagnostics.is_empty(),
        "expected zero diagnostics for {source:?}, got: {:#?}",
        result.diagnostics
    );
}

fn messages(result: &brasa_parser::ParseResult) -> Vec<String> {
    result
        .diagnostics
        .iter()
        .map(|d| d.message.clone())
        .collect()
}

// -- (1) `unit` usable as a type -------------------------------------------

#[test]
fn unit_as_return_type_parses_clean() {
    assert_clean("def f(): unit\n  0\nend\n");
}

#[test]
fn unit_as_param_type_parses_clean() {
    assert_clean("def f(x: unit): int\n  0\nend\n");
}

#[test]
fn unit_as_generic_arg_parses_clean() {
    assert_clean("def f(x: Vector<unit>): int\n  0\nend\n");
}

// -- (2) recursion-depth guard ---------------------------------------------

fn nested_parens(depth: usize) -> String {
    format!("{}1{}", "(".repeat(depth), ")".repeat(depth))
}

#[test]
fn deep_nesting_below_limit_parses_clean() {
    assert_clean(&nested_parens(400));
}

#[test]
fn deep_nesting_above_limit_reports_diagnostic_and_survives() {
    // The critical assertion here is implicit: if the recursion guard did
    // not stop descent, this call would blow the native stack and abort
    // the whole test process (a stack overflow cannot be caught as a
    // panic), so simply reaching the assertions below is proof the guard
    // worked.
    let result = parse(&nested_parens(600));
    assert_eq!(
        result.diagnostics.len(),
        1,
        "expected exactly one diagnostic, got: {:#?}",
        result.diagnostics
    );
    assert!(
        result.diagnostics[0].message.contains("nests too deep")
            || result.diagnostics[0].message.contains("nesting too deep")
    );
}

#[test]
fn pathological_nesting_still_survives() {
    // Mirrors the finding's own repro shape (~15k levels in under 1MB of
    // source): the guard must cut this off long before it, not just
    // before the smaller 600-level fixture above.
    let result = parse(&nested_parens(15_000));
    assert_eq!(result.diagnostics.len(), 1);
}

#[test]
fn deep_vector_nesting_is_guarded() {
    let source = format!("{}1{}", "[".repeat(600), "]".repeat(600));
    let result = parse(&source);
    assert_eq!(result.diagnostics.len(), 1);
}

#[test]
fn deep_call_nesting_is_guarded() {
    let source = format!("{}1{}", "f(".repeat(600), ")".repeat(600));
    let result = parse(&source);
    assert_eq!(result.diagnostics.len(), 1);
}

#[test]
fn deep_type_nesting_is_guarded() {
    let source = format!(
        "def f(x: {}int{}): int\n  0\nend\n",
        "Vector<".repeat(600),
        ">".repeat(600)
    );
    let result = parse(&source);
    assert_eq!(result.diagnostics.len(), 1);
}

#[test]
fn deep_pattern_nesting_is_guarded() {
    let mut source = String::from("match x\n  ");
    source.push_str(&"(".repeat(600));
    source.push('_');
    source.push_str(&")".repeat(600));
    source.push_str(" => 1\nend\n");
    let result = parse(&source);
    assert_eq!(result.diagnostics.len(), 1);
}

#[test]
fn deep_if_block_nesting_is_guarded() {
    let mut source = String::new();
    for _ in 0..600 {
        source.push_str("if true\n");
    }
    source.push_str("1\n");
    for _ in 0..600 {
        source.push_str("end\n");
    }
    let result = parse(&source);
    assert_eq!(result.diagnostics.len(), 1);
}

// -- (2b) tree-depth guard --------------------------------------------------
// The parser's recursion counter only sees how deep it descends. These
// shapes are built by the Pratt/postfix loops instead, so they cost a
// constant number of parser frames while nesting the tree once per term;
// every later phase then walks that tree with real recursion. Each case
// must report `P002` rather than hand a stack-overflowing tree onward.
// The oversized sources are generated here rather than committed.

fn assert_one_nesting_diagnostic(source: &str) {
    let result = parse(source);
    assert_eq!(
        result.diagnostics.len(),
        1,
        "expected exactly one diagnostic, got: {:#?}",
        result.diagnostics
    );
    assert_eq!(result.diagnostics[0].error_code, "P002");
    assert!(result.diagnostics[0].message.contains("nesting too deep"));
}

#[test]
fn left_leaning_operator_chain_is_guarded() {
    let source = format!("puts {}\n", vec!["1"; 20_000].join("+"));
    assert_one_nesting_diagnostic(&source);
}

#[test]
fn right_leaning_operator_chain_is_guarded() {
    let source = format!("puts {}1{}\n", "(1+".repeat(20_000), ")".repeat(20_000));
    assert_one_nesting_diagnostic(&source);
}

#[test]
fn deep_method_chain_is_guarded() {
    let source = format!("let s = \"a\"\nputs s{}\n", ".trim()".repeat(20_000));
    assert_one_nesting_diagnostic(&source);
}

#[test]
fn deep_pipe_chain_is_guarded() {
    let source = format!(
        "def id(x: int): int\n  x\nend\n\nputs 1{}\n",
        " |> id()".repeat(20_000)
    );
    assert_one_nesting_diagnostic(&source);
}

#[test]
fn deeply_nested_data_literal_is_guarded() {
    let source = format!("puts {}1{}.len()\n", "[".repeat(20_000), "]".repeat(20_000));
    assert_one_nesting_diagnostic(&source);
}

#[test]
fn a_chain_within_the_limit_still_parses_clean() {
    assert_clean(&format!("puts {}\n", vec!["1"; 400].join("+")));
}

// -- (3) unknown escapes are errors, in both strings and chars -------------

#[test]
fn unknown_escape_in_char_literal_is_an_error() {
    let result = parse("'\\q'\n");
    assert!(
        !result.diagnostics.is_empty(),
        "expected an error for an unknown escape in a char literal"
    );
    assert!(result.diagnostics[0].message.contains("unknown escape"));
}

#[test]
fn unknown_escape_in_string_literal_is_an_error() {
    let result = parse("\"a\\qb\"\n");
    assert!(
        !result.diagnostics.is_empty(),
        "expected an error for an unknown escape in a string literal"
    );
    assert!(result.diagnostics[0].message.contains("unknown escape"));
}

#[test]
fn valid_char_escapes_still_decode_with_no_diagnostics() {
    assert_clean("'\\n'\n");
    assert_clean("'\\t'\n");
    assert_clean("'\\\"'\n");
    assert_clean("'\\\\'\n");
    assert_clean("'\\#'\n");
}

#[test]
fn valid_string_escapes_still_decode_with_no_diagnostics() {
    assert_clean("\"a\\nb\\tc\\\"d\\\\e\\#f\"\n");
}

#[test]
fn raw_string_with_backslash_stays_literal_with_no_diagnostics() {
    assert_clean("\"\"\"a\\qb\"\"\"\n");
}

// -- (4) duplicate struct-literal fields ------------------------------------

#[test]
fn duplicate_struct_literal_field_is_reported() {
    let result = parse("Point { x: 1, x: 2 }\n");
    assert_eq!(
        result.diagnostics.len(),
        1,
        "expected exactly one diagnostic, got: {:#?}",
        result.diagnostics
    );
    assert!(result.diagnostics[0].message.contains('x'));
    assert!(result.diagnostics[0].message.contains("duplicate"));
}

#[test]
fn distinct_struct_literal_fields_parse_clean() {
    assert_clean("Point { x: 1, y: 2 }\n");
}

// -- (5) diagnostic-cascade dedup -------------------------------------------

#[test]
fn double_mut_yields_one_diagnostic_not_a_cascade() {
    let result = parse("let mut mut a = 1\n");
    insta::assert_debug_snapshot!(messages(&result));
}

#[test]
fn reserved_word_as_param_name_yields_bounded_diagnostics() {
    let result = parse("def f(let: int)\nend\n");
    insta::assert_debug_snapshot!(messages(&result));
}

#[test]
fn malformed_char_literal_yields_bounded_diagnostics() {
    let result = parse("'ab'\n");
    insta::assert_debug_snapshot!(messages(&result));
}

// -- (6) leading UTF-8 BOM ---------------------------------------------------

#[test]
fn leading_bom_is_stripped_and_parses_identically() {
    let without_bom = "let x = 1\n";
    let with_bom = format!("\u{FEFF}{without_bom}");

    let plain = parse(without_bom);
    let bomed = parse(&with_bom);

    assert!(plain.diagnostics.is_empty());
    assert!(
        bomed.diagnostics.is_empty(),
        "expected a leading BOM to parse cleanly, got: {:#?}",
        bomed.diagnostics
    );

    let plain_dump = brasa_parser::dump::dump(&plain.ast, &plain.roots);
    let bomed_dump = brasa_parser::dump::dump(&bomed.ast, &bomed.roots);
    assert_eq!(plain_dump, bomed_dump);
}

// -- (7) `0x_`/`0b_` (no digits) vs genuine overflow -------------------------

#[test]
fn hex_prefix_with_no_digits_reports_no_digits_message() {
    let result = parse("0x_\n");
    assert_eq!(result.diagnostics.len(), 1);
    assert!(
        result.diagnostics[0].message.contains("no digits")
            || result.diagnostics[0].message.contains("prefix"),
        "expected a 'no digits after prefix' style message, got: {}",
        result.diagnostics[0].message
    );
}

#[test]
fn binary_prefix_with_no_digits_reports_no_digits_message() {
    let result = parse("0b_\n");
    assert_eq!(result.diagnostics.len(), 1);
    assert!(
        result.diagnostics[0].message.contains("no digits")
            || result.diagnostics[0].message.contains("prefix"),
        "expected a 'no digits after prefix' style message, got: {}",
        result.diagnostics[0].message
    );
}

#[test]
fn genuine_overflow_still_reports_out_of_range() {
    let result = parse("99999999999999999999\n");
    assert_eq!(result.diagnostics.len(), 1);
    assert!(result.diagnostics[0].message.contains("out of range"));
}

// -- (8) non-empty enum/interface bodies ------------------------------------

#[test]
fn empty_enum_body_is_reported() {
    let result = parse("enum Color\nend\n");
    assert_eq!(
        result.diagnostics.len(),
        1,
        "expected exactly one diagnostic, got: {:#?}",
        result.diagnostics
    );
    assert!(result.diagnostics[0].message.contains("variant"));
}

#[test]
fn non_empty_enum_body_parses_clean() {
    assert_clean("enum Color\n  Red\n  Green\nend\n");
}

#[test]
fn empty_interface_body_is_reported() {
    let result = parse("interface Fetcher\nend\n");
    assert_eq!(
        result.diagnostics.len(),
        1,
        "expected exactly one diagnostic, got: {:#?}",
        result.diagnostics
    );
    assert!(result.diagnostics[0].message.contains("member"));
}

#[test]
fn non_empty_interface_body_parses_clean() {
    assert_clean("interface Fetcher\n  def fetch(url: string): string\nend\n");
}

// -- (9) parenthesized/tuple error recovery ---------------------------------
// The tuple element loop in `parse_paren_expr` only ever runs while the
// cursor sits on a comma, and consuming that comma always advances the
// cursor, so every case below must terminate. Each test reaching its
// assertions is the proof: a non-terminating loop would hang the suite
// rather than fail it. The snapshots pin the diagnostics that recovery
// produces, which is the part a future change can silently regress.

/// `()` is not a zero-element tuple (spec: 02 — Gramática formal): the unit
/// value is spelled `unit`, so this stays a parse error.
#[test]
fn empty_parens_are_a_parse_error_not_a_zero_tuple() {
    let result = parse("let p = ()\n");
    insta::assert_debug_snapshot!(messages(&result));
}

#[test]
fn tuple_with_a_missing_element_recovers_with_bounded_diagnostics() {
    let result = parse("let p = (1,,2)\n");
    insta::assert_debug_snapshot!(messages(&result));
}

#[test]
fn unterminated_tuple_at_eof_reports_the_missing_paren() {
    let result = parse("let p = (1, 2");
    insta::assert_debug_snapshot!(messages(&result));
}

#[test]
fn tuple_trailing_comma_at_eof_reports_the_missing_paren() {
    let result = parse("let p = (1,");
    insta::assert_debug_snapshot!(messages(&result));
}

/// A leading comma never reaches the tuple loop: the first element is
/// already missing, so this fails as a parenthesized expression.
#[test]
fn leading_comma_in_parens_fails_as_a_grouping() {
    let result = parse("let p = (,)\n");
    insta::assert_debug_snapshot!(messages(&result));
}

#[test]
fn open_paren_at_eof_reports_one_diagnostic() {
    let result = parse("let p = (");
    insta::assert_debug_snapshot!(messages(&result));
}

#[test]
fn nested_one_element_tuples_parse_clean() {
    assert_clean("let p = ((1,),)\n");
}

#[test]
fn unterminated_nested_tuple_at_eof_reports_the_missing_paren() {
    let result = parse("let p = ((1,),");
    insta::assert_debug_snapshot!(messages(&result));
}

/// Repeated missing elements must not cascade one diagnostic per comma.
#[test]
fn repeated_missing_tuple_elements_stay_bounded() {
    let result = parse("let p = (1,,,2)\n");
    insta::assert_debug_snapshot!(messages(&result));
}

/// The tuple path and the vector path are the same recovery shape: the
/// same single "expected an expression" for a missing element, and the
/// same "expected <closer> to close the ..." for the unterminated form.
/// Only the delimiter description may differ.
#[test]
fn tuple_recovery_matches_vector_recovery_in_shape() {
    let tuple_missing = messages(&parse("let p = (1,,2)\n"));
    let vector_missing = messages(&parse("let v = [1,,2]\n"));

    assert_eq!(tuple_missing, vector_missing);
    assert_eq!(
        tuple_missing.len(),
        1,
        "a delimiter that is present must not also be reported missing, got: {tuple_missing:#?}"
    );

    let tuple_unterminated = messages(&parse("let p = (1, 2"));
    let vector_unterminated = messages(&parse("let v = [1, 2"));

    assert_eq!(tuple_unterminated.len(), vector_unterminated.len());
    assert_eq!(
        tuple_unterminated[0]
            .replace("')'", "X")
            .replace("tuple", "Y"),
        vector_unterminated[0]
            .replace("']'", "X")
            .replace("vector literal", "Y")
    );
}

// -- underscore-placement leniency stays as-is (documenting, not testing a
// -- new rule): `1_` and `1__000` remain accepted, per spec ruling.

#[test]
fn lenient_underscore_placement_still_accepted() {
    assert_clean("1_\n");
    assert_clean("1__000\n");
}

// -- (10) recovery inside an unfinished bracket ------------------------------
// A failed expression inside `(`, `[` or `{` must not skip past the closer
// its opener still has to consume: doing so turns one mistake into a
// second, far-reaching "expected <closer>" report against a delimiter that
// is sitting right there.

#[test]
fn an_empty_index_reports_exactly_one_diagnostic() {
    for source in ["puts []\n", "v[]\n", "puts []\nlet b = 2\nlet c = 3\n"] {
        let result = parse(source);

        assert_eq!(
            result.diagnostics.len(),
            1,
            "expected exactly one diagnostic for {source:?}, got: {:#?}",
            result.diagnostics
        );
        assert_eq!(
            result.diagnostics[0].message,
            "expected an expression, found `]`"
        );
    }
}

/// The empty index is diagnosed at the brackets themselves, not at
/// whatever follows them: a span reaching to the end of a long file points
/// the reader nowhere near the mistake.
#[test]
fn an_empty_index_is_reported_at_the_brackets() {
    let source = "puts []\nlet b = 2\nlet c = 3\n";
    let result = parse(source);
    let span = result.diagnostics[0].primary_span;

    assert_eq!(&source[span.start.0 as usize..span.end.0 as usize], "]");
}

/// Newlines are insignificant inside brackets, so recovery must not treat
/// the end of a line as a boundary: an index spanning lines is well-formed.
#[test]
fn a_multi_line_index_still_parses_clean() {
    assert_clean("let a = v[\n  someLongExpression\n]\n");
    assert_clean("let a = v[\n  1 + 2\n]\n");
}

/// The same gap in the other bracketing constructs: an argument list and a
/// grouping recover in place too, across lines as well as within one.
#[test]
fn a_failed_expression_inside_other_brackets_reports_exactly_one_diagnostic() {
    for source in ["f(,)\n", "v[(1 + ]\n", "let a = f(\n  ,\n  2\n)\n"] {
        let result = parse(source);

        assert_eq!(
            result.diagnostics.len(),
            1,
            "expected exactly one diagnostic for {source:?}, got: {:#?}",
            result.diagnostics
        );
    }
}
