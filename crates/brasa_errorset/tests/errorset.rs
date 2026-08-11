//! Snapshot tests for error-set inference and its consuming checks.
//! Inputs are parsed, lowered, resolved, and checked (all with zero
//! diagnostics required), then inferred. Happy-path tests assert the
//! diagnostics channel is empty and snapshot the span-free error-set
//! dump; error tests snapshot the rendered diagnostics so wording,
//! labels, and spans are all pinned.

use std::path::PathBuf;

use brasa_source::SourceMap;

fn infer_source(
    name: &str,
    source: &str,
) -> (
    brasa_hir::LowerResult,
    brasa_errorset::ErrorSetResult,
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

    let inferred = brasa_errorset::infer(
        &lowered.hir,
        &lowered.roots,
        &resolved.resolutions,
        &checked.types,
    );

    (lowered, inferred, source_map)
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

macro_rules! errorset_test {
    ($test_name:ident, $source:expr) => {
        #[test]
        fn $test_name() {
            let (lowered, inferred, _map) = infer_source(stringify!($test_name), $source);
            assert!(
                inferred.diagnostics.is_empty(),
                "expected zero error-set diagnostics, got: {:#?}",
                inferred.diagnostics
            );
            let dump = brasa_errorset::dump::dump(&lowered.hir, &inferred);
            insta::assert_snapshot!(stringify!($test_name), dump);
        }
    };
}

macro_rules! errorset_error_test {
    ($test_name:ident, $source:expr) => {
        #[test]
        fn $test_name() {
            let (_lowered, inferred, map) = infer_source(stringify!($test_name), $source);
            assert!(
                !inferred.diagnostics.is_empty(),
                "expected error-set diagnostics, got none"
            );
            let rendered = render_diagnostics(&inferred.diagnostics, &map);
            insta::assert_snapshot!(stringify!($test_name), rendered);
        }
    };
}

errorset_test!(
    direct_and_transitive,
    r#"
struct IoError
  path: string
end

def readFile(path: string): string
  if path.len() == 0
    throw IoError { path: path }
  end
  "data"
end

def loadAll(path: string): string
  readFile(path)
end

def entry(): string
  loadAll("config")
end

def boom(): int
  throw "bad"
end
"#
);

errorset_test!(
    catch_subtraction_and_rethrow,
    r#"
struct NetError
  detail: string
end

struct ParseError
  line: int
end

struct ConfigError
  cause: string
end

def risky(mode: int): string
  if mode == 1
    throw NetError { detail: "down" }
  elsif mode == 2
    throw ParseError { line: 7 }
  end
  "ok"
end

def handleNet(mode: int): string
  risky(mode) catch (e)
    NetError => "net down"
  end
end

def handleGuarded(mode: int): string
  risky(mode) catch (e)
    NetError if mode > 0 => "sometimes"
  end
end

def swallowAll(mode: int): string
  risky(mode) catch (e)
    _ => "swallowed"
  end
end

def wrapNet(mode: int): string
  risky(mode) catch (e)
    NetError => throw ConfigError { cause: "net" }
  end
end
"#
);

errorset_test!(
    panic_arms_are_not_error_arms,
    r#"
struct NetError
  detail: string
end

def risky(mode: int): int
  if mode == 1
    throw NetError { detail: "down" }
  end
  10 / mode
end

def guard(mode: int): int
  risky(mode) catch (e)
    panics.DivisionByZero => 0
  end
end
"#
);

errorset_test!(
    mutual_recursion_converges,
    r#"
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
  alpha(n - 1)
end
"#
);

errorset_test!(
    lambdas_and_indirect_calls,
    r#"
struct MapError
  index: int
end

def bump(x: int): int
  if x < 0
    throw MapError { index: x }
  end
  x + 1
end

def bumpAll(values: Vector<int>): Vector<int>
  values.map(|x| bump(x))
end

def applyTwice(f: (int) -> int, x: int): int
  f(f(x))
end
"#
);

errorset_test!(
    unknown_throw_opens_the_set,
    r#"
import std::proc

def readAny(): string
  let data = proc.run
  throw data
end
"#
);

errorset_test!(
    throws_contracts_and_catch_bang_satisfied,
    r#"
struct NetError
  detail: string
end

struct ParseError
  line: int
end

def fetch(ok: bool): string throws NetError
  if !ok
    throw NetError { detail: "timeout" }
  end
  "<html>"
end

def relay(ok: bool): string throws NetError
  fetch(ok)
end

def parse(flag: int): int throws ParseError
  if flag == 0
    throw ParseError { line: 3 }
  end
  flag
end

def handleBoth(ok: bool, flag: int): int throws never
  let page = fetch(ok) catch! (e)
    NetError => "net down"
  end
  parse(page.len() + flag) catch! (e)
    ParseError => -1
  end
end

def wildcardAll(ok: bool): string
  fetch(ok) catch! (e)
    _ => "recovered"
  end
end

def pure(n: int): int throws never
  n + 1
end
"#
);

errorset_test!(
    native_parse_error_tags_and_subtraction,
    r#"
struct Boom
  detail: string
end

def parse(s: string): int
  if s.len() == 0
    throw Boom { detail: "empty" }
  end
  s.toInt()
end

def lenient(s: string): int
  s.toInt() catch (e)
    string.ParseError => -1
  end
end

def strict(s: string): int
  parse(s) catch! (e)
    Boom => -1
    string.ParseError => -2
  end
end

def wildcarded(s: string): int
  parse(s) catch! (e)
    Boom => -1
    _ => -2
  end
end
"#
);

errorset_test!(
    native_regex_error_tags_and_subtraction,
    r#"
def hasWord(s: string, pattern: string): bool
  s.match?(pattern)
end

def lenient(s: string, pattern: string): bool
  s.match?(pattern) catch (e)
    string.RegexError => false
  end
end

def scrubbed(s: string): string
  s.replaceRe("[0-9]+", "*") catch! (e)
    string.RegexError => s
  end
end
"#
);

errorset_error_test!(
    e001_unreachable_native_arm,
    r#"
def calm(n: int): int
  n catch (e)
    string.ParseError => -1
  end
end
"#
);

errorset_error_test!(
    e002_catch_bang_missing_native_error,
    r#"
struct Boom
  detail: string
end

def parse(s: string): int
  if s.len() == 0
    throw Boom { detail: "empty" }
  end
  s.toInt()
end

def strict(s: string): int
  parse(s) catch! (e)
    Boom => -1
  end
end
"#
);

errorset_error_test!(
    e001_unreachable_arms,
    r#"
struct NetError
  detail: string
end

struct ParseError
  line: int
end

def risky(ok: bool): string
  if !ok
    throw NetError { detail: "down" }
  end
  "ok"
end

def deadArm(ok: bool): string
  risky(ok) catch (e)
    ParseError => "never thrown"
    NetError => "net"
  end
end

def deadArmExhaustive(ok: bool): string
  risky(ok) catch! (e)
    NetError => "net"
    ParseError => "never thrown"
  end
end

def deadWildcard(ok: bool): string
  risky(ok) catch! (e)
    NetError => "net"
    _ => "unreachable"
  end
end
"#
);

errorset_error_test!(
    e002_catch_bang_missing_tags,
    r#"
struct NetError
  detail: string
end

struct ParseError
  line: int
end

def risky(mode: int): string
  if mode == 1
    throw NetError { detail: "down" }
  elsif mode == 2
    throw ParseError { line: 7 }
  end
  "ok"
end

def missesOne(mode: int): string
  risky(mode) catch! (e)
    NetError => "net"
  end
end

def guardedDoesNotCount(mode: int): string
  risky(mode) catch! (e)
    NetError if mode > 0 => "sometimes"
    ParseError => "parse"
  end
end
"#
);

errorset_error_test!(
    e002_panic_arms_do_not_count,
    r#"
struct NetError
  detail: string
end

def risky(mode: int): int
  if mode == 1
    throw NetError { detail: "down" }
  end
  10 / mode
end

def edge(mode: int): int
  risky(mode) catch! (e)
    panics.DivisionByZero => 0
  end
end
"#
);

errorset_error_test!(
    e003_open_catch_bang,
    r#"
def callThrough(f: () -> int): int
  f() catch! (e)
    _ => -1
  end
end
"#
);

errorset_error_test!(
    e004_undeclared_throw,
    r#"
struct NetError
  detail: string
end

struct DnsError
  host: string
end

def fetch(mode: int): string throws NetError
  if mode == 1
    throw NetError { detail: "down" }
  elsif mode == 2
    throw DnsError { host: "example" }
  end
  "ok"
end

def relay(mode: int): string throws NetError
  fetch(mode)
end
"#
);

// --- BRS-25 pinned territory: throwing lambdas in HOFs -------------------

errorset_test!(
    nested_lambda_in_hof_flows_both_levels,
    r#"
struct DepthError
  code: int
end

def bump(x: int): int
  if x < 0
    throw DepthError { code: x }
  end
  x + 1
end

def bumpRows(rows: Vector<Vector<int>>): Vector<Vector<int>>
  rows.map(|row| row.map(|x| bump(x)))
end
"#
);

errorset_test!(
    hof_lambda_throws_and_calls_throwing_function,
    r#"
struct LambdaError
  code: int
end

struct NamedError
  code: int
end

def fail(x: int): int
  if x < 0
    throw NamedError { code: x }
  end
  x
end

def both(values: Vector<int>): Vector<int>
  values.map do |x|
    if x == 0
      throw LambdaError { code: x }
    end
    fail(x)
  end
end
"#
);

errorset_test!(
    hof_receiver_built_from_throwing_call,
    r#"
struct BuildError
  size: int
end

def build(n: int): Vector<int>
  if n < 0
    throw BuildError { size: n }
  end
  [n]
end

def bumpBuilt(n: int): Vector<int>
  build(n).map(|x| x + 1)
end
"#
);

// A lambda stored in a local and then passed to a HOF is a NON-literal
// fn-typed argument: the set opens and the lambda's own tag does not
// flow (collect.rs, `hof_args` — the BRS-25 precision gap, pinned).
errorset_test!(
    stored_lambda_hof_argument_opens_the_set,
    r#"
struct SkipError
  code: int
end

def bump(x: int): int
  if x < 0
    throw SkipError { code: x }
  end
  x + 1
end

def viaLocal(values: Vector<int>): Vector<int>
  let f: (int) -> int = |x| bump(x)
  values.map(f)
end
"#
);

errorset_test!(
    same_lambda_literal_reinvoked_in_loop,
    r#"
struct StepError
  code: int
end

def bump(x: int): int
  if x < 0
    throw StepError { code: x }
  end
  x + 1
end

def loopInvoke(n: int): int
  let mut total = 0
  while total < n
    total = total + (|x: int| bump(x))(total)
  end
  total
end
"#
);

// --- BRS-25 pinned territory: generic fn-typed params ---------------------

// `apply` invokes its parameter indirectly, so its own set is open; the
// CALLER stays open too even though its lambda literal argument is
// statically known — per-call-site inheritance is the BRS-25 gap
// documented at `Collector::args`.
errorset_test!(
    generic_apply_is_open_at_definition_and_call_site,
    r#"
struct AppError
  code: int
end

def boomIf(x: int): int
  if x == 0
    throw AppError { code: x }
  end
  x
end

def apply<T, R>(f: (T) -> R, x: T): R
  f(x)
end

def useApply(n: int): int
  apply(|x: int| boomIf(x), n)
end
"#
);

// --- BRS-25 pinned territory: mutual recursion beyond convergence ---------

// Subtraction inside the cycle: `beta` catches `alpha`'s AlphaError, so
// the fixpoint must converge with alpha = {Alpha, Beta} and
// beta = {Beta} — the caught tag never re-enters through the cycle.
errorset_test!(
    mutual_recursion_with_internal_catch_converges,
    r#"
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
    AlphaError => 0
  end
end
"#
);

errorset_test!(
    three_function_cycle_converges,
    r#"
struct AError
  code: int
end

struct BError
  code: int
end

struct CError
  code: int
end

def first(n: int): int
  if n == 0
    throw AError { code: 1 }
  end
  second(n - 1)
end

def second(n: int): int
  if n == 0
    throw BError { code: 2 }
  end
  third(n - 1)
end

def third(n: int): int
  if n == 0
    throw CError { code: 3 }
  end
  first(n - 1)
end
"#
);

errorset_test!(
    recursion_through_lambda_literal_converges,
    r#"
struct LoopError
  code: int
end

def spiral(n: int): int
  if n == 0
    throw LoopError { code: 0 }
  end
  (|m: int| spiral(m))(n - 1)
end
"#
);

// --- BRS-25 pinned territory: throws contracts over open sets -------------

// A `throws` TYPE LIST over a body opened by an indirect call cannot
// be verified: `f` may throw anything, so a caller writing
// `catch (e) NetError => ...` on the strength of the declaration would
// not handle what escapes. E004 in its unverifiable wording, the same
// answer E003 gives `catch!` and E005 gives `throws never`.
errorset_error_test!(
    declared_throws_over_open_indirect_body_is_unverifiable,
    r#"
struct NetError
  detail: string
end

def callThrough(f: () -> int): int throws NetError
  f()
end
"#
);

// An undeclared tag and an open set are independent findings on the
// same function: the first names what the body demonstrably throws, the
// second says the list cannot be proven complete. Both are reported.
errorset_error_test!(
    declared_throws_reports_undeclared_tag_and_openness_together,
    r#"
struct NetError
  detail: string
end

struct DiskError
  path: string
end

def callThrough(f: () -> int): int throws NetError
  if f() == 0
    throw DiskError { path: "/tmp" }
  end
  1
end
"#
);

errorset_test!(
    throws_declared_hof_lambda_is_covered,
    r#"
struct MapError
  index: int
end

def bump(x: int): int
  if x < 0
    throw MapError { index: x }
  end
  x + 1
end

def bumpAll(values: Vector<int>): Vector<int> throws MapError
  values.map(|x| bump(x))
end
"#
);

// --- BRS-25 pinned territory: catch edge cases ----------------------------

errorset_test!(
    catch_inside_lambda_body_filters_locally,
    r#"
struct MapError
  index: int
end

def bump(x: int): int
  if x < 0
    throw MapError { index: x }
  end
  x + 1
end

def safeBump(values: Vector<int>): Vector<int>
  values.map do |x|
    bump(x) catch (e)
      MapError => 0
    end
  end
end
"#
);

// The guard expression runs, so its own error contributions join the
// set, while the guarded arm still subtracts nothing from the subject.
errorset_test!(
    guard_calling_throwing_function_contributes,
    r#"
struct NetError
  detail: string
end

struct GuardError
  code: int
end

def risky(mode: int): string
  if mode == 1
    throw NetError { detail: "down" }
  end
  "ok"
end

def noisy(mode: int): bool
  if mode < 0
    throw GuardError { code: mode }
  end
  true
end

def guarded(mode: int): string
  risky(mode) catch (e)
    NetError if noisy(mode) => "maybe"
  end
end
"#
);

// The callee is clean but its ARGUMENT throws: the argument's tag flows
// into the catch subject, so the arm both subtracts it and is not E001.
errorset_test!(
    caught_argument_throw_flows_through_clean_callee,
    r#"
struct ArgError
  code: int
end

def clean(x: int): int
  x + 1
end

def mk(n: int): int
  if n == 0
    throw ArgError { code: n }
  end
  n
end

def handled(n: int): int
  clean(mk(n)) catch (e)
    ArgError => -1
  end
end
"#
);

// --- BRS-46: top-level pseudo-body analysis -------------------------------

// The top level has no `throws` contract: an uncaught top-level throw
// draws no diagnostic (it ends the script at runtime, exit 70), and a
// handled one subtracts normally.
errorset_test!(
    top_level_uncaught_throw_is_allowed,
    r#"
struct BootError
  code: int
end

def boot(ok: bool): int
  if !ok
    throw BootError { code: 1 }
  end
  0
end

let status = boot(false)
puts status

let safe = boot(false) catch (e)
  BootError => -1
end
puts safe
"#
);

errorset_error_test!(
    e001_top_level_unreachable_wildcard,
    r#"
struct NetError
  detail: string
end

def risky(ok: bool): string
  if !ok
    throw NetError { detail: "down" }
  end
  "ok"
end

let page = risky(false) catch! (e)
  NetError => "net"
  _ => "unreachable"
end
puts page
"#
);

errorset_error_test!(
    e002_top_level_catch_bang_missing_tag,
    r#"
struct NetError
  detail: string
end

struct ParseError
  line: int
end

def risky(mode: int): string
  if mode == 1
    throw NetError { detail: "down" }
  elsif mode == 2
    throw ParseError { line: 7 }
  end
  "ok"
end

let page = risky(1) catch! (e)
  NetError => "net"
end
puts page
"#
);

errorset_error_test!(
    e005_throws_never_with_throwing_hof_lambda,
    r#"
struct MapError
  index: int
end

def bump(x: int): int
  if x < 0
    throw MapError { index: x }
  end
  x + 1
end

def sneaky(values: Vector<int>): Vector<int> throws never
  values.map(|x| bump(x))
end
"#
);

errorset_error_test!(
    e005_throws_never,
    r#"
struct BoomError
  code: int
end

def boom(flag: bool): int throws never
  if flag
    throw BoomError { code: 1 }
  end
  0
end

def runThrough(f: () -> int): int throws never
  f()
end
"#
);

errorset_test!(
    proc_runners_tag_the_set,
    r#"
import std::proc

def runIt(): string
  proc.run(["true"]).stdout
end

def shellIt(): string
  proc.shell("true").stdout
end

def tried(): int
  proc.tryRun(["true"]).code
end

def caught(): string
  proc.run(["true"]).stdout catch (e)
    proc.NonZeroExit => e
  end
end

def fullyCaught(): string
  proc.shell("true").stdout catch (e)
    proc.NonZeroExit => e
    proc.SpawnError => e
  end
end
"#
);

errorset_error_test!(
    e001_unreachable_proc_arm,
    r#"
import std::proc

def calm(): int
  proc.tryRun(["true"]).code catch (e)
    proc.NonZeroExit => -1
  end
end
"#
);

errorset_test!(
    fs_members_and_env_cd_tag_the_set,
    r#"
import std::fs
import std::env

def load(path: string): string
  fs.read(path)
end

def loaded(path: string): string
  fs.read(path) catch (e)
    fs.NotFound => "missing"
    fs.Denied => "denied"
    fs.IoError => e
  end
end

def rebuilt(path: string): string
  fs.join(fs.dir(path), fs.base(path)) + fs.ext(path)
end

def checks(path: string): bool
  fs.exists?(path)
end

def resolved(path: string): string
  fs.abs(path)
end

def hop(path: string): string
  env.cd(path)
  env.cwd()
end
"#
);

errorset_error_test!(
    e001_unreachable_fs_arm,
    r#"
import std::fs

def stem(path: string): string
  fs.base(path) catch (e)
    fs.NotFound => "unused"
  end
end
"#
);

errorset_test!(
    json_parse_tags_and_subtraction,
    r#"
import std::json
import std::io

def decode(text: string): Json
  json.parse(text)
end

def decoded(text: string): string
  json.stringify(json.parse(text)) catch (e)
    json.ParseError => e
  end
end

def echo(): string
  let line = io.readLine() ?? ""
  io.eprint(line)
  line + io.readAll()
end
"#
);

errorset_error_test!(
    e001_unreachable_json_arm,
    r#"
import std::json

def frozen(data: Json): string
  json.stringify(data) catch (e)
    json.ParseError => "unused"
  end
end
"#
);

errorset_test!(
    new_hof_methods_stay_transparent,
    r#"
struct FoldError
  index: int
end

def bump(x: int): int
  if x < 0
    throw FoldError { index: x }
  end
  x + 1
end

def total(values: Vector<int>): int
  values.reduce(0, |acc, x| acc + bump(x))
end

def firstBig(values: Vector<int>): int
  values.find(|x| bump(x) > 10) ?? 0
end

def anyBig(values: Vector<int>): bool
  values.any?(|x| bump(x) > 10)
end

def eachEntry(counts: Map<string, int>): int
  let mut seen = 0
  counts.each(|k, v| bump(v))
  seen
end
"#
);
