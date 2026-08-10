//! Snapshot tests for AST→HIR lowering: every desugaring plus a
//! structural passthrough of a sugar-free program. Inputs are parsed
//! with `brasa_parser` (zero diagnostics required), lowered, and the
//! span-free HIR dump is snapshotted so a lowering change that reshapes
//! the tree is caught.

use std::path::PathBuf;

use brasa_source::SourceMap;

fn lower_source(name: &str, source: &str) -> brasa_hir::LowerResult {
    let mut source_map = SourceMap::new();
    let file = source_map.add_file(PathBuf::from(format!("{name}.brs")), source.to_string());

    let parsed = brasa_parser::parse(source, file);
    assert!(
        parsed.diagnostics.is_empty(),
        "{name} expected zero parse diagnostics, got: {:#?}",
        parsed.diagnostics
    );

    let lowered = brasa_hir::lower(&parsed.ast, &parsed.roots);
    assert!(
        lowered.diagnostics.is_empty(),
        "{name} expected zero lowering diagnostics, got: {:#?}",
        lowered.diagnostics
    );

    lowered
}

macro_rules! lowering_test {
    ($test_name:ident, $source:expr) => {
        #[test]
        fn $test_name() {
            let lowered = lower_source(stringify!($test_name), $source);
            let dump = brasa_hir::dump::dump(&lowered.hir, &lowered.roots);
            insta::assert_snapshot!(stringify!($test_name), dump);
        }
    };
}

lowering_test!(
    pipe_into_call,
    r#"
let a = 1 |> add(2, 3)
"#
);

lowering_test!(
    pipe_into_qualified_call,
    r#"
let b = x |> foo.helper(y)
"#
);

lowering_test!(
    pipe_chain_plain_calls,
    r#"
let c = [1, 2]
  |> filter(|x| x > 0)
  |> foo.helper(", ")
"#
);

lowering_test!(
    pipe_into_bare_callable,
    r#"
let d = x |> foo.filter
"#
);

lowering_test!(
    coalesce,
    r#"
let x = lookup("k") ?? 0
"#
);

lowering_test!(
    coalesce_chained,
    r#"
let y = a ?? b ?? 0
"#
);

lowering_test!(
    safe_nav_field,
    r#"
let f = user?.name
"#
);

lowering_test!(
    safe_nav_method,
    r#"
let m = user?.rename("x", true)
"#
);

lowering_test!(
    safe_nav_chained,
    r#"
let c = user?.address?.city
"#
);

lowering_test!(
    interpolation,
    r#"
let e = 42
puts "x#{e}y"
"#
);

lowering_test!(
    interpolation_lone,
    r##"
let e = 42
let s = "#{e}"
"##
);

lowering_test!(
    interpolation_all_text,
    r#"
let plain = "hello\nworld"
let raw = """
line one \n stays literal
"""
"#
);

lowering_test!(
    interpolation_raw_with_interp,
    r#"
let name = "brasa"
let snippet = """
project: #{name} \n literal
"""
"#
);

lowering_test!(
    compound_assign_ident,
    r#"
let mut x = 0
x += 1
x -= 2
x *= 3
x /= 4
x %= 5
"#
);

lowering_test!(
    compound_assign_field_target,
    r#"
point().x += delta()
"#
);

lowering_test!(
    compound_assign_index_target,
    r#"
cells()[nextIndex()] *= 2
"#
);

lowering_test!(
    plain_assign_untouched,
    r#"
let mut x = 0
x = 1
grid[0] = 2
p.x = 3
"#
);

lowering_test!(
    for_over_range,
    r#"
for i in 0..10
  puts i
end
for j in 0..=3
  puts j
end
"#
);

lowering_test!(
    for_over_vector,
    r#"
for x in [1, 2, 3]
  puts x
end
"#
);

lowering_test!(
    no_sugar_passthrough,
    r#"
import std::fs
import "utils.brs"

interface Printable
  def describe(self): string
end

enum Shape
  Point
  Circle(radius: float)
end

pub struct Counter
  count: int

  def bump(self, by: int): int
    self.count + by
  end
end

pub let limit = 10

def classify<T: Printable>(value: T, shape: Shape): string throws never
  match shape
    Point => "point"
    Circle(r) if r > 1.0 => value.describe()
    _ => "other"
  end
end

def main()
  let mut total = 0
  let ch = 'a'
  let names = { "a": 1 }
  let c = Counter { count: 0 }

  for (key, value) in names
    puts key
  end

  while total < limit
    total = c.bump(total)
    if total == 3
      continue
    elsif total > 8
      break
    else
      puts total
    end
  end

  let risky = compute() catch (e)
    MathError => -1
  end
  puts risky

  return
end
"#
);

/// The desugarings guarantee single evaluation of `Field`/`Index`
/// receivers via `$tmp` lets; the snapshots above show the shape, and
/// this asserts the hygiene property directly: temp names never collide
/// because `$` cannot start or continue a Brasa identifier.
#[test]
fn temp_names_are_fresh_and_unhygienic_free() {
    let source = r#"
let a = x ?? y ?? z
p.q += r?.s ?? 0
"#;
    let lowered = lower_source("temp_names", source);
    let dump = brasa_hir::dump::dump(&lowered.hir, &lowered.roots);

    let mut seen = std::collections::HashSet::new();
    for line in dump.lines() {
        let trimmed = line.trim();
        if let Some(name) = trimmed
            .strip_prefix("Binding(")
            .and_then(|rest| rest.strip_suffix(")"))
            .filter(|name| name.starts_with("$tmp"))
        {
            assert!(seen.insert(name.to_string()), "temp {name} bound twice");
        }
    }
    assert!(!seen.is_empty(), "expected at least one $tmp binding");
}
