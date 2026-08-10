# Brasa — stdlib de scripting (bosquejo v1)

La stdlib es la razón de existir del lenguaje: el 60% del scripting es
manipulación de strings y buena parte del resto es llamar comandos. Este
documento fija módulos, superficie mínima y convenciones; las firmas
exactas se cierran módulo a módulo durante M4.

## Convenciones

- **La stdlib es nativa**: escrita en Rust y expuesta como builtins de la
  VM. No hay archivos `.brs` de stdlib que parsear en cada arranque;
  `Option` y `Json` son tipos conocidos por el compilador. Una capa fina en
  Brasa podría existir a futuro, nunca en el camino del arranque.
- Errores por `throw` (structs `*Error` por módulo: `fs.NotFound`,
  `proc.NonZeroExit`, `json.ParseError`); ausencia esperada por `Option`.
- Todo módulo se importa explícito (`import std::fs`) salvo el **prelude**:
  `puts`, `print`, `Option`/`Some`/`None`, `Vector`, `Map`, `Set`, rangos y
  los métodos de tipos primitivos están siempre disponibles.
- Nombres en `camelCase` (funciones, métodos, variables); tipos en
  `PascalCase`; predicados con `?` (`file.exists?`, `isDir?`).

## `string` (métodos del tipo, sin import)

Prioridad máxima.

- Corte y armado: `split`, `join`, `lines`, `chars`, `bytes`, `slice`,
  `repeat`, `reverse`.
- Limpieza: `trim`, `trimStart`, `trimEnd`, `padStart`, `padEnd`.
- Búsqueda: `contains?`, `startsWith?`, `endsWith?`, `find` (-> Option),
  `count`.
- Transformación: `replace`, `toUpper`, `toLower`, `toInt` (-> throw
  `ParseError`), `toFloat`.
- Regex integrada: `match?(re)`, `captures(re)` (-> Option<Vector<string>>),
  `replaceRe(re, with)`, `scan(re)`.

## `std::re`

Regex compiladas para reuso: `re.compile(pattern)`, tipo `Regex` con los
mismos métodos que recibe string. Sintaxis: la de `regex` de Rust (sin
backtracking, sin catastrofes).

## `std::fs`

- `read(path): string`, `readBytes`, `write`, `append`.
- `exists?`, `isDir?`, `isFile?`.
- `ls(path): Vector<string>`, `glob(pattern): Vector<string>`, `walk(path)`.
- `mkdir`, `mkdirAll`, `rm`, `rmAll`, `cp`, `mv`.
- `path` helpers: `join`, `base`, `dir`, `ext`, `abs`.
- Errores: `fs.NotFound`, `fs.Denied`, `fs.IoError`.

## `std::proc` — el reemplazo de bash

```ruby
import std::proc

let out = proc.run("git status --short")          # -> Output; throw si exit != 0
puts out.stdout

let r = proc.tryRun("grep -q foo bar.txt")       # -> Output, nunca lanza
if r.code == 0 ...

proc.run("wc -l") |> .stdin(texto)                # piping de stdin
proc.shell("ls *.brs | wc -l")                    # vía /bin/sh explícito
```

- `run` parsea el comando con splitting propio (sin shell: sin injection
  accidental); `shell` es el opt-in explícito a `/bin/sh -c`.
- `Output` = `{ stdout: string, stderr: string, code: int }`.
- `run` lanza `proc.NonZeroExit { output }` si code != 0 — el caso bash
  `set -e` por defecto, con `tryRun` como escape.
- `env.get(name): Option<string>`, `env.set`, `env.vars`, `args()`,
  `exit(code)`, `cwd()`, `cd(path)`.

## `std::json`

- `json.parse(s): Json` (-> throw `json.ParseError`), `json.stringify(v)`.
- `Json` es un enum (`Object(Map<string, Json>) | Array(Vector<Json>) |
  Str | Num | Bool | Null`) con azúcar de indexado que devuelve Option:
  `data["users"][0]["name"] ?? "anon"`.
- Puente tipado (`json.decode<T>(s)`) queda para v2.

## `std::io`

- `puts`, `print`, `eprint` (stderr), `readLine(): Option<string>`,
  `readAll(): string` (stdin completo — clave para filtros estilo Unix).

## `std::math`, `std::time`, `std::rand`

- `math`: `abs`, `min`, `max`, `floor`, `ceil`, `round`, `sqrt`, `pow`,
  constantes.
- `time`: `now()`, timestamps, `sleep(ms)`, formateo básico ISO-8601.
- `rand`: `int(range)`, `float()`, `choice(vector)`, `shuffle`.

## Colecciones (métodos, sin import)

- `Vector<T>`: `len`, `push`, `pop`, `map`, `filter`, `reduce`, `each`,
  `find`, `any?`, `all?`, `sort`, `sortBy`, `reverse`, `contains?`,
  `first`/`last` (-> Option), `zip`, `flatten`, `uniq`, `join`.
- `Map<K, V>`: `len`, `keys`, `values`, `entries`, `insert`, `remove`,
  `has?`, `get` (≡ `[k]`, -> Option), `merge`, `each`.
- `Set<T>`: `add`, `remove`, `has?`, `union`, `intersect`, `diff`.

## Fuera de v1

`http` (cliente), `csv`, `toml`/`yaml`, `crypto`/hashing, sockets,
concurrencia. Entran por demanda real después de M5.
