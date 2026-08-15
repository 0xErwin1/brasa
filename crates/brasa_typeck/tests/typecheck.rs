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
let stripped = "src/main.bras".removePrefix("src/")
let parsed = "42".toInt()
"#
);

typecheck_test!(
    set_ctor_and_zero_param_lambdas,
    r#"
let inferred = Set([1, 2, 2, 3])
let annotated: Set<string> = Set(["a", "b"])
let n = inferred.len()
let seen = annotated.has?("a")

let thunk = || 41 + 1
let value = thunk()
let effect = do ||
  puts "ran"
end
effect()
"#
);

typecheck_error_test!(
    set_ctor_arity_and_argument_errors,
    r#"
let none = Set()
let two = Set([1], [2])
let bad = Set(1)
"#
);

// A `Map` index READS `Option<V>` because a missing key is a normal
// case, but it WRITES a `V`: there is no key to be missing on the
// assigning side. A `Vector` element writes what it reads.
typecheck_test!(
    index_assignment_writes_the_element_type,
    r#"
let m: Map<string, int> = {}
m["a"] = 1
let read = m["a"]

let nested: Map<string, Vector<int>> = {}
nested["xs"] = [1, 2]

let options: Map<string, Option<int>> = {}
options["a"] = Some(1)
options["b"] = None

let v = [1, 2]
v[0] = 9
"#
);

// The write type is the element, so an `Option` is now rejected where
// it used to be demanded — and the demanded form stored a double-wrapped
// value the read then returned as `Some(Some(1))`.
typecheck_error_test!(
    index_assignment_rejects_the_read_type_and_the_wrong_element,
    r#"
let m: Map<string, int> = {}
m["a"] = Some(1)
m["b"] = "s"

let options: Map<string, Option<int>> = {}
options["a"] = 1

let v = [1, 2]
v[0] = "x"
"#
);

// A method's OWN type parameters are solved from the call's arguments,
// exactly like a generic free function's. The struct's parameters and
// the method's are solved by different owners, so a generic method on a
// generic struct has both live at once.
typecheck_test!(
    method_generics_are_solved_at_the_call_site,
    r##"
struct Box
  value: int

  def wrap<T>(self, x: T): Vector<T>
    [x]
  end

  def pick<T>(self, a: T, b: T): T
    a
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
let ints = b.wrap(5)
let strings = b.wrap("hi")
let picked = b.pick(1, 2)
let paired = b.pair(1, "x")
let biggest = b.largest(3, 9)

let h = Holder { item: 7 }
let mixed = h.with("a")
"##
);

// A lambda parameter binds through a pattern, like `match` and `for`
// already do. It lowers to a `match` over a synthetic parameter, so the
// bindings are ordinary locals with ordinary inferred types.
//
// The last case has SEVERAL pattern parameters in one lambda, one of
// them interleaved with an ordinary named parameter: each gets its own
// temporary, so the empty names the parser leaves behind cannot
// collide with each other.
typecheck_test!(
    lambda_parameters_destructure,
    r##"
let counts: Map<string, int> = { "a": 1 }
let ranked = counts.entries().sortBy(|(key, hits)| -hits)

let pairs = [(1, "a")]
let rendered = pairs.map(|(n, s)| "#{n}#{s}")
let firsts = pairs.map(|(n, _)| n)
let annotated = pairs.map(|(n, s): (int, string)| n)

let nested = [((1, 2), "x")]
let flattened = nested.map(|((a, b), s)| a + b)

let folded = pairs.reduce(0, |acc, (n, _)| acc + n)

def combine(f: ((int, int), int, (int, int)) -> int): int
  f((1, 2), 10, (3, 4))
end
let combined = combine(|(a, b), m, (c, d)| a + b + m + c + d)
"##
);

// The pattern has to match every value the parameter can take, and a
// lambda has no arms to add — so the diagnostic points somewhere the
// reader can actually go. A tuple pattern against a non-tuple is the
// ordinary pattern error.
typecheck_error_test!(
    a_lambda_parameter_pattern_must_match_every_value,
    r#"
struct P
  x: int
end

let refutable: Vector<(Option<int>, int)> = [(Some(1), 2)]
let bad = refutable.map(|(Some(a), b)| a + b)

let structs = [P { x: 1 }]
let wrong = structs.map(|(a, b)| a)
"#
);

// A `let` pattern has to match every value its right side can take —
// a `let` has no other arms to add — so a refutable one reports in
// `let` terms and points at binding and matching (BRS-128). A tuple
// pattern against a non-tuple stays the ordinary pattern error.
typecheck_error_test!(
    a_let_pattern_must_match_every_value,
    r#"
def first(pair: (Option<int>, int)): int
  let (Some(a), b) = pair
  a + b
end

let scalar = 5
let (x, y) = scalar
"#
);

// `Comparable` is structural like any other interface: a type with a
// conforming `cmp` satisfies it. Primitives are answered natively
// because they have no `cmp` to find, not because non-primitives are
// excluded.
typecheck_test!(
    comparable_is_satisfied_by_a_conforming_cmp,
    r#"
struct Money
  cents: int

  def cmp(self, other: Money): int
    self.cents - other.cents
  end
end

def maxOf<T: Comparable>(a: T, b: T): T
  if a > b then a else b end
end

let richest = maxOf(Money { cents: 1 }, Money { cents: 2 })
let bigger = maxOf(1, 2)
let later = maxOf("a", "b")
"#
);

// Satisfaction is transitive through a user interface: a parameter
// constrained by an `Ord` that declares `cmp` also satisfies
// `Comparable`. An unrelated constraint does not — a generic exposes
// only its own constraint's members.
typecheck_test!(
    comparable_is_satisfied_transitively_through_a_user_constraint,
    r#"
interface Ord
  def cmp(self, other: Self): int
end

def maxOf<T: Comparable>(a: T, b: T): T
  if a > b then a else b end
end

def viaOrd<U: Ord>(a: U, b: U): U
  maxOf(a, b)
end
"#
);

typecheck_error_test!(
    comparable_is_not_satisfied_by_an_unrelated_constraint,
    r#"
interface Named
  def name(self): string
end

def maxOf<T: Comparable>(a: T, b: T): T
  if a > b then a else b end
end

def viaNamed<U: Named>(a: U, b: U): U
  maxOf(a, b)
end
"#
);

// The three ways to miss: no `cmp` at all, a `cmp` with the wrong
// signature, and a type that has no members to look at. Each names the
// member rather than reporting a bare "constraint not satisfied".
typecheck_error_test!(
    comparable_names_the_member_a_candidate_is_missing,
    r#"
struct Plain
  n: int
end

struct WrongReturn
  n: int

  def cmp(self, other: WrongReturn): string
    "x"
  end
end

def maxOf<T: Comparable>(a: T, b: T): T
  if a > b then a else b end
end

let absent = maxOf(Plain { n: 1 }, Plain { n: 2 })
let mistyped = maxOf(WrongReturn { n: 1 }, WrongReturn { n: 2 })
let collection = maxOf([1], [2])
"#
);

// What opening the constraint deliberately does NOT open, each closed
// by its own rule: the ordering operators on two struct values, and
// `sort`, which stays limited to vectors of orderable primitives.
typecheck_error_test!(
    comparable_does_not_open_direct_ordering_or_sort,
    r#"
struct Money
  cents: int

  def cmp(self, other: Money): int
    self.cents - other.cents
  end
end

let direct = Money { cents: 1 } > Money { cents: 2 }
let sorted = [Money { cents: 2 }, Money { cents: 1 }].sort()
"#
);

// `< <= > >=` on a constrained parameter compile to a `cmp` call, and an
// operator cannot report a failure, so a throwing `cmp` fails
// conformance rather than escaping the caller's `throws never`. The note
// says the member is there and throws — reporting it as missing would be
// worse than the bug on a struct that visibly declares one.
typecheck_error_test!(
    comparable_is_not_satisfied_by_a_throwing_cmp,
    r#"
struct CmpError
end

struct Ver
  major: int

  def cmp(self, other: Ver): int throws CmpError
    throw CmpError {}
  end
end

def biggest<T: Comparable>(a: T, b: T): T throws never
  if a > b
    a
  else
    b
  end
end

let newest = biggest(Ver { major: 1 }, Ver { major: 2 })
"#
);

// The same rule through member-set entailment: an interface member may
// declare `throws`, so a parameter constrained by a throwing `Ord` does
// not carry the non-throwing `cmp` `Comparable` needs.
typecheck_error_test!(
    comparable_is_not_satisfied_transitively_by_a_throwing_constraint,
    r#"
struct CmpError
end

interface Ord
  def cmp(self, other: Self): int throws CmpError
end

def maxOf<T: Comparable>(a: T, b: T): T
  if a > b then a else b end
end

def viaOrd<U: Ord>(a: U, b: U): U
  maxOf(a, b)
end
"#
);

// Rendering has to be infallible: a `toString` override cannot declare
// `throws`, because `puts`, interpolation, and every container that
// renders its elements reach it — and so does error reporting itself.
// The report points at the clause, which is what has to go.
typecheck_error_test!(
    a_to_string_override_cannot_declare_throws,
    r#"
struct Boom
end

struct Loud
  def toString(self): string throws Boom
    throw Boom {}
  end
end

def render(): string throws never
  [Loud {}].join(",")
end
"#
);

// What the two contracts still accept: `throws never` on `toString`
// declares nothing thrown, and an ordinary `cmp` conforms as before.
typecheck_test!(
    an_infallible_to_string_and_cmp_still_conform,
    r#"
struct Quiet
  n: int

  def toString(self): string throws never
    "Quiet"
  end
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

let shown = Quiet { n: 1 }.toString()
let richest = maxOf(Money { cents: 1 }, Money { cents: 2 })
"#
);

// A method may reuse the struct's parameter name without capturing it:
// the two are different owners, so `Holder<int>.echo<T>("a")` returns a
// string while the receiver still holds an int.
typecheck_test!(
    a_method_generic_shadowing_the_struct_generic_stays_independent,
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
let echoed = h.echo("a")
let same = h.echo(1)
let rendered = h.both("a")

let s = Holder { item: "x" }
let flipped = s.echo(9)
"##
);

// A generic method does not satisfy a fixed interface signature: the
// member is compared as written, and a rigid parameter does not unify
// with a concrete type. A struct carrying a generic method still
// satisfies an interface through its other, non-generic members.
typecheck_test!(
    a_generic_method_does_not_block_satisfaction_by_other_members,
    r##"
interface Named
  def name(self): string
end

struct Tagged
  id: int

  def name(self): string
    "t#{self.id}"
  end

  def cast<T>(self, x: T): T
    x
  end
end

def show<N: Named>(n: N): string
  n.name()
end

let shown = show(Tagged { id: 1 })
"##
);

typecheck_error_test!(
    a_generic_method_does_not_satisfy_a_fixed_interface_signature,
    r#"
interface Boxer
  def box(self, x: int): Vector<int>
end

struct Impl
  n: int

  def box<T>(self, x: T): Vector<T>
    [x]
  end
end

def use<B: Boxer>(b: B): Vector<int>
  b.box(1)
end

let boxed = use(Impl { n: 1 })
"#
);

// The failure modes match the free-function ones: a parameter no
// argument determines is T026, a second argument conflicting with the
// first solution is a plain mismatch against the solved type, and an
// unsatisfied constraint is T027.
typecheck_error_test!(
    method_generics_report_unsolved_conflicting_and_unconstrained,
    r#"
struct Thing
  n: int
end

struct Box
  value: int

  def make<T>(self): Vector<T>
    []
  end

  def pick<T>(self, a: T, b: T): T
    a
  end

  def largest<T: Comparable>(self, a: T, b: T): T
    if a > b then a else b end
  end
end

let b = Box { value: 1 }
let unsolved = b.make()
let conflicting = b.pick(1, "two")
let unordered = b.largest(Thing { n: 1 }, Thing { n: 2 })
"#
);

// `??` produces the carried type, so the scrutinee IS the context: an
// empty literal on the fallback side infers from the `Option` without
// needing an annotation anywhere.
typecheck_test!(
    coalesce_propagates_the_carried_type_into_the_fallback,
    r#"
let ints: Option<Vector<int>> = None
let v = ints ?? []

let pairs: Option<Map<string, int>> = None
let m = pairs ?? {}

let names: Option<Set<string>> = None
let s = names ?? Set([])

def show(xs: Vector<int>): unit
  puts(xs.len())
end
show(ints ?? [])

let annotated: Vector<int> = ints ?? []
let chained: Option<Vector<int>> = None
let c = ints ?? chained ?? []
"#
);

// The hint is offered, not imposed: a fallback that disagrees with the
// carried type still reports T030 in the operator's own terms, and an
// empty literal with no context anywhere still reports T014.
typecheck_error_test!(
    coalesce_fallback_mismatch_and_contextless_literal_still_report,
    r#"
let ints: Option<Vector<int>> = None
let wrong = ints ?? "x"

let nested: Option<int> = None
let doubled = nested ?? nested

let bare = []
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

typecheck_test!(
    options_sugar_typing,
    r#"
struct User
  nickname: Option<string>
end

struct Inner
  value: Option<int>
end

struct Outer
  inner: Option<Inner>
end

def nickLen(user: User): Option<int>
  user.nickname?.len()
end

def deep(o: Option<Outer>): Option<int>
  o?.inner?.value
end

def orZero(opt: Option<int>, m: Map<string, int>): int
  let a = opt ?? 0
  let b = opt ?? opt ?? 0
  let c = m["k"] ?? 0
  a + b + c
end
"#
);

typecheck_test!(
    generics_and_functions,
    r#"
def maxOf<T: Comparable>(a: T, b: T): T
  if a > b then a else b end
end

def first<T>(v: Vector<T>): Option<T>
  v.first()
end

def log<T: Printable>(value: T)
  puts value.toString()
end

let m = maxOf(1, 2)
let s = maxOf("a", "b")
let f = first([1, 2])
log(3.5)
"#
);

typecheck_test!(
    generics_structs_and_interfaces,
    r##"
interface Greeter
  def greet(self, other: Self): string
end

struct Person
  name: string

  def greet(self, other: Person): string
    "#{self.name} greets #{other.name}"
  end
end

struct Point<T: Comparable>
  x: T
  y: T

  def swap(self): Point<T>
    Point { x: self.y, y: self.x }
  end
end

def meet<G: Greeter>(a: G, b: G): string
  a.greet(b)
end

def pick<T: { def toString(): string }>(value: T): string
  value.toString()
end

let p: Point<int> = Point { x: 1, y: 2 }
let sx = p.swap().x
let greeting = meet(Person { name: "Ana" }, Person { name: "Bo" })
let shown = pick(Person { name: "Cy" })
"##
);

typecheck_test!(
    hashable_keys,
    r#"
def tally<K: Hashable>(keys: Vector<K>): Map<K, int>
  let mut counts: Map<K, int> = {}
  for k in keys
    counts.insert(k, (counts[k] ?? 0) + 1)
  end
  counts
end

let words = tally(["a", "b", "a"])
let flags: Map<(int, bool), string> = {}
let grid = { 'x': "origin" }
let ids = Set([1, 2, 3])
let tags: Set<(char, bool)> = Set([])
"#
);

typecheck_error_test!(
    error_hashable_keys,
    r#"
struct Point
  x: int
end

def lookup(m: Map<float, int>): int
  m.len()
end

let weights = { 1.5: "heavy" }
let nested = Set([[1], [2]])
let byPoint = { Point { x: 1 }: "origin" }
let badTuple: Map<(int, Vector<int>), bool> = {}
let badSet: Set<Vector<string>> = Set([])
"#
);

typecheck_error_test!(
    error_constraint_annotations,
    r#"
struct Point<T: Comparable>
  x: T
end

def dist(p: Point<bool>): int
  1
end

def make(): Point<bool>
  Point { x: true }
end

let q: Point<bool> = Point { x: true }
"#
);

typecheck_test!(
    exhaustive_matches,
    r#"
enum Shape
  Circle(radius: float)
  Rect(w: float, h: float)
  Dot
end

def area(shape: Shape): float
  match shape
    Circle(r) => 2.0 * r
    Rect(w, h) => w * h
    Dot => 0.0
  end
end

def describe(o: Option<int>): string
  match o
    Some(n) if n > 10 => "big"
    Some(_) => "some"
    None => "none"
  end
end

def flag(b: bool): int
  match b
    true => 1
    false => 0
  end
end

def nested(pair: (bool, Option<int>)): int
  match pair
    (true, Some(n)) => n
    (true, None) => 0
    (false, Some(_)) => 1
    (false, None) => 2
  end
end

def digits(n: int): string
  match n
    0 => "zero"
    _ => "many"
  end
end

def measure(s: string): int
  match s
    text => text.len()
  end
end
"#
);

typecheck_error_test!(
    error_exhaustiveness,
    r#"
enum Shape
  Circle(radius: float)
  Rect(w: float, h: float)
  Dot
end

enum Compass
  N
  E
  S
  W
  Center
end

def partial(shape: Shape): int
  match shape
    Circle(_) => 1
  end
end

def noneless(o: Option<int>): int
  match o
    Some(n) => n
  end
end

def halfBool(b: bool): int
  match b
    true => 1
  end
end

def nested(pair: (bool, Option<int>)): int
  match pair
    (true, Some(_)) => 1
    (false, _) => 2
  end
end

def literalsOnly(n: int): string
  match n
    0 => "zero"
    1 => "one"
  end
end

def guardedOnly(shape: Shape): int
  match shape
    Circle(r) if r > 1.0 => 1
    Rect(_, _) => 2
    Dot => 3
  end
end

def wayward(c: Compass): int
  match c
    N => 0
  end
end

def opaque<T>(value: T, pick: bool): int
  match value
    x if pick => 1
  end
end
"#
);

typecheck_error_test!(
    error_generics,
    r#"
struct Box
  value: int
end

def maxOf<T: Comparable>(a: T, b: T): T
  if a > b then a else b end
end

def emptyOf<T>(): Vector<T>
  []
end

def shout<T: Printable>(a: T, b: T): bool
  a > b
end

def describe(x: Printable): string
  x.toString()
end

def unwrap(b: Box<int>): int
  b.value
end

def whisper<T: Comparable>(value: T): string
  value.shout()
end

let clash = maxOf(Box { value: 1 }, Box { value: 2 })
let nothing = emptyOf()
let conflict = maxOf(1, "x")
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

typecheck_test!(
    catch_narrowing,
    r#"
struct ParseError
  detail: string
  line: int
end

struct IoError
  path: string
end

def risky(flag: bool): int
  if flag
    throw ParseError { detail: "bad", line: 3 }
  end
  if !flag
    throw IoError { path: "/tmp/x" }
  end
  1
end

def recover(flag: bool): int
  risky(flag) catch (e)
    ParseError if e.line > 10 => e.line
    ParseError => e.detail.len()
    IoError => e.path.len()
    ParseError | IoError => e.toString().len()
    string => e.len()
    _ => -1
  end
end

let total = recover(true) + 1
"#
);

typecheck_test!(
    native_error_arm_narrowing,
    r#"
def parse(s: string): int
  s.toInt() catch (e)
    string.ParseError => e.message.len()
    _ => -1
  end
end

let widened = "1.5".toFloat()
let parsed = parse("42")
"#
);

typecheck_test!(
    panic_arm_narrowing,
    r#"
def guard(n: int): int
  10 / n catch (e)
    panics.DivisionByZero => e.len()
  end
end

let safe = guard(0)
"#
);

typecheck_error_test!(
    error_catch_narrowing,
    r#"
struct ParseError
  detail: string
end

def boom(): int
  throw ParseError { detail: "x" }
end

let r = boom() catch (e)
  ParseError => e.line
  _ => "not an int"
end
"#
);

typecheck_error_test!(
    error_options_sugar,
    r#"
struct User
  name: string
  nickname: Option<string>
end

def misuse(a: Option<int>, b: Option<int>): int
  let w = 1?.len()
  let x = 1 ?? 2
  let y = a ?? "fallback"
  let z = a ?? b
  let u = User { name: "ana" }
  x
end
"#
);

typecheck_test!(
    proc_and_env_module_signatures,
    r#"
import std::proc
import std::env

let out = proc.tryRun(["true"])
let combined = out.stdout + out.stderr
let next = out.code + 1
let sugared = proc.run("echo hi", "stdin").stdout
let piped = proc.shell("wc -l", "a\nb\n").stdout
let home = env.get("HOME")
env.set("BRASA_T", "v")
let all = env.vars()
let args = env.args()
"#
);

typecheck_error_test!(
    proc_module_call_errors,
    r#"
import std::proc

proc.run(42)
proc.run(["true"], "x", "y")
proc.shell(123)
proc.nope()
let r = proc.tryRun(["true"])
let bad = r.bogus
"#
);

typecheck_error_test!(
    env_module_call_errors,
    r#"
import std::env

env.get()
env.set("A", 1)
env.unknown("x")
"#
);

typecheck_test!(
    fs_module_signatures,
    r#"
import std::fs
import std::env

let text = fs.read("/tmp/in.txt")
fs.write("/tmp/out.txt", text)
fs.append("/tmp/out.txt", "more")
let there = fs.exists?("/tmp")
let file = fs.isFile?("/tmp/in.txt")
let folder = fs.isDir?("/tmp")
let names = fs.ls("/tmp")
let matched = fs.glob("/tmp/*.txt")
let everything = fs.walk("/tmp")
let pruned = fs.walk("/tmp", [".git"])
let attempt = fs.tryWalk("/tmp")
let reached = attempt.paths
let missed = attempt.unreadable
let rendered = fs.tryWalk("/tmp", [".git"]).toString()
fs.mkdir("/tmp/a")
fs.mkdirAll("/tmp/a/b/c")
fs.cp("/tmp/in.txt", "/tmp/copy.txt")
fs.mv("/tmp/copy.txt", "/tmp/moved.txt")
fs.rm("/tmp/moved.txt")
fs.rmAll("/tmp/a")
let rebuilt = fs.join(fs.dir("/tmp/in.txt"), fs.base("/tmp/in.txt"))
let extension = fs.ext("/tmp/in.txt")
let absolute = fs.abs("rel.txt")
let here = env.cwd()
env.cd("/tmp")
"#
);

typecheck_error_test!(
    fs_module_call_errors,
    r#"
import std::fs
import std::env

fs.read(42)
fs.write("/tmp/x")
fs.join("a", "b", "c")
fs.nope("x")
fs.tryWalk(42)
fs.tryWalk("/tmp", ".git")
fs.tryWalk("/tmp", [".git"], "extra")

# `Walk` is closed: the two fields and the universal `toString`, and
# nothing the shared method fallback might later grow.
let attempt = fs.tryWalk("/tmp")
let absent = attempt.bogus
let counted = attempt.len()
env.cwd("x")
env.cd()
"#
);

typecheck_test!(
    json_and_io_module_signatures,
    r#"
import std::json
import std::io

let data = json.parse("{\"users\": [{\"name\": \"ada\"}], \"count\": 1}")
let annotated: Json = data
let text = json.stringify(data)

let user = data["users"][0]
let name = user["name"].asString() ?? "anon"
let count = data["count"].asInt() ?? 0
let ratio = data["ratio"].asFloat() ?? 0.0
let active = data["active"].asBool() ?? false
let items = data.asArray()
let members = data.asObject()
let missing = data["nope"].null?()

io.puts(name)
io.print(count)
io.eprint(text)
let line = io.readLine() ?? ""
let everything = io.readAll()
"#
);

typecheck_error_test!(
    json_and_io_module_call_errors,
    r#"
import std::json
import std::io

json.parse(42)
json.stringify("not json")
json.decode("typed bridge is v2")
io.readLine("no args")
io.nope()

let data = json.parse("{}")
data[true]
data.asChar()
"#
);

typecheck_error_test!(
    json_index_is_not_assignable,
    r#"
import std::json

let data = json.parse("{\"a\": 1}")
data["a"] = data["b"]
data["a"][0] = data["b"]
"#
);

typecheck_test!(
    math_time_rand_module_signatures,
    r#"
import std::math
import std::time
import std::rand

let root = math.sqrt(9.0)
let lifted = math.pow(2.0, 8.0) + math.floor(1.9) + math.ceil(1.1) + math.round(2.5)
let wholeAbs = math.abs(-3) + 1
let realAbs = math.abs(-3.5) + 1.0
let small = math.min(1, 2) + 1
let big = math.max(1.5, 2.5) + 1.0
let tau = math.pi * 2.0
let euler = math.e

let seconds = time.now() + 1.0
let millis = time.nowMillis() + 1
time.sleep(0)
let stamp = time.iso(0) + "!"

rand.seed(7)
let n = rand.int(0..10) + 1
let x = rand.float() + 0.5
let pick = rand.choice(["a", "b"]).len()
let shuffled = rand.shuffle([1, 2, 3])
let head = shuffled.first() ?? 0
"#
);

typecheck_error_test!(
    math_time_rand_module_call_errors,
    r#"
import std::math
import std::time
import std::rand

math.sqrt(9)
math.abs("x")
math.min(1, 2.0)
math.pi()
math.nope(1)
time.sleep(1.5)
time.nope()
rand.int(5)
rand.choice("abc")
rand.nope()
"#
);

typecheck_test!(
    collection_method_signatures,
    r#"
let nums = [3, 1, 2]
let total = nums.reduce(0, |acc, x| acc + x)
let joined = nums.map(|x| x.toString()).reduce("", |acc, s| acc + s)
let found = nums.find(|x| x > 1) ?? 0
let anyEven = nums.any?(|x| x % 2 == 0)
let allPos = nums.all?(|x| x > 0)
let sorted = nums.sort()
let pairs = nums.zip(["a", "b", "c"])
let nested = [[1], [2, 3]]
let flat = nested.flatten()
let unique = nums.uniq()
let sliced = nums.slice(0, 2)
let glued = nums.join(", ")

let stock: Map<string, int> = { "a": 1 }
let entries = stock.entries()
let extra: Map<string, int> = { "b": 2 }
let merged = stock.merge(extra)
stock.each(|k, v| puts(k + v.toString()))

let s = Set([1, 2])
let u = s.union(Set([3]))
let i = s.intersect(Set([2]))
let d = s.diff(Set([1]))
"#
);

typecheck_test!(
    tuple_expressions,
    r#"
def swap(p: (int, string)): (string, int)
  match p
    (n, s) => (s, n)
  end
end

let pair = (1, "a")
let one = (7,)
let nested = (1, (2, 3))
let annotated: (int, Vector<int>) = (1, [])
let swapped = swap(pair)
let grid: Map<(int, int), string> = { (0, 0): "origin" }
let cell = grid[(0, 0)]
let corners = Set([(0, 0), (1, 1)])
"#
);

typecheck_error_test!(
    error_tuple_expressions,
    r#"
let element: (int, string) = (1, 2)
let arity: (int, int) = (1, 2, 3)
let scalar: int = (1, 2)
let unhashable = { (1.5, 2): "a" }
"#
);

typecheck_error_test!(
    collection_method_errors,
    r#"
let nums = [1, 2]
nums.sort("x")
let bools = [true, false]
let unsortable = bools.sort()
let unflattenable = nums.flatten()
nums.reduce(0)
let widened = nums.reduce(0, |acc, x| "s")
nums.zip(3)
let s = Set([1])
s.union([2])
let stock: Map<string, int> = { "a": 1 }
let wrongValues: Map<string, string> = { "b": "s" }
stock.merge(wrongValues)
"#
);

// A field named like an interface method does not stand in for one
// unless it holds a matching callable: a `string` field is neither
// callable nor a member with a compatible signature. A struct may no
// longer declare a field and a method of the same name at all
// (`R006`), so the field is the only `tag` this struct can have.
typecheck_error_test!(
    a_field_alone_does_not_provide_a_method,
    r#"
interface Tagged
  def tag(self): string
end

struct Both
  tag: string
end

def describe<T: Tagged>(v: T): string
  v.tag()
end

let b = Both { tag: "field" }
puts b.tag()
puts describe(b)
"#
);

// BRS-109: `break`/`continue` with no enclosing loop is decidable here,
// exactly like `return` outside a function (T019). Reported at every
// depth the code generator would have failed at, including inside a
// lambda: a lambda compiles to its own frame with its own loop stack, so
// a loop it merely appears inside is not one its `break` can reach.
typecheck_error_test!(
    loop_jumps_outside_a_loop_are_rejected,
    r#"
break

def top()
  continue
end

def inside_a_lambda()
  for i in 0..3
    let f = |x: int| do
      break
      x
    end
    puts f(i)
  end
end

def legal()
  for i in 0..3
    if i == 1
      continue
    end
    break
  end
  while true
    break
  end
end
"#
);

// BRS-109: a name that resolves to something which is not a first-class
// value. The legitimate positions — a module handle as a member
// receiver, a prelude function as a call target — type themselves
// without reaching the reporting path, so both stay accepted here.
typecheck_error_test!(
    a_module_handle_and_a_prelude_function_are_not_values,
    r#"
import std::math

puts math
puts puts
let f = print
let ok = math.abs(-1)
puts ok
"#
);

// BRS-53, item 4: the only diagnostic in the whole dogfooding port that
// pointed at the wrong thing. `puts (24).toFloat()` reports a missing
// method on `unit` and never mentions the real cause — parentheses right
// after a callee are a call, so the receiver is `puts`'s result. The
// second case is the same trap written without the space, and the third
// is an ordinary missing method that must NOT get the note.
typecheck_error_test!(
    a_grouping_mistake_after_a_prelude_call_names_its_real_cause,
    r#"
puts (24).toFloat()
print(24).toFloat()

let n = 1
puts n.nope()
"#
);

// Structured concurrency (BRS-133): `concurrent`'s result is the scope
// body's return type, `spawn` answers a `Task` of its block's return
// type, and `value()` unwraps it — the same argument-driven inference
// `Vector.map` uses for its element. The bound reads pin the
// value-position signatures too.
typecheck_test!(
    concurrent_scope_and_task_types,
    r##"
def total(): int
  concurrent do |scope|
    let t = scope.spawn do 21 end
    let spawner = scope.spawn
    let reader = t.value
    t.value() + t.value()
  end
end

def labels(names: Vector<string>): Vector<string>
  concurrent do |scope|
    let tasks = names.map(|name| scope.spawn do "#{name}!" end)
    tasks.map(|task| task.value())
  end
end
"##
);
