//! Snapshot tests for type checking. Inputs are parsed, lowered, and
//! resolved (all with zero diagnostics required), then checked.
//! Happy-path tests snapshot the span-free type dump; error tests
//! snapshot the rendered diagnostics so wording, labels, and spans are
//! all pinned.

use std::path::PathBuf;

use brasa_source::SourceMap;

fn check_source(
    name: &str,
    source: &str,
) -> (
    brasa_hir::LowerResult,
    brasa_resolver::ResolveResult,
    brasa_typeck::TypeckResult,
    SourceMap,
) {
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

    let resolved = brasa_resolver::resolve(&lowered.hir, &lowered.roots);
    assert!(
        resolved.diagnostics.is_empty(),
        "{name} expected zero resolve diagnostics, got: {:#?}",
        resolved.diagnostics
    );

    let checked = brasa_typeck::check(&lowered.hir, &lowered.roots, &resolved.resolutions);
    (lowered, resolved, checked, source_map)
}

fn render_diagnostics(
    diagnostics: &[brasa_diagnostics::Diagnostic],
    sources: &SourceMap,
) -> String {
    let mut out = Vec::new();
    for diag in diagnostics {
        brasa_diagnostics::render::render(diag, sources, &mut out, false)
            .expect("render should not fail");
    }
    String::from_utf8(out).expect("rendered output should be valid utf-8")
}

macro_rules! typecheck_test {
    ($test_name:ident, $source:expr) => {
        #[test]
        fn $test_name() {
            let (lowered, resolved, checked, _map) = check_source(stringify!($test_name), $source);
            assert!(
                checked.diagnostics.is_empty(),
                "expected zero typeck diagnostics, got: {:#?}",
                checked.diagnostics
            );
            let dump =
                brasa_typeck::dump::dump(&lowered.hir, &resolved.resolutions, &checked.types);
            insta::assert_snapshot!(stringify!($test_name), dump);
        }
    };
}

macro_rules! typecheck_error_test {
    ($test_name:ident, $source:expr) => {
        #[test]
        fn $test_name() {
            let (_lowered, _resolved, checked, map) = check_source(stringify!($test_name), $source);
            assert!(
                !checked.diagnostics.is_empty(),
                "expected typeck diagnostics, got none"
            );
            let rendered = render_diagnostics(&checked.diagnostics, &map);
            insta::assert_snapshot!(stringify!($test_name), rendered);
        }
    };
}

typecheck_test!(
    inference_and_annotations,
    r#"
def scale(base: int, factor: int): int
  let mut total = base
  total = total * factor
  total
end

def label(value: float): string
  let prefix: string = "v="
  let text = prefix + value.toString()
  text
end

def bounds(a: int, b: float): bool
  a > 0 && b < 1.5 || !(a == 3)
end

let big = scale(2, 10) > 15
let banner = "result: #{scale(3, 3)} (#{big})"
let half = 7.0 / 2.0
let ch = 'a' < 'b'
let squared = 2 ** 8 % 5
"#
);

typecheck_test!(
    control_flow,
    r#"
def classify(n: int): string
  if n < 0
    "negative"
  elsif n == 0
    "zero"
  else
    "positive"
  end
end

def clamp(n: int): int
  let bounded = if n > 100
    100
  elsif n < 0
    return 0
  else
    n
  end
  bounded
end

def scan(items: Vector<string>, counts: Map<string, int>): int
  let mut total = 0
  for item in items
    for ch in item
      if ch == 'x'
        total = total + 1
      end
    end
  end
  for (word, count) in counts
    total = total + count + word.len()
  end
  for i in 0..10
    total = total + i
    if total > 100
      break
    end
  end
  while total % 2 == 0
    total = total + 1
  end
  total
end

def report(n: int)
  match n
    0 => puts "zero"
    _ => n + 1
  end
end
"#
);

typecheck_test!(
    structs_and_methods,
    r##"
struct Counter
  count: int
  label: string

  def bump(self, by: int): int
    self.count = self.count + by
    self.count
  end

  def describe(self): string
    "#{self.label}: #{self.count}"
  end
end

let c = Counter { label: "hits", count: 0 }
c.count = 10
let n = c.bump(5)
puts c.describe()
let bump = c.bump
let derived = c.toString()
"##
);

typecheck_test!(
    collections_and_lambdas,
    r#"
def total(nums: Vector<int>): int
  let mut acc = 0
  nums.each do |n|
    acc = acc + n
  end
  acc
end

def process(nums: Vector<int>, tags: Set<string>): Vector<string>
  let doubled = nums.map(|n| n * 2)
  let evens = doubled.filter(|n| n % 2 == 0)
  let names = evens.map(|n| "n#{n}")
  tags.add("seen")
  if tags.has?("skip")
    names.reverse()
  else
    names.sortBy(|s| s.len())
  end
end

let empty: Vector<int> = []
let words = ["a", "b"]
let joined = words.join(", ")
let first = words[0]
let lookup: Map<string, int> = { "one": 1 }
let one = lookup["one"] ?? 0
let keys = lookup.keys()
let trimmed = "  hi  ".trim().toUpper()
let parsed = "42".toInt()
"#
);

typecheck_test!(
    options_and_wrap,
    r#"
struct Profile
  nickname: Option<string>
end

struct Account
  profile: Option<Profile>
end

struct Point
  x: int
end

def nick(acct: Option<Account>): string
  let profile = acct?.profile
  let name: Option<string> = profile?.nickname
  name ?? "anon"
end

def getX(p: Option<Point>): int
  p?.x ?? 0
end

def find(nums: Vector<int>): Option<int>
  match nums.first()
    Some(n) => Some(n * 2)
    None => None
  end
end

let some: Option<int> = Some(41)
let none: Option<int> = None
let fallback = none ?? 1
"#
);

typecheck_error_test!(
    error_let_and_assign,
    r#"
let top = 1
let mut counter = 0

def go(): int
  let n: int = "no"
  let s = "hi"
  s = "bye"
  let mut m = 1
  m = "one"
  counter = false
  top = 2
  return "text"
end

return 5
"#
);

typecheck_error_test!(
    error_operators,
    r#"
def ops(a: int, b: float, s: string, flag: bool): int
  let mixed = a + b
  let bad_sub = s - s
  let cross = a < s
  let and_int = flag && a
  let neg = -s
  let indexed = "abc"[0]
  if a
    puts s
  end
  a
end
"#
);

typecheck_error_test!(
    error_members_and_calls,
    r#"
struct Point
  x: int
  y: int

  def sum(self): int
    self.x + self.y
  end
end

def misuse(p: Point, nums: Vector<int>): int
  let a = p.z
  let b = p.sum(1)
  let lit = Point { x: 1, z: 3 }
  let pushed = nums.push("no")
  let shoved = nums.shove(1)
  let glued = nums.join(", ")
  a + b
end

def two(a: int, b: int): int
  a + b
end

let r = two(1)
let q = two(1, "x")
let called = two(1, 2)(3)
"#
);

typecheck_error_test!(
    error_inference,
    r#"
def build(): int
  let empty = []
  let table = {}
  let f = |x| x + 1
  let mixed = [1, "two", 3]
  let entries = { "a": 1, "b": false }
  let v = if true
    1
  else
    "one"
  end
  match 1
    n if n + 1 => 0
    _ => 1
  end
end
"#
);
