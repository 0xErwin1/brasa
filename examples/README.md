# Brasa examples

Target programs for the language as specified in `docs/spec/`. Nothing
runs yet (the interpreter lands in M1); these files serve three purposes:

1. A human-readable tour of the language.
2. Parser food: they should parse cleanly as soon as BRS-10/11 land.
3. The seed of the golden test suite (`tests/programs/`) once the
   tree-walker exists — each file will get an expected-output twin.

| File | Exercises |
|------|-----------|
| `hello.brs` | puts, interpolation, shebang |
| `fib.brs` | functions, recursion, inline `then` if, while |
| `fizzbuzz.brs` | elsif chains, ranges, `for` |
| `shapes.brs` | structs, methods, enums, match, Option |
| `errors.brs` | throw, catch, catch!, panics, throws clause |
| `pipeline.brs` | vectors, maps, lambdas, pipe operator |
| `strings.brs` | string methods, raw strings, chars |
| `modules/` | file imports, `pub`, `def main()` entry point |
| `stars.brs` | stdlib preview: fs, json, proc (needs M4) |

## `real/`

Full scripts ported from the bash and Python they replace, written to
measure the M4 stdlib against real work rather than to tour a feature.

| File | Replaces | Exercises |
|------|----------|-----------|
| `real/logstat.brs` | an awk/Python access-log summary | regex captures, `Map` aggregation, `sortBy`, `std::io`, `std::fs` |
| `real/gitreport.brs` | a bash release-readiness script | `std::proc` (`run`/`tryRun`), `proc.NonZeroExit`, `proc.SpawnError`, `std::env` |
| `real/lockaudit.brs` | a Python `flake.lock` auditor | `std::fs` walking and path helpers, `std::json`, `std::time` |

Run them as:

```sh
brasa examples/real/logstat.brs examples/real/data/access.log
brasa examples/real/gitreport.brs v0.1.0
brasa examples/real/lockaudit.brs .
```

`logstat.brs` and `lockaudit.brs` are pinned byte-for-byte in
`crates/brasa/tests/programs.rs`. `gitreport.brs` reads the live
repository, so only its shape is pinned there.
