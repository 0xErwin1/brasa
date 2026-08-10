# Brasa specification

Scripting language: strongly statically typed with inference, Ruby-like
syntax, bytecode VM with GC, implemented in Rust.

| Document | Contents | Status |
|-----------|-----------|--------|
| [00-vision.md](00-vision.md) | Goals, closed decisions, roadmap | closed |
| [01-syntax.md](01-syntax.md) | The language as seen by the user | closed |
| [02-grammar.md](02-grammar.md) | Lexicon + EBNF + ambiguities | draft for review |
| [03-types.md](03-types.md) | Type system and semantics | draft for review |
| [04-errors.md](04-errors.md) | BAML-style error system | closed (validated against BAML canary) |
| [05-stdlib.md](05-stdlib.md) | Stdlib v1 modules | sketch |
| [06-diagnostics.md](06-diagnostics.md) | Diagnostic model, code registry, wording | draft for review |

Predecessor: OTL (`~/dev/personal/OCaml/OTL`), the OCaml prototype that
validated the lexer → parser → checker → interpreter pipeline.
