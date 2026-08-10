//! Precedence, newline handling, trailing `do`-blocks, error recovery,
//! `if` forms, and generics: the fragment-level suite called for in the
//! BRS-10/11 task, complementing the whole-file `examples` suite.

use brasa_source::SourceMap;

fn dump_source(source: &str) -> String {
    let mut source_map = SourceMap::new();
    let file = source_map.add_virtual("t", source.to_string());
    let result = brasa_parser::parse(source, file);

    assert!(
        result.diagnostics.is_empty(),
        "unexpected diagnostics for {source:?}: {:#?}",
        result.diagnostics
    );

    brasa_parser::dump::dump(&result.ast, &result.roots)
}

macro_rules! snapshot_test {
    ($name:ident, $source:expr) => {
        #[test]
        fn $name() {
            insta::assert_snapshot!(stringify!($name), dump_source($source));
        }
    };
}

// -- (b) precedence -----------------------------------------------------

snapshot_test!(prec_add_mul, "1 + 2 * 3");
snapshot_test!(prec_pow_right_assoc, "2 ** 3 ** 2");
snapshot_test!(prec_unary_binds_tighter_than_pow, "-x ** 2");
snapshot_test!(prec_sub_left_assoc, "a - b - c");
snapshot_test!(prec_pipe_chain, "a |> f(b) |> g(c)");
snapshot_test!(prec_range, "0..10");
snapshot_test!(prec_not_dot_pred, "!x.valid?");
snapshot_test!(prec_index_chain, "items[0].name");
snapshot_test!(
    prec_catch_inside_larger_expr,
    "1 + (fetch(u) catch (e) NetError => \"x\" end).len()"
);

#[test]
fn prec_chained_range_is_rejected() {
    let mut source_map = SourceMap::new();
    let source = "a..b..c";
    let file = source_map.add_virtual("t", source.to_string());
    let result = brasa_parser::parse(source, file);

    assert_eq!(
        result.diagnostics.len(),
        1,
        "expected exactly one diagnostic for a chained range, got {:#?}",
        result.diagnostics
    );
    assert!(result.diagnostics[0].message.contains("non-associative"));
}

snapshot_test!(prec_coalesce_binds_looser_than_or, "x ?? y || z");
snapshot_test!(prec_coalesce_left_assoc_chain, "x ?? y ?? z");
snapshot_test!(prec_coalesce_then_pipe, "a ?? b |> f(c)");

#[test]
fn prec_eq_and_lt() {
    insta::assert_snapshot!("prec_eq_and_lt", dump_source("a == b && c < d"));
}

// -- (c) newline handling -------------------------------------------------

snapshot_test!(newline_multiline_call_args, "f(\n  a,\n  b,\n  c\n)");
snapshot_test!(
    newline_leading_pipe_continuation,
    "repos\n  |> filter(x)\n  |> map(y)"
);
snapshot_test!(
    newline_leading_dot_continuation,
    "value\n  .trim()\n  .toUpper()"
);
snapshot_test!(
    newline_map_literal_across_lines,
    "{\n  \"a\": 1,\n  \"b\": 2,\n}"
);

// -- (d) trailing do-blocks ------------------------------------------------

snapshot_test!(trailing_do_with_parens, "f(a) do |x|\n  puts x\nend");
snapshot_test!(
    trailing_do_without_parens,
    "recv.each do |x|\n  puts x\nend"
);
snapshot_test!(trailing_do_bare_ident, "spawn_task do |x|\n  puts x\nend");

// -- (e) error recovery -----------------------------------------------------

#[test]
fn error_recovery_three_errors_yield_three_items() {
    let source = "let a = )\nlet b = ]\nlet c = }\nlet d = 1\n";
    let mut source_map = SourceMap::new();
    let file = source_map.add_virtual("t", source.to_string());
    let result = brasa_parser::parse(source, file);

    assert_eq!(
        result.diagnostics.len(),
        3,
        "expected exactly three diagnostics, got {:#?}",
        result.diagnostics
    );
    assert_eq!(
        result.roots.len(),
        4,
        "expected all four `let` items to still be returned after recovery"
    );
}

// -- `do` reserved as a real keyword ---------------------------------------

#[test]
fn do_is_reserved_and_rejects_use_as_a_binding_name() {
    let mut source_map = SourceMap::new();
    let source = "let do = 5\n";
    let file = source_map.add_virtual("t", source.to_string());
    let result = brasa_parser::parse(source, file);

    assert!(
        !result.diagnostics.is_empty(),
        "expected `let do = 5` to fail to parse now that `do` is reserved"
    );
}

// -- command-call sugar, scoped to statement position ----------------------

snapshot_test!(command_call_two_args, "puts \"a\", \"b\"");
snapshot_test!(
    command_call_in_match_arm,
    "match x\n  Some(n) => puts n\n  None => puts \"none\"\nend"
);

#[test]
fn command_call_is_rejected_in_expression_position() {
    let mut source_map = SourceMap::new();
    let source = "let x = puts \"a\"\n";
    let file = source_map.add_virtual("t", source.to_string());
    let result = brasa_parser::parse(source, file);

    assert!(
        !result.diagnostics.is_empty(),
        "expected `let x = puts \"a\"` (command call in expression position) to fail to parse"
    );
}

snapshot_test!(
    command_call_with_explicit_parens_in_expression_position,
    "let x = puts(\"a\")"
);

// -- arm-body statement normalization ---------------------------------------

snapshot_test!(
    arm_body_return_normalizes_to_block,
    "match x\n  Some(n) => return n\n  None => 0\nend"
);

// -- (f) if forms -------------------------------------------------------

snapshot_test!(
    if_inline_expr_position,
    "let sign = if n < 0 then -1 elsif n > 0 then 1 else 0 end"
);
snapshot_test!(
    if_statement_elsif_chain,
    "if n % 15 == 0\n  \"FizzBuzz\"\nelsif n % 3 == 0\n  \"Fizz\"\nelse\n  \"other\"\nend"
);

// -- (g) generics ---------------------------------------------------------

snapshot_test!(
    generics_named_constraint,
    "def max<T: Comparable>(a: T, b: T): T\n  if a > b then a else b end\nend"
);
// Ruled: the grammar wins here. Inline constraint members require both
// a leading `def` and an explicit `self` parameter, matching the fixed
// `docs/spec/01-syntax.md` example.
snapshot_test!(
    generics_inline_constraint,
    "def log<T: { def toString(self): string }>(value: T)\n  puts value\nend"
);
snapshot_test!(
    throws_union,
    "def fetch(url: string): string throws NetError | DnsError\n  url\nend"
);
snapshot_test!(throws_never, "def pure(x: int): int throws never\n  x\nend");
snapshot_test!(
    interface_with_throws_member,
    "interface Fetcher\n  def fetch(url: string): string throws NetError\nend"
);
