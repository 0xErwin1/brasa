# Brasa — sintaxis

Cómo se ve el lenguaje completo. La gramática formal está en
[02-gramatica.md](02-gramatica.md); este documento es la vista humana.

## Un programa

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
    |> filter(|r| r.stars >= min)
    |> sortBy(|r| -r.stars)
end

let repos = topRepos("repos.json", 100) catch (e)
  fs.NotFound => []
end

for repo in repos
  puts "#{repo.name}: #{repo.stars}"
end
```

## Declaraciones y mutabilidad

```ruby
let x = 5              # inmutable, tipo inferido (int)
let y: float = 1.5     # anotación opcional
let mut count = 0      # mutable
count = count + 1      # solo válido sobre `let mut`
count += 1             # azúcar de asignación compuesta
```

Reasignar un `let` o cambiar el tipo de una variable es error de compilación.
El shadowing (`let x = ...` de nuevo en un scope interno) está permitido.

## Funciones y lambdas

```ruby
def greet(name: string, times: int): string
  "hola " * times + name
end

pub def area(w: float, h: float): float   # pub = exportada del módulo
  w * h
end
```

- Anotaciones de parámetros obligatorias; retorno opcional (default `unit`).
- Retorno implícito: el valor del cuerpo es la última expresión. Existe
  `return` para salida temprana.

Lambdas — sintaxis Ruby de pipes, con tipos inferidos del contexto:

```ruby
let double = |x: int| x * 2       # anotada (sin contexto que infiera)
nums.map(|x| x * 2)               # inferida: x es int si nums: Vector<int>

nums.each do |n|                  # forma multilínea
  puts n
end
```

Las lambdas capturan su entorno por referencia (closures).

## Control de flujo

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

for item in collection    # itera Vector, Map (pares), Set, rangos, strings
  ...
end

for i in 0..10            # rango exclusivo: 0 a 9
for i in 0..=10           # rango inclusivo: 0 a 10
```

`if` es expresión: vale su última expresión cuando todas las ramas tipan
igual. `break` y `continue` en bucles.

Forma inline con `then` (ramas de una sola expresión):

```ruby
let sign = if n < 0 then -1 elsif n > 0 then 1 else 0 end
```

## Structs y métodos

```ruby
struct Point
  x: float
  y: float

  def dist(self, other: Point): float
    ((self.x - other.x) ** 2 + (self.y - other.y) ** 2).sqrt()
  end
end

let p = Point { x: 0.0, y: 0.0 }
let q = Point { x: 3.0, y: 4.0 }
puts p.dist(q)                      # 5.0
```

- Sin herencia, sin polimorfismo de subtipo. La reutilización es composición
  y funciones libres.
- `self` explícito y obligatorio como primer parámetro de un método.
- **Paréntesis obligatorios en llamadas**: `p.dist(q)` llama, `p.x` accede
  al campo. No existe llamada sin paréntesis (nada de `v.len` estilo Ruby).
- Los structs viven en el heap y se pasan por referencia; `==` compara
  estructuralmente (por valor de los campos). Mutar campos (`p.x = 1.0`)
  no requiere `let mut` — ver doc 03, la inmutabilidad es del binding.

## Enums y pattern matching

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

- `match` es una expresión y es **exhaustivo**: cubrir todos los casos o
  usar `_`.
- Patrones: constructores de enum, literales, `_`, binding de variables,
  guardas (`Circle(r) if r > 1.0 =>`), tuplas y anidamiento.

`Option<T>` es un enum de la stdlib con azúcar:

```ruby
enum Option<T>
  Some(value: T)
  None
end

let name = user.nickname ?? "anon"    # default si None
let len = user.nickname?.len()        # Option<int>: propaga None
match user.nickname
  Some(n) => puts n
  None => puts "sin apodo"
end
```

## Interfaces (estructurales)

```ruby
interface Printable
  def toString(self): string
end

def log<T: Printable>(value: T)
  puts value.toString()
end
```

Un struct satisface una interface si tiene los métodos con esas firmas — no
hay `impl Printable for X`. La forma inline `<T: { toString(): string }>`
es una interface anónima; misma semántica.

## Genéricos

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

Constraints solo estructurales (interfaces con nombre o inline). Sin
uniones (`<T: int | string>` no existe; usá un enum).

## Pipe operator

```ruby
lines
  |> filter(|l| !l.startsWith("#"))
  |> map(|l| l.trim())
  |> join("\n")
```

`a |> f(b, c)` equivale a `f(a, b, c)`: el pipe inserta el lado izquierdo
como primer argumento. Sobre métodos: `a |> .method(b)` equivale a
`a.method(b)` (útil para mezclar funciones libres y métodos en una cadena).

## Colecciones y literales

```ruby
let v: Vector<int> = [1, 2, 3]
let m: Map<string, int> = { "a": 1, "b": 2 }
let s: Set<int> = Set([1, 2, 3])

v.map(|x| x * 2).filter(|x| x > 2)
m["a"]                    # Option<int> — indexar un Map da Option
v[0]                      # int — indexar un Vector fuera de rango es panic
```

## Strings

- UTF-8 siempre; `char` es un scalar Unicode.
- Interpolación: `"total: #{count * 2}"`.
- Multilínea con `"""..."""`.
- Métodos ricos en stdlib: `split`, `trim`, `startsWith`, `replace`,
  `match?` (regex), etc. Ver [05-stdlib.md](05-stdlib.md).

## Módulos

```ruby
# archivo: utils.brs
pub def slugify(s: string): string ... end
def helper() ... end                        # privado al módulo

# archivo: main.brs
import std::fs                              # stdlib: prefijo std::
import "utils.brs"                          # archivo relativo al importador
import "./sub/helpers.brs"

utils.slugify("Hola Mundo")                 # binding = último segmento / stem del archivo
fs.read("data.txt")
```

- Un archivo es un módulo. Todo es privado salvo `pub` (funciones, structs,
  enums, interfaces, `let` de nivel superior).
- Stdlib con path de `::`: `import std::fs`, `import std::proc`. El binding
  en scope es el último segmento (`fs`, `proc`).
- Archivos con string: `import "foo.brs"`, `import "./foo/bar.brs"` —
  resuelto relativo al archivo que importa. El binding es el stem
  (`bar.brs` → `bar`).
- **No hay import selectivo** (`import x.{y}` no existe). Todo acceso es
  calificado: `utils.slugify(...)`.
- **Los ciclos de import son error de compilación** (`a.brs` importa
  `b.brs` que importa `a.brs`): los `let` de nivel superior se evalúan al
  importar y un ciclo no tiene orden sano.
- Futuro (requiere archivo de proyecto): aliases de path definidos por el
  usuario al estilo de `std::` (p. ej. `import lib::helpers`), y
  posiblemente `import ... as alias` para colisiones de nombre. Fuera de v1.

## Entry point y ejecución

- Los statements de nivel superior de un módulo corren **la primera vez
  que se importa** (una sola vez, orden DFS post-orden — las dependencias
  primero, semántica Python).
- El archivo ejecutado corre su top-level y, **si define `def main()`,
  main se invoca después** como entry point. Sin `main`, el top-level es
  el programa completo.
- Los `main` de módulos importados NO se invocan — solo el del archivo
  ejecutado.

```ruby
# script simple: sin main, el top-level es el programa
puts "hola"

# programa estructurado: top-level para setup, main como entry point
let config = load()

def main()
  run(config)
end
```

## Errores

Resumen (el diseño completo está en [04-errores.md](04-errores.md)):

```ruby
def fetch(url: string): string
  if !ok
    throw NetError { detail: "timeout" }
  end
  "<html>"
end

let page = fetch(url) catch (e)
  NetError => "recuperado: #{e.detail}"
end
```

Sin `throws` en las firmas (se infiere), sin `Result`, `catch` no exhaustivo
por defecto (lo no manejado se relanza solo).

## Comentarios y separadores

- `#` hasta fin de línea. Sin comentarios de bloque en v1.
- Los saltos de línea separan sentencias; no hay `;`.
