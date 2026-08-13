# Brasa — type system

Static, strong, with local inference and structural compatibility for
interfaces. No implicit coercions of any kind: `int` does not convert
to `float` on its own, nothing is "truthy".

## Types

| Category | Types |
|-----------|-------|
| Primitives | `int` (i64), `float` (f64), `bool`, `string`, `char`, `unit` |
| `Range` | first-class value (two `int`s + an inclusive flag), by value, lazy: iterating it materializes nothing. It is NOT sugar for `Vector<int>` — `0..10_000_000` takes 17 bytes, not 80MB. Consumed by `for`, `slice`, `rand.int` |
| Nominal composites | `struct`, `enum` (includes `Option<T>`) |
| Structural composites | tuples `(A, B)`, functions `(A, B) -> C`, interfaces |
| Generics | `Vector<T>`, `Map<K, V>`, `Set<T>`, parameterized structs/enums/functions |

Value vs reference:

- Primitives have value semantics. Tuples are immutable structural values;
  their VM storage is an implementation detail described in
  [07-bytecode.md](07-bytecode.md).
- Structs, enums with a payload, collections, strings, closures: references
  to the GC heap. `==` is ALWAYS structural (compares content, not
  identity); there is no identity operator in v1.

Tuples are positional and immutable: there is no element assignment, and
a tuple's type is the tuple of its elements' types with no unification
across positions. `(1, "a"): (int, string)`. Arity is part of the type,
so `(int, int)` and `(int, int, int)` never match. An expected tuple type
of the same arity propagates element-wise, which is what lets
`let p: (int, Vector<int>) = (1, [])` infer the empty vector. Elements
are read by destructuring — a `match` arm, a `for` binding, or a lambda
parameter pattern — not by an index expression: there is no `p.0`, and
that is settled rather than deferred (BRS-65). The ruling was reviewed
against the whole example and test corpus once lambda patterns landed:
no site wanted a positional accessor, and the sort, map and fold
pressure that looked like demand for one is served by
`sortBy(|(_, hits)| -hits)` and its siblings.

## Inference

**Local** inference (Rust/Swift style, not global Hindley-Milner):

- Function signatures are the boundary: parameters are always annotated,
  the return type is annotated or `unit`. Everything inside the body is
  inferred.
- `let x = expr` takes `expr`'s type. `let x: T = expr` checks against
  `T` (necessary when the right-hand side is ambiguous, e.g. `[]`).
- Lambda parameters are inferred from the expected context
  (`nums.map(|x| ...)` knows `x: int`); without context, they are annotated.
- Generic type arguments are inferred at the call site
  (`first([1,2])` infers `T = int`); there is no syntax to pass them
  explicitly in v1. This covers a method's own type parameters as well
  as a free function's: a struct's parameters are fixed by the receiver,
  a method's by its arguments, and a generic method on a generic struct
  solves both at once.
- `??` supplies its own context: absent an expected type from outside,
  the fallback is checked against the type the `Option` carries, so
  `o ?? []` on an `Option<Vector<int>>` needs no annotation. The type is
  offered, not imposed — a fallback that disagrees is still reported as
  the operator's own error, not as a mismatch inside the literal.

Golden rule: no type is inferred "at a distance" — reading a function
never requires looking at its call sites.

## Variables

- `let` fixes the type forever; reassigning is an error (it is immutable).
- `let mut` allows reassignment **of the same type**.
- Shadowing is allowed in inner scopes, even with a different type.
- **Immutability belongs to the *variable*, not to the value** (closed
  decision, JS `const` semantics): `let` only forbids re-pointing the
  binding, nothing more. `let v = [1]` allows `v.push(2)`; `let p = Point {...}`
  allows `p.x = 1.0`. Heap values are always mutable, no `let mut` needed.
  Reason: with shared references and no ownership, requiring `mut` to
  mutate interiors would be a dishonest guarantee (another alias mutates
  just the same); we promise exactly what we can deliver.

## Structural interfaces

```ruby
interface Comparable
  def cmp(self, other: Self): int
end
```

- A type `T` satisfies an interface if it has all the methods with
  compatible signatures. There is no conformance declaration.
- `Self` in an interface refers to the type satisfying it (allows
  `cmp(self, other: Self)` instead of fixing the type).
- Satisfaction compares a member's signature as written, so a method
  generic over its own parameters does not satisfy an interface member
  that names concrete types: `def box<T>(self, x: T): Vector<T>` does
  not satisfy `def box(self, x: int): Vector<int>`. A generic method is
  more general and could serve every call, but accepting it would mean
  reasoning about a signature that is not yet instantiated, which v1
  does not do. Other members of the same type are unaffected — a struct
  satisfies an interface through whichever members match.
- Generic constraints (`<T: Comparable>`) are the ONLY place where
  interfaces are used in v1: there are no interface-typed values (no
  dynamic dispatch, no `Vector<Printable>`). This keeps the VM simple and
  avoids vtables. If heterogeneity is needed, use an enum.
- Stdlib interfaces: `Comparable` (`<`, `>`), `Printable`
  (`toString`, interpolation), `Hashable` (Map/Set keys).
- **Primitives satisfy stdlib interfaces natively**: the checker knows
  (built-in, no directives) that `int`/`float`/`string`/`char` are
  `Comparable`, that every type is `Printable`, etc. Without this,
  `max<T: Comparable>(1, 2)` would not type-check — a primitive has no
  `cmp` method to find.
- `Printable` needs no conformance check of its own, and satisfying it
  unconditionally stays correct: every type has a `toString` (derived, or
  an override that cannot throw — see below), so the member `Printable`
  requires is always present and always infallible. It has no counterpart
  to `Comparable`'s throwing-`cmp` rule because the question is settled
  before conformance is ever asked, on the override itself: `T034`
  rejects the declared clause and `E007` rejects a non-empty inferred
  error-set, so there is no throwing `toString` left for a conformance
  check to find.
- **`Comparable` is otherwise structural, like any interface**: a type
  with `cmp(self, other: Self): int` satisfies it, so
  `max<T: Comparable>(a, b)` works on a user struct. The ordering
  operators inside such a function call that `cmp` and compare its
  result against `0`. Two things this does NOT open, both closed by
  their own rule rather than by `Comparable`: `a > b` directly on two
  struct values is still an error (the operator table below lists only
  primitives and a constrained parameter), and `Vector.sort()` is still
  limited to vectors of orderable primitives (`docs/spec/05-stdlib.md`).
- **A `cmp` that declares `throws` does not satisfy `Comparable`.** The
  ordering operators call it and compare its result against `0`, and an
  operator has no channel to report a failure on: a throwing `cmp`
  reached through `<`/`>`/`<=`/`>=` would escape the `throws` contract of
  the function that only compared two values. The member is rejected
  where conformance is decided, so the diagnostic (`T027`) says the
  method is there and throws rather than reporting it as missing. This is
  a rule about the declared contract; the same reasoning applies to
  interface members, where `throws` must be declared if the method
  throws (`docs/spec/04-errors.md`), so a parameter constrained by a
  throwing `Ord` does not entail `Comparable` either.
- **`Hashable` is closed in v1**: only `int`, `string`, `char`, `bool` and
  tuples of those. Structs and collections are NOT Map/Set keys — they are
  mutable references with structural equality, and mutating a key after
  inserting it would corrupt the table. `float` is not either (NaN).
- **`for` only iterates built-in types in v1**: Vector, Map, Set, ranges,
  and string. A user-defined `Iterable` requires dynamic dispatch, which
  does not exist in v1; deferred to v2 together with interface values.

## Operators and their rules

| Operator | Rule |
|----------|------|
| `+ - * / % **` | `int×int -> int`, `float×float -> float`; no mixing (convert explicitly: `x.toFloat()`) |
| `+` | also `string + string` |
| `== !=` | both sides the same type; structural |
| `< <= > >=` | `int`, `float`, `string`, `char`, or `T: Comparable` |
| `&& \|\| !` | `bool` only; short-circuit |
| `??` | `Option<T> ?? T -> T` (also chainable with another `Option<T>`) |
| `?.` | `Option<T>?.method(...)` -> `Option<R>`; flattens (does not nest Options) |
| `\|>` | `a \|> f(b)` ≡ `f(a, b)`; the target is any callable expression; pure syntactic sugar, desugared in AST→HIR lowering (the AST keeps the node for the formatter) |
| `[i]` on Vector | `int -> T`; out of range is a **panic** (not Option: the common case is a bug) |
| `[k]` on Map | reads `K -> Option<V>` (a missing key is a normal case, not a bug); **assigns a `V`** |

An index assignment stores what the container holds, which on a `Map`
is not what the same expression reads: `m[k]` reads `Option<V>` because
the key may be absent, but `m[k] = v` takes a `V` — there is no key to
be missing on the writing side. A consequence worth stating: compound
assignment through a `Map` index (`m[k] += 1`) does not type-check,
because it reads before it writes and the read is an `Option<V>`. Write
the read and the write separately.

## Numbers and strings: fine-grained rules

- **`int` overflow is a panic** (`panics.IntegerOverflow`), consistent with
  "bug = panic". No silent wrapping.
- **`float` is IEEE 754**: `1.0 / 0.0` is `inf` (only integer division
  by zero panics), `NaN != NaN`, and `float` is not `Hashable` nor a Map
  key. `Comparable` on floats follows IEEE (NaN does not order; sorting
  floats with NaN panics with `panics.AssertionFailed`).
- **Strings are not indexed**: `s[i]` does not exist (by byte is a UTF-8
  trap, by char is O(n) disguised as O(1)). Use `chars()`, the
  high-level methods, and `slice(from, to)` with char indices,
  documented as O(n).
- **Implicit `toString`**: every type has an automatically derived
  representation (structs like `Point { x: 1.0, y: 2.0 }`, enums like
  `Circle(1.0)`), used by `puts`, interpolation, and the native
  `Printable`. Defining a custom `toString` on a struct replaces it. Floats
  always show the decimal point (`1.0`, never `1`) — the int/float
  separation stays visible.
- **A `toString` override cannot throw** (`T034`, `E007`). Rendering has
  to be infallible. `toString` is not only called where the author wrote
  it: `puts`, string interpolation, `Vector.join`, and every container,
  `Option`, tuple, and enum that renders its elements reach it, and so
  does the runtime while it is already reporting a failure — an uncaught
  error, a stack trace, a failing assertion. A throw from there has
  nowhere left to be reported. The rule is enforced twice, because
  `throws` is inferred rather than required
  ([04-errors.md](04-errors.md)): the declared clause is rejected at the
  declaration site (`T034`), and the method's INFERRED error-set must be
  empty (`E007`), which is what catches a `toString` that throws without
  writing anything down. `throws never` and an absent clause are both
  accepted as declarations: neither says anything is thrown, and the
  inferred set still has to back them up. The repair is a `catch` inside
  `toString` that renders a fallback, or a differently-named method for
  the fallible work.

## Cyclic values

Reference cycles ARE constructible (recursive struct types plus shared
mutable containers: `s.v.push(s)`), so every structural operation has to
define what it does on one. The VM follows these rules:

- **`==` is coinductive.** Two values are equal when assuming their
  cycle-capable cells (Vector, Map, Set, Struct) equal derives no
  contradiction. `a == b` where `a.v == [a]` and `b.v == [b]` is `true`:
  both denote the same infinite structure, and with no identity operator
  to fall back on there is nothing else `==` could honestly answer. A
  cyclic value that is *not* equivalent still compares `false`; the
  comparison always terminates. Identity is never observable: a pair is
  only ever assumed equal, never assumed unequal, so `==` stays
  reflexive-by-content, and `[a] == [b]`, `{ "k": a } == { "k": b }`, and
  the container operations built on `==` (`contains?`, `uniq`, `Set`
  dedupe) inherit the same answer.
- **`toString` renders a back-edge as `<cycle>`** and does not recurse
  into it, in the same marker family as `<lambda>` and `<bound method>`.
  The marker is a property of the current path, not of sharing: a value
  reachable twice as a sibling renders in full both times
  (`[[1, 2], [1, 2]]`), while `a.v.push(a)` renders as
  `Node { v: [<cycle>] }`.
- **Nesting depth is not a cycle.** `toString` still refuses to render
  more than 10000 levels of nesting, but that limit is about the host
  stack and says only that; it never claims to have found a cycle. The
  converse holds too and is why the message is worded that way: a cycle
  whose period exceeds the limit trips it before the back-edge comes
  back around, so a value reported as too deeply nested may in fact be
  cyclic. Only the marker `<cycle>` is a positive statement about a
  cycle; the nesting message is a statement about depth alone.
- **Ordering never descends on its own.** The built-in comparisons are
  defined on `int`, `float`, `string`, and `char`, so `<`/`<=`/`>`/`>=`
  and sort keys never walk into a container. A user `cmp` reached
  through `T: Comparable` can walk wherever its body walks, and one that
  recurses through a cycle overflows the host stack — the same way a
  user `toString` would. That is an ordinary bug in that function, not a
  gap in the checker: unlike `==`, ordering has no coinductive answer to
  fall back on, so there is nothing the language could decide on the
  author's behalf.
- A cyclic value is still not a Map or Set **key** (`Hashable` is
  closed); it is an ordinary Map value or Vector element.

## `if` and `match` as expressions

- `match` is always an expression; always exhaustive; all branches must
  type-check the same (or the whole match is used as a statement and types
  as `unit`).
- `if` is an expression when there is an `else` and the branches match; if
  not, `unit`.

## Exhaustiveness and flow

- The `match` exhaustiveness checker understands enums, bools, tuples, and
  nested patterns; for `int`/`string` it requires `_`.
- `return`, `throw`, `break`, `continue` type as `never` (compatible with
  everything), so `let x = if ok then v else return end` works.

## Option

- `Option<T>` is a normal stdlib enum with sugar (`?.`, `??`) and
  pattern matching. There is no `nil`, no null reference, no implicit
  default. A struct field with no possible value is declared `Option<T>`
  and the constructor forces it to be passed (explicit `None`).

## Generics: execution model

Type checking is fully static, but the VM executes a uniform
representation (tagged values), so there is **no monomorphization**:
`first<T>` is a single function in bytecode. Consequences:

- No code bloat or compilation cost per instantiation.
- Method dispatch via a constraint is resolved in the checker (which
  already knows the concrete type at each call site) and is emitted as a
  direct call when possible, or via a value's method table when not.
- None of this is observable in the language; it's an implementation
  detail free to change.

> Canonical spec. A Spanish reading copy is mirrored in the Atlas workspace 'brasa'.
