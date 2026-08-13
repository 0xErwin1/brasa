//! Snapshot tests for HIR→bytecode compilation. Inputs run the whole
//! frontend (parse, lower, resolve, typecheck — all with zero
//! diagnostics required), compile to a module, and snapshot the
//! deterministic disassembly (`brasa_bytecode::dump`), pinning opcode
//! selection, jump layout, handler tables, slot assignment, capture
//! order, and `max_stack`.

use std::path::PathBuf;

fn compile_source(name: &str, source: &str) -> String {
    let mut source_map = brasa_source::SourceMap::new();
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
    assert!(
        resolved.diagnostics.is_empty(),
        "{name} expected zero resolve diagnostics, got: {:#?}",
        resolved.diagnostics
    );

    let checked = brasa_typeck::check(
        &lowered.hir,
        &lowered.roots,
        &resolved.resolutions,
        &lowered.sugar_origins,
    );
    assert!(
        checked.diagnostics.is_empty(),
        "{name} expected zero typeck diagnostics, got: {:#?}",
        checked.diagnostics
    );

    let compiled = brasa_codegen::compile(
        &lowered.hir,
        &lowered.roots,
        &resolved.resolutions,
        &checked.types,
    );
    assert!(
        compiled.diagnostics.is_empty(),
        "{:?}",
        compiled.diagnostics
    );
    let module = compiled.module;
    brasa_bytecode::dump::dump(&module)
}

fn assert_compiles(name: &str, source: &str) {
    insta::assert_snapshot!(name, compile_source(name, source));
}

#[test]
fn arithmetic_and_strings() {
    assert_compiles(
        "arithmetic_and_strings",
        r##"
let a = 2 + 3 * 4 - 1
let b = 7 % 2
let c = 2 ** 10
let f = 1.5 * 2.0 / 4.0
let neg = -a
let fneg = -f
let s = "bra" + "sa"
let cmp = a < 10 && b != 0 || c > 100
let negated = !(a == 5)
"##,
    );
}

#[test]
fn control_flow() {
    assert_compiles(
        "control_flow",
        r##"
let mut total = 0
for i in 1..=5
  if i % 2 == 0
    continue
  end
  total = total + i
end

let mut n = 3
while n > 0
  n = n - 1
  if n == 1
    break
  end
end

let label = if total > 5 then "big" elsif total > 2 then "mid" else "small" end
puts label
"##,
    );
}

#[test]
fn closures_and_captures() {
    assert_compiles(
        "closures_and_captures",
        r##"
def makeAdder(k: int): (int) -> int
  |n| n + k
end

def compose(): int
  let base = 10
  let offset = 1
  let inner = |x: int| x + base + offset
  let outer = || inner(base)
  outer()
end

let add5 = makeAdder(5)
puts add5(compose())
"##,
    );
}

#[test]
fn match_decision_tree() {
    assert_compiles(
        "match_decision_tree",
        r##"
enum Shape
  Circle(radius: float)
  Rect(w: float, h: float)
  Dot
end

def describe(shape: Shape): string
  match shape
    Circle(r) if r > 10.0 => "big circle"
    Circle(_) => "circle"
    Rect(w, h) if w == h => "square"
    Rect(_, _) => "rect"
    Dot => "dot"
  end
end

def label(n: int): string
  match n
    0 => "zero"
    1 => "one"
    _ => "many"
  end
end

def unwrap(o: Option<int>): int
  match o
    Some(v) => v
    None => 0
  end
end

puts describe(Dot)
puts label(2)
puts unwrap(Some(3))
"##,
    );
}

#[test]
fn catch_dispatch() {
    assert_compiles(
        "catch_dispatch",
        r##"
struct NetError
  detail: string
end

struct ParseError
  line: int
end

def fetch(ok: bool): string
  if !ok
    throw NetError { detail: "timeout" }
  end
  "<html>"
end

let page = fetch(false) catch (e)
  NetError => "recovered: #{e.detail}"
  ParseError if e.line > 10 => "far"
  _ => "other"
end
puts page

let items = [1, 2, 3]
let out = items[10] catch (e)
  panics.IndexOutOfBounds => 0
end
puts out

let n = "nope".toInt() catch (e)
  string.ParseError => -1
end
puts n
"##,
    );
}

#[test]
fn interpolation() {
    assert_compiles(
        "interpolation",
        r##"
let name = "brasa"
let stars = 42
puts "repo #{name} has #{stars} stars (#{stars > 10}!)"
"##,
    );
}

#[test]
fn for_loops_and_collections() {
    assert_compiles(
        "for_loops_and_collections",
        r##"
let nums = [5, 3, 8]
let stock: Map<string, int> = { "ember": 3, "ash": 7 }

for n in nums
  puts n
end

for (name, count) in stock
  puts "#{name} -> #{count}"
end

for c in "abc"
  puts c
end

let doubled = nums.map(|n| n * 2)
puts doubled.first() ?? -1
puts stock["ember"] ?? 0
"##,
    );
}

#[test]
fn structs_methods_and_globals() {
    assert_compiles(
        "structs_methods_and_globals",
        r##"
struct Point
  x: float
  y: float

  def norm(self): float
    self.x * self.x + self.y * self.y
  end

  def toString(self): string
    "(#{self.x}, #{self.y})"
  end
end

let p = Point { x: 3.0, y: 4.0 }
p.x = 6.0
puts p.norm()
puts p
puts p.toString()
let bound = p.norm
puts bound()
"##,
    );
}

#[test]
fn safe_navigation_wrap_decisions() {
    assert_compiles(
        "safe_navigation_wrap_decisions",
        r##"
let maybe: Option<int> = Some(41)
puts maybe?.toString()
puts maybe ?? 0
"##,
    );
}
