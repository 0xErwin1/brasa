//! Snapshot tests for error-set inference. Inputs are parsed, lowered,
//! resolved, and checked (all with zero diagnostics required), then
//! inferred; each test snapshots the span-free error-set dump. The
//! inference itself emits no diagnostics in this unit (the consuming
//! checks are BRS-23), so every test also asserts the channel is empty.

use std::path::PathBuf;

use brasa_source::SourceMap;

fn infer_source(
    name: &str,
    source: &str,
) -> (brasa_hir::LowerResult, brasa_errorset::ErrorSetResult) {
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
    assert!(
        inferred.diagnostics.is_empty(),
        "{name} expected zero error-set diagnostics (checks are BRS-23), got: {:#?}",
        inferred.diagnostics
    );

    (lowered, inferred)
}

macro_rules! errorset_test {
    ($test_name:ident, $source:expr) => {
        #[test]
        fn $test_name() {
            let (lowered, inferred) = infer_source(stringify!($test_name), $source);
            let dump = brasa_errorset::dump::dump(&lowered.hir, &inferred);
            insta::assert_snapshot!(stringify!($test_name), dump);
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
