# Brasa

A statically-typed scripting language with Ruby-flavored syntax, designed to
replace Python and bash for ~90% of everyday scripting: text manipulation,
running commands, files, JSON, automation. Implemented in Rust with a
bytecode VM and GC.

> **Status:** Brasa runs scripts on its bytecode VM, checks them statically,
> formats source, runs `test` items, and bundles multi-file programs. The
> language and tooling are still evolving toward v1; see the roadmap below.

```ruby
struct Repo
  name: string
  stars: int
end

def topNames(repos: Vector<Repo>, min: int): Vector<string>
  repos
    .filter(|r| r.stars >= min)
    .sortBy(|r| -r.stars)
    .map(|r| r.name)
end

let repos = [
  Repo { name: "brasa", stars: 1 },
  Repo { name: "ignis", stars: 120 },
  Repo { name: "dbflux", stars: 48 },
]

for name in repos |> topNames(10)
  puts name
end
```

## Why another language

Scripting today means choosing between bash (no types, no data structures)
and Python (slow startup, `venv` ceremony, runtime type errors). Brasa aims
at the gap:

- **Static typing without ceremony** — strong types with local inference;
  annotations only on function signatures.
- **Errors without virality** — `throw` any value, error sets are
  *inferred* into signatures, `catch` is a non-exhaustive match. No
  `Result<T, E>` plumbing, no `unwrap`, no forgotten exceptions.
- **The stdlib is the product** — first-class strings, regex, processes
  (`proc.run` with `set -e` semantics by default), fs, glob, JSON.
- **Instant startup** — single binary, shebang support, no project files
  required for a single script.

## Language at a glance

| Feature | Shape |
|---------|-------|
| Types | `int`, `float`, `bool`, `string`, `char`, `unit`, `Range`; no `nil` — `Option<T>` with `?.` / `??` |
| Variables | `let` immutable binding, `let mut` reassignable; types inferred |
| Functions | `def f(a: int): string ... end`, implicit return, lambdas `\|x\| x * 2` |
| Data | `struct` (no inheritance), `enum` with payloads, exhaustive `match` |
| Generics | `def max<T: Comparable>(...)` — structural interface constraints |
| Errors | `expr catch (e) ... end`, inferred error sets, optional verified `throws` |
| Modules | one file = one module, explicit `pub`; `import "util.bras"` relative, `import lib::helpers` on a search path, `import std::fs` for the stdlib |
| Naming | `camelCase`, predicates end in `?` (`file.exists?`) |

The full specification lives in [`docs/spec/`](docs/spec/).

## Architecture

```
files ─→ lexer ─→ parser ─→ HIR ─→ resolver ─→ type check ─→ error sets ─→ codegen ─→ VM
 graph   logos    Pratt+RD  desugar            inference     fixpoint      bytecode
```

Every reachable file lowers into one HIR arena, so a multi-file program
is one compilation with globally unique node IDs, not several linked
together.

One crate per compiler phase under [`crates/`](crates/). Index arenas with
typed IDs for the AST, side tables per phase, no MIR — HIR lowers directly
to bytecode. See [`AGENTS.md`](AGENTS.md) for the crate map and invariants.

## Quick start

Requires [Nix](https://nixos.org) (with flakes) and optionally
[direnv](https://direnv.net):

```sh
direnv allow            # or: nix develop --impure
cargo build
target/debug/brasa examples/hello.bras
target/debug/brasa --check examples/pipeline.bras
target/debug/brasa fmt --check examples
```

Run a script as `brasa script.bras [args...]`. The CLI also provides
`brasa test script.bras`, `brasa fmt [paths...]`, and
`brasa bundle script.bras -o tool`; use `brasa --help` for diagnostic
dump options.

## Roadmap

| Milestone | Scope | Status |
|-----------|-------|--------|
| M0 | Lexer, parser, AST, diagnostics | delivered |
| M1 | Type checker + provisional tree walker | delivered; walker retired in M6 |
| M2 | Error system (inferred error sets, `catch`) | delivered |
| M3 | Bytecode VM + GC | delivered |
| M4 | Scripting stdlib | delivered core surface |
| M5 | Formatter, editor support, LSP and debugging tools | partial: formatter and editor grammar delivered |
| M6 | Multi-file programs, tests, bundling, CLI/HTTP stdlib, performance and hardening | in progress; major runtime and tooling units delivered |

## License

[GPL-3.0](LICENSE)
