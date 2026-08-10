# Especificación de Brasa

Lenguaje de scripting: tipado estático fuerte con inferencia, sintaxis
Ruby-like, VM de bytecode con GC, implementado en Rust.

| Documento | Contenido | Estado |
|-----------|-----------|--------|
| [00-vision.md](00-vision.md) | Objetivos, decisiones cerradas, roadmap | cerrado |
| [01-sintaxis.md](01-sintaxis.md) | El lenguaje visto por el usuario | cerrado |
| [02-gramatica.md](02-gramatica.md) | Léxico + EBNF + ambigüedades | borrador para revisión |
| [03-tipos.md](03-tipos.md) | Sistema de tipos y semántica | borrador para revisión |
| [04-errores.md](04-errores.md) | Sistema de errores estilo BAML | cerrado (validado contra BAML canary) |
| [05-stdlib.md](05-stdlib.md) | Módulos de la stdlib v1 | bosquejo |

Predecesor: OTL (`~/dev/personal/OCaml/OTL`), el prototipo OCaml que validó
el pipeline lexer → parser → checker → intérprete.
