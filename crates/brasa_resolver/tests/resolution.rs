//! Snapshot tests for name resolution. Inputs are parsed and lowered
//! (both with zero diagnostics required), then resolved. Happy-path
//! tests snapshot the span-free resolution dump; error tests snapshot
//! the rendered diagnostics so wording, labels, and spans are all
//! pinned.

use std::path::PathBuf;

use brasa_source::SourceMap;

fn resolve_source(
    name: &str,
    source: &str,
) -> (
    brasa_hir::LowerResult,
    brasa_resolver::ResolveResult,
    SourceMap,
) {
    let mut source_map = SourceMap::new();
    let file = source_map.add_file(PathBuf::from(format!("{name}.bras")), source.to_string());

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
    (lowered, resolved, source_map)
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

macro_rules! resolution_test {
    ($test_name:ident, $source:expr) => {
        #[test]
        fn $test_name() {
            let (lowered, resolved, _map) = resolve_source(stringify!($test_name), $source);
            assert!(
                resolved.diagnostics.is_empty(),
                "expected zero resolve diagnostics, got: {:#?}",
                resolved.diagnostics
            );
            let dump = brasa_resolver::dump::dump(&lowered.hir, &resolved.resolutions);
            insta::assert_snapshot!(stringify!($test_name), dump);
        }
    };
}

macro_rules! resolution_error_test {
    ($test_name:ident, $source:expr) => {
        #[test]
        fn $test_name() {
            let (_lowered, resolved, map) = resolve_source(stringify!($test_name), $source);
            assert!(
                !resolved.diagnostics.is_empty(),
                "expected resolve diagnostics, got none"
            );
            let rendered = render_diagnostics(&resolved.diagnostics, &map);
            insta::assert_snapshot!(stringify!($test_name), rendered);
        }
    };
}

resolution_test!(
    scopes_and_shadowing,
    r#"
def apply(x: int, f: (int) -> int): int
  f(x)
end

def main()
  let mut total = 0
  let x = 1
  while total < 10
    let x = x + 1
    total = total + x
  end
  if x > 0
    let x = "shadowed with another type"
    puts x
  else
    print x
  end
  for n in 1..x
    total = total + n
  end
  puts apply(total, |y| y * 2)
end
"#
);

resolution_test!(
    items_types_and_generics,
    r#"
interface Greeter
  def greet(self, other: Self): string
end

struct Point<T: Comparable>
  x: T
  y: T

  def swap(self): Point<T>
    Point { x: self.y, y: self.x }
  end
end

enum Shape
  Circle(radius: float)
  Dot
end

def maxOf<T: Comparable>(a: T, b: T): T
  if a > b then a else b end
end

def pick<T: { def toString(): string }>(items: Vector<T>): Option<T>
  items.first()
end

let best = maxOf(1, 2)
"#
);

resolution_test!(
    imports_ctors_and_catch,
    r#"
import std::fs
import "./helpers.bras"

enum Status
  Active(since: int)
  Idle
end

def describe(s: Status): string
  match s
    Active(since) if since > 0 => "active #{since}"
    Active(_) => "active"
    Idle => "idle"
  end
end

def load(path: string): string
  fs.read(path) catch (e)
    fs.NotFound => "missing"
    _ if path.len() > 0 => helpers.fallback(path)
    _ => ""
  end
end

let current = Some(Active(3))
match current
  Some(s) => puts describe(s)
  None => puts "nothing"
end
"#
);

resolution_test!(
    catch_arm_types,
    r#"
struct NetError
  detail: string
end

def load(n: int): int
  n catch (e)
    NetError => 0
    string | NetError => 1
    panics.DivisionByZero => 2
    _ => 3
  end
end
"#
);

resolution_test!(
    catch_arm_native_errors,
    r#"
def parse(s: string): int
  s.toInt() catch (e)
    string.ParseError => -1
    _ => -2
  end
end
"#
);

resolution_test!(
    catch_arm_regex_error,
    r#"
def matches(s: string): bool
  s.match?("[a-z]+") catch (e)
    string.RegexError => false
  end
end
"#
);

resolution_error_test!(
    unknown_native_error,
    r#"
def go(s: string): int
  s.toInt() catch (e)
    string.Nope => 0
    _ => 1
  end
end
"#
);

resolution_error_test!(
    unknown_panic,
    r#"
def go(n: int): int
  n catch (e)
    panics.Nope => 0
    _ => 1
  end
end
"#
);

resolution_error_test!(
    unknown_catch_arm_type,
    r#"
def go(n: int): int
  n catch (e)
    ParseError => 0
    _ => 1
  end
end
"#
);

resolution_test!(
    top_level_order,
    r#"
def double(n: int): int
  n * later + n
end

let base = 10
let derived = double(base)

puts derived
let later = base + 1
"#
);

resolution_error_test!(
    unknown_names_types_and_ctors,
    r#"
def go(p: Bogus): unit
  puts missing
  let s = Nope { x: 1 }
  match s
    Whatever(x) => puts x
    _ => puts "no"
  end
end
"#
);

resolution_test!(
    set_ctor_resolves_in_expression_position,
    r#"
let s = Set([1, 2, 3])
"#
);

resolution_error_test!(
    set_ctor_is_rejected_in_pattern_position,
    r#"
match 1
  Set(x) => puts x
  _ => puts "no"
end
"#
);

resolution_error_test!(
    duplicate_definitions,
    r#"
def twice(n: int, n: int): int
  let a = 1
  let a = 2
  a
end

def twice(): unit
end

let twice = 3

enum Pair
  Two(a: int, b: int)
end

match Two(1, 2)
  Two(x, x) => puts x
  _ => puts "no"
end
"#
);

resolution_error_test!(
    duplicate_variants_and_fields,
    r#"
enum Shape
  Circle(radius: float)
  Circle(r: float)
  Rect(w: float, w: float)
  Dot
end

struct Point
  x: int
  x: int
  y: int
end
"#
);

// A struct's fields and methods share one member namespace, so a
// method may not repeat a field name (BRS-57). `Shadowed` is the
// soundness case the rejection closes: the checker used to type
// `b.tag()` from the field while both runtimes dispatched the method.
// Fields and methods share one namespace, so a method collides with an
// earlier METHOD too, not only with a field. The triple pins that the
// first declaration keeps the slot: both later ones are blamed on it
// rather than chaining, matching how repeated enum variants report.
resolution_error_test!(
    duplicate_struct_methods,
    r#"
struct Counter
  n: int

  def label(self): string
    "first"
  end

  def label(self): string
    "second"
  end
end

struct Triple
  def a(self): int
    1
  end

  def a(self): int
    2
  end

  def a(self): int
    3
  end
end
"#
);

// A repeated interface member is worse than dead code: a second
// declaration at a DIFFERENT signature makes the interface
// unsatisfiable by construction, and without this the failure surfaced
// later as a satisfaction error blaming an innocent type.
//
// An ANONYMOUS inline constraint is the same namespace and gets the
// same check — it was the third place this defect lived.
resolution_error_test!(
    duplicate_interface_members,
    r#"
interface Greeter
  def greet(self): string
  def greet(self): string
end

interface Widened
  def size(self): int
  def size(self): string
end

def inline<T: { def a(self): int, def a(self): int }>(t: T): int
  t.a()
end
"#
);

// `Reversed` declares the method first, pinning that the labels follow
// source order rather than the field/method split. A callable field
// with no same-named method (`Fine`) stays legal.
resolution_error_test!(
    struct_field_and_method_collide,
    r#"
struct Shadowed
  tag: () -> string

  def tag(self): int
    7
  end
end

struct Reversed
  def name(self): string
    "method"
  end

  name: string
end

struct Fine
  handler: () -> string
end
"#
);

resolution_error_test!(
    self_imports_and_order,
    r#"
import std::teleport

def free(): int
  self.x
end

puts early
let early = 1
"#
);

resolution_test!(
    file_import_missing_target_still_binds_stem,
    r#"
import "./tools/missing.bras"

def run(path: string): string
  missing.transform(path)
end
"#
);

/// After a same-scope duplicate `let`, the newer binding must win:
/// later references resolve to it, not to the stale one, and exactly one
/// duplicate diagnostic is reported.
#[test]
fn duplicate_binding_newer_wins() {
    let source = r#"
def go(): int
  let a = 1
  let a = 2
  a
end
"#;
    let (_lowered, resolved, _map) = resolve_source("duplicate_binding_newer_wins", source);

    assert_eq!(
        resolved.diagnostics.len(),
        1,
        "expected exactly the duplicate diagnostic, got: {:#?}",
        resolved.diagnostics
    );
    assert!(
        resolved.diagnostics[0]
            .message
            .contains("duplicate definition of `a`"),
        "unexpected message: {}",
        resolved.diagnostics[0].message
    );

    let tables = &resolved.resolutions;
    let a_locals: Vec<brasa_resolver::LocalId> = tables
        .stmt_locals
        .values()
        .copied()
        .filter(|&local| tables.local(local).name == "a")
        .collect();
    assert_eq!(a_locals.len(), 2, "both `let a` sites allocate a local");
    let newest = *a_locals
        .iter()
        .max_by_key(|local| local.0)
        .expect("two locals");

    let a_refs: Vec<brasa_resolver::Res> = tables
        .expr_res
        .values()
        .copied()
        .filter(|res| matches!(res, brasa_resolver::Res::Local(local) if tables.local(*local).name == "a"))
        .collect();
    assert!(!a_refs.is_empty(), "the trailing `a` reference is recorded");
    for res in a_refs {
        assert_eq!(
            res,
            brasa_resolver::Res::Local(newest),
            "references after the duplicate must resolve to the newer binding"
        );
    }
}

/// `throws` lists resolve in the type namespace like catch arm types:
/// a known name records its `TypeRes` positionally, an unknown name is
/// `R003` and records `None` to keep the list aligned.
#[test]
fn throws_lists_resolve_and_unknown_names_report() {
    let source = r#"
struct NetError
  detail: string
end

def fetch(ok: bool): string throws NetError
  "ok"
end

def bad(): int throws MissingError
  0
end
"#;
    let (_lowered, resolved, _map) =
        resolve_source("throws_lists_resolve_and_unknown_names_report", source);

    assert_eq!(
        resolved.diagnostics.len(),
        1,
        "expected exactly the unknown-type diagnostic, got: {:#?}",
        resolved.diagnostics
    );
    assert_eq!(
        resolved.diagnostics[0].error_code,
        brasa_diagnostics::codes::R_UNKNOWN_TYPE
    );
    assert!(
        resolved.diagnostics[0]
            .message
            .contains("unknown type `MissingError`"),
        "unexpected message: {}",
        resolved.diagnostics[0].message
    );

    let mut lists: Vec<&Vec<Option<brasa_resolver::TypeRes>>> =
        resolved.resolutions.throws_types.values().collect();
    lists.sort_by_key(|list| list[0].is_none());
    assert_eq!(lists.len(), 2, "both `throws` lists are recorded");
    assert!(
        matches!(
            lists[0].as_slice(),
            [Some(brasa_resolver::TypeRes::Item(_))]
        ),
        "`fetch`'s list resolves `NetError`: {:?}",
        lists[0]
    );
    assert_eq!(
        lists[1].as_slice(),
        [None],
        "`bad`'s unknown name records `None`"
    );
}

// Interface-member `throws` names are validated like a function's
// (`R003` on an unknown name), in both interface bodies and inline
// anonymous constraints; a known name reports nothing. Contract
// enforcement is deferred to M3+, so nothing is recorded.
resolution_error_test!(
    iface_member_throws_names_are_validated,
    r#"
struct NetError
  detail: string
end

interface Fetcher
  def fetch(self): string throws NetError
  def probe(self): int throws GhostError
end

def scan<T: { def peek(self): int throws PhantomError }>(value: T): int
  value.peek()
end
"#
);

resolution_error_test!(
    ambiguous_ctor_and_bad_constraint,
    r#"
enum Coin
  Heads
  Tails
end

enum Toss
  Heads
  Edge
end

def flip<T: Coin>(value: T): T
  value
end

let c = Heads
"#
);
