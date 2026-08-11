# Brasa — diagnostics

> Status: normative for the compiler's error reporting. The code tables
> below ARE the public registry; `brasa_diagnostics::codes` must stay in
> sync with them (enforced by unit test).

Model: every phase returns structured diagnostics as plain data; only the
CLI renders them. Codes are stable, per-phase, and append-only. Wording
is terse and lowercase; spans point at the most precise node available.

## Principles

1. **Phases return, the CLI renders.** No phase writes to a terminal;
   each returns `Vec<Diagnostic>` and the CLI decides presentation
   (currently pretty terminal output via `ariadne`).
2. **One primary span** per diagnostic, plus optional labeled secondary
   spans and notes.
3. **Poisoning, not cascades.** An error yields a silently-unifying
   `Unknown`; downstream checks involving it stay quiet. One root cause,
   one diagnostic.
4. **Clean-phase gating.** Each phase runs only if the previous one
   produced no errors. A parse error means no resolution; a resolution
   error means no type checking.
5. **Deterministic output.** Diagnostics are sorted by span; exact
   `(message, span)` duplicates are dropped.

## Data model

| Field | Contents |
|-------|----------|
| severity | `error`, `warning`, `info`, or `hint` |
| message | terse, lowercase description of the problem |
| code | stable per-kind code (see the scheme below) |
| primary span | the most precise node responsible |
| labels | secondary spans, each with its own message |
| notes | free-standing hints, only when there is a concrete action |

Warnings never affect the exit code. Real lint controls (allow/deny,
lint groups) are M5+.

## Code scheme

Format: `<PhaseLetter><3 digits>` — e.g. `P001`, `R004`, `T012`.

| Letter | Phase |
|--------|-------|
| `L` | lexer |
| `P` | parser |
| `R` | resolver |
| `T` | type checker |
| `E` | error-sets |
| `X` | execution/VM (reserved, M3+) |

Rules:

- Codes are **append-only**: never renumbered, never reused after
  removal.
- Every emission site uses a **named constant** from the phase's code
  registry (`brasa_diagnostics::codes`); no inline code strings.
- Uniqueness and format are enforced by unit test.
- A code identifies an error **kind**: one code may back many wordings
  and spans (`P001` covers every "expected X, found Y").

## Wording style

- Messages are lowercase and terse: ``expected `int`, found `string` ``,
  not "Error: the types are incompatible".
- Names, types, and operators go in backticks: `` `int` ``, `` `..` ``,
  `` `fetchPage` ``.
- A label states what is wrong at its span, or the action: "expected
  `int`", "used here".
- Notes appear only when there is a concrete action to suggest.
- The rendered header shows the code:
  ``[P001] Error: expected an expression, found `end` ``.

## Span rules

- Every diagnostic points at the most precise node available: the
  offending token, not its statement; the field, not its struct.
- Named declarations carry their own name spans, and diagnostics about a
  name (duplicate, unknown, unused) point at the name, not the whole
  declaration. (Functions, parameters, fields, variants, and generic
  parameters carry dedicated name spans; `throws`-contract diagnostics
  (`E004`/`E005`) point at the declaring function's name.)

## Code registry

### Lexer (`L`)

| Code | Kind | Example message |
|------|------|-----------------|
| `L001` | unexpected character | ``unexpected character `@` `` |
| `L002` | unterminated string literal | `unterminated string literal` |
| `L003` | unterminated interpolation | `unterminated interpolation` |
| `L004` | malformed character literal | `malformed character literal` |

### Parser (`P`)

| Code | Kind | Example message |
|------|------|-----------------|
| `P001` | expected token/production | ``expected an expression, found `end` `` |
| `P002` | nesting too deep | `nesting too deep (limit 420)` |
| `P003` | unknown escape sequence | ``unknown escape sequence `\q` `` |
| `P004` | enum without variants | ``enum `Color` must have at least one variant`` |
| `P005` | interface without members | ``interface `Shape` must have at least one member`` |
| `P006` | non-associative range chain | `ranges are non-associative: use parentheses to chain them` |
| `P007` | invalid integer literal | `integer literal out of range` |
| `P008` | duplicate struct-literal field | ``duplicate field `x` in struct literal`` |
| `P009` | interpolation not allowed | `interpolation is not allowed here` |

### Resolver (`R`)

| Code | Kind | Example message |
|------|------|-----------------|
| `R001` | unknown name | ``unknown name `missing` `` |
| `R002` | use before definition | ``` `x` is used before its definition ``` |
| `R003` | unknown type | ``unknown type `Bogus` `` |
| `R004` | unknown constructor | ``unknown constructor `Whatever` `` |
| `R005` | ambiguous constructor | ``ambiguous constructor `Red` `` |
| `R006` | duplicate definition | ``duplicate definition of `x` `` |
| `R007` | `self` outside a method | `` `self` outside a method`` |
| `R008` | unknown import root | ``unknown import root `sys::io` `` |
| `R009` | unknown std module | ``unknown std module `netz` `` |
| `R010` | constraint is not an interface | `` `Point` is not an interface`` |
| `R011` | unknown panic | ``unknown panic `panics.Nope` `` |
| `R012` | unknown stdlib error | ``unknown stdlib error `string.Nope` `` |

Notes on kind boundaries:

- `R003` covers every type-name lookup failure: annotations, struct
  literals, and named constraints.
- `R006` covers every same-scope name clash: items, locals, generic
  parameters, struct/variant fields, and enum variants.
- `R008` (a `::` path whose root is not `std`) and `R009` (a `std::`
  path naming no known module) are distinct lookups and keep distinct
  codes.
- `R011` and `R012` cover the closed builtin `catch`-arm namespaces:
  `R011` the `panics.` union (`04-errors.md`), `R012` the landed
  stdlib-error names (`string.ParseError`, `string.RegexError`,
  `05-stdlib.md`) — both
  builtin, no import needed. Dotted arm names in other roots (`fs.`,
  `proc.`, `json.`) are not yet checked — their namespaces land in M4.

### Type checker (`T`)

| Code | Kind | Example message |
|------|------|-----------------|
| `T001` | mismatched types | ``mismatched types: expected `int`, found `string` `` |
| `T002` | invalid arithmetic operands | ``invalid operands for `+`: `int` and `float` `` |
| `T003` | cannot compare for equality | ``cannot compare `int` and `string` for equality`` |
| `T004` | unsupported ordering | `` `bool` does not support ordering with `<` `` |
| `T005` | wrong number of arguments | `wrong number of arguments: expected 2, found 1` |
| `T006` | not callable | ``cannot call a value of type `int` `` |
| `T007` | unknown member | ``no method `bogus` on `int` `` |
| `T008` | `join` requires `Vector<string>` | `` `join` requires `Vector<string>`, found `Vector<int>` `` |
| `T009` | cannot assign | ``cannot assign to immutable binding `x` `` |
| `T010` | invalid assignment target | `invalid assignment target` |
| `T011` | strings are not indexable | `strings are not indexable` |
| `T012` | cannot index | ``cannot index `int` `` |
| `T013` | cannot iterate | ``cannot iterate over `int` `` |
| `T014` | empty literal cannot infer | `cannot infer the element type of an empty vector literal` |
| `T015` | lambda parameter needs annotation | ``lambda parameter `x` needs a type annotation`` |
| `T016` | branch/arm type mismatch | `` `if` branches have mismatched types: `int` vs `string` `` |
| `T017` | non-exhaustive match | ``non-exhaustive match: `false` is not covered`` |
| `T018` | pattern/scrutinee mismatch | `` `Some` pattern does not match type `int` `` |
| `T019` | `return` outside a function | `` `return` outside a function`` |
| `T020` | unknown struct-literal field | ``unknown field `z` on struct `Point` `` |
| `T021` | duplicate struct-literal field | ``duplicate field `x` in struct literal`` |
| `T022` | missing struct-literal field | ``missing field `y` in struct literal of `Point` `` |
| `T023` | not a struct | `` `Color` is not a struct`` |
| `T024` | wrong number of type arguments | ``wrong number of type arguments for `Point`: expected 1, found 2`` |
| `T025` | interface used as a type | `interfaces cannot be used as types in v1; use a generic constraint` |
| `T026` | cannot infer type parameter | ``cannot infer type parameter `T` of `identity` `` |
| `T027` | constraint not satisfied | `` `bool` does not satisfy `Comparable` `` |
| `T028` | `?.` needs an `Option` receiver | `` `?.` requires an `Option` receiver, found `int` `` |
| `T029` | `??` needs an `Option` left side | `` `??` requires an `Option` on its left side, found `int` `` |
| `T030` | `??` fallback type mismatch | `` `??` fallback has type `string`, but the `Option` carries `int` `` |
| `T031` | key/element not `Hashable` | `` `float` cannot be a `Map` key: `Hashable` is closed to `int`, `string`, `char`, `bool`, and tuples of those `` |

Notes on kind boundaries:

- `T002` covers binary arithmetic (`+ - * / % **`) and unary `-`.
- `T004` covers both ordering failures: differently-typed sides and
  same-typed but unordered operands.
- `T005` covers every callee-arity failure: function and builtin calls,
  and `Some`/`None`/enum-variant constructors in both expression and
  pattern position — a constructor pattern mirrors the constructor
  call's arity.
- `T007` covers every receiver kind (struct, builtin, generic); the
  message names the receiver. `T008` stays separate: the member exists,
  only the element type is wrong.
- `T014` covers empty vector and empty map literals.
- `T016` covers `if` branches and `match` arms; `catch` arms report
  against the subject's type as plain `T001` mismatches.
- `T018` covers pattern-shape failures against the scrutinee's type,
  including a tuple pattern of the wrong length — tuple arity comes
  from the scrutinee, not from a constructor.
- `T028`–`T030` cover `?.`/`??` misuse and report in source terms: the
  spans point at the user's receiver, left side, or fallback, and the
  desugared `match` (its synthesized patterns and arms) never appears
  in a message — sugar misuse is never `T016` or `T018`.
- `T031` covers `Map` keys and `Set` elements, and fires once, where
  the key/element type is established: the type annotation, the map
  literal, or the `Set` constructor. Key-taking methods (`insert`,
  `get`, `has?`, `remove`, `add`) check against that established type
  and never re-report.

### Error-sets (`E`)

| Code | Kind | Example message |
|------|------|-----------------|
| `E001` | unreachable `catch` arm | ``unreachable `catch` arm: `ParseError` is not in the error-set here`` |
| `E002` | `catch_all` not exhaustive | ``catch_all does not handle `NetError` and `ParseError` `` |
| `E003` | unverifiable exhaustiveness | `catch_all cannot be verified: the subject's error-set is open` |
| `E004` | undeclared throw | `` `fetch` throws `DnsError` but does not declare it`` |
| `E005` | `throws never` violated | `` `boom` declares `throws never` but can throw `BoomError` `` |

Notes on kind boundaries:

- The `E` checks run only over closed error-sets, except where noted:
  the tags of an open set are a sound lower bound, so a "this CAN be
  thrown" finding still fires, but every "this CANNOT be thrown" claim
  is skipped or reported as unverifiable. Top-level code is analyzed
  as one pseudo-body: its `catch`/`catch_all` expressions get the same
  checks as any function body, but the top level has no `throws`
  contract, so its own set may be non-empty without a diagnostic (an
  uncaught top-level throw ends the script at runtime, exit 70).
- `E001` covers both unreachable-arm shapes: a named type the subject's
  closed set does not contain (in `catch` and `catch_all` alike,
  guarded or not — the guard runs only after the type matches), and a
  `_` arm in a `catch_all` whose unguarded named arms already handle
  every error. A defensive `_` in a plain `catch` is never flagged:
  non-exhaustive handling is the default there.
- `E002` counts only unguarded arms (and an unguarded `_`) toward
  exhaustiveness — a guard may be false, the same rule error-set
  subtraction uses.
- `E003` is `catch_all` over an OPEN subject set: soundness forbids
  claiming exhaustiveness over an incomplete list.
- `E004` checks the inferred tags against the declared `throws` list;
  an open actual set is tolerated (the declaration is the contract,
  there is no exhaustiveness claim to prove — deliberately asymmetric
  with `E003`). Over-declaration (declaring a type the body never
  throws) gets no diagnostic. Interface-member `throws` names are
  validated at resolution (`R003` on an unknown name), but the
  contracts themselves are not enforced yet (deferred with interface
  satisfaction, M3+).
- `E005` backs two wordings under one kind: a concrete violation
  (`throws never` with a non-empty set) and the unverifiable case
  (`throws never` with an open set).

## Deferred (M5)

- Machine-readable output (JSON diagnostics for tooling).
- `--explain <code>` with extended per-code documentation.

> Canonical spec. A Spanish reading copy is mirrored in the Atlas workspace 'brasa'.
