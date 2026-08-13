# Brasa examples

Programs written to the language as specified in `docs/spec/`. They are
a human-readable tour and test material: every file here is parsed,
formatted, statically checked, and pinned to an execution expectation.

- `crates/brasa_parser/tests/examples.rs` parses each one with zero
  diagnostics and snapshots the tree, so a grammar change that silently
  reshapes it is caught.
- `crates/brasa_fmt/tests/examples.rs` requires every example to remain
  formatter-stable.
- `crates/brasa/tests/programs.rs` runs each one on the bytecode VM
  against a pinned expectation.
- `crates/brasa_vm/tests/conformance.rs` runs its language programs at
  both the default and a tiny heap budget, pinning VM behavior under
  hot-GC pressure; `crates/brasa_vm/tests/gc.rs` adds focused collector
  stress cases.

The parser and behavior suites carry guards that walk this directory and
fail if a `.bras` is not exercised. The behavior guard reads the
exercised set out of its own source, so the name has to appear in code.
The parser guard compares the walk against a list generated from the same
declarations that emit the tests, so a name cannot be added without a
test coming with it. That guard exists because `stars.bras` spent a
whole milestone here without compiling: it previewed a stdlib API before
the real one landed, three separate work units reported it
independently, and CI never noticed because nothing ran it (BRS-63). An
unpinned example rots silently and is read as working code while it
does.

| File | Exercises |
|------|-----------|
| `hello.bras` | puts, interpolation, shebang |
| `fib.bras` | functions, recursion, inline `then` if, while |
| `fizzbuzz.bras` | elsif chains, ranges, `for` |
| `shapes.bras` | structs, methods, enums, match, Option |
| `errors.bras` | throw, catch, catch!, panics, throws clause |
| `pipeline.bras` | vectors, maps, lambdas, pipe operator |
| `strings.bras` | string methods, raw strings, chars |
| `modules/main.bras` | relative file import, exported calls, top-level initialization, `def main()` entry point |
| `modules/utils.bras` | exported and private module members; declaration-only modules run silently |
| `stars.bras` | `std::fs`, `std::json` total indexing, `std::proc` with stdin, `std::env` args, uncaught errors |

Run `stars.bras` against its committed fixture:

```sh
brasa examples/stars.bras examples/data/repos.json
```

It catches nothing: a missing file, unreadable JSON or a missing `wc`
each stop the run with their own type and message, which is what an
uncaught error already does. Only the JSON *fields* are total, and that
is the point — indexing yields `Option`, so each field ends in a `??`
with its fallback in plain sight. The fixture is built so each of those
fallbacks is actually taken: one repo has no `name`, one has a `stars`
that is not a number, one omits `archived` entirely, and one element is
not an object at all. The container
lookup has the same two failure modes, one fixture each:
`data/no-repos.json` has no `repos` key, and `data/repos-scalar.json`
has one that is not an array.

Run the multi-file example from its entry module:

```sh
brasa examples/modules/main.bras
```

`modules/utils.bras` is also checked and runnable on its own; it only
declares members, so it produces no output.

## `real/`

Full scripts ported from the bash and Python they replace, written to
measure the M4 stdlib against real work rather than to tour a feature.

| File | Replaces | Exercises |
|------|----------|-----------|
| `real/logstat.bras` | an awk/Python access-log summary | regex captures, `Map` aggregation, `sortBy`, `std::io`, `std::fs` |
| `real/gitreport.bras` | a bash release-readiness script | `std::proc` (`run`/`tryRun`), `proc.NonZeroExit`, `proc.SpawnError`, `std::env` |
| `real/lockaudit.bras` | a Python `flake.lock` auditor | `std::fs` walking and path helpers, `std::json`, `std::time` |
| `real/tally.bras` | helper module imported by the report scripts | exported struct and shared `Map` counting/ranking helpers |

Run them as:

```sh
brasa examples/real/logstat.bras examples/real/data/access.log
brasa examples/real/gitreport.bras v0.1.0
brasa examples/real/lockaudit.bras .
brasa examples/real/lockaudit.bras examples/real/data/lockfixture
```

`tally.bras` is a declaration-only helper and is pinned to load and run
silently on its own. `logstat.bras` and `lockaudit.bras` are pinned
byte-for-byte in
`crates/brasa/tests/programs.rs`, each against a committed fixture under
`real/data/` so the expectation never moves with unrelated repository
changes. `gitreport.bras` reads the live repository, so only its shape is
pinned there — the same reason `stars.bras` reads a fixture under
`data/` rather than whatever `repos.json` happens to be nearby.
