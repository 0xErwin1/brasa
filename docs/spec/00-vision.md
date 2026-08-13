# Brasa — vision and scope

Brasa is a scripting language with strong static typing and inference,
Ruby-inspired syntax, and a bytecode VM with GC implemented in Rust.
Goal: replace Python and bash in ~90% of scripting use cases: text
manipulation, command invocation, files, JSON, automation.

Extension: `.bras`. Execution: `brasa script.bras` or shebang
`#!/usr/bin/env brasa`. A standalone file runs without a project or manifest.

It was `.brs` until M6, and the reason for the change is worth keeping:
`.brs` is BrightScript's, so editors, linguist, and syntax-highlighting
registries already claim it. Sharing an extension with an unrelated
language means a Brasa file opens as BrightScript in a stock editor and
a repository reports the wrong language — a problem no amount of
tooling on this side can fix, and one that only gets more expensive to
undo the longer the extension is in the wild.

## Principles

1. **The happy path is written without ceremony.** No `Result`, no `unwrap`,
   no mandatory annotations outside function signatures.
2. **Strong always, never weak.** No implicit coercions. Flexibility comes
   from *structural* typing (shape-based interfaces), not from relaxing
   checks.
3. **Conservative defaults.** Immutable by default (`let` / `let mut`),
   private by default (explicit `pub`), no `nil` (`Option<T>`).
4. **Errors are not viral.** Values are thrown, error-sets are inferred
   in signatures, `catch` is a non-exhaustive match. See
   [04-errors.md](04-errors.md).
5. **The stdlib is the product.** First-class strings, processes, fs, JSON,
   regex, and glob. The language exists to serve these use cases.
6. **Instant startup.** Parse + typecheck + execution of a small script
   must feel immediate (< 10 ms cold for a hello world).

## Closed decisions

| Area | Decision |
|------|----------|
| Implementation | Rust; lexer with `logos`, hand-written recursive descent parser, diagnostics with `ariadne`/`codespan` |
| Execution | Custom bytecode VM, GC (v1: precise, simple; optimize later) |
| Typing | Static, strong, local inference, structural for interfaces |
| Mutability | `let` immutable, `let mut` mutable |
| Data semantics | Structs and collections by reference (GC heap); primitives by value |
| Nullability | No `nil`; `Option<T>` + `?.` and `??` sugar |
| Generics | Monomorphization not required in v1 (dynamic VM under static types); structural constraints, no unions |
| OOP | No inheritance or classes; structs + methods + structural interfaces |
| Errors | Throw values, error-set inference, catch-match |
| Modules | One file = one module; `import std::fs` (stdlib), `import "./foo.bras"` (files); no selective import; explicit `pub` |
| Stdlib | Native in Rust (VM builtins); never written in Brasa on the startup path |
| Concurrency | Out of v1; future design oriented toward a multi-threaded event loop |

## Compiler architecture

```
source ─→ Lexer ─→ Parser ─→ HIR (lowering) ─→ Resolver ─→ Type check ─→ Error-sets ─→ Codegen ─→ VM
          logos    Pratt+RD   desugar          names       inference     fixpoint      bytecode
          tokens   AST                         scopes      exhaustiveness
```

| Decision | Detail |
|----------|--------|
| Parser | Recursive descent for declarations/statements; **Pratt** (binding powers) for expressions. The precedence table in `02-grammar.md` translates directly into `(left_bp, right_bp)` pairs; `**` right-associative = inverted pair; `catch` is one more loop postfix |
| AST | **Index arenas**: `Vec<Expr>` per node kind + typed `Copy` IDs (`ExprId(u32)`, `FuncId`, ...). No `Box`, no viral lifetimes. rustc/rust-analyzer pattern |
| Side tables | The AST/HIR is immutable; each phase produces parallel tables keyed by ID: `types: Map<ExprId, Type>`, `spans`, `error_sets: Map<FuncId, ErrorSet>` |
| HIR | Desugared AST: `\|>` → calls, `?.`/`??` → match over Option, `for` → iteration protocol, interpolation → concat, `+=` → assignment. Checker, error-sets, and codegen work over the small core |
| Analyzer | Three passes over HIR: name resolution → type check → error-set inference (fixpoint over the call graph; needs the types, hence it runs last) |
| MIR? | **No.** HIR → direct bytecode (like Lua/CPython). An SSA/CFG MIR only pays off with a serious optimizer, which is a non-goal for v1. If it's ever needed, it slots in between HIR and codegen without touching earlier phases |
| Codegen | Stack-based VM. `match` compiles to **decision trees** from day one (the naive if-chain version is painful to replace later) |

## Non-goals (v1)

- Concurrency / async (reserved keyword, no semantics). Running several
  **subprocesses** in parallel is a different thing and landed as
  `proc.tryRunAll` (BRS-104): the children do the work and the script
  waits, so no Brasa code ever runs concurrently — no scheduler, no shared mutable state,
  no colored functions. The bash idiom being replaced is `xargs -P`,
  not threads.
- AOT or JIT compilation.
- General type unions (`int | string`); enums cover the case.
- Macros / metaprogramming.
- C interop (Ignis already covers that niche).

## Implementation roadmap

1. **M0** — lexer + parser + AST + pretty diagnostics (no execution).
2. **M1** — full type checker (inference, generics, structural
   interfaces, Option) over a provisional tree-walker, retired in
   BRS-108 once the VM had a conformance corpus of its own.
3. **M2** — error system (error-set inference + catch).
4. **M3** — bytecode VM + GC; the tree-walker stayed on as the
   reference oracle until BRS-108 replaced it with a conformance corpus.
5. **M4** — scripting stdlib (strings, fs, process, JSON, regex, glob).
6. **M5** — formatter, editor support, minimal LSP. `brasa fmt`
   (BRS-91) and the tree-sitter grammar with `.bras` editor
   registration (BRS-93) shipped. The LSP's prerequisite is answered
   (BRS-114): the analysis phases produce usable types, locals and
   error-sets over the incomplete tree an editor holds, and the whole
   pipeline runs in 1.4ms over the largest bundled script — so a query
   system for incrementality is a non-goal, not a deferral. Debugging
   tooling joins this milestone behind the LSP: a VM debug substrate
   (BRS-117) with breakpoints, stepping and frame inspection, then a
   non-interactive `brasa debug` (BRS-118), a DAP adapter (BRS-119),
   and a TUI narrowed to heap inspection (BRS-120) — the one view an
   editor's debug panels have no vocabulary for. A sampling profiler
   (BRS-121) is adjacent and mechanically separate: sampling a
   distribution is not stopping at a state. The REPL is deferred behind
   all of it: with immutable `let` and module-level typing, hover over
   inferred types and error-sets answers more, in the file the user is
   already editing, than a persistent environment does.
7. **M6** — from toy to tool. Landed: multi-file programs and `::`
   imports on a search path (BRS-97/102/115), the `test` item and
   `brasa test` (BRS-110), a byte-budgeted collector (BRS-100/101),
   bounded parallel subprocesses (BRS-104), single-artifact
   distribution (BRS-111), and the dispatch path (BRS-98), which took
   recursive calls from 2.9x CPython to 1.5x while leaving cold start
   untouched. Argument parsing (BRS-112) and blocking HTTP (BRS-113)
   have also landed. BRS-96 is in progress rather than wholly remaining:
   `Vector` receiver methods and the `fs` module use declarative tables,
   while the rest of the stdlib still declares its
   signatures, error contributions, and VM implementations separately.

> Canonical spec. A Spanish reading copy is mirrored in the Atlas workspace 'brasa'.
