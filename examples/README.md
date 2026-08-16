# Brasa examples

Programs written to the language as specified in the spec docs. They are
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
| `real/aiusage/` | a Go multi-provider usage command | a `brasa.toml` project over eight modules with its own `brasa test` suite, `fs.tryWalk` and `fs.stat`, `std::json` at scale, `time.parseIso`, a lambda parameter, `Set` dedup |

Run them as:

```sh
brasa examples/real/logstat.bras examples/real/data/access.log
brasa examples/real/gitreport.bras v0.1.0
brasa examples/real/lockaudit.bras .
brasa examples/real/lockaudit.bras examples/real/data/lockfixture
brasa examples/real/aiusage/src/main.bras \
  --responses examples/real/aiusage/data/providers examples/real/aiusage/data
```

### `real/aiusage/` — the one that is a project

The others are scripts. This one is a `brasa.toml` project, and it is
the only example of one:

```sh
cd examples/real/aiusage
brasa                     # no arguments: `build.entry` says what to run
```

The manifest is discovered by walking up from the working directory, so
the bare `brasa` above only works from inside the project — which is why
the script defaults its corpus to `.`. A positional argument would be
read as the script to run, so an entry that required one could never be
reached through the manifest at all.

Four services report usage in four different shapes — a float
percentage under a named key, an epoch second and a window width in
seconds, a percentage two levels down with the period named by an enum
— and a status bar can only draw one of them. Flattening that is what
the program is for, and it is why the modules fall where they do:

| Module | Moves when |
|--------|-----------|
| `src/provider.bras` | the normalized shape changes — the one thing every reader agrees on |
| `src/claude.bras`, `src/codex.bras`, `src/grok.bras`, `src/kimi.bras` | *that* service changes its response |
| `src/corpus.bras` | the on-disk transcript shape changes |
| `src/pricing.bras` | a provider reprices |
| `src/main.bras` | the report changes |

Kimi is the one worth reading. Moonshot exposes no consumption endpoint
at all — only the live balance — so spend is not read but *derived*
from how the balance moved since the last poll: a drop is consumption
billed across every client sharing the key, a rise is a recharge or a
voucher grant, and movement under half a cent is float noise rather
than either. That makes it the only provider whose answer depends on
the previous run, which is why it has a ledger and the other three do
not. It reads the ledger and does not write it: the real command writes
it back every poll, but an example that rewrote its own fixture would
report something different the second time and could not be pinned.

A provider that cannot answer still gets a line, with the reason:
silence and genuine zero usage look identical otherwise. All four have
fixtures, so that path is held by its own pinned run against a
directory that does not exist — with a fixture each and no such run, it
would ship untested.

### Tests

The project's own tests are written in Brasa, in `test` items beside
the code they cover, and `[test].globs` in the manifest is what finds
them:

```sh
cd examples/real/aiusage
brasa test
```

They cover what a pinned report cannot reach: the ledger derivation
only says anything when the balance *moves*, and a fixture cannot move.
The Rust pins in `crates/brasa/tests/programs.rs` stay because this
repository's guard requires every example to be exercised there — that
guard exists because `stars.bras` once spent a whole milestone here
without compiling. One of those pins runs `brasa test` and checks the
count, so the Brasa tests cannot quietly stop running either.

The Go command it replaces is a single file, which is what one concern
in one language usually earns; the split is the claim being
demonstrated, not an improvement asserted.

The provider responses are canned under `data/providers/`. The real
tool fetches them over HTTP with the credentials each vendor's CLI
maintains — an example may do neither, so `--responses` names a
directory of saved replies instead. Parsing them is the part worth
reading anyway; the fetch is four lines.

It differs from the script it was extracted from in exactly two places,
both so its output is identical on every machine: the window is anchored
to the newest turn in the corpus rather than to the clock, and days are
cut in UTC rather than in the reader's zone. The original uses
`fs.stat().modifiedMillis` to skip files that cannot be in the window
and `time.localOffsetMillis` to bucket by the user's day — a filter
deliberately dropped here, because a checkout older than the cutoff
would start excluding fixtures and the pin would then fail for a reason
that has nothing to do with the code.

`tally.bras` is a declaration-only helper and is pinned to load and run
silently on its own, as are `aiusage`'s six. `logstat.bras`,
`lockaudit.bras` and `aiusage` are pinned byte-for-byte in
`crates/brasa/tests/programs.rs`, each against a committed fixture under
`real/data/` so the expectation never moves with unrelated repository
changes. `gitreport.bras` reads the live repository, so only its shape is
pinned there — the same reason `stars.bras` reads a fixture under
`data/` rather than whatever `repos.json` happens to be nearby.
