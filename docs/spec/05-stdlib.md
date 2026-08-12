# Brasa — scripting stdlib (v1 sketch)

The stdlib is the language's reason to exist: 60% of scripting is
string manipulation and most of the rest is calling commands. This
document fixes modules, minimum surface, and conventions; exact
signatures are closed module by module during M4.

## Conventions

- **The stdlib is native**: written in Rust and exposed as VM builtins.
  There are no `.brs` stdlib files to parse on every startup;
  `Option` and `Json` are types known to the compiler. A thin Brasa layer
  might exist in the future, never on the startup path.
- Errors via `throw`, named by their dotted module-qualified name
  (`string.ParseError`, `fs.NotFound`, `proc.NonZeroExit`,
  `json.ParseError`); expected absence via `Option`.
- Every module is imported explicitly (`import std::fs`) except the
  **prelude**: `puts`, `print`, `Option`/`Some`/`None`, `Vector`, `Map`,
  `Set`, ranges, and primitive type methods are always available.
- Names in `camelCase` (functions, methods, variables); types in
  `PascalCase`; predicates with `?` (`file.exists?`, `isDir?`).

## `string` (type methods, no import needed)

Highest priority.

- Cutting and assembling: `split`, `join`, `lines`, `chars`, `bytes`, `slice`,
  `repeat`, `reverse`.
- Cleanup: `trim`, `trimStart`, `trimEnd`, `padStart`, `padEnd`.
- Search: `contains?`, `startsWith?`, `endsWith?`, `find` (-> Option),
  `count`.
- Transformation: `replace`, `toUpper`, `toLower`, `toInt`, `toFloat`
  (both throw `string.ParseError` on parse failure; `int.toFloat` is a
  pure conversion).
- Built-in regex: `match?(re)`, `captures(re)` (-> Option<Vector<string>>),
  `replaceRe(re, with)`, `scan(re)`.

Signatures closed in M4 (BRS-31):

- `bytes` returns `Vector<int>`: the string's UTF-8 byte values
  (0..=255). Scalar-indexed views stay with `chars`/`slice`/`len`.
- `padStart(width, pad)` / `padEnd(width, pad)`: `width` counts Unicode
  scalars, like `len`. The pad string repeats cyclically and is
  truncated so the result lands exactly on `width`; a string already
  at or past `width`, or an empty `pad`, returns the string unchanged.
- `reverse` reverses Unicode scalars (no grapheme clusters), consistent
  with `chars`.
- The regex methods take the pattern as a plain string in Rust `regex`
  crate syntax. An invalid pattern throws the native
  `string.RegexError` — a recoverable scripting error, alongside
  `string.ParseError` in the closed native-error list.
- `captures(re)` returns `None` when nothing matches; on a match the
  vector holds the full match first (group 0), then every capture
  group in order, with the empty string for a group that did not
  participate.
- `replaceRe(re, with)` replaces every non-overlapping match; the
  replacement expands `$1`/`${name}` group references, with `$$` as a
  literal `$` (the `regex` crate's `replace_all` semantics).
- `scan(re)` returns every non-overlapping full match, in order.

## `int` and `float` (type methods, no import needed)

- `int.toFloat()`, `float.toInt()` (truncating), the universal
  `toString`.
- `toFixed(digits): string` on both — the number rendered with EXACTLY
  `digits` decimals, never in exponent form.

`toFixed` exists because the ordinary rendering cannot promise a shape.
`toString` and interpolation print the shortest representation that
round-trips, so the decimal count follows the value: `1000.0`,
`333.335`, `0.5`, `12.1`. That is right for showing a number and
useless for a column,
which needs every row the same width. `toFixed` puts the count in the
call, and alignment then composes with the string `padStart`:

```ruby
puts "#{share.toFixed(1)}%"
puts amount.toFixed(2).padStart(10, " ")
```

Rules:

- The rendering is of the value the `float` actually holds, not of the
  literal that produced it. `(2.675).toFixed(2)` is `"2.67"`, because
  `2.675` is really `2.67499999999999982…` and a shade under the
  halfway point. Any implementation that instead multiplies by
  `10^digits` and rounds the product answers `"2.68"` here, having
  introduced its own error; a decimal you did not type is not a decimal
  the value has.
- An **exact** tie rounds **away from zero**, so `(2.5).toFixed(0)` is
  `"3"` and `(0.125).toFixed(2)` is `"0.13"`. This is `math.round`'s
  rule, deliberately: one stdlib does not get two rounding rules. Exact
  ties are rarer than they look — a `float` is a binary fraction, so
  only values like `0.5`, `0.25`, and `0.125` land exactly halfway.
- `digits` outside `0..=17` is a programmer error and panics with
  `panics.AssertionFailed`; past 17 there is no information a `float`
  can back.
- On an `int` the fractional part is always zeros, computed exactly —
  `(5).toFixed(2)` is `"5.00"` for every `int`, including the extremes,
  with no float conversion in between.
- A magnitude too small to show renders as zero without a sign:
  `(-0.000001).toFixed(2)` is `"0.00"`, since `-0.00` in a column reads
  as a distinct value rather than as zero.
- `NaN`, `inf`, and `-inf` render as themselves: there is no decimal
  expansion to give them.

## `std::fs`

- `read(path): string`, `write`, `append`.
- `exists?`, `isDir?`, `isFile?`.
- `ls(path): Vector<string>`, `glob(pattern): Vector<string>`, `walk(path)`.
- `mkdir`, `mkdirAll`, `rm`, `rmAll`, `cp`, `mv`.
- `path` helpers: `join`, `base`, `dir`, `ext`, `abs`.
- Errors: `fs.NotFound`, `fs.Denied`, `fs.IoError`.

Signatures closed in M4 (BRS-33):

- **Error mapping**: OS failures map by `ErrorKind` — not-found raises
  `fs.NotFound`, permission-denied raises `fs.Denied`, and everything
  else raises `fs.IoError` carrying the OS message. Every
  filesystem-touching member (`read`, `write`, `append`, `ls`, `glob`,
  `walk`, `mkdir`, `mkdirAll`, `rm`, `rmAll`, `cp`, `mv`) can raise
  all three.
- `read(path): string` requires valid UTF-8 and raises `fs.IoError`
  otherwise — silently replacing bytes would corrupt data on a
  write-back. The sketched `readBytes` is deferred until a real
  consumer appears (it needs a byte-vector story on the write side
  too).
- `write(path, contents)` truncates-or-creates; `append(path,
  contents)` creates the file when missing, like `>>` in a shell.
  Neither creates parent directories — that is `mkdirAll`'s job.
- The predicates `exists?`, `isFile?`, `isDir?` follow symlinks and
  never throw: a path the OS refuses to stat (missing, denied, broken
  link) is simply `false`.
- `ls(path): Vector<string>` returns entry NAMES (not paths), sorted
  bytewise, without `.`/`..`. `walk(path): Vector<string>` returns
  PATHS (the argument joined with each relative path) of every
  non-directory entry — files and symlinks — recursively, sorted
  bytewise; symlinks are reported as leaf entries and never followed.
- `glob(pattern): Vector<string>` uses Rust `glob`-crate syntax (`*`,
  `?`, `[...]`, `**`), resolves relative patterns against the current
  directory, and returns the matched paths sorted bytewise. An invalid
  pattern raises `fs.IoError`.
- `mkdir` creates one directory (existing parent required, existing
  target raises `fs.IoError`); `mkdirAll` creates the whole chain and
  tolerates an existing target.
- `rm` removes a file, a symlink, or an EMPTY directory; `rmAll`
  removes a whole tree (or a single file) recursively. Both raise
  `fs.NotFound` on a missing path — a silent `rm -f` is a one-line
  `catch` away, defaulting to it would hide typos.
- `cp(from, to)` copies one regular file (directory sources raise
  `fs.IoError`); `mv(from, to)` renames, falling back to
  copy-plus-delete for a FILE crossing filesystems (directories
  crossing filesystems raise `fs.IoError`).
- The `path` helpers are `fs` members (`fs.join`, ... — no separate
  module) and are pure lexical string operations with Rust `std::path`
  semantics, throwing nothing: `join(a, b)` is binary and an absolute
  `b` replaces `a`; `base` is the final component (`""` for `/` and
  empty paths, trailing slashes ignored); `dir` is the parent (`""`
  when there is none — note `dir("a")` is `""`, not `.`); `ext` is the
  extension without the dot (`""` when none; dotfiles like `.bashrc`
  have no extension).
- `abs(path)` absolutizes a relative path against the current
  directory and lexically normalizes `.`/`..` — no symlink resolution,
  no existence requirement. Its only error is `fs.IoError` when the
  current directory itself is unreadable.

## `std::proc` — the bash replacement

```ruby
import std::proc

let out = proc.run(["git", "status", "--short"])   # -> Output; throws if exit != 0
puts out.stdout

let out = proc.run("git status --short")          # sugar: whitespace split only

let r = proc.tryRun(["grep", "-q", pattern, file]) # -> Output, no NonZeroExit
if r.code == 0 ...

let counted = proc.run(["wc", "-l"], text)        # optional trailing stdin
proc.shell("ls *.brs | wc -l")                    # via explicit /bin/sh
```

- **The argv-array form is the primary API**: `run(Vector<string>)` passes
  arguments through untouched, so interpolated data (filenames with
  spaces, user input) can never split into extra arguments. The string
  form is sugar that splits on **whitespace only** — no quote handling, no
  escapes; it exists for literal commands typed by the author. Building a
  string command from variables is a bug, and the docs say so.
- `shell` is the explicit opt-in to `/bin/sh -c` — the only form where
  shell metacharacters mean anything. Interpolating data into the
  command line is shell injection; use `run` with an argv array for
  anything built from variables.
- **PATH resolution**: an unqualified command name resolves through `PATH`
  only. A relative path (`./script.sh`) must be written as such — the
  current directory is never implicitly searched.

Signatures closed in M4 (BRS-32):

- `run(cmd)`, `run(cmd, stdin)`, `tryRun(cmd)`, `tryRun(cmd, stdin)`,
  `shell(cmdline)`, `shell(cmdline, stdin)` — every runner returns
  `Output` and takes an optional trailing `stdin: string` piped to the
  child. Without it the child reads an empty stdin (never the script's
  own stdin, so a forgotten filter argument can never hang the run).
  The earlier `proc.run(...).stdin(text)` sketch was incoherent — the
  process has already run by the time `.stdin` could apply — and is
  replaced by the optional argument.
- `Output` is a compiler-known record type with exactly the fields
  `stdout: string`, `stderr: string`, `code: int`. It is native: not
  user-constructible, not a pattern, no members beyond the fields and
  the universal `toString`. Both streams are captured fully and decoded
  as lossy UTF-8 (invalid bytes become U+FFFD).
- `code` is the child's exit code; a signal-terminated child reports
  `128 + signal` (the Unix shell convention).
- `run` and `shell` throw `proc.NonZeroExit` when `code != 0` — the
  bash `set -e` default behavior, with `tryRun` as the escape hatch.
  **v1 limitation**: a native error carries only its qualified name and
  a message (like `string.ParseError`), so `NonZeroExit`'s message
  embeds the command, the exit code, and the child's trimmed stderr
  when non-empty; the structured `proc.NonZeroExit { output }` payload
  is deferred until native errors can carry values.
- All three runners throw `proc.SpawnError` when the child cannot
  start: missing binary, permission denied, or an empty command.
  `tryRun` never throws `NonZeroExit`, but a process that never ran has
  no `Output`, so it does throw `SpawnError`.
- **Environment**: children inherit the parent environment (scripting
  expects `PATH`, `HOME`, `SSH_AUTH_SOCK` to work) plus every
  `env.set` override. The sketched `proc.run(cmd, env: { ... })` and
  `proc.runClean(cmd)` forms are dropped from v1: the language has no
  named arguments, and `env.set` covers the common case.

## `std::env`

Closed in M4 (BRS-32):

- `env.get(name): Option<string>` — the process environment plus
  `env.set` overrides; an unset or non-UTF-8 value is `None`.
- `env.set(name, value)` — sets an override visible to `env.get`,
  `env.vars`, and every child spawned through `std::proc`. Overrides
  live in a runtime overlay; the host process's own OS-level
  environment block is not mutated (unobservable from the language).
- `env.vars(): Map<string, string>` — the merged environment (process
  plus overrides), entries sorted by name for deterministic iteration;
  non-UTF-8 names and values are decoded lossily.
- `env.args(): Vector<string>` — the script's trailing CLI arguments.

Closed in M4 (BRS-33, together with `std::fs` — a failed `cd` needs
the `fs` error namespace):

- `env.cwd(): string` — the process working directory; raises
  `fs.IoError` when it cannot be read (deleted cwd).
- `env.cd(path)` — changes the REAL host-process working directory
  (`fs.NotFound`/`fs.Denied`/`fs.IoError` on failure): relative paths
  everywhere, spawned children, and `fs.abs` all follow. Single-
  threaded scripting semantics; a virtual cwd overlay was rejected as
  complexity without a consumer.

Still deferred: `env.exit(code)` needs a clean-exit signal threaded
through both backends and the CLI.

## `std::json`

- `json.parse(s): Json` (-> throw `json.ParseError`), `json.stringify(v)`.
- `Json` is an enum (`Object(Map<string, Json>) | Array(Vector<Json>) |
  Str | Num | Bool | Null`) with indexing sugar that returns Option:
  `data["users"][0]["name"] ?? "anon"`.
- A typed bridge (`json.decode<T>(s)`) is deferred to v2.

Signatures closed in M4 (BRS-34):

- `Json` is a compiler-known type: the name is predeclared like
  `Option` (usable in annotations without an import), while the module
  members need `import std::json`. In v1 the enum shape above is
  DESCRIPTIVE, not surface syntax — `Json` has no constructors and no
  patterns; every read goes through indexing and the accessors below.
  Building `Json` values from language data (and with it a
  `stringify` of arbitrary values) is deferred with the typed bridge.
- `json.parse(s): Json` throws `json.ParseError` on invalid input; the
  message carries the position (`cannot parse JSON: expected value at
  line 2 column 8`).
- **Indexing is total and Option-yielding**: `data[key]` (object
  member, `string`) and `data[ix]` (array element, `int`) return
  `Option<Json>` — a missing key, an out-of-range or negative
  position, or a node of the wrong kind is `None`, never a panic.
  Chains flatten: indexing an `Option<Json>` propagates `None`, so
  `data["users"][0]["name"]` needs no unwrapping along the way.
- **Accessors** (on `Json` and, flattening, on `Option<Json>` — a
  chain has no other way to terminate, since `Json` values cannot be
  constructed in the language): `asString(): Option<string>`,
  `asInt(): Option<int>`, `asFloat(): Option<float>`,
  `asBool(): Option<bool>`, `asArray(): Option<Vector<Json>>`,
  `asObject(): Option<Map<string, Json>>`, and `null?(): bool`. Every
  `as*` is `None` when the node is not that JSON kind; on a `None`
  receiver the `as*` accessors propagate `None` and `null?` is `false`
  (an absent member is not an explicit JSON `null`).
- **Numbers do not coerce**: `asInt` succeeds only for integral
  numbers representable as `int` (JSON `2.0` is a float); `asFloat`
  succeeds for every number. Equality is structural over the tree,
  so `1` and `1.0` differ.
- `json.stringify(v: Json): string` emits compact JSON with object
  keys in bytewise-sorted order — objects are held sorted, so
  `stringify`, `toString` (the same text, in every rendering
  position), and `asObject` iteration are all deterministic; the
  source document's member order is not preserved.
- `Json` is immutable after `parse`: assigning through an index
  (`data["a"] = x`) is a compile error.
- Representation note (non-normative): both backends share one
  immutable serde_json tree behind `Rc`; indexing hands out subtree
  copies, which is unobservable for an immutable value.

## `std::io`

- `puts`, `print`, `eprint` (stderr), `readLine(): Option<string>`,
  `readAll(): string` (full stdin — key for Unix-style filters).

Signatures closed in M4 (BRS-34):

- `io.puts(v)` and `io.print(v)` are the prelude printers exposed as
  module members (the spec lists them here; the prelude simply
  re-exports them). `io.eprint(v)` writes to the real process stderr
  with no trailing newline, mirroring `print`. All three take any
  single value via the universal `toString`.
- `io.readLine(): Option<string>` reads one line from the REAL
  process stdin, stripping one trailing `\n` (and a preceding `\r`);
  a final line without a newline still yields its content; end of
  input is `None`. `io.readAll(): string` returns the whole remaining
  stdin verbatim, newlines intact.
- Input decodes as lossy UTF-8 (invalid bytes become U+FFFD),
  consistent with `std::proc`'s output capture — a filter must never
  die on a stray byte.
- `std::io` has no error namespace in v1: an OS-level stdin read
  failure is treated as end of input (`readLine` yields `None`,
  `readAll` yields what was readable). A closed read end on any
  output stream (`EPIPE`) is a silent successful exit, like the
  prelude printers.
- Testing note: a run is wired to three streams rather than reaching
  for the process handles directly, so the library-level parity harness
  injects stdin and captures stderr, and pins `eprint` and the readers
  on every backend leg. CLI-level tests keep pinning the wiring itself —
  that `brasa` hands the REAL process streams to the run.

## `std::math`, `std::time`, `std::rand`

- `math`: `abs`, `min`, `max`, `floor`, `ceil`, `round`, `sqrt`, `pow`,
  constants.
- `time`: `now()`, timestamps, `sleep(ms)`, basic ISO-8601 formatting.
- `rand`: `int(range)`, `float()`, `choice(vector)`, `shuffle`.

Signatures closed in M4 (BRS-35):

- **`math`** — `sqrt`, `floor`, `ceil`, `round` take and return
  `float`; `pow(base: float, exp: float): float`. `abs`, `min`, and
  `max` are polymorphic over `int` and `float` (mixing the two in one
  call is a type error); `math.abs` of `int` minimum panics with
  `panics.IntegerOverflow`. The constants are `math.pi` and `math.e`
  (`float`) — plain value members, so calling them (`math.pi()`) is a
  "not callable" type error. Nothing in `math` throws; float members
  follow IEEE semantics (`sqrt(-1.0)` is NaN, never an error).
- **`time`** — `now(): float` is Unix epoch seconds with sub-second
  precision; `nowMillis(): int` is epoch milliseconds (the integer
  timestamp form). `sleep(ms: int)` sleeps at least `ms` milliseconds;
  a negative duration panics with `panics.AssertionFailed` (a
  programmer error, like the sortBy-NaN rule). `iso(epochMillis: int):
  string` is the basic ISO-8601 formatter: UTC,
  `YYYY-MM-DDTHH:MM:SS.mmmZ`, proleptic Gregorian, negative
  (pre-1970) timestamps supported. Nothing in `time` throws, and
  `now`/`nowMillis` read the wall clock (no monotonicity guarantee
  beyond the clock's own).
- **`rand`** — a per-run deterministic PRNG (xoshiro256\*\*, seeded
  through SplitMix64; documented, not cryptographic) shared by both
  backends, so a seeded sequence is reproducible everywhere.
  `seed(n: int)` resets the state deterministically; an unseeded run
  starts from clock entropy. `int(r: Range): int` is uniform over the
  range's values (`0..10` and `0..=10` both work); an empty range
  panics with `panics.AssertionFailed`. `float(): float` is uniform in
  `[0, 1)` (53-bit). `choice(v: Vector<T>): T` picks one element; the
  empty vector panics with `panics.AssertionFailed`.
  `shuffle(v: Vector<T>): Vector<T>` returns a NEW Fisher-Yates
  shuffled vector; the argument is not modified. Nothing in `rand`
  throws — the empty-pick cases are panics, not catchable errors.

## Collections (methods, no import needed)

- `Vector<T>`: `len`, `push`, `pop`, `map`, `filter`, `reduce`, `each`,
  `find`, `any?`, `all?`, `sort`, `sortBy`, `reverse`, `contains?`,
  `first`/`last` (-> Option), `zip`, `flatten`, `uniq`, `join`.
- `Map<K, V>`: `len`, `keys`, `values`, `entries`, `insert`, `remove`,
  `has?`, `get` (≡ `[k]`, -> Option), `merge`, `each`.
- `Set<T>`: `add`, `remove`, `has?`, `union`, `intersect`, `diff`.
  Built with the `Set(v: Vector<T>) -> Set<T>` constructor
  (`Set([1, 2, 3])`): a set of the vector's contents, deduplicated by
  structural equality, first occurrence kept, insertion order
  preserved. `Set(...)` is constructor-only; it is not a pattern.

Signatures closed in M4 (BRS-35):

- **`Vector<T>`** — `reduce(init: U, f: (U, T) -> U): U` folds left
  over a snapshot; the accumulator type comes from `init`.
  `find(f: (T) -> bool): Option<T>` yields the first satisfying
  element. `any?`/`all?` take a `(T) -> bool` predicate and
  short-circuit (`any?` on the first `true`, `all?` on the first
  `false`); the empty vector is `false`/`true` respectively.
  `sort(): Vector<T>` is a NEW vector in natural ascending order and
  exists only on orderable elements (`int`, `float`, `string`,
  `char` — the `sortBy` key rule); a NaN element panics with
  `panics.AssertionFailed`, like a NaN `sortBy` key.
  `zip(other: Vector<U>): Vector<(T, U)>` pairs up to the shorter
  length (the longer vector's leftovers are dropped).
  `flatten(): Vector<T>` exists only on `Vector<Vector<T>>` and
  removes exactly one nesting level. `uniq(): Vector<T>` deduplicates
  by structural equality, first occurrence kept, order preserved (the
  `Set` constructor's rule). All of these return NEW vectors; only
  `push`/`pop` mutate, as before.
- **`Map<K, V>`** — `entries(): Vector<(K, V)>` in insertion order
  (the same order as `keys`/`values`). `merge(other: Map<K, V>):
  Map<K, V>` is a NEW map holding the receiver's entries then the
  argument's, the argument winning on duplicate keys; neither operand
  is modified. `each(f: (K, V) -> unit)` iterates a snapshot in
  insertion order — the function takes the key and value as two
  parameters, not a tuple.
- **`Set<T>`** — `union`, `intersect`, and `diff` take a `Set<T>` and
  return NEW sets; neither operand is modified. Order: `union` is the
  receiver's elements then the argument's unseen elements (each side
  in its insertion order); `intersect`/`diff` keep the receiver's
  order.
- **Higher-order arguments** — every HOF above participates in
  error-set HOF transparency: a literal lambda argument contributes
  its own error-set (`docs/spec/04-errors.md`), like `map`/`filter`.
- **Prelude (verified complete)** — always available without imports:
  `puts`, `print`, `Option`/`Some`/`None`, the `Vector`/`Map`/`Set`
  types with the `Set(...)` constructor, ranges, every primitive and
  collection method above, and the predeclared `Json` type name
  (BRS-34; the `json` module members still need `import std::json`).

## Out of v1

`http` (client), `csv`, `toml`/`yaml`, `crypto`/hashing, sockets,
concurrency. Added on real demand after M5.

Also `std::re`. Regex is a string feature here — `match?`, `captures`,
`replaceRe`, and `scan` above — and patterns are already compiled once
and cached per run, so a `Regex` value would buy no reuse that the
string methods do not already get. It would cost a compiler-known type:
a `BuiltinType`, a resolver entry, a member table, a value
representation, `toString`, `==`, and an implementation in both
backends. Revisit if something needs a pattern as a first-class value.

> Canonical spec. A Spanish reading copy is mirrored in the Atlas workspace 'brasa'.
