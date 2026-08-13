# Brasa — scripting stdlib (v1 sketch)

The stdlib is the language's reason to exist: 60% of scripting is
string manipulation and most of the rest is calling commands. This
document fixes modules, minimum surface, and conventions; exact
signatures are closed module by module during M4.

## Conventions

- **The stdlib is native**: written in Rust and exposed as VM builtins.
  There are no `.bras` stdlib files to parse on every startup;
  `Option` and `Json` are types known to the compiler. A thin Brasa layer
  might exist in the future, never on the startup path.
- Errors via `throw`, named by their dotted module-qualified name
  (`string.ParseError`, `fs.NotFound`, `proc.NonZeroExit`,
  `json.ParseError`); expected absence via `Option`.
- Every module is imported explicitly (`import std::fs`) except the
  **prelude**: `puts`, `print`, `assert`, `assertEq`,
  `Option`/`Some`/`None`, `Vector`, `Map`,
  `Set`, ranges, and primitive type methods are always available.

## Assertions (prelude, no import needed)

```ruby
assert x > 0
assertEq slugify("Hola Mundo"), "hola-mundo"
```

- `assert(cond: bool)` and `assertEq(a: T, b: T)`. `assertEq` compares
  two values of the SAME type — the rule `==` follows, because that is
  the comparison it performs — so a mismatch is a compile error, not a
  test that always fails.
- A failing assertion raises `panics.AssertionFailed`
  ([04-errors.md](04-errors.md)). It is a panic, not an error: it does
  not enter an error-set and `_` does not catch it.
- They are **prelude functions, not test-only syntax**. An assertion is
  useful in a script too, and a second vocabulary for one idea is worse
  than either. Inside a `test` item the panic is what the runner reports
  as a failure; outside one it aborts the run like any other panic.
- The detail names the assertion, not the operands. Rendering them would
  mean calling `toString` on the failure path, and a failing assertion is
  exactly where an unreliable `toString` must not run — the stack trace
  locates it.
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
- `ls(path): Vector<string>`, `glob(pattern): Vector<string>`,
  `walk(path)`, `walk(path, prune)`, `tryWalk(path)`,
  `tryWalk(path, prune)`.
- `mkdir`, `mkdirAll`, `rm`, `rmAll`, `cp`, `mv`.
- `path` helpers: `join`, `base`, `dir`, `ext`, `abs`, `resolve`.
- `isSymlink?`.
- Errors: `fs.NotFound`, `fs.Denied`, `fs.IoError`.

Signatures closed in M4 (BRS-33):

- **Error mapping**: OS failures map by `ErrorKind` — not-found raises
  `fs.NotFound`, permission-denied raises `fs.Denied`, and everything
  else raises `fs.IoError` carrying the OS message. Every
  filesystem-touching member (`read`, `write`, `append`, `ls`, `glob`,
  `walk`, `tryWalk`, `mkdir`, `mkdirAll`, `rm`, `rmAll`, `cp`, `mv`)
  can raise all three — `tryWalk` only for its root, as below.
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
  link) is simply `false`. `isSymlink?` is the one that does NOT
  follow — its whole job is to answer about the path rather than about
  its target — and it never throws either.
- **`abs` is lexical, `resolve` is not.** `abs` normalizes `.`/`..`
  without touching an inode and accepts a path that does not exist.
  `resolve(path): string` follows every symlink and requires the path
  to exist, so it answers where a path actually LEADS rather than where
  it reads as leading. Both are needed and they are not
  interchangeable: a containment check written on `abs` is wrong, since
  a path under the root can be a link out of it and still pass —

  ```ruby
  def contained?(root: string, candidate: string): bool
    let r = fs.resolve(root)
    let c = fs.resolve(candidate)
    c == r || c.startsWith?(r + "/")
  end
  ```

  A dangling link raises `fs.NotFound` (there is no real path to
  report) and a symlink loop raises `fs.IoError`, the OS's own answer.
  Two caveats worth stating rather than papering over: `resolve`
  requires existence, so for a not-yet-created target resolve the
  parent and join; and the check is still TOCTOU — a link swapped
  between the check and the open defeats it, and nothing at this layer
  can fix that.
- `ls(path): Vector<string>` returns entry NAMES (not paths), sorted
  bytewise, without `.`/`..`. `walk(path): Vector<string>` returns
  PATHS (the argument joined with each relative path) of every
  non-directory entry — files and symlinks — recursively, sorted
  bytewise; symlinks are reported as leaf entries and never followed.

  Bytewise means the encoded bytes, not the rendered text. That matters
  only for a name holding bytes that are not valid UTF-8: every such
  byte renders as the same replacement character, so ordering the
  rendered strings would order those names by a character none of them
  contains. `ls` and `walk` sorted the rendered strings until BRS-66 and
  now sort the bytes, which is what this sentence always said and what
  `tryWalk` does — the members must agree on the same tree.

  `glob` is the exception, and by omission rather than by order: the
  matcher behind it compares names as text, so an entry whose name is
  not valid UTF-8 is never matched at all. Its ordering promise holds
  because nothing that could test it ever reaches the sort.
- `walk(path, prune)` takes an optional trailing `Vector<string>` of
  directory NAMES to skip, along with everything beneath them. Without
  it a walk of any real repository is a walk of its object store, which
  is why every script that tried hand-rolled its own descent instead.

  The rules, each chosen so the member stays predictable:

  - Matching is on the entry NAME, exactly, no globbing. Names are the
    vocabulary `ls` already speaks, and `glob` is the member for
    patterns.
  - It prunes DIRECTORIES. A pruned name that is a file is still
    returned, because pruning is about subtrees and a file has none.
  - The root is never pruned, even when its own base name is in the
    list: pruning the argument you just passed is a mistake, not a
    request.
  - An empty list is exactly the one-argument form.

  A predicate would be more expressive — prune by full path, by depth —
  but it would be the first higher-order argument in the module
  surface, and every higher-order member is committed to error-set
  transparency, which means threading a lambda's error set through the
  collector. A name list needs none of that and covers what real
  scripts ask for. Widening `prune` to accept a predicate later is
  source-compatible.

- `tryWalk(path)` / `tryWalk(path, prune)` is the best-effort form,
  returning the compiler-known `Walk` record with exactly the fields
  `paths: Vector<string>` and `unreadable: Vector<string>`, both sorted
  bytewise. It takes the same arguments and prunes by the same rules.

  The pairing is `proc.run` / `proc.tryRun`: the strict member throws
  and the tolerant one hands the failure back as data. `walk` aborting
  on the first unreadable directory is deliberate — a short list
  presented as a complete one is how a backup script loses files with
  nobody noticing — and pruning cannot substitute, because a script
  cannot know a directory is unreadable before trying it.

  What `tryWalk` does NOT do is skip quietly: `unreadable` names every
  place it could not reach, so a best-effort caller can still report
  what it missed. That is the difference from Python's `os.walk`, whose
  default swallows the failure entirely.

  Each place appears once, so `unreadable.len()` is a count of skipped
  locations. A directory it could not open stands for its whole
  subtree; an entry it could not stat is named itself when it has a
  path, and by its parent when it does not (a failed directory entry
  carries no path of its own).

  A pruned name can appear in `unreadable`, because pruning is decided
  from an entry's kind and that is exactly what could not be read. The
  alternative is to guess that an unstattable entry is the directory
  the caller wanted skipped, and a wrong guess drops a subtree while
  reporting nothing.

  The root is the one thing `tryWalk` does not tolerate: a root that is
  absent or unreadable raises exactly what `walk` would. Asking to
  traverse something that is not there is a different mistake from
  reaching a corner of a tree that is closed.

  That rule is about opening the root. Once it is open, an entry
  directly under it that cannot be listed is named by its parent, so
  the root can appear in `unreadable` — meaning "some entries here
  could not be listed", never "the root could not be opened", which
  raises instead.

  `Walk`, like `Output`, is native: not user-constructible, not a
  pattern, no members beyond the two fields and the universal
  `toString`. Unlike `Output`, whose fields are scalars, its two fields
  are `Vector`s — and a `Vector` is a shared mutable reference, so
  reading a field hands back the record's own vector rather than a
  copy. Pushing into it is the ordinary meaning of pushing into a
  vector you hold; it does not corrupt the record, but the counts above
  describe what `tryWalk` returned, not what a caller left there.

  A consequence worth knowing before you hit it: a `catch` arm cannot
  produce one, so a `tryWalk` call that needs catching is wrapped in a
  function returning something you can build.
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
proc.shell("ls *.bras | wc -l")                   # via explicit /bin/sh

let outs = proc.tryRunAll(jobs, 8)                # bounded parallelism
```

- `tryRunAll(commands: Vector<Vector<string>>, limit?: int)` runs every
  command with at most `limit` running at once and returns the `Output`
  values **in input order**, whatever order they finished in. `limit`
  defaults to the machine's parallelism and is clamped to at least one:
  an unbounded fan-out is never offered, because the caller is
  processing a list whose length it does not control.

  It is tolerant like `tryRun`, one step further: a non-zero exit is
  data, not a throw. A batch that aborted on the first failure would
  have already paid for the work it then discarded, and the codes are
  what the caller asked for. A child that cannot START is still
  `proc.SpawnError` — an environment failure is not a result.

  Commands are argv arrays only; the whitespace-split string sugar
  exists for a literal command an author typed, and a batch is built
  from data. There is no piped stdin: a shared input would have to be
  split or duplicated across children, and neither is a choice this
  member should make for the caller.

  **This is not concurrency.** No Brasa value crosses a thread — the
  commands go out as argv arrays and the results come back as data after
  every child has exited — so the VM and the collector stay
  single-threaded and no interleaving is observable in the language
  ([00-vision.md](00-vision.md): concurrency is out of v1). It is the
  `xargs -P` a bash replacement cannot do without.

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

- `env.exit(code)` ends the run with `code` as the process status and
  prints nothing: a chosen exit is not a failure to report. It is the
  way a CLI-shaped script says "failed" without saying "crashed" —
  before it existed, the only way to leave with a nonzero status was to
  throw, which printed a runtime banner and always chose 70.

  Three rules make it usable:

  - **It is not catchable.** A `_` arm is written for domain failures;
    letting it swallow a deliberate exit would make the exit a
    suggestion. Mechanically this falls out of the design rather than
    being enforced: handler unwinding tests for errors and panics, so
    a distinct exit signal passes every `catch` by construction.
  - **Output written before it still arrives.** The run unwinds and
    the CLI flushes as usual, rather than the builtin calling the
    host's exit and dropping whatever is buffered.
  - **`code` outside `0..=255` panics** with `panics.AssertionFailed`.
    A process status is one byte; truncating `exit(256)` to `0` would
    turn a mistake into a silent success, which is the accident this
    member exists to remove.

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

Every builtin that takes a callback — `map`, `filter`, `each`,
`reduce`, `find`, `any?`, `all?`, `sortBy`, and `Map.each` — traverses a
snapshot of the receiver taken before the first call. The callback may
mutate the receiver; that changes neither which elements are visited nor
their order, and the receiver is never read again after the snapshot.

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
