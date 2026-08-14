//! Conformance corpus: every program runs through the full frontend
//! once, then on the VM, and its outcome plus every captured stream
//! must match the expectation written beside it.
//!
//! It was a walker/VM parity suite until BRS-108. The reference
//! tree-walker was the oracle, and a disagreement was a VM bug by
//! definition; retiring it meant every expectation resting on
//! agreement had to become one written here (BRS-108 Phase 2, recorded
//! from the walker before it went). What is left is a better oracle
//! for the same regressions: a recorded output is readable in a diff
//! and costs nothing per feature, where a second implementation only
//! catches divergence between two things written from the same
//! misunderstanding, and demands the work twice.
//!
//! Each program still runs TWICE — once at the default heap budget
//! and once at a pathologically low one, with both required
//! to produce identical observable behavior. That leg outlived its
//! original purpose: it is the only automated defense the collector
//! has.
//!
//! The harness runs exactly the phases the CLI runs, in the CLI's order
//! — the error-set pass included, since it can reject a program the
//! checker accepted — and wires all three streams, so `io.eprint` and
//! `io.readLine`/`io.readAll` are as observable here as `puts`.

use brasa_runtime::{Outcome, Streams};

/// What the VM consumes, produced by the CLI's phase sequence with
/// every diagnostic asserted empty.
struct Frontend {
    module: brasa_bytecode::Module,
}

/// What one run of one backend produced.
#[derive(Debug, PartialEq, Eq)]
struct Run {
    outcome: Outcome,
    stdout: String,
    stderr: String,
}

/// Runs `source` through the whole frontend, asserting each phase is
/// clean. The order mirrors `crates/brasa/src/main.rs`: any phase the
/// CLI can reject a program on must be able to reject it here too.
fn compile_frontend(source: &str) -> Frontend {
    let mut sources = brasa_source::SourceMap::new();
    let file = sources.add_file("parity.bras", source.to_string());

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

    let inferred = brasa_errorset::infer(
        &lowered.hir,
        &lowered.roots,
        &resolved.resolutions,
        &checked.types,
    );
    assert!(
        inferred.diagnostics.is_empty(),
        "{:?}",
        inferred.diagnostics
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

    Frontend {
        module: compiled.module,
    }
}

/// Compiles `source` through the whole frontend (it must be clean) and
/// runs it with the given call-depth limit, asserting identical
/// outcome and stdout across both GC legs; returns the result.
fn assert_parity_with_depth(source: &str, max_depth: usize) -> (Outcome, String) {
    assert_parity_configured(source, max_depth, &[])
}

/// [`assert_parity_with_depth`] with explicit script arguments, served
/// by `env.args()` (BRS-32).
fn assert_parity_configured(source: &str, max_depth: usize, args: &[String]) -> (Outcome, String) {
    let run = assert_parity_io(source, max_depth, args, b"");
    assert_eq!(
        run.stderr, "",
        "a program that writes to stderr must use `assert_success_io`"
    );

    (run.outcome, run.stdout)
}

/// One run of the program, plus the hot-GC leg it must agree with:
/// outcome, stdout AND stderr, on the same `stdin`.
fn assert_parity_io(source: &str, max_depth: usize, args: &[String], stdin: &[u8]) -> Run {
    let front = compile_frontend(source);

    let run = run_vm(
        &front,
        max_depth,
        brasa_vm::DEFAULT_GC_BUDGET_BYTES,
        args,
        stdin,
    );

    // Hot-GC leg (BRS-30): the same module under a heap budget smaller
    // than a single arena cell, so collections fire at essentially
    // every allocation. GC pressure must never change observable
    // behavior.
    let hot = run_vm(&front, max_depth, 8, args, stdin);
    assert_eq!(run, hot, "hot-GC parity failed");

    run
}

fn run_vm(
    front: &Frontend,
    max_depth: usize,
    gc_budget_bytes: usize,
    args: &[String],
    stdin: &[u8],
) -> Run {
    let mut out = Vec::new();
    let mut err = Vec::new();
    let mut input = stdin;

    let (outcome, _) = brasa_vm::run_with_streams(
        &front.module,
        Streams {
            out: &mut out,
            err: &mut err,
            input: &mut input,
        },
        max_depth,
        gc_budget_bytes,
        args,
    );

    Run {
        outcome,
        stdout: decode(out, "VM stdout"),
        stderr: decode(err, "VM stderr"),
    }
}

fn decode(bytes: Vec<u8>, what: &str) -> String {
    String::from_utf8(bytes).unwrap_or_else(|_| panic!("{what} is UTF-8"))
}

fn assert_parity(source: &str) -> (Outcome, String) {
    assert_parity_with_depth(source, brasa_vm::DEFAULT_MAX_CALL_DEPTH)
}

/// A program that fails before producing any output, returning the
/// outcome so the caller can pin the message.
///
/// The failure cases used to discard the stream entirely, which was
/// safe only while a second backend was around to compare it against:
/// dropping it pinned the outcome and left "printed nothing on the way
/// down" resting on walker/VM agreement. Recorded from the walker
/// before it was retired (BRS-108).
fn assert_fails_silently(source: &str) -> Outcome {
    let (outcome, stdout) = assert_parity(source);
    assert_eq!(stdout, "", "expected no output before the failure");
    outcome
}

/// [`assert_fails_silently`] with an explicit call-depth limit.
fn assert_fails_silently_with_depth(source: &str, max_depth: usize) -> Outcome {
    let (outcome, stdout) = assert_parity_with_depth(source, max_depth);
    assert_eq!(stdout, "", "expected no output before the failure");
    outcome
}

/// Parity plus an explicit success expectation with pinned stdout, so
/// a shared walker/VM regression cannot slip through as "still equal".
fn assert_success(source: &str, expected_stdout: &str) {
    let (outcome, stdout) = assert_parity(source);
    assert_eq!(outcome, Outcome::Success);
    assert_eq!(stdout, expected_stdout);
}

/// [`assert_success`] for a program that reads stdin and/or writes
/// stderr: every stream is pinned.
fn assert_success_io(source: &str, stdin: &[u8], expected_stdout: &str, expected_stderr: &str) {
    let run = assert_parity_io(source, brasa_vm::DEFAULT_MAX_CALL_DEPTH, &[], stdin);
    assert_eq!(run.outcome, Outcome::Success);
    assert_eq!(run.stdout, expected_stdout, "stdout mismatch");
    assert_eq!(run.stderr, expected_stderr, "stderr mismatch");
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
    let outcome = assert_fails_silently("puts 9223372036854775807 + 1\n");
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
    let outcome = assert_fails_silently("let z = 0\nputs 1 / z\n");
    assert!(matches!(outcome, Outcome::Panic { message } if message.contains("division by zero")));

    let outcome = assert_fails_silently("let z = 0\nputs 1 % z\n");
    assert!(matches!(outcome, Outcome::Panic { message } if message.contains("remainder by zero")));
}

#[test]
fn negative_int_exponent_panics_match() {
    let outcome = assert_fails_silently("let e = -2\nputs 2 ** e\n");
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
    let outcome = assert_fails_silently("let m = -9223372036854775807 - 1\nlet n = -m\nputs n\n");
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

#[test]
fn remove_prefix_is_total() {
    assert_success(
        r##"
puts "src/main.bras".removePrefix("src/")
puts "src/main.bras".removePrefix("lib/")
puts "abc".removePrefix("")
puts "abc".removePrefix("abc") + "|"
puts "abc".removePrefix("abcd")
puts "aab".removePrefix("a")
puts "ñandú".removePrefix("ñ")
puts "".removePrefix("a") + "|"
"##,
        "main.bras\nsrc/main.bras\nabc\n|\nabc\nab\nandú\n|\n",
    );
}

// --- `slice`: one contract, two receivers ------------------------------

/// The two `slice` members must agree case for case, so every pair of
/// bounds is asserted as `s.chars().slice(a, b) == s.slice(a, b).chars()`
/// — the vector and the string reaching the same elements, including
/// where the bounds fall outside the receiver.
#[test]
fn vector_slice_mirrors_string_slice() {
    assert_success(
        r##"
let s = "abcde"
puts s.chars().slice(0, 2)
puts s.slice(0, 2).chars()
puts s.chars().slice(1, 3)
puts s.slice(1, 3).chars()
puts s.chars().slice(2, 2)
puts s.slice(2, 2).chars()
puts s.chars().slice(3, 1)
puts s.slice(3, 1).chars()
puts s.chars().slice(-5, 2)
puts s.slice(-5, 2).chars()
puts s.chars().slice(0, 99)
puts s.slice(0, 99).chars()
puts s.chars().slice(-9, -4)
puts s.slice(-9, -4).chars()
puts s.chars().slice(99, 100)
puts s.slice(99, 100).chars()
"##,
        "['a', 'b']\n['a', 'b']\n['b', 'c']\n['b', 'c']\n[]\n[]\n[]\n[]\n['a', 'b']\n['a', 'b']\n\
         ['a', 'b', 'c', 'd', 'e']\n['a', 'b', 'c', 'd', 'e']\n[]\n[]\n[]\n[]\n",
    );
}

#[test]
fn vector_slice_returns_a_new_vector() {
    assert_success(
        r##"
let nums = [1, 2, 3]
let part = nums.slice(1, 3)
puts part
part.push(9)
puts nums
let empty: Vector<int> = []
puts empty.slice(0, 2)
puts([["a"], ["b"]].slice(0, 1))
"##,
        "[2, 3]\n[1, 2, 3]\n[]\n[[\"a\"]]\n",
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

    let outcome = assert_fails_silently("puts \"abc\".scan(\"(\")\n");
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

    let outcome = assert_fails_silently("puts \"abc\".toInt()\n");
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

/// `join` renders every element through the derived `toString`, so a
/// string element must still come out RAW — quoting it would be the
/// regression that turns `join` into `toString` on the whole vector.
#[test]
fn join_renders_every_element_type() {
    assert_success(
        r##"
struct Point
  x: int
end

struct Tagged
  n: int

  def toString(self): string
    "<#{self.n}>"
  end
end

puts(["a", "b"].join(""))
puts(["a", "b"].join(", "))
puts([1, 2, 3].join(", "))
puts([1.5, 2.0].join("|"))
puts([true, false].join("-"))
puts(['a', 'b'].join(","))
puts([[1, 2], [3]].join(" "))
puts([Point { x: 1 }, Point { x: 2 }].join("; "))
puts([Tagged { n: 1 }, Tagged { n: 2 }].join(""))
let empty: Vector<int> = []
puts empty.join(",") + "|"
puts([7].join(","))
"##,
        "ab\na, b\n1, 2, 3\n1.5|2.0\ntrue-false\na,b\n[1, 2] [3]\n\
         Point { x: 1 }; Point { x: 2 }\n<1><2>\n|\n7\n",
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

/// A `Map` index assignment stores the element, not the `Option` the
/// same expression reads back. The double-wrap this pins against was
/// observable: writing `Some(1)` — the only form the checker used to
/// accept — made the read answer `Some(Some(1))`.
#[test]
fn map_index_assignment_stores_the_element() {
    assert_success(
        r##"
let m: Map<string, int> = {}
m["a"] = 1
m["b"] = 2
m["a"] = 3
puts(m["a"])
puts(m["missing"])
puts(m)

let nested: Map<string, Vector<int>> = {}
nested["xs"] = [1, 2]
puts(nested["xs"])
"##,
        "Some(3)\nNone\n{ \"a\": 3, \"b\": 2 }\nSome([1, 2])\n",
    );
}

/// The write rule takes the declared value type, so a `Map` whose value
/// type is ITSELF an `Option` stays coherent: the write is an
/// `Option<int>` and the read wraps it once more. That second wrap is
/// load-bearing rather than noise — it is what keeps a present key
/// holding `None` (`Some(None)`) distinguishable from an absent one
/// (`None`).
#[test]
fn map_of_options_keeps_presence_distinguishable_from_absence() {
    assert_success(
        r##"
let m: Map<string, Option<int>> = {}
m["a"] = Some(1)
m["b"] = None
puts(m["a"])
puts(m["b"])
puts(m["z"])
puts(m)
"##,
        "Some(Some(1))\nSome(None)\nNone\n{ \"a\": Some(1), \"b\": None }\n",
    );
}

/// A method generic over its own parameters runs as one uniform
/// function, like every other generic: the checker solves the call
/// site, the backends dispatch on the value. Both must agree on what
/// comes back for each instantiation, including a method generic on a
/// generic struct, where the struct's parameter and the method's are
/// solved by different owners.
#[test]
fn method_generics_run_uniformly_on_both_backends() {
    assert_success(
        r##"
struct Box
  value: int

  def wrap<T>(self, x: T): Vector<T>
    [x]
  end

  def pair<A, B>(self, a: A, b: B): Vector<string>
    ["#{a}", "#{b}"]
  end

  def largest<T: Comparable>(self, a: T, b: T): T
    if a > b then a else b end
  end
end

struct Holder<T>
  item: T

  def with<U>(self, other: U): Vector<string>
    ["#{self.item}", "#{other}"]
  end
end

let b = Box { value: 1 }
puts(b.wrap(5))
puts(b.wrap("hi"))
puts(b.pair(1, "x"))
puts(b.largest(3, 9))
puts(b.largest("a", "z"))

let h = Holder { item: 7 }
puts(h.with("a"))
"##,
        "[5]\n[\"hi\"]\n[\"1\", \"x\"]\n9\nz\n[\"7\", \"a\"]\n",
    );
}

/// A `T: Comparable` parameter compiles to the ordinary ordering ops —
/// the static type is the parameter, not the instantiation — and both
/// backends fall back to the receiver's `cmp` when the operands turn
/// out to be structs. One generic function, two instantiations, one
/// answer.
#[test]
fn comparable_structs_order_through_their_cmp() {
    assert_success(
        r##"
struct Money
  cents: int

  def cmp(self, other: Money): int
    self.cents - other.cents
  end
end

def maxOf<T: Comparable>(a: T, b: T): T
  if a > b then a else b end
end

def ordered<T: Comparable>(a: T, b: T): Vector<bool>
  [a < b, a <= b, a > b, a >= b]
end

puts(maxOf(Money { cents: 1 }, Money { cents: 2 }))
puts(maxOf(Money { cents: 5 }, Money { cents: 5 }))
puts(ordered(Money { cents: 1 }, Money { cents: 2 }))
puts(ordered(Money { cents: 2 }, Money { cents: 2 }))
puts(maxOf(3, 9))
puts(ordered(1, 2))
"##,
        "Money { cents: 2 }\n\
         Money { cents: 5 }\n\
         [true, true, false, false]\n\
         [false, true, false, true]\n\
         9\n\
         [true, true, false, false]\n",
    );
}

/// `env.exit` is how a CLI-shaped script says "failed" without saying
/// "crashed". Three properties, all of which a naive implementation
/// gets wrong:
///
/// - it is NOT catchable, so a `_` arm written for domain failures
///   cannot swallow a deliberate exit;
/// - output written before it still arrives, which is why it unwinds
///   as a signal rather than calling the host's `exit` and dropping
///   whatever is buffered;
/// - the status survives the unwind unchanged.
#[test]
fn env_exit_is_uncatchable_and_keeps_the_output_written_before_it() {
    let source = r##"
import std::env

def leave(): int
  env.exit(3)
  0
end

for i in 0..200
  puts("line #{i}")
end

let guarded = leave() catch (e)
  _ => 99
end
puts("unreachable #{guarded}")
"##;

    let (outcome, stdout) = assert_parity(source);
    assert_eq!(outcome, Outcome::Exit { code: 3 });

    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 200, "buffered output before the exit was lost");
    assert_eq!(lines[0], "line 0");
    assert_eq!(lines[199], "line 199");
    assert!(
        !stdout.contains("unreachable"),
        "a `_` arm intercepted a deliberate exit"
    );
}

/// A status outside the range a process can carry is a programmer
/// error, so it panics rather than being silently truncated: `exit(256)`
/// quietly becoming `0` is the accident this member exists to remove.
#[test]
fn env_exit_rejects_a_status_outside_the_process_range() {
    let outcome = assert_fails_silently("import std::env\nenv.exit(300)\n");
    let Outcome::Panic { message } = outcome else {
        panic!("expected a panic, got {outcome:?}");
    };
    assert_eq!(
        message,
        "panic: panics.AssertionFailed: `env.exit` takes a status of 0 to 255, got 300"
    );
}

/// A lambda parameter can destructure, which is what makes a vector of
/// pairs usable without unpacking it by hand first. The shape the
/// defect was filed for — rank a counter map — is the first line.
///
/// It desugars to a `match` over a synthetic parameter, so the runtime
/// path is the one `match` already had, and it must hold for every
/// arity and for nesting.
#[test]
fn lambda_parameters_destructure_at_every_arity() {
    assert_success(
        r##"
let counts: Map<string, int> = { "a": 3, "b": 1, "c": 7 }
puts(counts.entries().sortBy(|(key, hits)| -hits))

let pairs = [(1, "a"), (2, "b")]
puts(pairs.map(|(n, s)| "#{n}#{s}"))
puts(pairs.map(|(n, _)| n))

let triples = [(1, 2, 3)]
puts(triples.map(|(a, b, c)| a + b + c))

let nested = [((1, 2), "x")]
puts(nested.map(|((a, b), s)| "#{a + b}#{s}"))

puts(pairs.reduce(0, |acc, (n, _)| acc + n))

let outer = 10
puts(pairs.map(|(n, _)| n + outer))

pairs.each do |(n, s)|
  puts("#{n}=#{s}")
end

let lefts = [(1, 2)]
let rights = [(3, 4)]
puts(lefts.zip(rights).map(|((a, b), (c, d))| a + b + c + d))
"##,
        "[(\"c\", 7), (\"a\", 3), (\"b\", 1)]\n\
         [\"1a\", \"2b\"]\n\
         [1, 2]\n\
         [6]\n\
         [\"3x\"]\n\
         3\n\
         [11, 12]\n\
         1=a\n\
         2=b\n\
         [10]\n",
    );
}

/// A `let` binds through the same patterns `match`, `for`, and lambda
/// parameters already use (BRS-128): the last binding position that
/// still demanded a name. It desugars to single-arm `match`es over a
/// hidden temp, so the runtime path is the one `match` already had.
///
/// The cases pin the scope decisions: nesting, `_` components, a type
/// annotation, statement and top-level position, a single-variant
/// constructor (irrefutable, so it is accepted), and evaluation of the
/// right side exactly once and before any name binds — `let (a, b) =
/// (b, a)` reads the outer names.
#[test]
fn let_destructures_through_irrefutable_patterns() {
    assert_success(
        r##"
enum Boxed
  Wrap(value: int)
end

let pair = (1, "a")
let (n, s) = pair
puts("#{n}#{s}")

let ((a, b), c) = ((1, 2), 3)
puts(a + b + c)

let (_, second) = (10, 20)
puts(second)

let (x, y): (int, string) = (7, "z")
puts("#{x}#{y}")

let Wrap(inner) = Wrap(9)
puts(inner)

def swap(p: (int, int)): (int, int)
  let (lo, hi) = p
  (hi, lo)
end
puts(swap((1, 2)))

def once(): (int, int)
  puts("evaluated")
  (1, 2)
end

def shadow(): int
  let a = 100
  let b = 200
  if true
    let (a, b) = (b, a)
    puts(a - b)
  end
  a - b
end
let (l, r) = once()
puts(l + r)
puts(shadow())
"##,
        "1a\n6\n20\n7z\n9\n(2, 1)\nevaluated\n3\n100\n-100\n",
    );
}

/// `toFixed` exists so a report column can promise its own shape. The
/// property under test is that the decimal count comes from the CALL
/// and not from the value: every row below is the same width, which the
/// shortest-round-trip printer cannot deliver on its own.
///
/// The tie row is the reason this does not defer to Rust's `{:.N}`
/// formatting, which rounds ties to even: `2.5` must render `3` here,
/// because `math.round(2.5)` is `3.0` and one stdlib does not get two
/// rounding rules.
#[test]
fn to_fixed_pins_the_decimal_count_and_the_tie_rule() {
    assert_success(
        r##"
let costs: Vector<float> = [1000.0, 333.335, 0.5, 12.1, 0.000001]
for c in costs
  puts("|#{c.toFixed(2).padStart(10, " ")}|")
end

puts((2.5).toFixed(0))
puts((3.5).toFixed(0))
puts((-2.5).toFixed(0))
puts((0.125).toFixed(2))

puts((0.1 + 0.2).toFixed(2))
puts((-0.006).toFixed(2))
puts((1.0 / 3.0).toFixed(4))

puts((5).toFixed(2))
puts((5).toFixed(0))
puts((-5).toFixed(1))
"##,
        "|   1000.00|\n\
         |    333.33|\n\
         |      0.50|\n\
         |     12.10|\n\
         |      0.00|\n\
         3\n4\n-3\n0.13\n\
         0.30\n-0.01\n0.3333\n\
         5.00\n5\n-5.0\n",
    );
}

/// A decimal count a float cannot back is a programmer error, so it
/// panics rather than throwing — the rule `time.sleep` and `rand.int`
/// already follow.
#[test]
fn to_fixed_rejects_a_digit_count_out_of_range() {
    let outcome = assert_fails_silently("puts((1.5).toFixed(-1))\n");
    let Outcome::Panic { message } = outcome else {
        panic!("expected a panic, got {outcome:?}");
    };
    assert_eq!(
        message,
        "panic: panics.AssertionFailed: `toFixed` takes 0 to 17 digits, got -1"
    );
}

/// `Comparable` reached transitively: a parameter constrained by a USER
/// interface that declares `cmp` satisfies `Comparable` too, and this is
/// the one shape other than a struct that can reach the ordering
/// fallback. It is sound by construction — whatever instantiates `U`
/// must itself satisfy `Ord`, so the backends still see a struct — and
/// both must agree on the answer.
#[test]
fn comparable_is_satisfied_transitively_through_a_user_constraint() {
    assert_success(
        r##"
interface Ord
  def cmp(self, other: Self): int
end

struct Money
  cents: int

  def cmp(self, other: Money): int
    self.cents - other.cents
  end
end

def maxOf<T: Comparable>(a: T, b: T): T
  if a > b then a else b end
end

def viaOrd<U: Ord>(a: U, b: U): U
  maxOf(a, b)
end

puts(viaOrd(Money { cents: 1 }, Money { cents: 2 }))
puts(viaOrd(Money { cents: 7 }, Money { cents: 3 }))
"##,
        "Money { cents: 2 }\nMoney { cents: 7 }\n",
    );
}

/// Every module the resolver accepts must have a runtime behind it.
///
/// `std::re` was in the accept list with no implementation anywhere:
/// `import std::re` type-checked clean and then died at run time. A set
/// comparison against the backends' dispatch arms would not have caught
/// the reverse mistake either — a name present on both sides with a
/// broken body — so every module is exercised by an actual call whose
/// output is pinned, and the covered set is compared against
/// `STD_MODULES` so a new module cannot be accepted without landing
/// here first.
#[test]
fn every_std_module_has_a_runtime_behind_it() {
    // Deliberately deterministic calls: nothing that reads the clock,
    // the environment, or the filesystem in a way a machine could
    // disagree about.
    const PROBES: &[(&str, &str, &str)] = &[
        (
            "env",
            r#"puts(env.get("BRASA_PARITY_PROBE_UNSET") ?? "unset")"#,
            "unset\n",
        ),
        (
            "fs",
            r#"puts(fs.exists?("/nonexistent-brasa-parity-probe"))"#,
            "false\n",
        ),
        ("io", r#"puts(io.readLine() ?? "none")"#, "none\n"),
        (
            "json",
            r#"puts(json.stringify(json.parse("[1,2]")))"#,
            "[1,2]\n",
        ),
        ("math", "puts(math.abs(-2))", "2\n"),
        // Probing the spawn-failure path rather than running a real
        // program: it reaches the same module dispatch and its error
        // namespace, without depending on any binary existing on the
        // machine that runs the suite.
        (
            "proc",
            "puts(proc.run([\"brasa-parity-no-such-binary\"]).code catch (e)\n\
             \x20 proc.SpawnError => -1\n\
             end)",
            "-1\n",
        ),
        (
            "cli",
            "puts(cli.parse([\"-v\"], [[\"flag\", \"verbose\", \"v\", \"loud\"]]).flag(\"verbose\"))",
            "true\n",
        ),
        // Port 1 is refused everywhere, so this reaches the module
        // dispatch and its error namespace without needing a network
        // or a server.
        (
            "http",
            "puts(http.get(\"http://127.0.0.1:1/\").status catch (e)\n\
             \x20 http.RequestError => -1\n\
             end)",
            "-1\n",
        ),
        // A single-value range: random, but only one answer.
        ("rand", "puts(rand.int(5..6))", "5\n"),
        ("time", "puts(time.nowMillis() > 0)", "true\n"),
    ];

    let mut covered: Vec<&str> = PROBES.iter().map(|(module, _, _)| *module).collect();
    covered.sort_unstable();
    let mut declared = brasa_resolver::STD_MODULES.to_vec();
    declared.sort_unstable();
    assert_eq!(
        covered, declared,
        "every std module needs a probe here: the resolver accepting a name \
         is a promise that it can be run"
    );

    for (module, call, expected) in PROBES {
        let source = format!("import std::{module}\n{call}\n");
        assert_success(&source, expected);
    }
}

/// A method may reuse the struct's parameter name without capturing it.
/// The receiver holds an `int` while the method instantiates to
/// `string`, which is only observable at runtime if the two parameters
/// really are separate.
#[test]
fn a_method_generic_shadowing_the_struct_generic_stays_independent() {
    assert_success(
        r##"
struct Holder<T>
  item: T

  def echo<T>(self, other: T): T
    other
  end

  def both<T>(self, other: T): Vector<string>
    ["#{self.item}", "#{other}"]
  end
end

let h = Holder { item: 7 }
puts(h.echo("a"))
puts(h.echo(1))
puts(h.both("a"))

let s = Holder { item: "x" }
puts(s.echo(9))
puts(s.both(9))
"##,
        "a\n1\n[\"7\", \"a\"]\n9\n[\"x\", \"9\"]\n",
    );
}

/// `??` yields the carried type, so an empty literal on the fallback
/// side takes its type from the `Option` itself, with no annotation
/// anywhere. Both backends must produce the empty container, not a
/// differently-shaped one.
#[test]
fn coalesce_fallback_infers_from_the_option() {
    assert_success(
        r##"
let ints: Option<Vector<int>> = None
let v = ints ?? []
puts(v)
puts(v.len())

let pairs: Option<Map<string, int>> = None
puts(pairs ?? {})

let present: Option<Vector<int>> = Some([1, 2])
puts(present ?? [])
"##,
        "[]\n0\n{}\n[1, 2]\n",
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

/// A closure captures the lexical BINDING, not a snapshot of its value
/// (BRS-106, spec: 01 — Sintaxis): rebinding the name after the
/// closure was created is visible through the closure.
#[test]
fn rebinding_a_captured_local_is_visible_through_the_closure() {
    assert_success(
        r##"
def demo(): int
  let mut count = 0
  let read = || count
  puts read()
  count = 10
  read()
end

puts demo()
"##,
        "0\n10\n",
    );
}

/// The same program, one scope out. A top-level `let` is a global
/// rather than a frame slot, so it takes a different path through code
/// generation — and has to mean exactly the same thing.
#[test]
fn rebinding_a_captured_top_level_binding_is_visible_through_the_closure() {
    assert_success(
        r##"
let mut count = 0
let read = || count
puts read()
count = 10
puts read()
"##,
        "0\n10\n",
    );
}

/// Sharing the binding is not sharing a value: mutation THROUGH a
/// captured object was always visible and stays visible, because the
/// object is a reference either way (spec: 03 — Sistema de tipos).
#[test]
fn mutation_through_a_captured_container_stays_visible() {
    assert_success(
        r##"
let items = [1]
let observe = || items.len()
items.push(2)
puts observe()

def local(): int
  let inner = [1]
  let count = || inner.len()
  inner.push(2)
  inner.push(3)
  count()
end

puts local()
"##,
        "2\n3\n",
    );
}

/// The binding is shared in both directions: a rebinding INSIDE the
/// lambda outlives the invocation that made it, and the binding scope
/// reads it back.
#[test]
fn a_closure_rebinding_its_capture_is_visible_to_the_binding_scope() {
    assert_success(
        r##"
def total(): int
  let mut sum = 0
  [1, 2, 3].each do |n|
    sum = sum + n
  end
  sum
end

puts total()
"##,
        "6\n",
    );
}

/// A closure that outlives the frame that bound its captures keeps
/// reading the binding, so the last rebinding before the frame returned
/// is what it sees.
#[test]
fn a_returned_closure_reads_the_last_binding_of_its_dead_frame() {
    assert_success(
        r##"
def makeReader(): () -> int
  let mut n = 1
  let read = || n
  n = 99
  read
end

let reader = makeReader()
puts reader()
puts reader()
"##,
        "99\n99\n",
    );
}

/// Each execution of a binding site establishes a NEW binding, so a
/// closure made in one loop iteration does not share with the next
/// one's — sharing is per binding, not per slot.
#[test]
fn each_iteration_binds_a_binding_of_its_own() {
    assert_success(
        r##"
def perIteration(): int
  let mut fns: Vector<() -> int> = []
  for i in 0..3
    let mut seen = i
    let read = || seen
    seen = seen + 10
    fns.push(read)
  end

  let mut total = 0
  fns.each do |f|
    total = total + f()
  end
  total
end

puts perIteration()
"##,
        "33\n",
    );
}

/// The other half of the same rule (spec: 01 — Sintaxis): a name
/// declared OUTSIDE the loop is declared once, so every iteration's
/// closure captures the one binding and all of them read the last value
/// written to it.
///
/// Pinned separately from `each_iteration_binds_a_binding_of_its_own`
/// because the two look like a contradiction until you notice that what
/// differs is where the declaration sits, not how capture works. A
/// change to loop scoping that made this one agree with its sibling
/// would be a silent semantic regression.
#[test]
fn a_binding_declared_outside_the_loop_is_shared_by_every_iteration() {
    assert_success(
        r##"
def sharedAcrossIterations(): int
  let mut fns: Vector<() -> int> = []
  let mut last = 0

  for i in 0..3
    last = i
    fns.push(|| last)
  end

  let mut total = 0
  fns.each do |f|
    total = total + f()
  end
  total
end

puts sharedAcrossIterations()
"##,
        "6\n",
    );
}

/// A shared binding can CLOSE a reference cycle, which is why its cell
/// is an arena kind and not an `Rc` (`brasa_vm::heap` module docs): the
/// cell holds the vector, the vector holds the closure, and the closure
/// captured the cell.
///
/// The hot-GC leg is what makes this a collector test — a cell the
/// tracer does not walk is swept while the vector inside it is still
/// live, and the two legs stop agreeing.
#[test]
fn a_binding_cell_on_a_reference_cycle_survives_collection() {
    assert_success(
        r##"
def cyclic(): int
  let mut fns: Vector<() -> int> = []
  let count = || fns.len()
  fns.push(count)
  fns = [count, count]

  for i in 0..50
    let garbage = [i, i]
  end

  count()
end

puts cyclic()
puts "done"
"##,
        "2\ndone\n",
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
/// named arm, so the checker accepts it with no `_`.
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
    let outcome = assert_fails_silently(
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

/// The caught arm is reachable (the callee's error-set holds both
/// types) but the raised signal is the OTHER one, so it escapes the
/// handler. Every arm must stay in the inferred error-set: an arm that
/// cannot fire is an error-set diagnostic, not a runtime scenario.
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

def boom(which: int): int
  if which == 0
    throw AError { code: 1 }
  end
  throw BError { code: 7 }
end

let v = boom(1) catch (e)
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

def inner(which: int): int
  if which == 0
    throw InnerError { code: 1 }
  end
  throw OuterError { code: 9 }
end

def middle(which: int): int
  inner(which) catch (e)
    InnerError => 1
  end
end

let v = middle(1) catch (e)
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
    let outcome = assert_fails_silently(
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

/// The repair `E007` points at: a `toString` whose work can fail keeps
/// the `catch` inside itself and renders a fallback, so its inferred
/// error-set is empty and every rendering path stays infallible. The
/// throwing branch has to reach the fallback through `puts`,
/// interpolation and a container alike.
#[test]
fn a_to_string_that_catches_internally_renders_its_fallback() {
    assert_success(
        r##"
struct Boom
end

def widthOf(n: int): int
  if n < 0
    throw Boom {}
  end
  n
end

struct Cell
  n: int

  def toString(self): string
    "w#{widthOf(self.n)}" catch (e)
      Boom => "w?"
    end
  end
end

puts Cell { n: 3 }
puts "#{Cell { n: -1 }}"
puts([Cell { n: 3 }, Cell { n: -1 }])
"##,
        "w3\nw?\n[w3, w?]\n",
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
// (spec: 03 — Sistema de tipos, "Generics: execution model"), so the
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
/// the builtin table.
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

/// A struct field holding a callable satisfies a constraint method
/// structurally: both the call and the bare member read reach it, and
/// on a generic receiver only through the dynamic lookup.
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

    let outcome = assert_fails_silently_with_depth(source, 64);
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
    let front = compile_frontend(source);
    let mut err = Vec::new();
    let mut input: &[u8] = b"";

    brasa_vm::run_with_streams(
        &front.module,
        Streams {
            out,
            err: &mut err,
            input: &mut input,
        },
        brasa_vm::DEFAULT_MAX_CALL_DEPTH,
        brasa_vm::DEFAULT_GC_BUDGET_BYTES,
        &[],
    )
    .0
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

/// `print` writes exactly its argument in the bare command form: no
/// separator between two of them, no trailing newline. Ported from the
/// walker's own suite (BRS-108). The parenthesized form is covered
/// separately, by the `print` entry in the builtin table — do not read
/// this test as covering both.
#[test]
fn print_in_the_command_form_writes_without_a_separator() {
    assert_success("puts \"hello\"\nprint 1\nprint 2\n", "hello\n12");
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

    // The harness runs the program twice, once per GC leg; `write`
    // truncates, so the script is naturally re-runnable.
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

/// The defect this closes: a containment check written on `fs.abs` is
/// wrong, because `abs` is lexical and never touches an inode. A path
/// under the root that is a symlink out of it passes the check and then
/// reads a file outside — verified before the fix, on the fixture built
/// here.
///
/// `resolve` follows the link, so the same check answers correctly, and
/// `isSymlink?` is the one predicate that must NOT follow: it answers
/// about the path, not about its target.
#[test]
#[cfg(unix)]
fn fs_resolve_and_is_symlink_make_a_containment_check_possible() {
    use std::os::unix::fs::symlink;

    let tmp = fs_temp_dir("resolve");
    std::fs::create_dir_all(tmp.join("root/sub")).expect("fixture dirs");
    std::fs::create_dir_all(tmp.join("outside")).expect("fixture dirs");
    std::fs::write(tmp.join("outside/secret.txt"), "s").expect("fixture written");
    symlink("../../outside/secret.txt", tmp.join("root/sub/leak")).expect("link created");
    symlink("/definitely/not/here", tmp.join("root/dangling")).expect("link created");

    let t = tmp.display();
    assert_success(
        &format!(
            r##"
import std::fs

def contained?(root: string, candidate: string): bool
  let r = fs.resolve(root)
  let c = fs.resolve(candidate)
  c == r || c.startsWith?(r + "/")
end

let root = "{t}/root"

puts(contained?(root, "{t}/root/sub"))
puts(contained?(root, root))
puts(contained?(root, "{t}/root/sub/leak"))

puts(fs.isSymlink?("{t}/root/sub/leak"))
puts(fs.isSymlink?("{t}/root/sub"))
puts(fs.isSymlink?("{t}/root/dangling"))
puts(fs.isSymlink?("{t}/root/nothing-here"))

puts(fs.exists?("{t}/root/dangling"))

puts(fs.resolve("{t}/root/dangling") catch (e)
  fs.NotFound => "dangling has no real path"
  fs.Denied => "denied"
  fs.IoError => "io"
end)
"##
        ),
        "true\ntrue\nfalse\n\
         true\nfalse\ntrue\nfalse\n\
         false\n\
         dangling has no real path\n",
    );

    let _ = std::fs::remove_dir_all(&tmp);
}

/// `walk` on a real tree walks the object store unless it can skip a
/// subtree, which is why both dogfooding scripts hand-rolled their own
/// descent. Pruning is by entry NAME — the vocabulary `ls` already
/// speaks — and the three rules that make it predictable are pinned
/// here: an empty list is the one-argument form, a pruned name that is
/// a FILE is still returned because pruning is about subtrees, and the
/// root is never pruned by its own base name.
#[test]
fn fs_walk_prunes_subtrees_by_directory_name() {
    let tmp = fs_temp_dir("walkprune");
    std::fs::create_dir_all(tmp.join(".git/objects/ab")).expect("fixture dirs");
    std::fs::create_dir_all(tmp.join("node_modules/left-pad")).expect("fixture dirs");
    std::fs::create_dir_all(tmp.join("src")).expect("fixture dirs");
    std::fs::write(tmp.join(".git/config"), "x").expect("fixture written");
    std::fs::write(tmp.join(".git/objects/ab/deadbeef"), "y").expect("fixture written");
    std::fs::write(tmp.join("node_modules/left-pad/index.js"), "z").expect("fixture written");
    std::fs::write(tmp.join("src/main.bras"), "s").expect("fixture written");
    std::fs::write(tmp.join("README.md"), "r").expect("fixture written");

    let t = tmp.display();
    assert_success(
        &format!(
            r##"
import std::fs

let root = "{t}"

let kept = fs.walk(root, [".git", "node_modules"])
puts(kept.len())
for p in kept
  puts(fs.base(p))
end

puts(fs.walk(root, []) == fs.walk(root))
puts(fs.walk(root, ["README.md"]).len())
puts(fs.walk(root, [fs.base(root)]).len())
"##
        ),
        "2\nREADME.md\nmain.bras\ntrue\n5\n5\n",
    );

    let _ = std::fs::remove_dir_all(&tmp);
}

/// A symlink loop has no real path either, and the OS says so with a
/// kind that is neither not-found nor denied.
#[test]
#[cfg(unix)]
fn fs_resolve_reports_a_symlink_loop_as_an_io_error() {
    use std::os::unix::fs::symlink;

    let tmp = fs_temp_dir("resolveloop");
    std::fs::create_dir_all(&tmp).expect("fixture dir");
    symlink("b", tmp.join("a")).expect("link created");
    symlink("a", tmp.join("b")).expect("link created");

    let t = tmp.display();
    assert_success(
        &format!(
            r##"
import std::fs

puts(fs.resolve("{t}/a") catch (e)
  fs.NotFound => "not found"
  fs.Denied => "denied"
  fs.IoError => "a loop has no end"
end)
"##
        ),
        "a loop has no end\n",
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

/// The defect BRS-66 records: one directory the process cannot read
/// threw away everything already collected from the rest of the tree.
/// `walk` still does that, on purpose — a short list presented as a
/// complete one is how a backup script loses files quietly. `tryWalk`
/// is the way to ask for best effort, and it reports what it skipped
/// rather than swallowing it.
#[test]
#[cfg(unix)]
fn try_walk_reports_what_it_could_not_read_and_walk_still_refuses() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::PermissionsExt;

    /// Restores the fixture's mode on the way out, including out of a
    /// panicking assertion: a mode-000 directory left behind makes the
    /// cleanup below fail for a reason that has nothing to do with the
    /// defect under test.
    struct Unlock(std::path::PathBuf);

    impl Drop for Unlock {
        fn drop(&mut self) {
            let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o700));
        }
    }

    let tmp = fs_temp_dir("trywalk");
    // Two unreadable directories, created in reverse, so `unreadable`
    // pins its own bytewise order the way the eight files pin `paths`.
    for dir in ["open", "secret-b", "secret-a", "empty"] {
        std::fs::create_dir(tmp.join(dir)).expect("fixture dir created");
    }
    // Created in reverse so the sorted answer is not the order the
    // directory hands back: without the sort this is one of 8! orders,
    // and the assertion below is the only thing that pins it.
    for name in ["h", "g", "f", "e", "d", "c", "b", "a"] {
        std::fs::write(tmp.join(format!("open/{name}.txt")), name).expect("fixture written");
    }
    // `open.txt` against the `open/` directory: bytewise puts the file
    // first ('.' is 0x2E, '/' is 0x2F), component order puts it last.
    // The pair is what separates the two, and `walk` is bytewise.
    std::fs::write(tmp.join("open.txt"), "o").expect("fixture written");

    // And a pair that separates bytewise from sorting the RENDERED
    // names: every byte that is not valid UTF-8 renders as the same
    // replacement character, so these two compare equal on their first
    // character and then invert — rendered order puts the `a` first,
    // byte order puts the 0x80 first. Both render distinctly, so the
    // assertion below can tell which one happened.
    // Probed rather than expected: APFS and HFS+ reject a filename
    // that is not valid UTF-8, so on macOS these cannot be created at
    // all. Failing there would take the ordering, prune and aliasing
    // assertions down with them for a reason that has nothing to do
    // with the traversal.
    let raw_paths = [b"\x80b.txt".as_slice(), b"\xFFa.txt".as_slice()]
        .map(|name| tmp.join("open").join(OsStr::from_bytes(name)));
    let written = raw_paths
        .each_ref()
        .map(|path| std::fs::write(path, "u").is_ok());
    let raw_names = written.iter().all(|written| *written);

    if !raw_names {
        // Both or neither: one name landing and the other not would
        // leave a file the traversal reports and every expectation
        // below, keyed off this flag, omits — a correct traversal
        // failing as an ordering regression.
        for path in &raw_paths {
            let _ = std::fs::remove_file(path);
        }

        eprintln!(
            "skipping the byte-ordering assertion: this filesystem rejects names that \
             are not valid UTF-8, so bytewise and rendered order cannot be told apart"
        );
    }
    for dir in ["secret-b", "secret-a"] {
        std::fs::write(tmp.join(dir).join("hidden.txt"), "h").expect("fixture written");
    }

    let _unlock = ["secret-b", "secret-a"].map(|dir| {
        std::fs::set_permissions(tmp.join(dir), std::fs::Permissions::from_mode(0o000))
            .expect("permissions set");

        Unlock(tmp.join(dir))
    });

    // Root ignores mode bits; probe rather than assert a Denied that
    // cannot happen here.
    let denied = matches!(
        std::fs::read_dir(tmp.join("secret-a")),
        Err(err) if err.kind() == std::io::ErrorKind::PermissionDenied
    );

    let raw_tail = if raw_names {
        ",\u{fffd}b.txt,\u{fffd}a.txt"
    } else {
        ""
    };

    if denied {
        let t = tmp.display();

        assert_success(
            &format!(
                r##"
import std::fs
let r = fs.tryWalk("{t}")
puts(r.paths.map(|p| fs.base(p)).join(","))
puts(r.unreadable.map(|p| fs.base(p)).join(","))
"##
            ),
            &format!(
                "open.txt,a.txt,b.txt,c.txt,d.txt,e.txt,f.txt,g.txt,h.txt{raw_tail}\nsecret-a,secret-b\n"
            ),
        );

        // The strict form is unchanged, and the error names the
        // directory it could not read.
        assert_success(
            &format!(
                r##"
import std::fs
let outcome = fs.walk("{t}") catch (e)
  fs.Denied => []
end
puts outcome.len()
"##
            ),
            "0
",
        );

        // An unreadable ROOT is not tolerated either — the case the
        // single read of the root bears on.
        assert_success(
            &format!(
                r##"
import std::fs

def reach(path: string): string
  "reached #{{fs.tryWalk(path).paths.len()}}"
end

puts(reach("{t}/secret-a") catch (e)
  fs.Denied => "threw"
end)
"##
            ),
            "threw
",
        );
    } else {
        // Worth being plain about what a root run does NOT verify: the
        // tolerance itself. Everything below this branch — ordering,
        // the prune form, the shared field — still runs, but a
        // regression that stopped recording `unreadable`, or that made
        // `tryWalk` abort like `walk`, would satisfy every remaining
        // assertion. Mode bits are the only portable way to make a
        // directory unreadable, and root ignores them.
        eprintln!(
            "skipping the tryWalk tolerance assertions: this user bypasses mode 000, \
             so an unreadable directory cannot be built"
        );
    }

    let t = tmp.display();

    // The byte order of every member that promises one, over the one
    // pair that can tell it from ordering the rendered names: both
    // invalid bytes render as the same replacement character, so
    // rendered order inverts them. `open` is readable, so this is
    // outside the `denied` branch — nothing here needs an unreadable
    // directory, and under a root runner it is the only place the
    // ordering is pinned at all. `glob` is pinned alongside them to
    // record that it never sees such a name: the crate behind it
    // matches on `str`, so its bytewise promise is vacuous rather
    // than kept. Gated on the names existing, because without them
    // nothing here can tell the two orders apart.
    if raw_names {
        assert_success(
            &format!(
                r##"
import std::fs
puts(fs.walk("{t}/open").map(|p| fs.base(p)).join(","))
puts(fs.tryWalk("{t}/open").paths.map(|p| fs.base(p)).join(","))
puts(fs.ls("{t}/open").join(","))
puts(fs.glob("{t}/open/*").map(|p| fs.base(p)).join(","))
"##
            ),
            "a.txt,b.txt,c.txt,d.txt,e.txt,f.txt,g.txt,h.txt,\u{fffd}b.txt,\u{fffd}a.txt
a.txt,b.txt,c.txt,d.txt,e.txt,f.txt,g.txt,h.txt,\u{fffd}b.txt,\u{fffd}a.txt
a.txt,b.txt,c.txt,d.txt,e.txt,f.txt,g.txt,h.txt,\u{fffd}b.txt,\u{fffd}a.txt
a.txt,b.txt,c.txt,d.txt,e.txt,f.txt,g.txt,h.txt
",
        );
    }

    drop(_unlock);
    let _ = std::fs::remove_dir_all(&tmp);
}

/// The half of the `tryWalk` surface that needs no unix: byte ordering
/// against the `open.txt` / `open/` pair, the two-argument prune form,
/// the field that hands back the record's own vector, and the render.
/// Kept out of the unix test because none of it needs a mode-000
/// directory or a filename holding invalid UTF-8, and a regression in
/// any of the four would otherwise pass everywhere else.
#[test]
fn walk_and_try_walk_agree_on_order_prune_and_fields() {
    let tmp = fs_temp_dir("trywalkorder");
    for dir in ["open", "secret-a", "secret-b", "empty"] {
        std::fs::create_dir_all(tmp.join(dir)).expect("fixture dir created");
    }

    // Created in reverse so the sorted answer is not the order the
    // directory hands back.
    for name in ["h", "g", "f", "e", "d", "c", "b", "a"] {
        std::fs::write(tmp.join(format!("open/{name}.txt")), name).expect("fixture written");
    }

    // The pair that separates bytewise order from `PathBuf`'s
    // component order: `.` is 0x2E and `/` is 0x2F, so bytewise puts
    // the file first and component order puts the directory first.
    std::fs::write(tmp.join("open.txt"), "o").expect("fixture written");

    for dir in ["secret-a", "secret-b"] {
        std::fs::write(tmp.join(dir).join("hidden.txt"), "h").expect("fixture written");
    }

    let t = tmp.display();
    assert_success(
        &format!(
            r##"
import std::fs
let pruned = ["secret-a", "secret-b"]
puts(fs.walk("{t}", pruned).map(|p| fs.base(p)).join(","))
puts(fs.tryWalk("{t}", pruned).paths.map(|p| fs.base(p)).join(","))
puts fs.tryWalk("{t}", pruned).unreadable.len()

# Reading a field hands back the record's own vector, as the spec
# says: a `Vector` is a shared reference, so this is what pushing
# into a vector you hold means. Pinned because the two engines
# reach the field by different routes and could drift.
let r = fs.tryWalk("{t}", pruned)
let held = r.paths
held.push("extra")
puts r.paths.len()

# `Walk` promises the two fields and the universal `toString`, and
# the second is reached by a different dispatch arm than the render
# `puts` uses. An empty directory is the one root whose rendering
# does not carry the temp path.
puts fs.tryWalk("{t}/empty").toString()
"##
        ),
        "open.txt,a.txt,b.txt,c.txt,d.txt,e.txt,f.txt,g.txt,h.txt
open.txt,a.txt,b.txt,c.txt,d.txt,e.txt,f.txt,g.txt,h.txt
0
10
Walk { paths: [], unreadable: [] }
",
    );

    let _ = std::fs::remove_dir_all(&tmp);
}

/// `tryWalk` tolerates everything BELOW the root and nothing about the
/// root itself: a root that is not there is the caller asking for the
/// wrong thing, not a subtree it could not reach.
///
/// The call is wrapped in a function returning a string because `Walk`
/// is not constructible, so a `catch` arm cannot produce one — the same
/// constraint `Output` carries.
#[test]
fn try_walk_still_throws_for_the_root() {
    let tmp = fs_temp_dir("trywalk-root");
    let t = tmp.display();

    assert_success(
        &format!(
            r##"
import std::fs

def reach(path: string): string
  "reached #{{fs.tryWalk(path).paths.len()}}"
end

puts(reach("{t}/does-not-exist") catch (e)
  fs.NotFound => e.startsWith?("cannot tryWalk ").toString()
end)
"##
        ),
        // Only the member naming is pinned, not the OS message around
        // it: telling the two members apart is the point of having
        // both, and the rest of the text is the platform's.
        "true\n",
    );

    let _ = std::fs::remove_dir_all(&tmp);
}

// --- std::io streams (BRS-34) ------------------------------------------
//
// `io.eprint` and the stdin readers are wired to the run's streams, so
// both are compared here on every GC leg exactly like stdout. The CLI-level suite (`crates/brasa/tests/io_cli.rs`)
// keeps pinning the same behavior through the real process streams.

#[test]
fn io_printers_split_across_stdout_and_stderr() {
    assert_success_io(
        r##"
import std::io
io.puts("to stdout")
io.print("no newline")
io.eprint("to stderr")
io.eprint(42)
puts "prelude"
"##,
        b"",
        "to stdout\nno newlineprelude\n",
        "to stderr42",
    );
}

#[test]
fn read_line_strips_newlines_and_reports_end_of_input() {
    assert_success_io(
        r##"
import std::io
puts io.readLine() ?? "<eof>"
puts io.readLine() ?? "<eof>"
puts io.readLine() ?? "<eof>"
puts io.readLine() ?? "<eof>"
"##,
        b"alpha\r\nbeta\nlast without newline",
        "alpha\nbeta\nlast without newline\n<eof>\n",
        "",
    );
}

#[test]
fn read_all_takes_the_rest_of_the_input_verbatim() {
    assert_success_io(
        r##"
import std::io
let first = io.readLine() ?? "<eof>"
puts "first: #{first}"
puts "rest: #{io.readAll()}"
"##,
        b"one\ntwo\nthree\n",
        "first: one\nrest: two\nthree\n\n",
        "",
    );
}

#[test]
fn empty_input_reads_cleanly_on_both_readers() {
    assert_success_io(
        r##"
import std::io
puts io.readLine() ?? "<eof>"
puts "all: #{io.readAll()}"
"##,
        b"",
        "<eof>\nall: \n",
        "",
    );
}

/// Invalid UTF-8 decodes lossily rather than failing the run: a Unix
/// filter must never die on a stray byte (spec: 05 — Stdlib de scripting).
#[test]
fn invalid_utf8_input_decodes_lossily() {
    assert_success_io(
        r##"
import std::io
puts io.readLine() ?? "<eof>"
puts io.readAll()
"##,
        b"ok \xff\xfe end\nrest \xc3\x28\n",
        "ok \u{fffd}\u{fffd} end\nrest \u{fffd}(\n\n",
        "",
    );
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
    let outcome = assert_fails_silently("let v = [1.0, 0.0 / 0.0]\nv.sort()\n");
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
    let outcome = assert_fails_silently("import std::time\ntime.sleep(-1)\n");
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
    // 42 (`brasa_runtime::rand_glue`); a change here means the PRNG or
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
    let outcome = assert_fails_silently("import std::rand\nrand.int(5..5)\n");
    let Outcome::Panic { message } = outcome else {
        panic!("expected a panic, got {outcome:?}");
    };
    assert!(
        message.contains("cannot pick from an empty range"),
        "unexpected message: {message}"
    );

    let outcome =
        assert_fails_silently("import std::rand\nlet v: Vector<int> = []\nrand.choice(v)\n");
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
fn large_hashed_map_and_set_keep_their_order() {
    // 20k keys: with the previous association-list scan this program
    // was quadratic. It pins the observable contract
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
/// structural (spec: 03 — Sistema de tipos) with no identity operator to
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

// --- stdlib layer cross-check -----------------------------------------
//
// Adding one builtin means editing three layers: the checker's
// signature table (`brasa_typeck::builtins`), the `BUILTINS` registry
// (`brasa_bytecode::builtin`), and the VM's implementation
// (`brasa_vm::builtins`). Nothing makes forgetting one a compile error,
// and the failure surfaces only at runtime. It was four layers until
// BRS-108 retired the walker's own implementation.
//
// This test closes that hole by driving every registry entry through
// all three at once: a missing checker signature fails the frontend, a
// missing VM implementation fatals at run time, and a registry entry
// that no longer exists takes its snippet's coverage assertion with
// it.

/// The `BUILTINS` entries that are not stdlib surface: the code
/// generator's internal raisers, which a checked program cannot reach
/// through a stdlib call. They are pinned by id resolution instead of
/// by a snippet.
const INTERNAL_BUILTINS: &[&str] = &["<fatal>", "<assert-failed>"];

/// The stdin every cross-check snippet sees, so `io.readLine` and
/// `io.readAll` have something to consume.
const CROSS_CHECK_STDIN: &[u8] = b"line\n";

/// Whether `module` reaches `builtin` — either by calling it or by
/// binding it as a value. Guards against a snippet that compiles and
/// runs but never actually exercises the entry it claims to cover.
fn module_reaches(module: &brasa_bytecode::Module, builtin: brasa_bytecode::BuiltinId) -> bool {
    module.functions.iter().any(|func| {
        func.chunk.ops().iter().any(|op| {
            matches!(
                op,
                brasa_bytecode::Op::CallBuiltin { builtin: found, .. }
                    | brasa_bytecode::Op::BindBuiltin(found)
                    if *found == builtin
            )
        })
    })
}

/// One snippet per stdlib `BUILTINS` entry, keyed by the entry's name.
/// Each snippet must succeed and must be re-runnable: the harness
/// executes it once per GC leg.
///
/// `dir` is a scratch directory holding a read-only fixture at `ro/`
/// (`ro/a.txt` and `ro/sub/b.txt`); the mutating `std::fs` snippets own
/// disjoint paths under it and reset themselves.
/// A loopback HTTP server that answers every request identically, for
/// the `std::http` snippets.
///
/// Started once and left running for the process. Loopback only, so the
/// corpus stays offline-safe: the alternative is either a network
/// dependency, which makes the suite fail on a build machine with no
/// route, or leaving `status`/`body`/`header` uncovered, which is what
/// the cross-layer guard exists to prevent.
fn http_probe_url() -> &'static str {
    use std::io::{Read, Write};
    use std::sync::OnceLock;

    static URL: OnceLock<String> = OnceLock::new();

    URL.get_or_init(|| {
        let listener =
            std::net::TcpListener::bind("127.0.0.1:0").expect("a loopback port is available");
        let port = listener.local_addr().expect("the bound address").port();

        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };

                // The request is drained only far enough to reach the
                // end of its headers; the body, if any, is irrelevant
                // to a canned answer.
                let mut seen = Vec::new();
                let mut byte = [0u8; 1];
                while stream.read_exact(&mut byte).is_ok() {
                    seen.push(byte[0]);
                    if seen.ends_with(b"\r\n\r\n") {
                        break;
                    }
                }

                let _ = stream.write_all(
                    b"HTTP/1.1 201 Created\r\n\
                      Content-Type: text/plain\r\n\
                      Content-Length: 2\r\n\
                      Connection: close\r\n\r\nok",
                );
                let _ = stream.flush();
            }
        });

        format!("http://127.0.0.1:{port}/")
    })
    .as_str()
}

fn builtin_snippets(dir: &str) -> Vec<(&'static str, String, &'static str)> {
    let math = "import std::math\n";
    let proc = "import std::proc\n";
    let env = "import std::env\n";
    let fs = "import std::fs\n";
    let json = "import std::json\n";
    let io = "import std::io\n";
    let time = "import std::time\n";
    let rand = "import std::rand\n";
    let http = "import std::http\n";
    let cli = "import std::cli\n";
    let spec = "let spec = [[\"option\", \"top\", \"t\", \"rows\"], [\"flag\", \"verbose\", \"v\", \"loud\"]]\n";
    let probe = http_probe_url();

    // A JSON document holding one node of every kind, for the accessors.
    let doc = concat!(
        "import std::json\n",
        "let d = json.parse(\"{\\\"s\\\": \\\"x\\\", \\\"n\\\": 1, \\\"f\\\": 1.5, ",
        "\\\"b\\\": true, \\\"z\\\": null, \\\"v\\\": [1], \\\"o\\\": {\\\"k\\\": 1}}\")\n"
    );

    let map = "let m: Map<string, int> = { \"a\": 1 }\n";

    vec![
        // Prelude printers.
        ("puts", "puts 1\n".to_string(), "1\n"),
        ("print", "print(\"x\")\nputs \"\"\n".to_string(), "x\n"),
        // The universal derived `toString`, as a bound value: the call
        // form compiles to `Op::ToString`, not to this entry.
        (
            "toString",
            "let f = [1].toString\nputs f()\n".to_string(),
            "[1]\n",
        ),
        // string.
        ("len", "puts \"ab\".len()\n".to_string(), "2\n"),
        ("count", "puts \"aa\".count(\"a\")\n".to_string(), "2\n"),
        ("trim", "puts \"  x  \".trim()\n".to_string(), "x\n"),
        ("trimStart", "puts \"  x\".trimStart()\n".to_string(), "x\n"),
        ("trimEnd", "puts \"x  \".trimEnd()\n".to_string(), "x\n"),
        ("toUpper", "puts \"a\".toUpper()\n".to_string(), "A\n"),
        ("toLower", "puts \"A\".toLower()\n".to_string(), "a\n"),
        (
            "contains?",
            "puts \"ab\".contains?(\"a\")\n".to_string(),
            "true\n",
        ),
        (
            "startsWith?",
            "puts \"ab\".startsWith?(\"a\")\n".to_string(),
            "true\n",
        ),
        (
            "endsWith?",
            "puts \"ab\".endsWith?(\"b\")\n".to_string(),
            "true\n",
        ),
        (
            "split",
            "puts \"a,b\".split(\",\")\n".to_string(),
            "[\"a\", \"b\"]\n",
        ),
        (
            "lines",
            "puts \"a\\nb\".lines()\n".to_string(),
            "[\"a\", \"b\"]\n",
        ),
        (
            "chars",
            "puts \"ab\".chars()\n".to_string(),
            "[\'a\', \'b\']\n",
        ),
        ("bytes", "puts \"ab\".bytes()\n".to_string(), "[97, 98]\n"),
        // Both receivers, since `slice` is one registry entry serving a
        // string and a Vector arm in the backend.
        (
            "slice",
            "puts \"abc\".slice(0, 2)\nputs([1, 2, 3].slice(0, 2))\n".to_string(),
            "ab\n[1, 2]\n",
        ),
        (
            "removePrefix",
            "puts \"src/main.bras\".removePrefix(\"src/\")\n".to_string(),
            "main.bras\n",
        ),
        ("repeat", "puts \"a\".repeat(2)\n".to_string(), "aa\n"),
        (
            "replace",
            "puts \"aa\".replace(\"a\", \"b\")\n".to_string(),
            "bb\n",
        ),
        (
            "padStart",
            "puts \"7\".padStart(2, \"0\")\n".to_string(),
            "07\n",
        ),
        (
            "padEnd",
            "puts \"7\".padEnd(2, \"0\")\n".to_string(),
            "70\n",
        ),
        (
            "find",
            "puts \"abc\".find(\"b\") ?? -1\n".to_string(),
            "1\n",
        ),
        ("toInt", "puts \"1\".toInt()\n".to_string(), "1\n"),
        ("toFloat", "puts \"1.5\".toFloat()\n".to_string(), "1.5\n"),
        // Both receivers, since `toFixed` is one registry entry serving
        // an int and a float arm in each backend.
        (
            "toFixed",
            "puts((1.5).toFixed(2))\nputs((3).toFixed(1))\n".to_string(),
            "1.50\n3.0\n",
        ),
        (
            "match?",
            "puts \"ab\".match?(\"a\")\n".to_string(),
            "true\n",
        ),
        (
            "captures",
            "puts \"ab\".captures(\"(a)\")\n".to_string(),
            "Some([\"a\", \"a\"])\n",
        ),
        (
            "replaceRe",
            "puts \"a1\".replaceRe(\"[0-9]\", \"#\")\n".to_string(),
            "a#\n",
        ),
        (
            "scan",
            "puts \"a1\".scan(\"[0-9]\")\n".to_string(),
            "[\"1\"]\n",
        ),
        // Vector.
        (
            "push",
            "let v = [1]\nv.push(2)\nputs v\n".to_string(),
            "[1, 2]\n",
        ),
        (
            "pop",
            "let v = [1]\nputs v.pop() ?? -1\n".to_string(),
            "1\n",
        ),
        (
            "first",
            "let v = [1]\nputs v.first() ?? -1\n".to_string(),
            "1\n",
        ),
        (
            "last",
            "let v = [1]\nputs v.last() ?? -1\n".to_string(),
            "1\n",
        ),
        (
            "reverse",
            "let v = [1, 2]\nputs v.reverse()\n".to_string(),
            "[2, 1]\n",
        ),
        // Both element kinds, since `join` renders a non-string element
        // through the derived `toString` rather than rejecting it.
        (
            "join",
            "let v = [\"a\", \"b\"]\nputs v.join(\",\")\nputs([1, 2].join(\",\"))\n".to_string(),
            "a,b\n1,2\n",
        ),
        (
            "map",
            "let v = [1]\nputs v.map(|n| n + 1)\n".to_string(),
            "[2]\n",
        ),
        (
            "filter",
            "let v = [1]\nputs v.filter(|n| n > 0)\n".to_string(),
            "[1]\n",
        ),
        (
            "each",
            "let v = [1]\nv.each(|n| puts(n))\n".to_string(),
            "1\n",
        ),
        (
            "sortBy",
            "let v = [2, 1]\nputs v.sortBy(|n| n)\n".to_string(),
            "[1, 2]\n",
        ),
        (
            "reduce",
            "let v = [1, 2]\nputs v.reduce(0, |acc, x| acc + x)\n".to_string(),
            "3\n",
        ),
        (
            "any?",
            "let v = [1]\nputs v.any?(|n| n > 0)\n".to_string(),
            "true\n",
        ),
        (
            "all?",
            "let v = [1]\nputs v.all?(|n| n > 0)\n".to_string(),
            "true\n",
        ),
        (
            "sort",
            "let v = [2, 1]\nputs v.sort()\n".to_string(),
            "[1, 2]\n",
        ),
        (
            "zip",
            "let v = [1]\nputs v.zip([\"a\"])\n".to_string(),
            "[(1, \"a\")]\n",
        ),
        (
            "flatten",
            "let v = [[1], [2]]\nputs v.flatten()\n".to_string(),
            "[1, 2]\n",
        ),
        (
            "uniq",
            "let v = [1, 1]\nputs v.uniq()\n".to_string(),
            "[1]\n",
        ),
        // Map.
        ("keys", format!("{map}puts m.keys()\n"), "[\"a\"]\n"),
        ("values", format!("{map}puts m.values()\n"), "[1]\n"),
        (
            "insert",
            format!("{map}m.insert(\"b\", 2)\nputs m.len()\n"),
            "2\n",
        ),
        (
            "remove",
            format!("{map}puts m.remove(\"a\") ?? -1\n"),
            "1\n",
        ),
        ("get", format!("{map}puts m.get(\"a\") ?? -1\n"), "1\n"),
        ("has?", format!("{map}puts m.has?(\"a\")\n"), "true\n"),
        (
            "entries",
            format!("{map}puts m.entries()\n"),
            "[(\"a\", 1)]\n",
        ),
        (
            "merge",
            format!("{map}puts m.merge({{ \"b\": 2 }})\n"),
            "{ \"a\": 1, \"b\": 2 }\n",
        ),
        // Set.
        (
            "add",
            "let s = Set([1])\ns.add(2)\nputs s.len()\n".to_string(),
            "2\n",
        ),
        (
            "union",
            "puts Set([1]).union(Set([2]))\n".to_string(),
            "Set([1, 2])\n",
        ),
        (
            "intersect",
            "puts Set([1]).intersect(Set([1]))\n".to_string(),
            "Set([1])\n",
        ),
        (
            "diff",
            "puts Set([1]).diff(Set([2]))\n".to_string(),
            "Set([1])\n",
        ),
        // std::math.
        ("math.sqrt", format!("{math}puts math.sqrt(4.0)\n"), "2.0\n"),
        (
            "math.floor",
            format!("{math}puts math.floor(1.7)\n"),
            "1.0\n",
        ),
        ("math.ceil", format!("{math}puts math.ceil(1.2)\n"), "2.0\n"),
        (
            "math.round",
            format!("{math}puts math.round(1.5)\n"),
            "2.0\n",
        ),
        (
            "math.pow",
            format!("{math}puts math.pow(2.0, 3.0)\n"),
            "8.0\n",
        ),
        ("math.abs", format!("{math}puts math.abs(-1)\n"), "1\n"),
        ("math.min", format!("{math}puts math.min(1, 2)\n"), "1\n"),
        ("math.max", format!("{math}puts math.max(1, 2)\n"), "2\n"),
        (
            "math.pi",
            format!("{math}puts math.pi\n"),
            "3.141592653589793\n",
        ),
        (
            "math.e",
            format!("{math}puts math.e\n"),
            "2.718281828459045\n",
        ),
        // std::proc, plus the `Output` field reads.
        (
            "proc.run",
            format!("{proc}puts proc.run([\"/bin/sh\", \"-c\", \"printf hi\"]).stdout\n"),
            "hi\n",
        ),
        (
            "proc.tryRun",
            format!("{proc}puts proc.tryRun(\"true\").code\n"),
            "0\n",
        ),
        (
            "proc.shell",
            format!("{proc}puts proc.shell(\"printf hi\").stdout\n"),
            "hi\n",
        ),
        (
            "cli.parse",
            format!("{cli}{spec}puts cli.parse([\"--top\", \"3\"], spec).option(\"top\") ?? \"none\"\n"),
            "3\n",
        ),
        (
            "cli.help",
            format!("{cli}{spec}puts cli.help(\"tool\", spec).lines().first() ?? \"none\"\n"),
            "usage: tool [options]\n",
        ),
        (
            "flag",
            format!("{cli}{spec}puts cli.parse([\"-v\"], spec).flag(\"verbose\")\n"),
            "true\n",
        ),
        (
            "option",
            format!("{cli}{spec}puts cli.parse([], spec).option(\"top\") ?? \"default\"\n"),
            "default\n",
        ),
        (
            "rest",
            format!("{cli}{spec}puts cli.parse([\"--\", \"--literal\"], spec).rest.join(\",\")\n"),
            "--literal\n",
        ),
        (
            "http.get",
            // Port 1 is refused everywhere and needs no network, so the
            // corpus stays offline-safe and deterministic. The error
            // path is the only one a test can pin without a server.
            "import std::http\nputs http.get(\"http://127.0.0.1:1/\").status catch (e)\n  http.RequestError => -1\nend\n".to_string(),
            "-1\n",
        ),
        (
            "http.post",
            "import std::http\nputs http.post(\"http://127.0.0.1:1/\", \"body\").status catch (e)\n  http.RequestError => -1\nend\n".to_string(),
            "-1\n",
        ),
        (
            "status",
            format!("{http}puts http.get(\"{probe}\").status\n"),
            "201\n",
        ),
        (
            "body",
            format!("{http}puts http.get(\"{probe}\").body\n"),
            "ok\n",
        ),
        (
            "header",
            // Asked with a different case than the server sent, because
            // header names are case-insensitive and the lookup has to
            // be too.
            format!("{http}puts http.get(\"{probe}\").header(\"Content-Type\") ?? \"missing\"\n"),
            "text/plain\n",
        ),
        (
            "proc.tryRunAll",
            format!(
                "{proc}let outs = proc.tryRunAll([[\"/bin/sh\", \"-c\", \"printf a\"], [\"/bin/sh\", \"-c\", \"printf b\"]], 2)\nputs outs[0].stdout + outs[1].stdout\n"
            ),
            "ab\n",
        ),
        (
            "stdout",
            format!("{proc}let o = proc.shell(\"printf hi\")\nputs o.stdout\n"),
            "hi\n",
        ),
        (
            "stderr",
            format!("{proc}let o = proc.shell(\"printf e 1>&2\")\nputs o.stderr\n"),
            "e\n",
        ),
        (
            "code",
            format!("{proc}let o = proc.shell(\"true\")\nputs o.code\n"),
            "0\n",
        ),
        // std::env. `env.cd` targets the current directory so the
        // process cwd never moves: this binary runs its tests in
        // parallel and another test pins relative-path resolution.
        (
            "env.get",
            format!("{env}puts env.get(\"BRASA_CROSS_CHECK_MISSING\")\n"),
            "None\n",
        ),
        (
            "env.set",
            format!(
                "{env}env.set(\"BRASA_CROSS_CHECK\", \"v\")\nputs env.get(\"BRASA_CROSS_CHECK\")\n"
            ),
            "Some(\"v\")\n",
        ),
        (
            "env.vars",
            format!("{env}puts env.vars().len() > 0\n"),
            "true\n",
        ),
        ("env.args", format!("{env}puts env.args()\n"), "[]\n"),
        (
            "env.cwd",
            format!("{env}puts env.cwd().len() > 0\n"),
            "true\n",
        ),
        (
            "env.cd",
            format!("{env}env.cd(env.cwd())\nputs \"cd ok\"\n"),
            "cd ok\n",
        ),
        // The one snippet that deliberately does not end in success:
        // choosing a status IS its behavior.
        (
            "env.exit",
            format!("{env}puts \"bye\"\nenv.exit(0)\n"),
            "bye\n",
        ),
        // std::fs: read-only members over the fixture.
        (
            "fs.read",
            format!("{fs}puts fs.read(\"{dir}/ro/a.txt\")\n"),
            "a\n",
        ),
        (
            "fs.exists?",
            format!("{fs}puts fs.exists?(\"{dir}/ro/a.txt\")\n"),
            "true\n",
        ),
        (
            "fs.isFile?",
            format!("{fs}puts fs.isFile?(\"{dir}/ro/a.txt\")\n"),
            "true\n",
        ),
        (
            "fs.isDir?",
            format!("{fs}puts fs.isDir?(\"{dir}/ro\")\n"),
            "true\n",
        ),
        (
            "fs.ls",
            format!("{fs}puts fs.ls(\"{dir}/ro\")\n"),
            "[\"a.txt\", \"sub\"]\n",
        ),
        (
            "fs.glob",
            format!("{fs}puts fs.glob(\"{dir}/ro/*.txt\")\n"),
            "[\"<TMP>/ro/a.txt\"]\n",
        ),
        (
            "fs.walk",
            format!("{fs}puts fs.walk(\"{dir}/ro\")\n"),
            "[\"<TMP>/ro/a.txt\", \"<TMP>/ro/sub/b.txt\"]\n",
        ),
        (
            "fs.tryWalk",
            format!("{fs}puts fs.tryWalk(\"{dir}/ro\")\n"),
            "Walk { paths: [\"<TMP>/ro/a.txt\", \"<TMP>/ro/sub/b.txt\"], unreadable: [] }\n",
        ),
        (
            "paths",
            format!("{fs}puts fs.tryWalk(\"{dir}/ro\").paths\n"),
            "[\"<TMP>/ro/a.txt\", \"<TMP>/ro/sub/b.txt\"]\n",
        ),
        (
            "unreadable",
            format!("{fs}puts fs.tryWalk(\"{dir}/ro\").unreadable\n"),
            "[]\n",
        ),
        // std::fs: mutating members, each on paths it owns alone and
        // re-runnable from any starting state.
        (
            "fs.write",
            format!(
                "{fs}fs.write(\"{dir}/write.txt\", \"x\")\nputs fs.read(\"{dir}/write.txt\")\n"
            ),
            "x\n",
        ),
        (
            "fs.append",
            format!(
                "{fs}fs.write(\"{dir}/append.txt\", \"x\")\n\
                 fs.append(\"{dir}/append.txt\", \"y\")\n\
                 puts fs.read(\"{dir}/append.txt\")\n"
            ),
            "xy\n",
        ),
        (
            "fs.mkdir",
            format!(
                "{fs}let p = \"{dir}/mkdir\"\n\
                 if fs.exists?(p)\n  fs.rmAll(p)\nend\n\
                 fs.mkdir(p)\nputs fs.isDir?(p)\n"
            ),
            "true\n",
        ),
        (
            "fs.mkdirAll",
            format!(
                "{fs}let p = \"{dir}/mkdirall\"\n\
                 if fs.exists?(p)\n  fs.rmAll(p)\nend\n\
                 fs.mkdirAll(p + \"/a/b\")\nputs fs.isDir?(p + \"/a/b\")\n"
            ),
            "true\n",
        ),
        (
            "fs.rm",
            format!(
                "{fs}let p = \"{dir}/rm.txt\"\n\
                 fs.write(p, \"x\")\nfs.rm(p)\nputs fs.exists?(p)\n"
            ),
            "false\n",
        ),
        (
            "fs.rmAll",
            format!(
                "{fs}let p = \"{dir}/rmall\"\n\
                 fs.mkdirAll(p + \"/a\")\nfs.rmAll(p)\nputs fs.exists?(p)\n"
            ),
            "false\n",
        ),
        (
            "fs.cp",
            format!(
                "{fs}fs.write(\"{dir}/cp-src.txt\", \"x\")\n\
                 fs.cp(\"{dir}/cp-src.txt\", \"{dir}/cp-dst.txt\")\n\
                 puts fs.read(\"{dir}/cp-dst.txt\")\n"
            ),
            "x\n",
        ),
        (
            "fs.mv",
            format!(
                "{fs}fs.write(\"{dir}/mv-src.txt\", \"x\")\n\
                 fs.mv(\"{dir}/mv-src.txt\", \"{dir}/mv-dst.txt\")\n\
                 puts fs.read(\"{dir}/mv-dst.txt\")\n"
            ),
            "x\n",
        ),
        // std::fs: the pure path helpers.
        (
            "fs.join",
            format!("{fs}puts fs.join(\"a\", \"b\")\n"),
            "a/b\n",
        ),
        (
            "fs.base",
            format!("{fs}puts fs.base(\"a/b.txt\")\n"),
            "b.txt\n",
        ),
        ("fs.dir", format!("{fs}puts fs.dir(\"a/b.txt\")\n"), "a\n"),
        ("fs.ext", format!("{fs}puts fs.ext(\"a/b.txt\")\n"), "txt\n"),
        ("fs.abs", format!("{fs}puts fs.abs(\"/a/./b\")\n"), "/a/b\n"),
        (
            "fs.resolve",
            format!("{fs}puts fs.resolve(\"{dir}/ro\").len() > 0\n"),
            "true\n",
        ),
        (
            "fs.isSymlink?",
            format!("{fs}puts fs.isSymlink?(\"{dir}/ro\")\n"),
            "false\n",
        ),
        // std::json.
        (
            "json.parse",
            format!("{json}puts json.parse(\"1\")\n"),
            "1\n",
        ),
        (
            "json.stringify",
            format!("{json}puts json.stringify(json.parse(\"1\"))\n"),
            "1\n",
        ),
        (
            "asString",
            format!("{doc}puts d[\"s\"].asString() ?? \"?\"\n"),
            "x\n",
        ),
        (
            "asInt",
            format!("{doc}puts d[\"n\"].asInt() ?? -1\n"),
            "1\n",
        ),
        (
            "asFloat",
            format!("{doc}puts d[\"f\"].asFloat() ?? -1.0\n"),
            "1.5\n",
        ),
        (
            "asBool",
            format!("{doc}puts d[\"b\"].asBool() ?? false\n"),
            "true\n",
        ),
        (
            "asArray",
            format!("{doc}let items: Vector<Json> = d[\"v\"].asArray() ?? []\nputs items.len()\n"),
            "1\n",
        ),
        (
            "asObject",
            format!(
                "{doc}let members: Map<string, Json> = d[\"o\"].asObject() ?? {{}}\n\
                 puts members.len()\n"
            ),
            "1\n",
        ),
        ("null?", format!("{doc}puts d[\"z\"].null?()\n"), "true\n"),
        // std::io.
        ("io.puts", format!("{io}io.puts(\"x\")\n"), "x\n"),
        (
            "io.print",
            format!("{io}io.print(\"x\")\nputs \"\"\n"),
            "x\n",
        ),
        // Its output is stderr, so the pinned stdout below is empty
        // and says nothing about the builtin. What it writes is pinned
        // by `io_printers_split_across_stdout_and_stderr`, which
        // asserts both streams.
        ("io.eprint", format!("{io}io.eprint(\"x\")\n"), ""),
        (
            "io.readLine",
            format!("{io}puts io.readLine() ?? \"<eof>\"\n"),
            "line\n",
        ),
        ("io.readAll", format!("{io}puts io.readAll()\n"), "line\n\n"),
        // std::time. Only pinned properties: the clock members move.
        (
            "time.now",
            format!("{time}puts time.now() > 0.0\n"),
            "true\n",
        ),
        (
            "time.nowMillis",
            format!("{time}puts time.nowMillis() > 0\n"),
            "true\n",
        ),
        (
            "time.sleep",
            format!("{time}time.sleep(0)\nputs \"slept\"\n"),
            "slept\n",
        ),
        (
            "time.iso",
            format!("{time}puts time.iso(0)\n"),
            "1970-01-01T00:00:00.000Z\n",
        ),
        // std::rand. Seeded or single-outcome, so every leg agrees.
        (
            "rand.seed",
            format!("{rand}rand.seed(1)\nputs \"seeded\"\n"),
            "seeded\n",
        ),
        ("rand.int", format!("{rand}puts rand.int(3..=3)\n"), "3\n"),
        (
            "rand.float",
            format!("{rand}puts rand.float() < 1.0\n"),
            "true\n",
        ),
        (
            "rand.choice",
            format!("{rand}puts rand.choice([7])\n"),
            "7\n",
        ),
        (
            "rand.shuffle",
            format!("{rand}puts rand.shuffle([9])\n"),
            "[9]\n",
        ),
    ]
}

#[test]
fn every_builtin_crosses_all_three_stdlib_layers() {
    use std::collections::BTreeSet;

    let tmp = fs_temp_dir("crosscheck");
    std::fs::create_dir_all(tmp.join("ro/sub")).expect("fixture dirs");
    std::fs::write(tmp.join("ro/a.txt"), "a").expect("fixture written");
    std::fs::write(tmp.join("ro/sub/b.txt"), "b").expect("fixture written");

    let dir = tmp.display().to_string();
    let snippets = builtin_snippets(&dir);

    let covered: BTreeSet<&str> = snippets.iter().map(|(name, _, _)| *name).collect();
    assert_eq!(
        covered.len(),
        snippets.len(),
        "the snippet table names a builtin twice"
    );

    let surface: BTreeSet<&str> = brasa_bytecode::BUILTINS
        .iter()
        .map(|def| def.name)
        .filter(|name| !INTERNAL_BUILTINS.contains(name))
        .collect();
    assert_eq!(
        covered, surface,
        "the snippet table and the BUILTINS registry describe different surfaces"
    );

    for name in INTERNAL_BUILTINS {
        assert!(
            brasa_bytecode::builtin_id(name).is_some(),
            "the code generator's internal `{name}` entry left the registry"
        );
    }

    for (name, source, expected_stdout) in &snippets {
        let id = brasa_bytecode::builtin_id(name)
            .unwrap_or_else(|| panic!("`{name}` is not in the registry"));

        // The checker must know the signature, or this panics.
        let front = compile_frontend(source);
        assert!(
            module_reaches(&front.module, id),
            "the `{name}` snippet compiles without reaching builtin id {id:?}"
        );

        let run = run_vm(
            &front,
            brasa_vm::DEFAULT_MAX_CALL_DEPTH,
            brasa_vm::DEFAULT_GC_BUDGET_BYTES,
            &[],
            CROSS_CHECK_STDIN,
        );
        // Only `env.exit` is allowed to end anywhere but success —
        // choosing a status IS its behavior. Every other snippet must
        // still finish cleanly, or it did not exercise its builtin.
        let expected_outcome = if *name == "env.exit" {
            Outcome::Exit { code: 0 }
        } else {
            Outcome::Success
        };
        assert_eq!(
            run.outcome, expected_outcome,
            "`{name}` did not finish cleanly: {run:?}"
        );
        // The written expectation, not just backend agreement: a
        // snippet whose only oracle is "the other backend printed the
        // same thing" stops being an oracle the day there is one
        // backend. `<TMP>` stands for the fixture root, which moves
        // per run. Recorded from the walker before it was retired
        // (BRS-108).
        //
        // Two groups of entries pin an ORDER, and both orders are
        // specified rather than incidental — which is what makes them
        // safe to freeze into a corpus that outlives the oracle.
        // `ls` and `walk` sort bytewise instead of handing back
        // readdir order (`brasa_runtime::fs_glue`), and `Map`/`Set`
        // are insertion-ordered by spec: 07 — Diseño del bytecode, backed
        // by `OrderedMap`/`OrderedSet` whose hash index only answers
        // lookups and never decides iteration.
        assert_eq!(
            run.stdout,
            expected_stdout.replace("<TMP>", &dir),
            "`{name}` printed something other than its pinned output"
        );

        // `io.eprint` is the only builtin here that writes to stderr,
        // and what it writes is pinned by
        // `io_printers_split_across_stdout_and_stderr`. For everything
        // else the stream must stay empty: the walker comparison used
        // to catch a builtin leaking onto it, and after BRS-108 this is
        // what does.
        let expected_stderr = if *name == "io.eprint" { "x" } else { "" };
        assert_eq!(
            run.stderr, expected_stderr,
            "`{name}` wrote something unexpected to stderr"
        );

        let hot = run_vm(
            &front,
            brasa_vm::DEFAULT_MAX_CALL_DEPTH,
            8,
            &[],
            CROSS_CHECK_STDIN,
        );
        assert_eq!(run, hot, "`{name}` disagrees under GC pressure");
    }

    let _ = std::fs::remove_dir_all(&tmp);
}
