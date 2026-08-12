use brasa_source::FileId;

use crate::{FormatError, format};

/// Formats `source`, asserting it succeeds, and asserts the result is a
/// fixed point: formatting it again changes nothing.
fn fmt(source: &str) -> String {
    let file = FileId::new(0);
    let once = match format(source, file) {
        Ok(text) => text,
        Err(FormatError::Parse(diagnostics)) => {
            panic!("source did not parse: {:?}", diagnostics[0].message)
        }
        Err(FormatError::Unstable(reason)) => panic!("{reason}"),
    };

    let twice = format(&once, file).expect("formatted output reformats");
    assert_eq!(once, twice, "formatting is not idempotent");

    once
}

#[test]
fn normalizes_spacing_and_indentation() {
    let out = fmt("def   add(a:int,b:int):int\n      a+b\nend\n");

    assert_eq!(out, "def add(a: int, b: int): int\n  a + b\nend\n");
}

#[test]
fn keeps_at_most_one_blank_line_between_items() {
    let out = fmt("let a = 1\n\n\n\nlet b = 2\nlet c = 3\n");

    assert_eq!(out, "let a = 1\n\nlet b = 2\nlet c = 3\n");
}

#[test]
fn keeps_leading_and_trailing_comments() {
    let out = fmt("# a header\n\nlet x = 1  # why\n# the end\n");

    assert_eq!(out, "# a header\n\nlet x = 1  # why\n# the end\n");
}

#[test]
fn keeps_comments_inside_a_block() {
    let out = fmt("def f()\n  # first\n  let x = 1\n\n  # last\nend\n");

    assert_eq!(out, "def f()\n  # first\n  let x = 1\n\n  # last\nend\n");
}

#[test]
fn hoists_a_comment_that_has_no_line_of_its_own() {
    let out = fmt("let v = [1, # one\n  2]\n");

    assert_eq!(out, "# one\nlet v = [1, 2]\n");
}

#[test]
fn keeps_literals_exactly_as_written() {
    let out = fmt("let a = 0xFF\nlet b = 1.50\nlet c = \"a\\tb\"\nlet d = 1_000\n");

    assert_eq!(
        out,
        "let a = 0xFF\nlet b = 1.50\nlet c = \"a\\tb\"\nlet d = 1_000\n"
    );
}

#[test]
fn keeps_the_operator_spelling_the_author_chose() {
    let out = fmt("let a = x and y\nlet b = x && y\nlet c = not x\nlet d = !x\n");

    assert_eq!(
        out,
        "let a = x and y\nlet b = x && y\nlet c = not x\nlet d = !x\n"
    );
}

#[test]
fn keeps_the_command_call_form() {
    let out = fmt("puts \"hi\"\nputs(\"hi\")\nputs a, b\n");

    assert_eq!(out, "puts \"hi\"\nputs(\"hi\")\nputs a, b\n");
}

#[test]
fn keeps_the_inline_if_form() {
    let out = fmt("let sign = if n<0 then -1 elsif n>0 then 1 else 0 end\n");

    assert_eq!(out, "let sign = if n < 0 then -1 elsif n > 0 then 1 else 0 end\n");
}

#[test]
fn reindents_the_block_if_form() {
    let out = fmt("if a\nputs 1\nelsif b\nputs 2\nelse\nputs 3\nend\n");

    assert_eq!(
        out,
        "if a\n  puts 1\nelsif b\n  puts 2\nelse\n  puts 3\nend\n"
    );
}

#[test]
fn keeps_a_trailing_do_block() {
    let out = fmt("nums.each do |n|\nputs n\nend\n");

    assert_eq!(out, "nums.each do |n|\n  puts n\nend\n");
}

#[test]
fn keeps_a_chain_the_author_split() {
    let out = fmt("let x = lines\n  .filter(|l| l)\n  .join(\"\\n\")\n");

    assert_eq!(out, "let x = lines\n  .filter(|l| l)\n  .join(\"\\n\")\n");
}

#[test]
fn joins_a_chain_the_author_wrote_on_one_line() {
    let out = fmt("let x = v.map(|n| n).filter(|n| n)\n");

    assert_eq!(out, "let x = v.map(|n| n).filter(|n| n)\n");
}

#[test]
fn breaks_an_argument_list_that_does_not_fit() {
    let long = "x".repeat(40);
    let out = fmt(&format!("call(\"{long}\", \"{long}\", \"{long}\")\n"));

    assert_eq!(
        out,
        format!("call(\n  \"{long}\",\n  \"{long}\",\n  \"{long}\",\n)\n")
    );
}

#[test]
fn hugs_a_lone_argument_instead_of_indenting_it() {
    let long = "y".repeat(60);
    let out = fmt(&format!(
        "rows.push(Input {{ name: \"{long}\", rev: \"{long}\" }})\n"
    ));

    assert_eq!(
        out,
        format!("rows.push(Input {{\n  name: \"{long}\",\n  rev: \"{long}\",\n}})\n")
    );
}


#[test]
fn restores_the_parentheses_the_ast_dropped() {
    let out = fmt("let a = (1 + 2) * 3\nlet b = 1 + 2 * 3\nlet c = -(a + b)\n");

    assert_eq!(
        out,
        "let a = (1 + 2) * 3\nlet b = 1 + 2 * 3\nlet c = -(a + b)\n"
    );
}

/// `**` is the one right-associative binary operator, so it is the one
/// whose left child needs parentheses the precedence table does not
/// hand out for free.
#[test]
fn parenthesizes_the_left_operand_of_a_power() {
    let out = fmt("let a = (2 ** 2) ** 3\nlet b = 2 ** 2 ** 3\n");

    assert_eq!(out, "let a = (2 ** 2) ** 3\nlet b = 2 ** 2 ** 3\n");
}

/// A struct method's span is not recorded, so its body's territory has
/// to be clipped to its own `end`; otherwise it swallows whatever was
/// written between that `end` and the next member.
#[test]
fn a_comment_stays_above_the_member_it_describes() {
    let source = "struct P\n  def f(self): int\n    1\n  end\n\n  # about g\n  def g(self): int\n    2\n  end\nend\n";

    assert_eq!(fmt(source), source);
}

#[test]
fn keeps_a_one_element_tuple_comma() {
    let out = fmt("let one = (7,)\nlet pair = (1, \"a\")\n");

    assert_eq!(out, "let one = (7,)\nlet pair = (1, \"a\")\n");
}

#[test]
fn prints_struct_members_in_source_order() {
    let out = fmt("struct P\n  x: float\n\n  def f(self): float\n    self.x\n  end\n\n  y: float\nend\n");

    assert_eq!(
        out,
        "struct P\n  x: float\n\n  def f(self): float\n    self.x\n  end\n\n  y: float\nend\n"
    );
}

#[test]
fn formats_match_and_catch_arms() {
    let out = fmt("let a = match s\nCircle(r) if r>1.0 => r*2.0\n_ =>\n0.0\nend\n");

    assert_eq!(
        out,
        "let a = match s\n  Circle(r) if r > 1.0 => r * 2.0\n  _ =>\n    0.0\nend\n"
    );
}

#[test]
fn formats_a_catch_clause() {
    let out = fmt("let page = fetch(url) catch (e)\nNetError|Timeout => \"\"\nend\n");

    assert_eq!(
        out,
        "let page = fetch(url) catch (e)\n  NetError | Timeout => \"\"\nend\n"
    );
}

#[test]
fn refuses_to_format_source_that_does_not_parse() {
    let result = format("def f(\n", FileId::new(0));

    assert!(matches!(result, Err(FormatError::Parse(_))));
}
