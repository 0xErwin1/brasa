# Brasa — Agent Guide

Brasa is a statically-typed scripting language (Ruby-flavored syntax,
bytecode VM, GC) implemented in Rust. Goal: replace Python/bash for ~90% of
scripting tasks.

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
| `brasa_lsp` | Language server (BRS-92): diagnostics + hover with inferred types and error-sets |
| `brasa` | CLI binary (clap): runs a script, `brasa fmt`, `brasa test`, or `brasa lsp` |

There is no separate interpreter crate. The stdlib is native builtins:
`brasa_bytecode::builtin` mints the ids, `brasa_typeck::builtins`
resolves signatures and `brasa_vm::builtins` implements them. BRS-96
collapsed the surface those three used to declare separately into one
table per module in `brasa_stdlib`, and it is **complete**. Every free
module (`fs`, `json`, `io`, `env`, `proc`, `http`, `cli`, `math`,
`time`, `rand`), every receiver (`string`, `int`, `float`, `Vector`,
`Map`, `Set`, `Json`) and all four records are declared there,
signatures and error contributions alike. **No stdlib surface is
hand-written in `brasa_typeck` or `brasa_errorset` any more** — if you
are adding a stdlib member, you are editing a table in `brasa_stdlib`.

There are three table shapes, deliberately separate because almost
nothing about their columns overlaps:

| Macro | For | Registry |
|-------|-----|----------|
| `method_table!` | a receiver: `string`/`int`/`float`/`Json` (`Plain`), `Vector<T>`/`Set<T>` (`Elem`), `Map<K,V>` (`KeyValue`) | `brasa_stdlib::RECEIVERS` |
| `module_table!` | a free module, with optional trailing parameters | `brasa_stdlib::FREE_MODULES` |
| `record_table!` | a record (`Output`, `Response`, `Args`, `Walk`): a concrete receiver, no type arguments | `brasa_stdlib::RECORDS` |

A receiver's `RecvShape` says which receiver-derived type names its
rows may use — `elem`, or `key`/`value`, or none. Naming one the
receiver lacks is a declaration bug a guard rejects, rather than a
panic inside the checker the first time a user calls that member.
Both `method_table!` and `module_table!` carry a `throws` column;
receiver methods do throw (`string.toInt`, the regex four).

The layers that cover a whole surface at once — the checker's lookup,
the bytecode registry's cross-check, the table guards — walk those
registries rather than naming modules, so adding one means joining a
list. A name shared across receiver kinds (`len`, `remove`, `get`)
holds ONE builtin id and is declared once per receiver that carries it,
with that receiver's signature: `remove` answers a bool on a `Set` and
the removed value on a `Map`.

Three things are deliberately NOT data. A parameter may be a rule
(`ParamDesc::Command`, the argv-or-split-string a `proc` runner takes);
a result never is, so `TyDesc` always lowers to exactly one type. A
member may be read rather than called (`ModuleKind::Constant` for
`math.pi`, `RecordKind::Field` for `output.stdout`) — the surface's own
distinction. And `ModuleKind::Custom`/`RetDesc::Custom` hand a
signature to the checker, which is correct only when it is not
expressible as data at all (`math.abs` answers in the kind it was
given; `rand.choice` is generic over an element a free module cannot
name). A delegated module member must state its reason in the table,
and guards pin both delegated sets so they cannot grow quietly.

The checker owns the map from its own `Type` to a receiver's or
record's table, since `brasa_stdlib` has no dependencies and does not
know what a `Type` is. That map is also where `Option<Json>` flattens
onto the `Json` table: which table a receiver selects is a question
about types, not about rows. Ids stay hand-minted in `brasa_bytecode` and are
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
- Two callers, two tolerances. Batch compilation GATES: `brasa_module::load`
  drops a file that did not parse, and the CLI stops at the first phase that
  reported. The editor does the opposite — `brasa_module::load_partial` lowers
  what the parser salvaged and `brasa_lsp::analysis` runs every phase — because
  the file under a cursor is broken most of the time and its sound parts are
  still worth answering about. BRS-114 is what settled that the phases tolerate
  this; `crates/brasa/tests/partial.rs` and `crates/brasa_module/tests/partial.rs`
  defend it. Do not make the editor path gate, and do not make the batch path
  tolerate.

## Language conventions (Brasa code, `.bras` files and examples)

- `camelCase` for functions/methods/variables, `PascalCase` for types,
  `?` suffix for predicates. Keywords lowercase (`catch!` is a keyword).
- No `nil`, no implicit coercions, no inheritance.

## Rust conventions

- `cargo check` clean, idiomatic Rust, edition 2024.
- English-only code, comments, and commit messages. Conventional Commits.
- Tests: unit tests next to code, snapshot tests with `insta` in the
  parser, `.bras` golden programs (from M1) under `tests/programs/`.
