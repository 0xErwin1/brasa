# Brasa — error system

> Status: closed.

Model: errors are ordinary values that are thrown (`throw`), each
function's error-set is **inferred** (never declared), and `catch`
is a **non-exhaustive** match over those errors. There is no `Result<T, E>`,
no `unwrap`, no "propagate on failure" intermediate state.

## Philosophy

1. The happy path is written as if nothing could fail.
2. Handling an error is a minimal diff: you add a `catch`, you don't
   rewrite signatures or wrap return values.
3. Either you handle the error or you don't — and if not, it propagates
   on its own. No intermediate ceremony.

## Throwing

```ruby
struct NetError
  detail: string
end

def fetchPage(ok: bool): string
  if !ok
    throw NetError { detail: "timeout" }
  end
  "<html>"
end
```

- `throw` accepts **any value**: struct, enum, string, int. There is no
  base `Exception` class or hierarchies.
- Stdlib convention: structs with an `Error` suffix and context fields.

## Error-set inference

- The checker computes, for each function, the set of types it can
  throw: its own `throw`s ∪ the error-sets of what it calls, minus what
  it catches. It is an interprocedural fixpoint analysis (recursion
  converges because the sets only grow and are finite).
- The written signature does NOT change: `def fetchPage(ok: bool): string`.
  The error-set is derived metadata, visible to tooling (LSP hover,
  generated docs), not required by syntax.
- The set travels between modules: if `a()` calls `b.helper()` which
  throws `NetError`, `a`'s set includes `NetError`.

`throws` is **optional and verified**: a function can declare its
contract and the compiler checks it against the body:

```ruby
def fetch(url: string): string throws NetError
  ...
end
```

- If the body can throw something undeclared, it's a compile error.
- `throws never` asserts that the function does not throw (it may still
  panic).
- `interface` methods MUST declare `throws` if they throw: an interface
  is a contract and contracts are not inferred.
- Without a declaration, pure inference — the default for scripts.

## Catching: `catch` as a match

`catch` attaches to an expression (typically a call):

```ruby
let page = fetchPage(ok) catch (e)
  NetError => "recovered: #{e.detail}"
end
```

- Each arm matches an error TYPE; `e` is bound with that type inside
  the arm (like a match over an anonymous enum of the error-set).
- **Non-exhaustive by default**: if the expression can throw 10 errors
  and you handle 1, the other 9 automatically re-throw.
- Arms must type-check the same as the expression (`catch` produces the
  same type, it's an expression).
- An arm matching a type the expression CANNOT throw is a compile
  error ("unreachable arm") — this exists thanks to error-set inference.
- The `e` binding is retyped per arm: inside the `NetError => ...` arm,
  `e` IS `NetError` (narrowing, like TS). Arms can group types with
  `|`: `NetError | DnsError => ...` (inside, `e` only allows what's
  common to both). This is the ONLY appearance of unions in the
  language, scoped to catch arms.
- `_ =>` as the last arm catches any remaining error — but it **never
  catches panics**.
- Matching is **nominal**: `catch` distinguishes by the declared type of
  the thrown value, not by its shape.
- Stdlib-native errors are named by their dotted module-qualified name
  (`string.ParseError`, `fs.NotFound` — `05-stdlib.md`), so they never
  collide with a user-defined `ParseError`. They behave as ordinary
  errors: they appear in error-sets, an arm naming them catches them,
  and `_` catches them too (unlike panics).
- Re-throwing with wrapping is a normal `throw` inside the arm:

```ruby
let cfg = load(path) catch (e)
  fs.NotFound => throw ConfigError { detail: "no config", cause: e.toString() }
end
```

`catch` is a **postfix expression operator**: it attaches to
any expression, there is no "try block" form. To cover several
statements, extract a function — which is exactly the pattern the
design wants to encourage.

## Opt-in exhaustiveness

```ruby
let page = fetchPage(ok) catch! (e)
  NetError => "..."
  ParseError => "..."
end
```

- `catch!`: the compiler requires an arm (or `_`) for EVERY type in the
  inferred error-set, and forbids unreachable arms. If `fetchPage` later
  throws something new, this call site stops compiling — that's the
  point: the program's edges (main, handlers) declare "nothing goes
  unhandled here."

Spelling: the keyword is `catch!`, never `catch_all`. Every keyword in
the language is a single lowercase word, and a trailing `!` is already
part of the identifier lexicon (see `IDENT` in
[02-grammar.md](02-grammar.md), and the `mut!` convention) where it reads
as "the strict variant". `catch!` therefore keeps the reserved-word list
lowercase and underscore-free without inventing notation the language did
not already use.

## Panics vs errors

Panics are a **closed union in the stdlib** — in v1 exactly
`panics.IndexOutOfBounds`, `panics.DivisionByZero`,
`panics.IntegerOverflow`, `panics.AssertionFailed`, and
`panics.StackOverflow` (the recursion limit panics rather than crash
the runtime) — separate from the error channel. They do
not appear in error-sets and `_` does not catch them. The only way to
catch one is to **name it explicitly** in an arm; inside that arm, `e`
is bound to the panic's detail message (a `string`):

```ruby
let x = items[i] catch (e)
  panics.IndexOutOfBounds => 0
end
```

(This replaces the original design's `catch_all_panics` keyword: naming
the panic expresses the same intent with one fewer mechanism.)

| | Error (`throw`) | Panic |
|---|---|---|
| Origin | domain: network, IO, parsing, validation | bug: index out of range, div/0, assertion |
| In the error-set | yes, inferred | no |
| Arm with its type | catches it | catches it (explicit opt-in) |
| `_` arm | catches it | does NOT catch it |
| Unhandled in main | message + exit ≠ 0 | message + stacktrace + exit ≠ 0 |

## Interaction with the rest of the language

- **Lambdas**: their error-set flows to whoever invokes them
  (`map(|x| parse(x))` makes `map` "throw" whatever the lambda throws).
  Higher-order functions are transparent to errors.
- **Generics**: a generic function receiving `(T) -> R` inherits the
  error-set of the concrete argument at each call site.
- **`for`/pipes**: a `throw` inside cuts the iteration/chain and
  propagates, like any expression.
- **Interop with Option**: `Option` is for expected ABSENCE (a key that
  isn't there); `throw` is for operation FAILURE (the file couldn't be
  read). The stdlib is consistent with that line.

## Design decisions

| Question | Decision | Rationale |
|----------|----------|-----------|
| Binding | a single `catch (e)`, retyped (narrowed) per arm | One binding covers every arm, so no per-arm syntax is needed to name the error |
| Catch over blocks? | No: postfix on an expression only | Extracting a function is the pattern the design wants to encourage; a try-block invites wrapping unrelated statements together |
| Nominal or structural? | Nominal | Errors are identity: two unrelated structs that happen to share a `detail: string` field are not the same failure |
| `catch_all_panics` | Removed; panics are caught by naming them | Naming the panic type is already an explicit opt-in, so a second mechanism earns nothing |
| Explicit `throws` | Optional and verified; mandatory in interfaces; `throws never` | Inference is the default for scripts, but a contract that is written down should be checked — and an interface is a contract, so it cannot be inferred |
| `_` arm | Catches errors, never panics | A bug must not be swallowed by a catch-all written for domain failures |

Known risk: the interaction of error-set inference with recursion and
with generics is Brasa's own design, with no prior implementation to
compare against. The M2 plan must include tests for mutual recursion,
higher-order functions with lambdas that throw, and generics with a
`(T) -> R` argument that throws.

> Canonical spec. A Spanish reading copy is mirrored in the Atlas workspace 'brasa'.
