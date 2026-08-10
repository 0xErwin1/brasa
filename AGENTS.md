# Brasa — Agent Guide

Brasa is a statically-typed scripting language (Ruby-flavored syntax,
bytecode VM, GC) implemented in Rust. Goal: replace Python/bash for ~90% of
scripting tasks.

## Source of truth

`docs/spec/` is normative. Read it before changing anything user-visible:

| Doc | Contents |
|-----|----------|
| `docs/spec/00-vision.md` | Goals, closed decisions, compiler architecture, roadmap |
| `docs/spec/01-sintaxis.md` | User-facing syntax |
| `docs/spec/02-gramatica.md` | Lexical grammar + EBNF + ambiguity resolutions |
| `docs/spec/03-tipos.md` | Type system semantics |
| `docs/spec/04-errores.md` | BAML-style error system (inferred error-sets) |
| `docs/spec/05-stdlib.md` | Stdlib surface (native, written in Rust) |

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

Linking uses mold via `.cargo/config.toml` (clang + `-fuse-ld=mold`).

## Workspace layout

One crate per responsibility under `crates/` (pattern borrowed from Ignis):

| Crate | Responsibility |
|-------|----------------|
| `brasa_arena` | Typed index arena (`Id<T>`, `Store<T>`) used by every tree the compiler builds |
| `brasa_source` | Spans (file-qualified byte ranges), source file table, byte offset -> line/col |
| `brasa_diagnostics` | Diagnostic builder (severity, labels, notes); rendering (ariadne) lands in BRS-12 |
| `brasa_token` | Token types (no lexing logic) |
| `brasa_lexer` | logos lexer: text → tokens (newlines are tokens; string-interpolation sub-mode) |
| `brasa_ast` | Index arenas, typed `Copy` IDs, span side tables |
| `brasa_parser` | Recursive descent (items/stmts) + Pratt (exprs) |
| `brasa` | CLI binary (clap) |

Planned: `brasa_hir` (desugared core), `brasa_resolver`, `brasa_typeck`,
`brasa_errorset` (fixpoint inference), `brasa_interp` (reference
tree-walker), `brasa_vm` + `brasa_codegen` (M3), `brasa_stdlib` (native).

## Architecture invariants

- AST/HIR nodes are immutable after construction; phases communicate
  through side tables keyed by node ID. Never add mutable state to nodes.
- No MIR: HIR lowers directly to bytecode. Do not introduce an extra IR.
- All sugar (`|>`, `?.`, `??`, `for`, interpolation, `+=`) desugars in the
  AST→HIR lowering, exactly once. Later phases handle core HIR only.
- The stdlib is native Rust builtins. No `.brs` files on the startup path.
- Diagnostics: phases return structured errors; only the CLI renders them.

## Language conventions (Brasa code, `.brs` files and examples)

- `camelCase` for functions/methods/variables, `PascalCase` for types,
  `?` suffix for predicates. Keywords lowercase (`catch_all` is a keyword).
- No `nil`, no implicit coercions, no inheritance.

## Rust conventions

- `cargo check` clean, idiomatic Rust, edition 2024.
- English-only code, comments, and commit messages. Conventional Commits.
- Tests: unit tests next to code, snapshot tests with `insta` in the
  parser, `.brs` golden programs (from M1) under `tests/programs/`.
