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
import std::fs

def readAny(path: string): string
  let data = fs.read(path)
  throw data
end
"#
);

errorset_test!(
    throws_contracts_and_catch_all_satisfied,
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
  let page = fetch(ok) catch_all (e)
    NetError => "net down"
  end
  parse(page.len() + flag) catch_all (e)
    ParseError => -1
  end
end

def wildcardAll(ok: bool): string
  fetch(ok) catch_all (e)
    _ => "recovered"
  end
end

def pure(n: int): int throws never
  n + 1
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
  risky(ok) catch_all (e)
    NetError => "net"
    ParseError => "never thrown"
  end
end

def deadWildcard(ok: bool): string
  risky(ok) catch_all (e)
    NetError => "net"
    _ => "unreachable"
  end
end
"#
);

errorset_error_test!(
    e002_catch_all_missing_tags,
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
  risky(mode) catch_all (e)
    NetError => "net"
  end
end

def guardedDoesNotCount(mode: int): string
  risky(mode) catch_all (e)
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
  risky(mode) catch_all (e)
    panics.DivisionByZero => 0
  end
end
"#
);

errorset_error_test!(
    e003_open_catch_all,
    r#"
def callThrough(f: () -> int): int
  f() catch_all (e)
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
