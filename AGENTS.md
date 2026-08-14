# Brasa — Agent Guide

Brasa is a statically-typed scripting language (Ruby-flavored syntax,
bytecode VM, GC) implemented in Rust. Goal: replace Python/bash for ~90% of
scripting tasks.

## Source of truth

`docs/spec/` is normative. Read it before changing anything user-visible:

| Doc | Contents |
|-----|----------|
| `docs/spec/00-vision.md` | Goals, closed decisions, compiler architecture, roadmap |
| `docs/spec/01-syntax.md` | User-facing syntax |
| `docs/spec/02-grammar.md` | Lexical grammar + EBNF + ambiguity resolutions |
| `docs/spec/03-types.md` | Type system semantics |
| `docs/spec/04-errors.md` | Error system (thrown values, inferred error-sets) |
| `docs/spec/05-stdlib.md` | Stdlib surface (native, written in Rust) |
| `docs/spec/06-diagnostics.md` | Diagnostic model, code registry, wording |
| `docs/spec/07-bytecode.md` | Bytecode: execution model, values, instruction set, module format |

Spec changes must be mirrored to the Atlas workspace `brasa` (project
`brasa-lang`, folder Spec). Work items live on the Atlas board `Roadmap`
as epics BRS-1..BRS-6 with subtasks; reference them in commits.

## Environment

Nix + direnv + devenv. Never install toolchains imperatively.

```sh
direnv allow          # or: nix develop --impure
cargo check           # must always pass
cargo test
cargo insta review    # snapshot tests
```

`.bras` sources are formatted by the language's own formatter, and the
bundled examples are its corpus — `crates/brasa_fmt/tests/examples.rs`
fails if any of them is left unformatted:

```sh
cargo run -- fmt examples          # rewrite in place
cargo run -- fmt --check examples  # report, exit 1 if any would change
```

Linking uses mold via `.cargo/config.toml` (clang + `-fuse-ld=mold`).

## Workspace layout

One crate per responsibility under `crates/` (pattern borrowed from Ignis):

| Crate | Responsibility |
|-------|----------------|
| `brasa_arena` | Typed index arena (`Id<T>`, `Store<T>`) used by every tree the compiler builds |
| `brasa_source` | Spans (file-qualified byte ranges), source file table, byte offset -> line/col |
| `brasa_diagnostics` | Diagnostic builder (severity, labels, notes); pretty terminal rendering via `ariadne` |
| `brasa_token` | Token types (no lexing logic) |
| `brasa_lexer` | logos lexer: text → tokens (newlines are tokens; string-interpolation sub-mode) |
| `brasa_ast` | Index arenas, typed `Copy` IDs, span side tables |
| `brasa_parser` | Recursive descent (items/stmts) + Pratt (exprs) |
| `brasa_hir` | Desugared core: every sugar form is gone by this point |
| `brasa_module` | The module graph: follows imports, loads every reachable file into ONE `Hir` |
| `brasa_resolver` | Scopes, imports, visibility; the tables every later phase reads |
| `brasa_typeck` | Inference and checking; `Type::Unknown` poisons instead of aborting |
| `brasa_errorset` | Error-set inference (fixpoint over the call graph) and `throws` verification |
| `brasa_stdlib` | Stdlib DECLARATION tables (one per module): surface names, signatures, return rules, and the errors a module member raises. Declares only — no implementation, no dependencies |
| `brasa_bytecode` | Bytecode containers: opcodes, chunks, constant pool, module format, disassembler |
| `brasa_codegen` | HIR → bytecode |
| `brasa_runtime` | Execution glue the backend does not own: stdlib's contact with the OS, ordered collections, `Outcome` |
| `brasa_vm` | The dispatch loop, the heap and the collector |
| `brasa_fmt` | Formatter: prints the AST, recovers leaf spelling and comments from the source |
| `brasa` | CLI binary (clap): runs a script, `brasa fmt`, or `brasa test` |

There is no separate interpreter crate. The stdlib is native builtins:
`brasa_bytecode::builtin` mints the ids, `brasa_typeck::builtins`
resolves signatures and `brasa_vm::builtins` implements them. BRS-96 is
collapsing the surface those three used to declare separately into one
table per module in `brasa_stdlib`; `Vector` (a receiver type, via
`method_table!`) and the free modules `std::fs`, `std::json`, `std::io`,
`std::env` and `std::proc` (via `module_table!`, which also carries each
member's error contribution) are converted; `math`, `time` and `rand`
still declare their signature, their errors and their implementation by
hand. Converted free modules are listed in
`brasa_stdlib::FREE_MODULES`, which is what the checker's lookup, the
bytecode registry's cross-check and the table guards walk, so
converting the next one does not mean editing them. The two shapes are
deliberately separate: a
free module has no receiver element type and no argument-dependent
result, and a receiver method has neither optional trailing parameters
nor an error list. A parameter may be a rule rather than a type
(`ParamDesc::Command`, the argv-or-split-string a `proc` runner takes);
a result never is, so `TyDesc` always lowers to exactly one type. The
stdlib records (`Output`, `Response`, `Args`, `Walk`) are the third
shape, `record_table!`: a concrete receiver with no element type, no
optional parameters and no error list, whose members are either a
field or a method. They are listed in `brasa_stdlib::RECORDS`; the
checker owns the map from its own `Type` to a record's table, since
`brasa_stdlib` does not know what a `Type` is. Ids stay hand-minted in `brasa_bytecode` and are
frozen by `crates/brasa_bytecode/tests/builtin_ids.rs`.
`brasa_interp`, the M1 tree-walker, was deleted in BRS-108; the
behaviour oracle it used to be is now the conformance corpus at
`crates/brasa_vm/tests/conformance.rs`.

## Architecture invariants

- AST/HIR nodes are immutable after construction; phases communicate
  through side tables keyed by node ID. Never add mutable state to nodes.
- No MIR: HIR lowers directly to bytecode. Do not introduce an extra IR.
- All sugar (`|>`, `?.`, `??`, `for`, interpolation, `+=`) desugars in the
  AST→HIR lowering, exactly once. Later phases handle core HIR only.
- The stdlib is native Rust builtins. No `.bras` files on the startup path.
- Diagnostics: phases return structured errors; only the CLI renders them.

## Language conventions (Brasa code, `.bras` files and examples)

- `camelCase` for functions/methods/variables, `PascalCase` for types,
  `?` suffix for predicates. Keywords lowercase (`catch!` is a keyword).
- No `nil`, no implicit coercions, no inheritance.

## Rust conventions

- `cargo check` clean, idiomatic Rust, edition 2024.
- English-only code, comments, and commit messages. Conventional Commits.
- Tests: unit tests next to code, snapshot tests with `insta` in the
  parser, `.bras` golden programs (from M1) under `tests/programs/`.
