# Brasa — syntax

What the whole language looks like. The formal grammar is in
[02-grammar.md](02-grammar.md); this document is the human-facing view.

## A program

```ruby
import std::fs
import std::json

struct Repo
  name: string
  stars: int
end

def topRepos(path: string, min: int): Vector<Repo>
  let data = json.parse(fs.read(path))
  data.repos
    .filter(|r| r.stars >= min)
    .sortBy(|r| -r.stars)
end

let repos = topRepos("repos.json", 100) catch (e)
  fs.NotFound => []
end

for repo in repos
  puts "#{repo.name}: #{repo.stars}"
end
```

## Declarations and mutability

```ruby
let x = 5              # immutable, inferred type (int)
let y: float = 1.5     # optional annotation
let mut count = 0      # mutable
count = count + 1      # only valid on `let mut`
count += 1             # compound assignment sugar
```

Reassigning a `let` or changing a variable's type is a compile error.
Shadowing (`let x = ...` again in an inner scope) is allowed.

## Functions and lambdas

```ruby
def greet(name: string, times: int): string
  "hello " * times + name
end

pub def area(w: float, h: float): float   # pub = exported from the module
  w * h
end
```

- Parameter annotations are mandatory; return type is optional (default `unit`).
- Implicit return: the body's value is its last expression. `return`
  exists for early exit.

Lambdas — Ruby pipe syntax, with types inferred from context:

```ruby
let double = |x: int| x * 2       # annotated (no context to infer from)
nums.map(|x| x * 2)               # inferred: x is int if nums: Vector<int>

nums.each do |n|                  # multi-line form
  puts n
end
```

A parameter may destructure instead of naming, which is what makes a
vector of pairs usable without unpacking it by hand first:

```ruby
counts.entries().sortBy(|(key, hits)| -hits)
```

`match` and `for` already bind through patterns; this is the same
binding, in the one position that used to require a name. The pattern
has to match every value the parameter can take — a lambda has no arms
to add — so a refutable one is an error that points you at binding the
parameter and writing the `match` yourself.

Lambdas capture their environment by reference (closures).

## Control flow

```ruby
if cond
  ...
elsif other
  ...
else
  ...
end

while cond
  ...
end

for item in collection    # iterates Vector, Map (pairs), Set, ranges, strings
  ...
end

for i in 0..10            # exclusive range: 0 to 9
for i in 0..=10           # inclusive range: 0 to 10
```

`if` is an expression: it evaluates to its last expression when all
branches type-check the same. `break` and `continue` in loops.

Inline form with `then` (single-expression branches):

```ruby
let sign = if n < 0 then -1 elsif n > 0 then 1 else 0 end
```

## Structs and methods

```ruby
import std::math

struct Point
  x: float
  y: float

  def dist(self, other: Point): float
    math.sqrt((self.x - other.x) ** 2 + (self.y - other.y) ** 2)
  end
end

let p = Point { x: 0.0, y: 0.0 }
let q = Point { x: 3.0, y: 4.0 }
puts p.dist(q)                      # 5.0
```

- No inheritance, no subtype polymorphism. Reuse is composition and free
  functions.
- `self` is explicit and mandatory as a method's first parameter.
- **Parentheses are mandatory in calls**: `p.dist(q)` calls, `p.x` accesses
  the field. There is no call without parentheses (no `v.len`-style Ruby).
- Structs live on the heap and are passed by reference; `==` compares
  structurally (by field value). Mutating fields (`p.x = 1.0`)
  does not require `let mut` — see doc 03, immutability belongs to the
  binding.

## Enums and pattern matching

```ruby
enum Shape
  Circle(radius: float)
  Rect(w: float, h: float)
  Point
end

let area = match shape
  Circle(r) => 3.14159 * r * r
  Rect(w, h) => w * h
  Point => 0.0
end
```

- `match` is an expression and is **exhaustive**: cover every case or
  use `_`.
- Patterns: enum constructors, literals, `_`, variable binding,
  guards (`Circle(r) if r > 1.0 =>`), tuples, and nesting.

`Option<T>` is a stdlib enum with sugar:

```ruby
enum Option<T>
  Some(value: T)
  None
end

let name = user.nickname ?? "anon"    # default when None
let len = user.nickname?.len()        # Option<int>: propagates None
match user.nickname
  Some(n) => puts n
  None => puts "no nickname"
end
```

## Interfaces (structural)

```ruby
interface Printable
  def toString(self): string
end

def log<T: Printable>(value: T)
  puts value.toString()
end
```

A struct satisfies an interface if it has methods with those signatures —
there is no `impl Printable for X`. The inline form `<T: { def toString(self): string }>`
is an anonymous interface; same semantics.

## Generics

```ruby
def first<T>(items: Vector<T>): Option<T>
  if items.len() > 0
    Some(items[0])
  else
    None
  end
end

def max<T: Comparable>(a: T, b: T): T
  if a > b then a else b end
end
```

Constraints are structural only (named or inline interfaces). No
unions (`<T: int | string>` does not exist; use an enum).

## Pipe operator

```ruby
lines
  .filter(|l| !l.startsWith("#"))
  .map(|l| l.trim())
  .join("\n")

readAll() |> parseConfig(defaults)
```

`a |> f(b, c)` is equivalent to `f(a, b, c)`: the pipe inserts the left-hand
side as the first argument of the target call. The target may be any callable
expression: `x |> foo.transform(y)` is equivalent to `foo.transform(x, y)`,
and a bare callable calls it with the piped value alone (`x |> foo.filter` is
`foo.filter(x)`). There is no method form — a method only ever exists off an
explicit receiver, so collection pipelines are written as method chains with
leading dots (first snippet above).

## Collections and literals

```ruby
let v: Vector<int> = [1, 2, 3]
let m: Map<string, int> = { "a": 1, "b": 2 }
let s: Set<int> = Set([1, 2, 3])

v.map(|x| x * 2).filter(|x| x > 2)
m["a"]                    # Option<int> — indexing a Map gives Option
v[0]                      # int — indexing a Vector out of range panics
```

## Tuples

```ruby
let pair = (1, "a")             # (int, string)
let nested = (1, (2, 3))        # (int, (int, int))
let one = (7,)                  # (int) — the comma is required
let grouped = (1 + 2) * 3       # grouping, not a tuple: no comma

let grid: Map<(int, int), string> = { (0, 0): "origin" }
grid[(0, 0)]                    # Some("origin")

match pair
  (0, s) => puts "zero #{s}"
  (n, s) => puts "#{n} #{s}"
end
```

- A **top-level comma** inside parentheses is what makes a tuple; `(a)`
  stays a grouped expression, so the one-element tuple is written
  `(a,)`. Tuple patterns and types do not need that comma (`(x)`,
  `(int)`): only expressions have a grouping form to disambiguate from.
- There are no zero-element tuples; the unit value is `unit`.
- Parentheses right after a callee are still an argument list, so pass a
  tuple with its own parentheses: `f((1, 2))`, not `f(1, 2)`.
- Tuples compare structurally and are `Hashable` when every element is,
  which is what makes them usable as `Map`/`Set` keys (doc 03).
- `toString` renders them in source form, keeping the comma at arity 1:
  `(1, "a")`, `(7,)`.

## Strings

- Always UTF-8; `char` is a Unicode scalar.
- Interpolation: `"total: #{count * 2}"`.
- Multi-line with `"""..."""`.
- Rich stdlib methods: `split`, `trim`, `startsWith`, `replace`,
  `match?` (regex), etc. See [05-stdlib.md](05-stdlib.md).

## Modules

```ruby
# file: utils.bras
pub def slugify(s: string): string ... end
def helper() ... end                        # private to the module

# file: main.bras
import std::fs                              # stdlib: `std::` prefix
import "utils.bras"                         # file relative to the importer
import "./sub/helpers.bras"

utils.slugify("Hello World")                # binding = last segment / file stem
fs.read("data.txt")
```

- A file is a module. Everything is private except `pub` (functions, structs,
  enums, interfaces, top-level `let`).
- Stdlib with `::` path: `import std::fs`, `import std::proc`. The binding
  in scope is the last segment (`fs`, `proc`).
- File imports with a string: `import "foo.bras"`, `import "./foo/bar.bras"` —
  resolved relative to the importing file. The binding is the stem
  (`bar.bras` → `bar`).
- **No selective import** (`import x.{y}` does not exist). All access is
  qualified: `utils.slugify(...)`.
- **Import cycles are a compile error** (`a.bras` imports
  `b.bras` which imports `a.bras`): top-level `let`s evaluate on
  import and a cycle has no sound order.
- Future (requires a project file): user-defined path aliases in
  `std::` style (e.g. `import lib::helpers`), and
  possibly `import ... as alias` for name collisions. Out of v1.

## Entry point and execution

- A module's top-level statements run **the first time it is
  imported** (once only, post-order DFS — dependencies
  first, Python semantics).
- The executed file runs its top level and, **if it defines `def main()`,
  main is invoked afterward** as the entry point. Without `main`, the
  top level is the entire program.
- Imported modules' `main` functions are NOT invoked — only the executed
  file's.

```ruby
# simple script: no main, the top level is the program
puts "hello"

# structured program: top-level for setup, main as the entry point
let config = load()

def main()
  run(config)
end
```

## Errors

Summary (the full design is in [04-errors.md](04-errors.md)):

```ruby
def fetch(url: string): string
  if !ok
    throw NetError { detail: "timeout" }
  end
  "<html>"
end

let page = fetch(url) catch (e)
  NetError => "recovered: #{e.detail}"
end
```

No `throws` in signatures (inferred), no `Result`, `catch` non-exhaustive
by default (unhandled cases re-throw automatically).

## Comments and separators

- `#` to end of line. No block comments in v1.
- Line breaks separate statements; there is no `;`.

> Canonical spec. A Spanish reading copy is mirrored in the Atlas workspace 'brasa'.
