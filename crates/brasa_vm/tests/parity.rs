//! Walker/VM parity suite: every program runs through the full
//! frontend once, then through BOTH backends — the reference
//! tree-walker (`brasa_interp`) and the bytecode VM (`brasa_vm`) — and
//! the outcome plus captured stdout must be identical. The walker is
//! the oracle: a disagreement is a VM (or codegen) bug by definition.

use brasa_interp::Outcome;

/// Compiles `source` through the whole frontend (it must be clean) and
/// runs it on both backends with the given call-depth limit, asserting
/// identical outcome and stdout; returns the shared result.
fn assert_parity_with_depth(source: &str, max_depth: usize) -> (Outcome, String) {
    assert_parity_configured(source, max_depth, &[])
}

/// [`assert_parity_with_depth`] with explicit script arguments, served
/// by `env.args()` (BRS-32).
fn assert_parity_configured(source: &str, max_depth: usize, args: &[String]) -> (Outcome, String) {
    let mut sources = brasa_source::SourceMap::new();
    let file = sources.add_file("parity.brs", source.to_string());

    let parsed = brasa_parser::parse(source, file);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);

    let lowered = brasa_hir::lower(&parsed.ast, &parsed.roots);
    assert!(lowered.diagnostics.is_empty(), "{:?}", lowered.diagnostics);

    let resolved = brasa_resolver::resolve(&lowered.hir, &lowered.roots);
    assert!(
        resolved.diagnostics.is_empty(),
        "{:?}",
        resolved.diagnostics
    );

    let checked = brasa_typeck::check(
        &lowered.hir,
        &lowered.roots,
        &resolved.resolutions,
        &lowered.sugar_origins,
    );
    assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);

    let mut walker_out = Vec::new();
    let walker_outcome = brasa_interp::run_with_depth(
        &lowered.hir,
        &lowered.roots,
        &resolved.resolutions,
        &checked.types,
        &mut walker_out,
        max_depth,
        args,
    );
    let walker_stdout = String::from_utf8(walker_out).expect("walker output is UTF-8");

    let module = brasa_codegen::compile(
        &lowered.hir,
        &lowered.roots,
        &resolved.resolutions,
        &checked.types,
    );
    let mut vm_out = Vec::new();
    let vm_outcome = brasa_vm::run_with_depth(&module, &mut vm_out, max_depth, args);
    let vm_stdout = String::from_utf8(vm_out).expect("VM output is UTF-8");

    assert_eq!(
        walker_outcome, vm_outcome,
        "outcome parity failed\nwalker stdout: {walker_stdout:?}\nvm stdout: {vm_stdout:?}"
    );
    assert_eq!(walker_stdout, vm_stdout, "stdout parity failed");

    // Hot-GC leg (BRS-30): the same module under a tiny allocation
    // threshold, so collections fire constantly mid-run. GC pressure
    // must never change observable behavior. `run_with_gc_threshold`
    // has no depth or args parameter, so the depth-limited and
    // args-carrying tests keep only the default comparison above.
    if max_depth == brasa_vm::DEFAULT_MAX_CALL_DEPTH && args.is_empty() {
        let mut hot_out = Vec::new();
        let (hot_outcome, _) = brasa_vm::run_with_gc_threshold(&module, &mut hot_out, 8);
        let hot_stdout = String::from_utf8(hot_out).expect("hot-GC VM output is UTF-8");

        assert_eq!(
            walker_outcome, hot_outcome,
            "hot-GC outcome parity failed\nwalker stdout: {walker_stdout:?}\nhot stdout: {hot_stdout:?}"
        );
        assert_eq!(walker_stdout, hot_stdout, "hot-GC stdout parity failed");
    }

    (walker_outcome, walker_stdout)
}

fn assert_parity(source: &str) -> (Outcome, String) {
    assert_parity_with_depth(source, brasa_vm::DEFAULT_MAX_CALL_DEPTH)
}

/// Parity plus an explicit success expectation with pinned stdout, so
/// a shared walker/VM regression cannot slip through as "still equal".
fn assert_success(source: &str, expected_stdout: &str) {
    let (outcome, stdout) = assert_parity(source);
    assert_eq!(outcome, Outcome::Success);
    assert_eq!(stdout, expected_stdout);
}

// --- arithmetic and overflow panics -----------------------------------

#[test]
fn arithmetic_ints_and_floats() {
    assert_success(
        r##"
let a = 2 + 3 * 4 - 1
let b = 7 % 2
let c = 2 ** 10
let f = 1.5 * 2.0 / 4.0
let g = 7.5 % 2.0
let neg = -a
let fneg = -f
puts a
puts b
puts c
puts f
puts g
puts neg
puts fneg
puts 1.0 / 0.0
puts 10 / 3
"##,
        "13\n1\n1024\n0.75\n1.5\n-13\n-0.75\ninf\n3\n",
    );
}

#[test]
fn integer_overflow_panics_match() {
    let (outcome, _) = assert_parity("puts 9223372036854775807 + 1\n");
    let Outcome::Panic { message } = outcome else {
        panic!("expected a panic, got {outcome:?}");
    };
    assert_eq!(
        message,
        "panic: panics.IntegerOverflow: integer overflow in `+`"
    );
}

#[test]
fn division_and_remainder_by_zero_panics_match() {
    let (outcome, _) = assert_parity("let z = 0\nputs 1 / z\n");
    assert!(matches!(outcome, Outcome::Panic { message } if message.contains("division by zero")));

    let (outcome, _) = assert_parity("let z = 0\nputs 1 % z\n");
    assert!(matches!(outcome, Outcome::Panic { message } if message.contains("remainder by zero")));
}

#[test]
fn negative_int_exponent_panics_match() {
    let (outcome, _) = assert_parity("let e = -2\nputs 2 ** e\n");
    let Outcome::Panic { message } = outcome else {
        panic!("expected a panic, got {outcome:?}");
    };
    assert_eq!(
        message,
        "panic: panics.AssertionFailed: negative exponent in integer `**`"
    );
}

#[test]
fn unary_negation_overflow_panics_match() {
    let (outcome, _) = assert_parity("let m = -9223372036854775807 - 1\nlet n = -m\nputs n\n");
    assert!(matches!(outcome, Outcome::Panic { message } if message.contains("unary `-`")));
}

// --- strings and interpolation ----------------------------------------

#[test]
fn strings_and_interpolation() {
    assert_success(
        r##"
let name = "brasa"
let stars = 42
puts "repo #{name} has #{stars} stars (#{stars > 10}!)"
puts "Hello, " + name.toUpper()
puts name.len()
puts name.slice(1, 3)
puts "a,b,c".split(",").join(" | ")
puts "  pad  ".trim()
puts "abcabc".count("bc")
puts "abc".contains?("b")
puts "ñandú".chars()
puts "one\ntwo".lines()
puts "ha".repeat(3)
puts "hello".replace("l", "L")
puts "hello".find("llo") ?? -1
"##,
        "repo brasa has 42 stars (true!)\nHello, BRASA\n5\nra\na | b | c\npad\n2\ntrue\n['ñ', 'a', 'n', 'd', 'ú']\n[\"one\", \"two\"]\nhahaha\nheLLo\n2\n",
    );
}

#[test]
fn string_cutting_cleanup_and_padding() {
    assert_success(
        r##"
puts "brasa".reverse()
puts "ñandú".reverse()
puts "  pad  ".trimStart() + "|"
puts "  pad  ".trimEnd() + "|"
puts "7".padStart(3, "0")
puts "7".padEnd(3, "0")
puts "abc".padStart(2, "0")
puts "ñu".padStart(4, "ab")
puts "x".padEnd(6, "ab")
puts "x".padStart(3, "")
let w = -2
puts "x".padStart(w, "0")
puts "ñandú".bytes()
puts "".bytes()
puts "".split(",")
puts "a".split("")
"##,
        "asarb\núdnañ\npad  |\n  pad|\n007\n700\nabc\nabñu\nxababa\nx\nx\n[195, 177, 97, 110, 100, 195, 186]\n[]\n[\"\"]\n[\"a\"]\n",
    );
}

// --- built-in regex ----------------------------------------------------

#[test]
fn regex_methods_agree() {
    assert_success(
        r##"
puts "hello world".match?("wor..")
puts "hello".match?("^h.*o$")
puts "hello".match?("[0-9]+")
puts "2026-08-11".captures("([0-9]+)-([0-9]+)-([0-9]+)")
puts "ab".captures("a(x)?(b)")
puts "abc".captures("[0-9]")
puts "a1b22c".replaceRe("[0-9]+", "#")
puts "john smith".replaceRe("(\\w+) (\\w+)", "$2 $1")
puts "cost: 5$".replaceRe("[0-9]", "$$")
puts "a1b22c333".scan("[0-9]+")
puts "abc".scan("[0-9]+")
puts "ab".scan("x*")
puts "ñandú".scan("[añú]")
"##,
        "true\ntrue\nfalse\nSome([\"2026-08-11\", \"2026\", \"08\", \"11\"])\nSome([\"ab\", \"\", \"b\"])\nNone\na#b#c\nsmith john\ncost: $$\n[\"1\", \"22\", \"333\"]\n[]\n[\"\", \"\", \"\"]\n[\"ñ\", \"a\", \"ú\"]\n",
    );
}

#[test]
fn invalid_regex_throws_the_native_regex_error() {
    assert_success(
        r##"
let ok = "abc".match?("[") catch (e)
  string.RegexError => false
end
puts ok
let n = "abc".scan("(").len() catch (e)
  string.RegexError => e.len()
end
puts n
"##,
        "false\n17\n",
    );

    let (outcome, _) = assert_parity("puts \"abc\".scan(\"(\")\n");
    let Outcome::Error { message } = outcome else {
        panic!("expected an error, got {outcome:?}");
    };
    assert_eq!(message, "error: string.RegexError: invalid regex \"(\"");
}

#[test]
fn to_int_and_to_float_parse_errors_match() {
    assert_success(
        r##"
let n = "42".toInt()
let f = "1.5".toFloat()
puts n
puts f
let bad = "abc".toInt() catch (e)
  string.ParseError => e.len()
end
puts bad
let alsobad = "1.5x".toFloat() catch (e)
  _ => -1.0
end
puts alsobad
"##,
        "42\n1.5\n25\n-1.0\n",
    );

    let (outcome, _) = assert_parity("puts \"abc\".toInt()\n");
    let Outcome::Error { message } = outcome else {
        panic!("expected an error, got {outcome:?}");
    };
    assert_eq!(
        message,
        "error: string.ParseError: cannot parse \"abc\" as int"
    );
}

// --- collections, HOFs, closures --------------------------------------

#[test]
fn collections_and_hofs() {
    assert_success(
        r##"
let nums = [5, 3, 8, 1]
puts nums.len()
puts nums.map(|n| n * 2)
puts nums.filter(|n| n > 2)
puts nums.sortBy(|n| n)
puts nums.reverse()
puts nums.first() ?? -1
puts nums.last() ?? -1
puts nums.contains?(8)
nums.push(13)
puts nums.pop() ?? -1
puts nums

let stock: Map<string, int> = { "ember": 3, "ash": 7 }
puts stock["ember"] ?? 0
puts stock["missing"] ?? 0
puts stock.keys()
puts stock.values()
stock.insert("coal", 1)
puts stock.len()
puts stock.remove("ash") ?? -1
puts stock.has?("ash")
puts stock
"##,
        "4\n[10, 6, 16, 2]\n[5, 3, 8]\n[1, 3, 5, 8]\n[1, 8, 3, 5]\n5\n1\ntrue\n13\n[5, 3, 8, 1]\n3\n0\n[\"ember\", \"ash\"]\n[3, 7]\n3\n7\nfalse\n{ \"ember\": 3, \"coal\": 1 }\n",
    );
}

#[test]
fn each_side_effects_and_index_assignment() {
    assert_success(
        r##"
let nums = [1, 2, 3]
nums.each(|n| puts(n))
nums[1] = 20
puts nums
let alias = nums
alias.push(4)
puts nums
"##,
        "1\n2\n3\n[1, 20, 3]\n[1, 20, 3, 4]\n",
    );
}

/// The capture-order contract exercised end to end: `self` first when
/// captured, then free locals in ascending `LocalId` order, chained
/// through nested lambdas (the codegen `closures_and_captures` shape).
#[test]
fn closure_capture_order_chain() {
    assert_success(
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
        "26\n",
    );
}

#[test]
fn capture_is_by_value_but_heap_stays_shared() {
    assert_success(
        r##"
def demo(): int
  let mut count = 0
  let bump = || count + 1
  count = 10
  bump()
end

puts demo()

let items = [1]
let observe = || items.len()
items.push(2)
puts observe()
"##,
        "1\n2\n",
    );
}

/// Top-level `let`s are globals, not locals: closures read them live
/// instead of capturing (the walker's M1 decision, mirrored by
/// `load_global` in compiled code).
#[test]
fn closures_read_globals_live() {
    assert_success(
        r##"
let mut count = 0
let bump = || count + 1
count = 10
puts bump()
"##,
        "11\n",
    );
}

#[test]
fn functions_and_bound_methods_as_values() {
    assert_success(
        r##"
struct Point
  x: float
  y: float

  def norm(self): float
    self.x * self.x + self.y * self.y
  end
end

def double(n: int): int
  n * 2
end

let f = double
puts f(21)
let p = Point { x: 3.0, y: 4.0 }
let bound = p.norm
puts bound()
let pusher = [1, 2].pop
puts pusher() ?? -1
"##,
        "42\n25.0\n2\n",
    );
}

// --- match with guards -------------------------------------------------

#[test]
fn match_with_guards_and_shapes() {
    assert_success(
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

puts describe(Circle(11.0))
puts describe(Circle(1.0))
puts describe(Rect(2.0, 2.0))
puts describe(Rect(2.0, 3.0))
puts describe(Dot)
puts label(0)
puts label(2)
puts unwrap(Some(3))
puts unwrap(None)
"##,
        "big circle\ncircle\nsquare\nrect\ndot\nzero\nmany\n3\n0\n",
    );
}

/// A guarded arm never counts toward exhaustiveness, so `match` cannot
/// fall through at runtime in a checked program; guard fall-through IS
/// reachable in `catch`, where a failed guard with no later match
/// propagates the original signal.
#[test]
fn catch_guard_fall_through_propagates_the_signal() {
    let (outcome, stdout) = assert_parity(
        r##"
struct GuardedError
  code: int
end

def boom(): int
  throw GuardedError { code: 1 }
end

let v = boom() catch (e)
  GuardedError if e.code > 5 => -1
end
puts v
"##,
    );
    let Outcome::Error { message } = outcome else {
        panic!("expected the error to propagate, got {outcome:?}");
    };
    assert_eq!(message, "error: GuardedError: GuardedError { code: 1 }");
    assert!(stdout.is_empty());
}

/// Tuple destructuring over the pairs a `for` loop yields from a Map,
/// the tuple source that predates tuple expressions.
#[test]
fn match_on_map_iteration_tuples() {
    assert_success(
        r##"
let grid: Map<string, int> = { "a": 0, "b": 2 }
for pair in grid
  let label = match pair
    (name, 0) => "#{name}: zero"
    (name, v) => "#{name}: #{v}"
  end
  puts label
end
"##,
        "a: zero\nb: 2\n",
    );
}

// --- tuple expressions -------------------------------------------------

#[test]
fn tuple_construction_nesting_and_display() {
    assert_success(
        r##"
let pair = (1, "a")
let one = (7,)
let nested = (1, (2, "b"))
let trailing = (1, 2,)
let computed = (1 + 2, "x" + "y")
let grouped = (1 + 2) * 3
puts pair
puts one
puts nested
puts trailing
puts computed
puts grouped
puts((1, 2).toString())
puts "in a string: #{(1, 2)}"
"##,
        "(1, \"a\")\n(7,)\n(1, (2, \"b\"))\n(1, 2)\n(3, \"xy\")\n9\n(1, 2)\nin a string: (1, 2)\n",
    );
}

#[test]
fn tuple_equality_is_structural() {
    assert_success(
        r##"
puts((1, "a") == (1, "a"))
puts((1, "a") != (1, "b"))
puts((1, (2, 3)) == (1, (2, 3)))
puts((1, [2, 3]) == (1, [2, 3]))
let a = (1, 2)
let b = a
puts(a == b)
"##,
        "true\ntrue\ntrue\ntrue\ntrue\n",
    );
}

#[test]
fn tuple_as_map_and_set_key() {
    assert_success(
        r##"
let grid: Map<(int, int), string> = { (0, 0): "origin", (1, 2): "b" }
puts grid[(0, 0)]
puts grid[(1, 2)]
puts grid[(9, 9)]
grid.insert((3, 4), "c")
puts grid[(3, 4)]
puts grid.has?((1, 2))
puts grid.len()

let seen = Set([(0, 0), (1, 1), (0, 0)])
puts seen.len()
puts seen.has?((1, 1))
puts seen.has?((5, 5))
"##,
        "Some(\"origin\")\nSome(\"b\")\nNone\nSome(\"c\")\ntrue\n3\n2\ntrue\nfalse\n",
    );
}

#[test]
fn match_and_for_destructure_constructed_tuples() {
    assert_success(
        r##"
def describe(p: (int, string)): string
  match p
    (0, s) => "zero #{s}"
    (n, "stop") => "halt at #{n}"
    (n, s) => "#{n} #{s}"
  end
end

puts describe((0, "a"))
puts describe((4, "stop"))
puts describe((7, "go"))

let points = [(0, 0), (1, 2)]
for pt in points
  let label = match pt
    (x, y) => "#{x}/#{y}"
  end
  puts label
end
"##,
        "zero a\nhalt at 4\n7 go\n0/0\n1/2\n",
    );
}

#[test]
fn tuples_flow_through_functions_and_collections() {
    assert_success(
        r##"
def swap(p: (int, string)): (string, int)
  match p
    (n, s) => (s, n)
  end
end

let pairs = [(1, "a"), (2, "b")]
let swapped = pairs.map(|p| swap(p))
puts swapped
puts swapped.len()

let nested: Vector<(int, (int, int))> = [(1, (2, 3))]
puts nested
"##,
        "[(\"a\", 1), (\"b\", 2)]\n2\n[(1, (2, 3))]\n",
    );
}

// --- catch -------------------------------------------------------------

#[test]
fn catch_user_arms_guards_and_wildcard() {
    assert_success(
        r##"
struct NetError
  detail: string
end

struct ParseFail
  line: int
end

def fetch(mode: int): string
  if mode == 0
    throw NetError { detail: "timeout" }
  elsif mode == 1
    throw ParseFail { line: 42 }
  elsif mode == 2
    throw ParseFail { line: 3 }
  end
  "<html>"
end

def attempt(mode: int): string
  fetch(mode) catch (e)
    NetError => "net: #{e.detail}"
    ParseFail if e.line > 10 => "parse far: #{e.line}"
    _ => "other"
  end
end

puts attempt(0)
puts attempt(1)
puts attempt(2)
puts attempt(9)
"##,
        "net: timeout\nparse far: 42\nother\n<html>\n",
    );
}

/// `catch!` is the exhaustive variant: every inferred error type has a
/// named arm, so the checker accepts it with no `_` and both backends
/// dispatch the same arms.
#[test]
fn catch_bang_handles_every_inferred_error() {
    assert_success(
        r##"
struct NetError
  detail: string
end

struct ParseFail
  line: int
end

def fetch(mode: int): string
  if mode == 0
    throw NetError { detail: "timeout" }
  elsif mode == 1
    throw ParseFail { line: 42 }
  end
  "<html>"
end

def attempt(mode: int): string
  fetch(mode) catch! (e)
    NetError => "net: #{e.detail}"
    ParseFail => "parse: #{e.line}"
  end
end

puts attempt(0)
puts attempt(1)
puts attempt(9)
"##,
        "net: timeout\nparse: 42\n<html>\n",
    );
}

#[test]
fn catch_panic_arms_bind_the_detail() {
    assert_success(
        r##"
let items = [1, 2, 3]
let bad = 10
let out = items[bad] catch (e)
  panics.IndexOutOfBounds => e.len()
end
puts out

let z = 0
let d = (1 / z) catch (e)
  panics.DivisionByZero => -1
  panics.IntegerOverflow => -2
end
puts d
"##,
        "29\n-1\n",
    );
}

#[test]
fn wildcard_never_catches_a_panic() {
    let (outcome, _) = assert_parity(
        r##"
let items = [1]
let idx = 5
let out = items[idx] catch (e)
  _ => -1
end
puts out
"##,
    );
    let Outcome::Panic { message } = outcome else {
        panic!("expected the panic to escape `_`, got {outcome:?}");
    };
    assert_eq!(
        message,
        "panic: panics.IndexOutOfBounds: index 5 out of range (len 1)"
    );
}

#[test]
fn catch_native_error_arm_binds_the_message() {
    assert_success(
        r##"
let n = "abc".toInt() catch (e)
  string.ParseError => e.len()
end
puts n
"##,
        "25\n",
    );
}

#[test]
fn rethrow_of_unhandled_signals() {
    let (outcome, stdout) = assert_parity(
        r##"
struct AError
  code: int
end

struct BError
  code: int
end

def boom(): int
  throw BError { code: 7 }
end

let v = boom() catch (e)
  AError => 1
end
puts v
"##,
    );
    let Outcome::Error { message } = outcome else {
        panic!("expected the error to propagate, got {outcome:?}");
    };
    assert_eq!(message, "error: BError: BError { code: 7 }");
    assert!(stdout.is_empty());
}

#[test]
fn rethrow_wrapping_replaces_the_original_error() {
    assert_success(
        r##"
struct NetError
  detail: string
end

struct ConfigError
  cause: string
end

def fetch(ok: bool): string
  if !ok
    throw NetError { detail: "down" }
  end
  "page"
end

def wrap(ok: bool): string
  fetch(ok) catch (e)
    NetError => throw ConfigError { cause: e.detail }
  end
end

let page = wrap(false) catch (e)
  ConfigError => e.cause
end
puts page
puts wrap(true)
"##,
        "down\npage\n",
    );
}

#[test]
fn nested_catch_inner_wins() {
    assert_success(
        r##"
struct AError
  code: int
end

struct BError
  code: int
end

def boom(which: int): int
  if which == 0
    throw AError { code: 1 }
  end
  throw BError { code: 2 }
end

def layered(which: int): int
  let inner = boom(which) catch (e)
    AError => 100
  end catch (e)
    BError => 200
  end
  inner
end

puts layered(0)
puts layered(1)
"##,
        "100\n200\n",
    );
}

#[test]
fn catch_inside_loops() {
    assert_success(
        r##"
struct OddError
  n: int
end

def check(n: int): int
  if n % 2 == 1
    throw OddError { n: n }
  end
  n * 10
end

let mut total = 0
for n in 1..=5
  let v = check(n) catch (e)
    OddError => -1
  end
  total = total + v
end
puts total

let mut hits = 0
let mut i = 0
while i < 4
  i = i + 1
  let v = check(i) catch (e)
    _ => 0
  end
  if v > 0
    hits = hits + 1
  end
end
puts hits
"##,
        "57\n2\n",
    );
}

#[test]
fn mutual_recursion_internal_catch() {
    assert_success(
        r##"
struct AlphaError
  code: int
end

struct BetaError
  code: int
end

def alpha(n: int): int
  if n == 0
    throw AlphaError { code: 1 }
  end
  beta(n - 1)
end

def beta(n: int): int
  if n == 0
    throw BetaError { code: 2 }
  end
  alpha(n - 1) catch (e)
    AlphaError => 100
  end
end

puts beta(3)
let fallback = beta(0) catch (e)
  BetaError => -1
end
puts fallback
"##,
        "100\n-1\n",
    );
}

#[test]
fn lambda_throw_in_map_is_caught_outside_the_hof() {
    assert_success(
        r##"
struct MapError
  index: int
end

def bump(x: int): int
  if x < 0
    throw MapError { index: x }
  end
  x + 1
end

def total(values: Vector<int>): int
  let bumped = values.map(|x| bump(x)) catch (e)
    MapError => [-1]
  end
  bumped.len()
end

puts total([1, 2, 3])
puts total([1, -2, 3])
"##,
        "3\n1\n",
    );
}

// --- unwinding edge cases ---------------------------------------------

#[test]
fn unwinding_throw_through_multiple_frames() {
    assert_success(
        r##"
struct DeepError
  depth: int
end

def level3(): int
  throw DeepError { depth: 3 }
end

def level2(): int
  level3() + 1000
end

def level1(): int
  level2() + 1000
end

let v = level1() catch (e)
  DeepError => e.depth
end
puts v
"##,
        "3\n",
    );
}

#[test]
fn unwinding_handler_in_outer_frame_skips_inner_tables() {
    assert_success(
        r##"
struct InnerError
  code: int
end

struct OuterError
  code: int
end

def inner(): int
  throw OuterError { code: 9 }
end

def middle(): int
  inner() catch (e)
    InnerError => 1
  end
end

let v = middle() catch (e)
  OuterError => e.code
end
puts v
"##,
        "9\n",
    );
}

/// The catch subject sits deep inside an expression, so the handler's
/// recorded operand depth is nonzero: the pending operands must
/// survive the truncation and the arm value must slot in correctly.
#[test]
fn unwinding_truncates_to_the_handler_depth() {
    assert_success(
        r##"
struct E
  code: int
end

def boom(): int
  throw E { code: 5 }
end

let v = 100 + (10 * (boom() catch (e)
  E => e.code
end))
puts v

let w = [1, 2, boom() catch (e)
  _ => 30
end]
puts w
"##,
        "150\n[1, 2, 30]\n",
    );
}

#[test]
fn uncaught_panic_stacktrace_matches_through_frames() {
    let (outcome, stdout) = assert_parity(
        r##"
def inner(items: Vector<int>): int
  items[99]
end

def outer(items: Vector<int>): int
  inner(items)
end

puts "start"
puts outer([1, 2])
"##,
    );
    let Outcome::Panic { message } = outcome else {
        panic!("expected a panic, got {outcome:?}");
    };
    assert_eq!(
        message,
        "panic: panics.IndexOutOfBounds: index 99 out of range (len 2)\n  in inner\n  in outer"
    );
    assert_eq!(stdout, "start\n");
}

#[test]
fn uncaught_panic_stacktrace_includes_lambdas() {
    let (outcome, _) = assert_parity(
        r##"
def apply(f: (int) -> int, v: int): int
  f(v)
end

let z = 0
puts apply(|n| n / z, 6)
"##,
    );
    let Outcome::Panic { message } = outcome else {
        panic!("expected a panic, got {outcome:?}");
    };
    assert_eq!(
        message,
        "panic: panics.DivisionByZero: division by zero\n  in <lambda>\n  in apply"
    );
}

// --- for loops ---------------------------------------------------------

#[test]
fn for_loops_ranges_and_collections() {
    assert_success(
        r##"
let mut total = 0
for i in 1..=5
  if i % 2 == 0
    continue
  end
  total = total + i
end
puts total

for n in [10, 20]
  puts n
end

for c in "ab"
  puts c
end

let mut count = 0
for i in 0..10
  if i == 3
    break
  end
  count = count + 1
end
puts count
"##,
        "9\n10\n20\na\nb\n3\n",
    );
}

#[test]
fn for_loop_tuple_binding_over_maps() {
    assert_success(
        r##"
let stock: Map<string, int> = { "ember": 3, "ash": 7 }
for (name, count) in stock
  puts "#{name} -> #{count}"
end
"##,
        "ember -> 3\nash -> 7\n",
    );
}

#[test]
fn for_loop_snapshots_the_collection() {
    assert_success(
        r##"
let items = [1, 2]
for n in items
  items.push(n + 10)
end
puts items
"##,
        "[1, 2, 11, 12]\n",
    );
}

// --- structs, methods, toString ---------------------------------------

#[test]
fn structs_methods_and_user_to_string() {
    assert_success(
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
puts([p])
"##,
        "52.0\n(6.0, 4.0)\n(6.0, 4.0)\n[(6.0, 4.0)]\n",
    );
}

#[test]
fn derived_to_string_rendering() {
    assert_success(
        r##"
struct Pair
  a: int
  label: string
end

enum Kind
  Plain
  Tagged(name: string)
end

let p = Pair { a: 1, label: "x" }
puts p
let plain: Kind = Plain
puts plain
let tagged: Kind = Tagged("deep")
puts tagged
puts([Some(1), None])
let m: Map<string, Vector<float>> = { "k": [1.0] }
puts m
let r = 0..10
puts r
let ri = 0..=10
puts ri
puts Some("quoted")
"##,
        "Pair { a: 1, label: \"x\" }\nPlain\nTagged(\"deep\")\n[Some(1), None]\n{ \"k\": [1.0] }\n0..10\n0..=10\nSome(\"quoted\")\n",
    );
}

#[test]
fn struct_literal_field_reordering() {
    assert_success(
        r##"
struct Wide
  a: int
  b: int
  c: int
end

def trace(label: string, v: int): int
  puts label
  v
end

let w = Wide { c: trace("c", 3), a: trace("a", 1), b: trace("b", 2) }
puts w
"##,
        "c\na\nb\nWide { a: 1, b: 2, c: 3 }\n",
    );
}

/// `T: Comparable` inside an unmonomorphized generic reaches the plain
/// ordering ops (the code generator cannot specialize them), so the VM
/// must order dynamically like the walker. Only `int`/`float`/
/// `string`/`char` satisfy `Comparable` in the current checker.
#[test]
fn comparable_generics_order_through_the_plain_ops() {
    assert_success(
        r##"
def maxOf<T: Comparable>(a: T, b: T): T
  if a > b then a else b end
end

puts maxOf(5, 3)
puts maxOf("a", "b")
puts maxOf(1.5, 2.5)
puts maxOf('x', 'c')
"##,
        "5\nb\n2.5\nx\n",
    );
}

// --- structural interfaces as generic constraints ----------------------
//
// A constrained generic body is compiled once for every instantiation
// (`docs/spec/03-types.md`, "Generics: execution model"), so the
// constraint's method has no single static target: the walker
// dispatches on the runtime value and the VM must reach the same
// target through the value's method table.

#[test]
fn named_interface_constraint_dispatches_to_the_struct_method() {
    assert_success(
        r##"
interface Printable2
  def label(self): string
end

struct Tag
  name: string

  def label(self): string
    "tag:" + self.name
  end
end

def show<T: Printable2>(v: T): string
  v.label()
end

puts show(Tag { name: "x" })
"##,
        "tag:x\n",
    );
}

#[test]
fn inline_interface_constraint_dispatches_to_the_struct_method() {
    assert_success(
        r##"
struct Tag
  name: string

  def label(self): string
    "tag:" + self.name
  end
end

def show<T: { def label(self): string }>(v: T): string
  v.label()
end

puts show(Tag { name: "x" })
"##,
        "tag:x\n",
    );
}

/// One shared body reached at two concrete types, plus repeated and
/// nested constraint calls: the dispatch is per value, not per call
/// site.
#[test]
fn one_generic_body_serves_several_concrete_types() {
    assert_success(
        r##"
interface Labeled
  def label(self): string
end

struct A
  n: int

  def label(self): string
    "A#{self.n}"
  end
end

struct B
  s: string

  def label(self): string
    "B" + self.s
  end
end

def twice<T: Labeled>(v: T): string
  v.label() + "/" + v.label()
end

def outer<T: Labeled>(v: T): string
  "[" + twice(v) + "]"
end

puts outer(A { n: 1 })
puts outer(B { s: "z" })
"##,
        "[A1/A1]\n[Bz/Bz]\n",
    );
}

/// The constrained receiver crossing a lambda capture, a `for` body,
/// the pipe operator, and a higher-order builtin.
#[test]
fn constraint_methods_work_through_lambdas_loops_and_pipes() {
    assert_success(
        r##"
interface Labeled
  def label(self): string
end

struct A
  n: int

  def label(self): string
    "A#{self.n}"
  end
end

def wrap(s: string): string
  "<" + s + ">"
end

def viaLambda<T: Labeled>(v: T): string
  let f = || v.label()
  f()
end

def viaFor<T: Labeled>(v: T): string
  let mut out = ""
  for i in 0..2
    out = out + v.label()
  end
  out
end

def viaPipe<T: Labeled>(v: T): string
  v.label() |> wrap()
end

def viaMap<T: Labeled>(v: T): string
  [1, 2].map(|i| v.label()).join(",")
end

let a = A { n: 1 }
puts viaLambda(a)
puts viaFor(a)
puts viaPipe(a)
puts viaMap(a)
"##,
        "A1\nA1A1\n<A1>\nA1,A1\n",
    );
}

/// The constraint method taking arguments (including `Self`), and a
/// generic struct satisfying the constraint.
#[test]
fn constraint_methods_take_arguments_and_generic_receivers() {
    assert_success(
        r##"
interface Adder
  def add(self, other: Self): int
end

struct N
  v: int

  def add(self, other: N): int
    self.v + other.v
  end
end

interface Labeled
  def label(self): string
end

struct Box<T>
  item: T

  def label(self): string
    "box"
  end
end

def sum<T: Adder>(a: T, b: T): int
  a.add(b)
end

def show<T: Labeled>(v: T): string
  v.label()
end

puts sum(N { v: 1 }, N { v: 2 })
puts show(Box { item: 1 })
puts show(Box { item: "s" })
"##,
        "3\nbox\nbox\n",
    );
}

/// A builtin type satisfying a user interface: the constraint method is
/// a builtin method name, so the dynamic lookup must fall through to
/// the builtin table exactly like the walker.
#[test]
fn builtin_receivers_satisfy_user_interfaces() {
    assert_success(
        r##"
interface Lengthy
  def len(self): int
end

def size<T: Lengthy>(v: T): int
  v.len()
end

puts size("abc")
puts size([1, 2])
puts size({"k": 1})
"##,
        "3\n2\n1\n",
    );
}

/// The universal `toString` on a generic value, with and without a
/// user override, and a constraint method bound as a value instead of
/// called.
#[test]
fn generic_receivers_bind_members_and_render_to_string() {
    assert_success(
        r##"
interface Labeled
  def label(self): string
end

struct A
  n: int

  def label(self): string
    "A"
  end
end

struct Custom
  n: int

  def label(self): string
    "c"
  end

  def toString(self): string
    "custom<#{self.n}>"
  end
end

def bound<T: Labeled>(v: T): string
  let f = v.label
  f()
end

def render<T>(v: T): string
  v.toString()
end

puts bound(A { n: 1 })
puts render(A { n: 1 })
puts render(Custom { n: 2 })
puts render(5)
puts render([1, 2])
"##,
        "A\nA { n: 1 }\ncustom<2>\n5\n[1, 2]\n",
    );
}

/// A struct field holding a callable shadows nothing on the call path
/// (methods win) but is what a bare member read yields; both reach the
/// generic receiver only through the dynamic lookup.
#[test]
fn generic_receivers_reach_struct_fields_holding_callables() {
    assert_success(
        r##"
interface Labeled
  def label(self): string
end

struct FieldOnly
  label: () -> string
end

def show<T: Labeled>(v: T): string
  v.label()
end

def bound<T: Labeled>(v: T): string
  let f = v.label
  f()
end

let v = FieldOnly { label: || "fromField" }
puts show(v)
puts bound(v)
"##,
        "fromField\nfromField\n",
    );
}

/// `?.` on an `Option` of a constrained parameter: the flattened
/// member call still lands on the dynamic path.
#[test]
fn optional_chaining_on_a_generic_receiver() {
    assert_success(
        r##"
interface Labeled
  def label(self): string
end

struct A
  n: int

  def label(self): string
    "A#{self.n}"
  end
end

def show<T: Labeled>(v: Option<T>): Option<string>
  v?.label()
end

puts show(Some(A { n: 1 }))
let missing: Option<A> = None
puts show(missing)
"##,
        "Some(\"A1\")\nNone\n",
    );
}

/// A signal raised inside a constraint method unwinds through the
/// dynamic call site like any other frame.
#[test]
fn signals_unwind_through_a_constraint_method_call() {
    assert_success(
        r##"
interface Risky
  def boom(self): int
end

struct BadThing
  why: string
end

struct R
  def boom(self): int
    throw BadThing { why: "no" }
  end
end

def run<T: Risky>(v: T): int
  v.boom()
end

let out = run(R {}) catch (e)
  BadThing => -1
end
puts out
"##,
        "-1\n",
    );
}

/// Recursion through a constraint method must be bounded by the shared
/// call-depth guard, not by the host stack: the dynamic call enters its
/// frame in place, exactly like a direct call.
#[test]
fn recursion_through_a_constraint_method_hits_the_depth_guard() {
    let source = r##"
interface Steps
  def step(self, n: int): int
end

struct Down
  def step(self, n: int): int
    self.step(n + 1)
  end
end

def go<T: Steps>(v: T, n: int): int
  v.step(n)
end

puts go(Down {}, 0)
"##;

    let (outcome, _) = assert_parity_with_depth(source, 64);
    let Outcome::Panic { message } = outcome else {
        panic!("expected a panic, got {outcome:?}");
    };
    assert!(
        message.contains("panics.StackOverflow"),
        "unexpected message: {message}"
    );
}

// --- globals, entry, recursion ----------------------------------------

#[test]
fn main_runs_after_the_top_level_and_globals_flow() {
    assert_success(
        r##"
puts "top"

let greeting = "hello from a global"

def main()
  puts greeting
  puts "main"
end

puts "level"
"##,
        "top\nlevel\nhello from a global\nmain\n",
    );
}

#[test]
fn global_used_before_initialization_is_the_same_fatal() {
    let (outcome, stdout) = assert_parity(
        r##"
def peek(): int
  late
end

puts peek()

let late = 7
"##,
    );
    let Outcome::Error { message } = outcome else {
        panic!("expected an error, got {outcome:?}");
    };
    assert_eq!(message, "brasa: `late` used before initialization");
    assert!(stdout.is_empty());
}

#[test]
fn recursion_depth_guard_panics_identically() {
    let (outcome, stdout) = assert_parity_with_depth(
        r##"
def spin(n: int): int
  spin(n + 1)
end

puts spin(0)
"##,
        64,
    );
    let Outcome::Panic { message } = outcome else {
        panic!("expected a panic, got {outcome:?}");
    };
    assert!(
        message.starts_with("panic: panics.StackOverflow: recursion limit (64 frames) exceeded"),
        "{message}"
    );
    assert!(message.contains("\n  in spin"), "{message}");
    assert!(stdout.is_empty());
}

#[test]
fn recursion_limit_panic_is_catchable_by_its_named_arm() {
    let (outcome, stdout) = assert_parity_with_depth(
        r##"
def spin(n: int): int
  spin(n + 1)
end

let d = spin(0) catch (e)
  panics.StackOverflow => -1
end
puts d
"##,
        64,
    );
    assert_eq!(outcome, Outcome::Success);
    assert_eq!(stdout, "-1\n");
}

#[test]
fn plain_recursion_agrees() {
    assert_success(
        r##"
def fib(n: int): int
  if n < 2
    n
  else
    fib(n - 1) + fib(n - 2)
  end
end

puts fib(15)
"##,
        "610\n",
    );
}

// --- uncaught signals --------------------------------------------------

#[test]
fn uncaught_error_message_matches() {
    let (outcome, stdout) = assert_parity(
        r##"
struct BoomError
  why: string
end

puts "before"
throw BoomError { why: "kaput" }
"##,
    );
    let Outcome::Error { message } = outcome else {
        panic!("expected an error, got {outcome:?}");
    };
    assert_eq!(message, "error: BoomError: BoomError { why: \"kaput\" }");
    assert_eq!(stdout, "before\n");
}

#[test]
fn math_module_members_agree() {
    assert_success(
        r##"
import std::math

puts math.sqrt(9.0)
puts math.floor(1.7)
puts math.ceil(1.2)
puts math.round(1.5)
puts math.pow(2.0, 10.0)
puts math.abs(-3.5)
puts math.abs(-3)
puts math.min(2, 5)
puts math.max(2.0, 5.0)
"##,
        "3.0\n1.0\n2.0\n2.0\n1024.0\n3.5\n3\n2\n5.0\n",
    );
}

#[test]
fn option_wrapping_and_safe_navigation() {
    assert_success(
        r##"
let maybe: Option<int> = Some(41)
puts maybe?.toString()
puts maybe ?? 0
let empty: Option<int> = None
puts empty ?? -1
"##,
        "Some(\"41\")\n41\n-1\n",
    );
}

// --- output stream failures -------------------------------------------

/// A writer whose every write fails with the given error kind.
struct FailingWriter(std::io::ErrorKind);

impl std::io::Write for FailingWriter {
    fn write(&mut self, _buf: &[u8]) -> std::io::Result<usize> {
        Err(std::io::Error::new(self.0, "injected write failure"))
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Compiles `source` and runs only the VM into `out`.
fn run_vm_into<W: std::io::Write + Send>(source: &str, out: &mut W) -> Outcome {
    let mut sources = brasa_source::SourceMap::new();
    let file = sources.add_file("parity.brs", source.to_string());

    let parsed = brasa_parser::parse(source, file);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);

    let lowered = brasa_hir::lower(&parsed.ast, &parsed.roots);
    assert!(lowered.diagnostics.is_empty(), "{:?}", lowered.diagnostics);

    let resolved = brasa_resolver::resolve(&lowered.hir, &lowered.roots);
    assert!(
        resolved.diagnostics.is_empty(),
        "{:?}",
        resolved.diagnostics
    );

    let checked = brasa_typeck::check(
        &lowered.hir,
        &lowered.roots,
        &resolved.resolutions,
        &lowered.sugar_origins,
    );
    assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);

    let module = brasa_codegen::compile(
        &lowered.hir,
        &lowered.roots,
        &resolved.resolutions,
        &checked.types,
    );
    brasa_vm::run(&module, out, &[])
}

#[test]
fn broken_pipe_on_write_becomes_the_silent_broken_pipe_outcome() {
    let mut out = FailingWriter(std::io::ErrorKind::BrokenPipe);
    let outcome = run_vm_into("puts \"hi\"\n", &mut out);

    assert_eq!(outcome, Outcome::BrokenPipe);
}

#[test]
fn broken_pipe_is_not_catchable() {
    let source = r##"
let v = 0 catch (e)
  _ => 1
end
puts "hi"
puts v
"##;
    let mut out = FailingWriter(std::io::ErrorKind::BrokenPipe);
    let outcome = run_vm_into(source, &mut out);

    assert_eq!(outcome, Outcome::BrokenPipe);
}

#[test]
fn other_write_errors_stay_fatal() {
    let mut out = FailingWriter(std::io::ErrorKind::Other);
    let outcome = run_vm_into("puts \"hi\"\n", &mut out);

    let Outcome::Error { message } = outcome else {
        panic!("expected an error outcome, got {outcome:?}");
    };
    assert!(message.contains("failed to write output"), "{message}");
}

// --- std::proc + std::env (BRS-32) -------------------------------------

#[test]
fn proc_run_captures_stdout_stderr_and_code() {
    assert_success(
        r##"
import std::proc
let out = proc.run(["/bin/sh", "-c", "printf hi; printf err 1>&2"])
puts out.stdout
puts out.stderr
puts out.code
puts out
"##,
        "hi\nerr\n0\nOutput { stdout: \"hi\", stderr: \"err\", code: 0 }\n",
    );
}

#[test]
fn proc_run_string_sugar_splits_on_whitespace() {
    assert_success(
        r##"
import std::proc
puts proc.run("echo hi").stdout.trim()
"##,
        "hi\n",
    );
}

#[test]
fn proc_run_non_zero_exit_throws_a_catchable_native_error() {
    assert_success(
        r##"
import std::proc
let message = proc.run(["/bin/sh", "-c", "printf boom 1>&2; exit 3"]).stdout catch (e)
  proc.NonZeroExit => e
end
puts message
"##,
        "command `/bin/sh -c printf boom 1>&2; exit 3` exited with code 3: boom\n",
    );

    let (outcome, _) =
        assert_parity("import std::proc\nputs proc.run([\"/bin/sh\", \"-c\", \"exit 7\"])\n");
    let Outcome::Error { message } = outcome else {
        panic!("expected an error, got {outcome:?}");
    };
    assert_eq!(
        message,
        "error: proc.NonZeroExit: command `/bin/sh -c exit 7` exited with code 7"
    );
}

#[test]
fn proc_try_run_reports_the_code_without_throwing() {
    assert_success(
        r##"
import std::proc
puts proc.tryRun(["/bin/sh", "-c", "exit 5"]).code
puts proc.tryRun(["/bin/sh", "-c", "kill -9 $$"]).code
puts proc.tryRun("true").code
puts proc.tryRun("false").code
"##,
        "5\n137\n0\n1\n",
    );
}

#[test]
fn proc_stdin_round_trips_through_cat() {
    assert_success(
        r##"
import std::proc
puts proc.run(["cat"], "hello\nworld").stdout
puts proc.run(["cat"], "").stdout + "|"
"##,
        "hello\nworld\n|\n",
    );
}

#[test]
fn proc_shell_runs_a_pipeline() {
    assert_success(
        r##"
import std::proc
puts proc.shell("printf 'a\nb\nc' | wc -l").stdout.trim()
puts proc.shell("cat | wc -c", "1234").stdout.trim()
"##,
        "2\n4\n",
    );
}

#[test]
fn proc_spawn_failures_throw_spawn_error() {
    assert_success(
        r##"
import std::proc
let missing = proc.run(["/definitely/not/a/real/brasa-binary"]).stdout catch (e)
  proc.SpawnError => "missing"
end
puts missing
let empty = proc.tryRun("").stdout catch (e)
  proc.SpawnError => e
end
puts empty
"##,
        "missing\nempty command\n",
    );
}

#[test]
fn env_get_set_and_vars_agree() {
    assert_success(
        r##"
import std::env
puts env.get("BRASA_PARITY_DEFINITELY_MISSING")
env.set("BRASA_PARITY_VAR", "v1")
puts env.get("BRASA_PARITY_VAR")
puts env.vars().get("BRASA_PARITY_VAR")
"##,
        "None\nSome(\"v1\")\nSome(\"v1\")\n",
    );
}

#[test]
fn env_set_overrides_reach_spawned_children() {
    assert_success(
        r##"
import std::proc
import std::env
env.set("BRASA_PARITY_CHILD", "inherited")
puts proc.shell("printf %s \"$BRASA_PARITY_CHILD\"").stdout
"##,
        "inherited\n",
    );
}

#[test]
fn env_args_are_the_script_arguments() {
    let args = vec!["alpha".to_string(), "beta gamma".to_string()];
    let (outcome, stdout) = assert_parity_configured(
        "import std::env\nputs env.args()\n",
        brasa_vm::DEFAULT_MAX_CALL_DEPTH,
        &args,
    );
    assert_eq!(outcome, Outcome::Success);
    assert_eq!(stdout, "[\"alpha\", \"beta gamma\"]\n");

    let (outcome, stdout) = assert_parity("import std::env\nputs env.args()\n");
    assert_eq!(outcome, Outcome::Success);
    assert_eq!(stdout, "[]\n");
}

// --- std::fs + path helpers + env.cwd/env.cd (BRS-33) ------------------

/// A fresh unique temp directory for one fs parity test. Not cleaned
/// automatically: each test removes it best-effort at the end.
fn fs_temp_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("brasa-fs-parity-{tag}-{}", std::process::id()));

    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir is creatable");
    dir
}

#[test]
fn fs_write_read_append_and_predicates() {
    let tmp = fs_temp_dir("rw");
    let t = tmp.display();

    std::fs::write(tmp.join("bin.dat"), [0xff_u8, 0xfe]).expect("fixture written");

    // Parity runs the program three times (walker, VM, hot-GC VM);
    // `write` truncates, so the script is naturally re-runnable.
    assert_success(
        &format!(
            r##"
import std::fs
let file = "{t}/data.txt"
fs.write(file, "hello")
fs.append(file, " world")
puts fs.read(file)
puts fs.exists?(file)
puts fs.isFile?(file)
puts fs.isDir?(file)
puts fs.isDir?("{t}")
puts fs.exists?("{t}/missing")
let bad = fs.read("{t}/bin.dat") catch (e)
  fs.IoError => "bad utf8"
end
puts bad
let gone = fs.read("{t}/missing") catch (e)
  fs.NotFound => "gone"
end
puts gone
"##
        ),
        "hello world\ntrue\ntrue\nfalse\ntrue\nfalse\nbad utf8\ngone\n",
    );

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn fs_ls_glob_and_walk_are_sorted() {
    let tmp = fs_temp_dir("list");
    let t = tmp.display();

    std::fs::write(tmp.join("b.txt"), "b").expect("fixture written");
    std::fs::write(tmp.join("a.txt"), "a").expect("fixture written");
    std::fs::write(tmp.join("c.md"), "c").expect("fixture written");
    std::fs::create_dir_all(tmp.join("sub/deep")).expect("fixture dirs");
    std::fs::write(tmp.join("sub/d.txt"), "d").expect("fixture written");
    std::fs::write(tmp.join("sub/deep/e.txt"), "e").expect("fixture written");

    assert_success(
        &format!(
            r##"
import std::fs
puts fs.ls("{t}")
puts fs.glob("{t}/*.txt")
puts fs.walk("{t}")
let bad = fs.glob("[") catch (e)
  fs.IoError => ["bad pattern"]
end
puts bad
"##
        ),
        &format!(
            "[\"a.txt\", \"b.txt\", \"c.md\", \"sub\"]\n\
             [\"{t}/a.txt\", \"{t}/b.txt\"]\n\
             [\"{t}/a.txt\", \"{t}/b.txt\", \"{t}/c.md\", \"{t}/sub/d.txt\", \"{t}/sub/deep/e.txt\"]\n\
             [\"bad pattern\"]\n"
        ),
    );

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn fs_mkdir_rm_cp_and_mv_round_trip() {
    let tmp = fs_temp_dir("tree");
    let t = tmp.display();

    assert_success(
        &format!(
            r##"
import std::fs
let work = "{t}/work"
if fs.exists?(work)
  fs.rmAll(work)
end
fs.mkdirAll(work + "/a/b")
fs.mkdir(work + "/solo")
fs.mkdir(work + "/solo") catch (e)
  fs.IoError => puts "already there"
end
fs.write(work + "/a/f.txt", "payload")
fs.cp(work + "/a/f.txt", work + "/a/g.txt")
fs.mv(work + "/a/g.txt", work + "/a/b/h.txt")
puts fs.read(work + "/a/b/h.txt")
puts fs.exists?(work + "/a/g.txt")
fs.rm(work + "/a/b/h.txt")
fs.rm(work + "/a/b")
puts fs.exists?(work + "/a/b")
fs.rmAll(work)
puts fs.exists?(work)
"##
        ),
        "already there\npayload\nfalse\nfalse\nfalse\n",
    );

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn fs_path_helpers_are_pure_and_lexical() {
    assert_success(
        r##"
import std::fs
puts fs.join("a", "b")
puts fs.join("a/", "b")
puts fs.join("a", "/etc")
puts fs.base("a/b/c.txt")
puts fs.base("a/b/")
puts fs.base("/")
puts fs.dir("a/b/c.txt")
puts fs.dir("a/b/")
puts fs.dir("a")
puts fs.ext("x.tar.gz")
puts fs.ext(".bashrc")
puts fs.ext("noext")
puts fs.abs("/x/./y/../z")
puts fs.abs("/../up")
"##,
        "a/b\na/b\n/etc\nc.txt\nb\n\na/b\na\n\ngz\n\n\n/x/z\n/up\n",
    );
}

#[test]
fn env_cwd_cd_and_relative_abs_agree() {
    let tmp = fs_temp_dir("cwd");
    // `getcwd` reports a symlink-free path, so the embedded expectation
    // must be canonical too.
    let canonical = std::fs::canonicalize(&tmp).expect("temp dir canonicalizes");
    let t = canonical.display();

    // `env.cd` moves the real process cwd; the script restores it, and
    // no other test in this binary depends on relative paths.
    assert_success(
        &format!(
            r##"
import std::env
import std::fs
let orig = env.cwd()
env.cd("{t}")
puts env.cwd() == "{t}"
puts fs.abs("rel.txt") == "{t}/rel.txt"
env.cd(orig)
puts env.cwd() == orig
env.cd("{t}/missing") catch (e)
  fs.NotFound => puts "cd missing"
end
"##
        ),
        "true\ntrue\ntrue\ncd missing\n",
    );

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
#[cfg(unix)]
fn fs_denied_maps_permission_errors() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = fs_temp_dir("denied");
    let locked = tmp.join("locked");
    std::fs::create_dir(&locked).expect("locked dir created");
    std::fs::write(locked.join("secret.txt"), "s").expect("fixture written");
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000))
        .expect("permissions set");

    // Root (some sandboxes) ignores mode bits; probe first and skip
    // honestly instead of asserting a Denied that cannot happen.
    let denied = matches!(
        std::fs::read(locked.join("secret.txt")),
        Err(err) if err.kind() == std::io::ErrorKind::PermissionDenied
    );

    if denied {
        let t = tmp.display();
        assert_success(
            &format!(
                r##"
import std::fs
let blocked = fs.read("{t}/locked/secret.txt") catch (e)
  fs.Denied => "denied"
end
puts blocked
"##
            ),
            "denied\n",
        );
    } else {
        eprintln!("skipping the fs.Denied assertion: this user bypasses mode 000");
    }

    let _ = std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o700));
    let _ = std::fs::remove_dir_all(&tmp);
}

// --- std::json (BRS-34) ------------------------------------------------

#[test]
fn json_parse_stringify_and_to_string_agree() {
    // Objects live in a sorted map: stringify (and toString, which is
    // the same text) emits compact JSON with bytewise-sorted keys,
    // regardless of the source document's member order.
    assert_success(
        r##"
import std::json
let data = json.parse("{\"b\": 2, \"a\": [true, null, \"x\"], \"f\": 2.0}")
puts json.stringify(data)
puts data
puts "inline: #{data}"
"##,
        "{\"a\":[true,null,\"x\"],\"b\":2,\"f\":2.0}\n{\"a\":[true,null,\"x\"],\"b\":2,\"f\":2.0}\ninline: {\"a\":[true,null,\"x\"],\"b\":2,\"f\":2.0}\n",
    );
}

#[test]
fn json_indexing_chains_yield_option_and_flatten() {
    assert_success(
        r##"
import std::json
let data = json.parse("{\"users\": [{\"name\": \"ada\"}, {\"name\": \"grace\"}]}")
puts data["users"][0]["name"].asString() ?? "anon"
puts data["users"][1]["name"].asString() ?? "anon"
puts data["users"][2]["name"].asString() ?? "anon"
puts data["missing"][0]["name"].asString() ?? "anon"
puts data["users"][-1].null?()
puts data["users"]["not an index"].asString() ?? "wrong kind"
"##,
        "ada\ngrace\nanon\nanon\nfalse\nwrong kind\n",
    );
}

#[test]
fn json_accessors_agree() {
    // `asInt` takes only integral i64 numbers; `asFloat` takes every
    // number; no coercions between JSON kinds. `null?` distinguishes
    // an explicit `null` from an absent member.
    assert_success(
        r##"
import std::json
let data = json.parse("{\"n\": 2, \"f\": 2.5, \"s\": \"hi\", \"b\": true, \"z\": null, \"v\": [1, 2], \"o\": {\"k\": 1}}")
puts data["n"].asInt() ?? -1
puts data["n"].asFloat() ?? -1.0
puts data["f"].asInt() ?? -1
puts data["f"].asFloat() ?? -1.0
puts data["s"].asString() ?? "?"
puts data["b"].asBool() ?? false
puts data["z"].null?()
puts data["missing"].null?()
puts data["s"].asInt() ?? -1
let items: Vector<Json> = data["v"].asArray() ?? []
puts items.len()
puts items[0].asInt() ?? -1
let members: Map<string, Json> = data["o"].asObject() ?? {}
puts members.len()
puts members["k"].asInt() ?? -1
"##,
        "2\n2.0\n-1\n2.5\nhi\ntrue\ntrue\nfalse\n-1\n2\n1\n1\n1\n",
    );
}

#[test]
fn json_equality_is_structural_over_the_tree() {
    assert_success(
        r##"
import std::json
let a = json.parse("{\"x\": [1, 2]}")
let b = json.parse("{ \"x\" : [ 1 , 2 ] }")
let c = json.parse("{\"x\": [1, 2.0]}")
puts a == b
puts a == c
"##,
        "true\nfalse\n",
    );
}

#[test]
fn json_parse_errors_are_catchable_with_position() {
    assert_success(
        r##"
import std::json
let bad = json.stringify(json.parse("{\n  \"a\": }")) catch (e)
  json.ParseError => e
end
puts bad
"##,
        "cannot parse JSON: expected value at line 2 column 8\n",
    );
}

#[test]
fn json_uncaught_parse_error_message_matches() {
    let (outcome, stdout) = assert_parity("import std::json\njson.parse(\"nope\")\n");
    assert_eq!(stdout, "");
    assert_eq!(
        outcome,
        Outcome::Error {
            message: "error: json.ParseError: cannot parse JSON: expected ident at line 1 column 2"
                .to_string()
        }
    );
}

// --- BRS-35: collection surfaces, math/time/rand closure --------------

#[test]
fn vector_reduce_find_any_all() {
    assert_success(
        r##"
let nums = [1, 2, 3, 4]
puts nums.reduce(0, |acc, x| acc + x)
puts nums.reduce(1, |acc, x| acc * x)
puts nums.reduce("", |acc, x| acc + x.toString())
puts nums.find(|x| x > 2) ?? -1
puts nums.find(|x| x > 9) ?? -1
puts nums.any?(|x| x % 2 == 0)
puts nums.any?(|x| x > 9)
puts nums.all?(|x| x > 0)
puts nums.all?(|x| x > 1)
let empty: Vector<int> = []
puts empty.reduce(10, |acc, x| acc + x)
puts empty.any?(|x| true)
puts empty.all?(|x| false)
"##,
        "10\n24\n1234\n3\n-1\ntrue\nfalse\ntrue\nfalse\n10\nfalse\ntrue\n",
    );
}

#[test]
fn vector_sort_zip_flatten_uniq() {
    assert_success(
        r##"
let nums = [3, 1, 2]
puts nums.sort()
puts nums
let floats = [2.5, 1.5, 3.5]
puts floats.sort()
let words = ["pear", "apple", "fig"]
puts words.sort()
let chars = ['b', 'a']
puts chars.sort()
let pairs = [1, 2].zip(["a", "b", "c"])
puts pairs
let mixed = [1, 2, 3].zip([true])
puts mixed
let nested = [[1, 2], [3], []]
puts nested.flatten()
let dupes = [1, 2, 1, 3, 2]
puts dupes.uniq()
let empty: Vector<int> = []
puts empty.sort()
puts empty.uniq()
"##,
        "[1, 2, 3]\n[3, 1, 2]\n[1.5, 2.5, 3.5]\n[\"apple\", \"fig\", \"pear\"]\n['a', 'b']\n[(1, \"a\"), (2, \"b\")]\n[(1, true)]\n[1, 2, 3]\n[1, 2, 3]\n[]\n[]\n",
    );
}

#[test]
fn vector_sort_nan_panics_match() {
    let (outcome, _) = assert_parity("let v = [1.0, 0.0 / 0.0]\nv.sort()\n");
    let Outcome::Panic { message } = outcome else {
        panic!("expected a panic, got {outcome:?}");
    };
    assert!(
        message.contains("cannot sort a NaN element"),
        "unexpected message: {message}"
    );
}

#[test]
fn map_entries_merge_each() {
    assert_success(
        r##"
let stock: Map<string, int> = { "ember": 2, "ash": 1 }
puts stock.entries()
let extra: Map<string, int> = { "ash": 5, "coal": 3 }
let merged = stock.merge(extra)
puts merged
puts stock
puts extra
stock.each(|name, count| puts("#{name}=#{count}"))
"##,
        "[(\"ember\", 2), (\"ash\", 1)]\n{ \"ember\": 2, \"ash\": 5, \"coal\": 3 }\n{ \"ember\": 2, \"ash\": 1 }\n{ \"ash\": 5, \"coal\": 3 }\nember=2\nash=1\n",
    );
}

#[test]
fn set_algebra_members() {
    assert_success(
        r##"
let a = Set([1, 2, 3])
let b = Set([3, 4])
puts a.union(b)
puts a.intersect(b)
puts a.diff(b)
puts b.diff(a)
puts a
puts b
puts a.union(a)
"##,
        "Set([1, 2, 3, 4])\nSet([3])\nSet([1, 2])\nSet([4])\nSet([1, 2, 3])\nSet([3, 4])\nSet([1, 2, 3])\n",
    );
}

#[test]
fn math_constants_and_polymorphic_members() {
    assert_success(
        r##"
import std::math

puts math.pi
puts math.e
puts math.max(math.pi, math.e)
puts math.min(math.pi, math.e)
let tau = math.pi * 2.0
puts math.floor(tau)
"##,
        "3.141592653589793\n2.718281828459045\n3.141592653589793\n2.718281828459045\n6.0\n",
    );
}

#[test]
fn time_iso_formatting_is_pinned() {
    assert_success(
        r##"
import std::time

puts time.iso(0)
puts time.iso(1700000000123)
puts time.iso(951782400000)
puts time.iso(-1)
"##,
        "1970-01-01T00:00:00.000Z\n2023-11-14T22:13:20.123Z\n2000-02-29T00:00:00.000Z\n1969-12-31T23:59:59.999Z\n",
    );
}

#[test]
fn time_clock_properties_hold() {
    assert_success(
        r##"
import std::time

let a = time.now()
let b = time.now()
puts b >= a
puts a > 1700000000.0
let m1 = time.nowMillis()
time.sleep(15)
let m2 = time.nowMillis()
puts m2 - m1 >= 15
time.sleep(0)
puts "done"
"##,
        "true\ntrue\ntrue\ndone\n",
    );
}

#[test]
fn time_negative_sleep_panics_match() {
    let (outcome, _) = assert_parity("import std::time\ntime.sleep(-1)\n");
    let Outcome::Panic { message } = outcome else {
        panic!("expected a panic, got {outcome:?}");
    };
    assert!(
        message.contains("cannot sleep a negative duration"),
        "unexpected message: {message}"
    );
}

#[test]
fn rand_seeded_sequences_are_pinned() {
    // The pinned values are the shared xoshiro256** sequence for seed
    // 42 (`brasa_interp::rand_glue`); a change here means the PRNG or
    // its consumption order changed, which breaks seeded scripts.
    assert_success(
        r##"
import std::rand

rand.seed(42)
puts rand.int(0..100)
puts rand.int(0..100)
puts rand.int(1..=6)
puts rand.float()
puts rand.choice(["ember", "ash", "coal"])
puts rand.shuffle([1, 2, 3, 4, 5])
rand.seed(42)
puts rand.int(0..100)
puts rand.int(-5..=5)
"##,
        "42\n2\n6\n0.9246929453253876\nash\n[2, 4, 1, 3, 5]\n42\n2\n",
    );
}

#[test]
fn rand_unseeded_properties_hold() {
    assert_success(
        r##"
import std::rand

let n = rand.int(10..20)
puts n >= 10 && n < 20
let m = rand.int(-3..=3)
puts m >= -3 && m <= 3
let f = rand.float()
puts f >= 0.0 && f < 1.0
puts rand.int(3..=3)
puts rand.choice([7])
puts rand.shuffle([9]) 
"##,
        "true\ntrue\ntrue\n3\n7\n[9]\n",
    );
}

#[test]
fn rand_empty_picks_panic() {
    let (outcome, _) = assert_parity("import std::rand\nrand.int(5..5)\n");
    let Outcome::Panic { message } = outcome else {
        panic!("expected a panic, got {outcome:?}");
    };
    assert!(
        message.contains("cannot pick from an empty range"),
        "unexpected message: {message}"
    );

    let (outcome, _) = assert_parity("import std::rand\nlet v: Vector<int> = []\nrand.choice(v)\n");
    let Outcome::Panic { message } = outcome else {
        panic!("expected a panic, got {outcome:?}");
    };
    assert!(
        message.contains("cannot pick from an empty vector"),
        "unexpected message: {message}"
    );
}

// --- hashed Map/Set tables --------------------------------------------

#[test]
fn large_hashed_map_and_set_agree_across_backends() {
    // 20k keys: with the previous association-list scan this program
    // was quadratic in both backends. It pins the observable contract
    // the hashed position index must not move — insertion order after
    // interior removals, upsert keeping the first occurrence's slot,
    // string keys compared by content rather than by handle (the VM
    // interns constant-pool strings but not runtime-built ones), and
    // tuple keys.
    assert_success(
        r##"
let m: Map<int, int> = {}
for i in 0..20000
  m.insert(i, i * 2)
end
puts m.len()
puts m[19999] ?? -1
puts m.has?(20000)
puts m.remove(0) ?? -1
puts m.remove(0) ?? -1
puts m.remove(10000) ?? -1
puts m.len()

let keys = m.keys()
puts keys[0]
puts keys[9999]
puts keys[keys.len() - 1]
puts m[10001] ?? -1
puts m[9999] ?? -1
puts m.has?(10000)

let mut sum = 0
for (k, v) in m.entries()
  sum += v - k
end
puts sum

let sm = {"alpha": 1, "beta": 2}
sm.insert("al" + "pha", 3)
puts sm.len()
puts sm["alpha"] ?? 0
puts sm["al" + "pha"] ?? 0
puts sm.keys().join(",")

let tm: Map<(int, string), int> = {}
tm.insert((1, "a"), 10)
tm.insert((1, "" + "a"), 11)
puts tm.len()
puts tm[(1, "a")] ?? 0

let s = Set([1, 2, 3])
for i in 0..20000
  s.add(i)
end
puts s.len()
puts s.has?(19999)
puts s.remove(1)
puts s.remove(1)
puts s.len()
puts s.diff(Set([0])).len()
"##,
        "20000\n39998\nfalse\n0\n-1\n20000\n19998\n1\n10001\n19999\n20002\n19998\nfalse\n199980000\n2\n3\n3\nalpha,beta\n1\n11\n20000\ntrue\ntrue\nfalse\n19999\n19998\n",
    );
}

// --- reference cycles (BRS-55) ----------------------------------------

/// The prelude every cycle test builds on: `a` and `b` are two
/// separately allocated one-node self-cycles, `c`/`d` a two-node cycle.
const CYCLE_PRELUDE: &str = r##"
struct Node
  v: Vector<Node>
end

let a = Node { v: [] }
a.v.push(a)
let b = Node { v: [] }
b.v.push(b)
let c = Node { v: [] }
let d = Node { v: [] }
c.v.push(d)
d.v.push(c)
"##;

fn assert_cycle_success(body: &str, expected_stdout: &str) {
    assert_success(&format!("{CYCLE_PRELUDE}{body}"), expected_stdout);
}

/// Two equivalent cyclic structures compare EQUAL: `==` is always
/// structural (`docs/spec/03-types.md`) with no identity operator to
/// fall back on, so equality on a cyclic value is the coinductive one —
/// assume the pair equal, and report equal when nothing contradicts it.
/// Before BRS-55 every line here aborted the process with an
/// unrecoverable host stack overflow.
#[test]
fn structural_equality_on_cyclic_values_agrees() {
    assert_cycle_success(
        r##"
puts a == b
puts a == a
puts a.v == b.v
puts a == c
puts c == d
"##,
        "true\ntrue\ntrue\ntrue\ntrue\n",
    );
}

/// A cyclic value is not a Map key (`Hashable` is closed) but it is a
/// perfectly ordinary Vector element and Map value, so the containers
/// have to survive it too.
#[test]
fn cyclic_values_inside_containers_compare() {
    assert_cycle_success(
        r##"
let va = [a]
let vb = [b]
puts va == vb
let m = { "k": a }
let n = { "k": b }
puts m == n
puts Set([1, 2]).has?(2)
let dup = [a, b]
puts dup.uniq().len()
puts va.contains?(b)
"##,
        "true\ntrue\ntrue\n1\ntrue\n",
    );
}

/// Unequal cyclic structures must still terminate with `false` rather
/// than assuming their way to equality.
#[test]
fn cyclic_values_that_differ_compare_false() {
    assert_cycle_success(
        r##"
let e = Node { v: [] }
let f = Node { v: [] }
f.v.push(e)
puts e == f
puts a == e
let g = Node { v: [] }
g.v.push(a)
g.v.push(g)
puts g == a
"##,
        "false\nfalse\nfalse\n",
    );
}

/// `toString` renders the back-edge as `<cycle>` instead of recursing.
/// The marker is a path property: a value that merely appears twice as
/// a sibling still renders in full.
#[test]
fn to_string_marks_the_back_edge_of_a_cycle() {
    assert_cycle_success(
        r##"
puts a.toString()
let va = [a]
puts va.toString()
let m = { "k": c }
puts m.toString()
let shared = [1, 2]
let pair = [shared, shared]
puts pair.toString()
"##,
        "Node { v: [<cycle>] }\n[Node { v: [<cycle>] }]\n{ \"k\": Node { v: [Node { v: [<cycle>] }] } }\n[[1, 2], [1, 2]]\n",
    );
}

/// A deep but ACYCLIC structure renders in full and compares in full.
/// The old depth-100 cap failed both, and blamed a cycle that did not
/// exist while doing it.
#[test]
fn deep_acyclic_values_render_and_compare_in_full() {
    assert_cycle_success(
        r##"
def chain(n: int): Node
  let mut acc = Node { v: [] }
  for i in 0..n
    let outer = Node { v: [] }
    outer.v.push(acc)
    acc = outer
  end
  acc
end

let deep = chain(300)
puts deep.toString().len()
puts deep == chain(300)
puts deep == chain(299)
"##,
        "4214\ntrue\nfalse\n",
    );
}
