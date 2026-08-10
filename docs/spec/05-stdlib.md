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
- Errors via `throw` (`*Error` structs per module: `fs.NotFound`,
  `proc.NonZeroExit`, `json.ParseError`); expected absence via `Option`.
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
- Transformation: `replace`, `toUpper`, `toLower`, `toInt` (-> throw
  `ParseError`), `toFloat`.
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

let out = proc.run("git status --short")          # -> Output; throws if exit != 0
puts out.stdout

let r = proc.tryRun("grep -q foo bar.txt")       # -> Output, never throws
if r.code == 0 ...

proc.run("wc -l") |> .stdin(text)                 # piping stdin
proc.shell("ls *.brs | wc -l")                    # via explicit /bin/sh
```

- `run` parses the command with its own splitting (no shell: no
  accidental injection); `shell` is the explicit opt-in to `/bin/sh -c`.
- `Output` = `{ stdout: string, stderr: string, code: int }`.
- `run` throws `proc.NonZeroExit { output }` if code != 0 — the bash
  `set -e` default behavior, with `tryRun` as the escape hatch.
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

## Out of v1

`http` (client), `csv`, `toml`/`yaml`, `crypto`/hashing, sockets,
concurrency. Added on real demand after M5.

> Canonical spec. A Spanish reading copy is mirrored in the Atlas workspace 'brasa'.
