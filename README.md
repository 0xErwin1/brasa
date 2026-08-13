# Brasa

A statically-typed scripting language with Ruby-flavored syntax, designed to
replace Python and bash for ~90% of everyday scripting: text manipulation,
running commands, files, JSON, automation. Implemented in Rust with a
bytecode VM and GC.

> **Status: early development.** The language specification is complete;
> the compiler frontend is being built (lexer done, parser in progress).
> Nothing is runnable yet.

```ruby
import std::fs
import std::json

struct Repo
  name: string
  stars: int
end

def topRepos(path: string, min: int): Vector<Repo>
  let data = json.parse(fs.read(path))
  data.repos
    .filter(|r| r.stars >= min)
    .sortBy(|r| -r.stars)
end

let repos = topRepos("repos.json", 100) catch (e)
  fs.NotFound => []
end

for repo in repos
  puts "#{repo.name}: #{repo.stars}"
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

## Building

Requires [Nix](https://nixos.org) (with flakes) and optionally
[direnv](https://direnv.net):

```sh
direnv allow            # or: nix develop --impure
cargo build
cargo test
```

## Roadmap

| Milestone | Scope | Status |
|-----------|-------|--------|
| M0 | Lexer, parser, AST, diagnostics | in progress |
| M1 | Type checker + reference tree-walking interpreter (retired in M6) | — |
| M2 | Error system (inferred error sets, `catch`) | — |
| M3 | Bytecode VM + GC | — |
| M4 | Scripting stdlib (strings, proc, fs, json, regex) | — |
| M5 | REPL, formatter, LSP | — |

## License

[GPL-3.0](LICENSE)
