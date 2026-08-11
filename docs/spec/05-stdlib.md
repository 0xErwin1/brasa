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

## `std::re`

Compiled regexes for reuse: `re.compile(pattern)`, `Regex` type with the
same methods received by string. Syntax: Rust's `regex` crate syntax
(no backtracking, no catastrophic cases).

## `std::fs`

- `read(path): string`, `readBytes`, `write`, `append`.
- `exists?`, `isDir?`, `isFile?`.
- `ls(path): Vector<string>`, `glob(pattern): Vector<string>`, `walk(path)`.
- `mkdir`, `mkdirAll`, `rm`, `rmAll`, `cp`, `mv`.
- `path` helpers: `join`, `base`, `dir`, `ext`, `abs`.
- Errors: `fs.NotFound`, `fs.Denied`, `fs.IoError`.

## `std::proc` — the bash replacement

```ruby
import std::proc

let out = proc.run(["git", "status", "--short"])   # -> Output; throws if exit != 0
puts out.stdout

let out = proc.run("git status --short")          # sugar: whitespace split only

let r = proc.tryRun(["grep", "-q", pattern, file]) # -> Output, never throws
if r.code == 0 ...

proc.run(["wc", "-l"]).stdin(text)                # piping stdin
proc.shell("ls *.brs | wc -l")                    # via explicit /bin/sh
```

- **The argv-array form is the primary API**: `run(Vector<string>)` passes
  arguments through untouched, so interpolated data (filenames with
  spaces, user input) can never split into extra arguments. The string
  form is sugar that splits on **whitespace only** — no quote handling, no
  escapes; it exists for literal commands typed by the author. Building a
  string command from variables is a bug, and the docs say so.
- `shell` is the explicit opt-in to `/bin/sh -c` — the only form where
  shell metacharacters mean anything.
- `Output` = `{ stdout: string, stderr: string, code: int }`.
- `run` throws `proc.NonZeroExit { output }` if code != 0 — the bash
  `set -e` default behavior, with `tryRun` as the escape hatch.
- **Environment**: children inherit the parent environment by default
  (scripting expects `PATH`, `HOME`, `SSH_AUTH_SOCK` to work). Overrides
  via `proc.run(cmd, env: { ... })`; `proc.runClean(cmd)` starts from an
  empty environment for the paranoid case.
- **PATH resolution**: an unqualified command name resolves through `PATH`
  only. A relative path (`./script.sh`) must be written as such — the
  current directory is never implicitly searched.
- `env.get(name): Option<string>`, `env.set`, `env.vars`, `args()`,
  `exit(code)`, `cwd()`, `cd(path)`.

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
