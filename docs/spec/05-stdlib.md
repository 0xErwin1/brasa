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

## `std::re`

Compiled regexes for reuse: `re.compile(pattern)`, `Regex` type with the
same methods received by string. Syntax: Rust's `regex` crate syntax
(no backtracking, no catastrophic cases).

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

## `std::io`

- `puts`, `print`, `eprint` (stderr), `readLine(): Option<string>`,
  `readAll(): string` (full stdin — key for Unix-style filters).

## `std::math`, `std::time`, `std::rand`

- `math`: `abs`, `min`, `max`, `floor`, `ceil`, `round`, `sqrt`, `pow`,
  constants.
- `time`: `now()`, timestamps, `sleep(ms)`, basic ISO-8601 formatting.
- `rand`: `int(range)`, `float()`, `choice(vector)`, `shuffle`.

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

## Out of v1

`http` (client), `csv`, `toml`/`yaml`, `crypto`/hashing, sockets,
concurrency. Added on real demand after M5.

> Canonical spec. A Spanish reading copy is mirrored in the Atlas workspace 'brasa'.
