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

/// Tuples only arise from Map iteration in M1 (no tuple literal
/// exists), so tuple destructuring rides on `for` over a map.
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
