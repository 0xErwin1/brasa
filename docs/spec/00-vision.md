# Brasa — visión y alcance

Brasa es un lenguaje de scripting con tipado estático fuerte e inferencia,
sintaxis inspirada en Ruby, y una VM de bytecode con GC implementada en Rust.
Objetivo: reemplazar Python y bash en el ~90% de los casos de scripting:
manipulación de texto, llamadas a comandos, archivos, JSON, automatización.

Extensión: `.brs`. Ejecución: `brasa script.brs` o shebang
`#!/usr/bin/env brasa`. Un archivo suelto corre sin proyecto ni manifest.

## Principios

1. **El happy path se escribe sin ceremonia.** Sin `Result`, sin `unwrap`,
   sin anotaciones obligatorias fuera de las firmas de función.
2. **Fuerte siempre, nunca débil.** No hay coerciones implícitas. La
   flexibilidad viene del tipado *estructural* (interfaces por forma), no de
   relajar el chequeo.
3. **Defaults conservadores.** Inmutable por defecto (`let` / `let mut`),
   privado por defecto (`pub` explícito), sin `nil` (`Option<T>`).
4. **Los errores no son virales.** Sistema estilo BAML: se lanzan valores,
   los error-sets se infieren en las firmas, `catch` es un match no
   exhaustivo. Ver [04-errores.md](04-errores.md).
5. **La stdlib es el producto.** Strings, procesos, fs, JSON, regex y glob
   de primera clase. El lenguaje existe para servir a esos casos.
6. **Arranque instantáneo.** Parse + typecheck + ejecución de un script
   chico debe sentirse inmediato (< 10 ms de frío para un hello world).

## Decisiones cerradas

| Área | Decisión |
|------|----------|
| Implementación | Rust; lexer con `logos`, parser recursivo descendente a mano, diagnósticos con `ariadne`/`codespan` |
| Ejecución | VM de bytecode propia, GC (v1: precise, simple; optimizar después) |
| Tipado | Estático fuerte, inferencia local, estructural para interfaces |
| Mutabilidad | `let` inmutable, `let mut` mutable |
| Semántica de datos | Structs y colecciones por referencia (heap GC); primitivos por valor |
| Nulabilidad | Sin `nil`; `Option<T>` + azúcar `?.` y `??` |
| Genéricos | Monomorfización no requerida en v1 (VM dinámica bajo tipos estáticos); constraints estructurales, sin uniones |
| OOP | No hay herencia ni clases; structs + métodos + interfaces estructurales |
| Errores | Modelo BAML (throw de valores, inferencia de error-sets, catch-match) |
| Módulos | Un archivo = un módulo; `import std::fs` (stdlib), `import "./foo.brs"` (archivos); sin import selectivo; `pub` explícito |
| Stdlib | Nativa en Rust (builtins de la VM); nunca escrita en Brasa en el camino del arranque |
| Concurrencia | Fuera de v1; diseño futuro orientado a event loop multi-hilo |

## Arquitectura del compilador

```
fuente ─→ Lexer ─→ Parser ─→ HIR (lowering) ─→ Resolver ─→ Type check ─→ Error-sets ─→ Codegen ─→ VM
          logos    Pratt+RD   desugar          nombres     inferencia    fixpoint      bytecode
          tokens   AST        ↓                scopes      exhaustividad
                              tree-walker (M1) corre sobre HIR
```

| Decisión | Detalle |
|----------|---------|
| Parser | Recursive descent para declaraciones/sentencias; **Pratt** (binding powers) para expresiones. La tabla de precedencias de `02-gramatica.md` se traduce directo a pares `(left_bp, right_bp)`; `**` derecha = par invertido; `catch` es un postfix más del loop |
| AST | **Arenas de índices**: `Vec<Expr>` por tipo de nodo + IDs tipados `Copy` (`ExprId(u32)`, `FuncId`, ...). Sin `Box`, sin lifetimes virales. Patrón rustc/rust-analyzer |
| Side tables | El AST/HIR es inmutable; cada fase produce tablas paralelas por ID: `types: Map<ExprId, Type>`, `spans`, `error_sets: Map<FuncId, ErrorSet>` |
| HIR | AST desazucarado: `\|>` → calls, `?.`/`??` → match sobre Option, `for` → protocolo de iteración, interpolación → concat, `+=` → asignación. Checker, error-sets, tree-walker y codegen trabajan sobre el núcleo chico |
| Analyzer | Tres pasadas sobre HIR: resolución de nombres → type check → inferencia de error-sets (fixpoint sobre el grafo de llamadas; necesita los tipos, por eso va después) |
| ¿MIR? | **No.** HIR → bytecode directo (como Lua/CPython). Un MIR SSA/CFG solo paga con optimizador serio, que es no-objetivo de v1. Si algún día hace falta, se inserta entre HIR y codegen sin tocar las fases anteriores |
| Codegen | VM de pila. `match` compila a **árboles de decisión** desde el día uno (la versión naïve en cadena de ifs es dolorosa de reemplazar después) |

## No-objetivos (v1)

- Concurrencia / async (palabra clave reservada, sin semántica).
- Compilación AOT o JIT.
- Uniones de tipos generales (`int | string`); los enums cubren el caso.
- Macros / metaprogramación.
- Interop con C (Ignis ya cubre ese nicho).

## Roadmap de implementación

1. **M0** — lexer + parser + AST + diagnósticos bonitos (sin ejecutar).
2. **M1** — type checker completo (inferencia, genéricos, interfaces
   estructurales, Option) sobre tree-walking provisional.
3. **M2** — sistema de errores BAML (inferencia de error-sets + catch).
4. **M3** — VM de bytecode + GC; el tree-walker queda como referencia.
5. **M4** — stdlib de scripting (strings, fs, proceso, JSON, regex, glob).
6. **M5** — REPL, formatter, LSP mínimo.
