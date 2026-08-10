//! Regression tests for the review-findings hardening pass: `unit` as a
//! type, a parser recursion-depth guard, unknown-escape errors, duplicate
//! struct-literal fields, diagnostic-cascade dedup, a leading UTF-8 BOM,
//! `0x_`/`0b_` vs genuine overflow, and non-empty enum/interface bodies.

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

// -- underscore-placement leniency stays as-is (documenting, not testing a
// -- new rule): `1_` and `1__000` remain accepted, per spec ruling.

#[test]
fn lenient_underscore_placement_still_accepted() {
    assert_clean("1_\n");
    assert_clean("1__000\n");
}
