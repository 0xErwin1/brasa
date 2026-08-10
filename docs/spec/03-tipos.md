# Brasa — sistema de tipos

Estático, fuerte, con inferencia local y compatibilidad estructural para
interfaces. Sin coerciones implícitas de ningún tipo: `int` no se convierte
solo a `float`, nada es "truthy".

## Tipos

| Categoría | Tipos |
|-----------|-------|
| Primitivos | `int` (i64), `float` (f64), `bool`, `string`, `char`, `unit` |
| `Range` | valor de primera clase (dos `int` + flag inclusivo), por valor, lazy: iterarlo no materializa nada. NO es azúcar de `Vector<int>` — `0..10_000_000` ocupa 17 bytes, no 80MB. Lo consumen `for`, `slice`, `rand.int` |
| Compuestos nominales | `struct`, `enum` (incluye `Option<T>`) |
| Compuestos estructurales | tuplas `(A, B)`, funciones `(A, B) -> C`, interfaces |
| Genéricos | `Vector<T>`, `Map<K, V>`, `Set<T>`, structs/enums/funciones parametrizadas |

Valor vs referencia:

- Primitivos y tuplas: por valor.
- Structs, enums con payload, colecciones, strings, closures: referencias a
  heap con GC. `==` es SIEMPRE estructural (compara contenido, no
  identidad); no hay operador de identidad en v1.

## Inferencia

Inferencia **local** (estilo Rust/Swift, no Hindley-Milner global):

- Las firmas de función son la frontera: parámetros anotados siempre,
  retorno anotado o `unit`. Dentro del cuerpo todo se infiere.
- `let x = expr` toma el tipo de `expr`. `let x: T = expr` chequea contra
  `T` (necesario cuando el lado derecho es ambiguo, p. ej. `[]`).
- Los parámetros de lambda se infieren del contexto esperado
  (`nums.map(|x| ...)` sabe que `x: int`); sin contexto, se anotan.
- Los argumentos de tipo genéricos se infieren en el call site
  (`first([1,2])` infiere `T = int`); no hay sintaxis para pasarlos
  explícitos en v1.

Regla de oro: ningún tipo se infiere "a distancia" — leer una función nunca
requiere mirar sus call sites.

## Variables

- `let` fija el tipo para siempre; reasignar es error (es inmutable).
- `let mut` permite reasignación **del mismo tipo**.
- Shadowing permitido en scopes internos, incluso con otro tipo.
- **La inmutabilidad es de la *variable*, no del valor** (decisión cerrada,
  semántica `const` de JS): `let` prohíbe re-apuntar el binding, nada más.
  `let v = [1]` permite `v.push(2)`; `let p = Point {...}` permite
  `p.x = 1.0`. Los valores heap son mutables siempre, sin `let mut`.
  Razón: con referencias compartidas y sin ownership, exigir `mut` para
  mutar interiores sería una garantía mentirosa (otro alias muta igual);
  prometemos exactamente lo que podemos cumplir.

## Interfaces estructurales

```ruby
interface Comparable
  def cmp(self, other: Self): int
end
```

- Un tipo `T` satisface una interface si tiene todos los métodos con firmas
  compatibles. No hay declaración de conformidad.
- `Self` en una interface refiere al tipo que la satisface (permite
  `cmp(self, other: Self)` en vez de fijar el tipo).
- Las constraints de genéricos (`<T: Comparable>`) son el ÚNICO lugar donde
  se usan interfaces en v1: no hay valores de tipo interface (sin dynamic
  dispatch, sin `Vector<Printable>`). Eso mantiene la VM simple y evita
  vtables. Si se necesita heterogeneidad, se usa un enum.
- Interfaces de la stdlib: `Comparable` (`<`, `>`, `sort`), `Printable`
  (`toString`, interpolación), `Hashable` (claves de Map/Set).
- **Los primitivos satisfacen las interfaces de stdlib de forma nativa**:
  el checker sabe (built-in, sin directivas) que `int`/`float`/`string`/
  `char` son `Comparable`, que todo tipo es `Printable`, etc. Sin esto,
  `max<T: Comparable>(1, 2)` no tiparía.
- **`Hashable` en v1 es cerrado**: solo `int`, `string`, `char`, `bool` y
  tuplas de esos. Structs y colecciones NO son claves de Map/Set — son
  referencias mutables con igualdad estructural, y mutar una clave después
  de insertarla corrompería la tabla. `float` tampoco (NaN).
- **`for` itera solo tipos built-in en v1**: Vector, Map, Set, rangos y
  string. Un `Iterable` definido por el usuario requiere dispatch dinámico,
  que no existe en v1; queda para v2 junto con los valores de interface.

## Operadores y sus reglas

| Operador | Regla |
|----------|-------|
| `+ - * / % **` | `int×int -> int`, `float×float -> float`; sin mezclas (convertí explícito: `x.toFloat()`) |
| `+` | también `string + string` |
| `== !=` | ambos lados del mismo tipo; estructural |
| `< <= > >=` | `int`, `float`, `string`, `char`, o `T: Comparable` |
| `&& \|\| !` | solo `bool`; cortocircuito |
| `??` | `Option<T> ?? T -> T` (también encadenable con otro `Option<T>`) |
| `?.` | `Option<T>?.metodo(...)` -> `Option<R>`; se aplana (no anida Options) |
| `\|>` | `a \|> f(b)` ≡ `f(a, b)`; puro azúcar sintáctico, se resuelve en el parser |
| `[i]` en Vector | `int -> T`; fuera de rango es **panic** (no Option: el caso común es un bug) |
| `[k]` en Map | `K -> Option<V>`; la clave ausente es un caso normal, no un bug |

## Números y strings: reglas finas

- **Overflow de `int` es panic** (`panics.IntegerOverflow`), coherente con
  "bug = panic". Sin wrapping silencioso.
- **`float` es IEEE 754**: `1.0 / 0.0` es `inf` (solo la división entera
  por cero panickea), `NaN != NaN`, y `float` no es `Hashable` ni clave de
  Map. `Comparable` sobre floats sigue IEEE (NaN no ordena; `sort` de
  floats con NaN es panic `panics.AssertionFailed`).
- **Strings no se indexan**: `s[i]` no existe (por byte es una trampa
  UTF-8, por char es O(n) disfrazado de O(1)). Se usa `chars()`, los
  métodos de alto nivel, y `slice(from, to)` con índices de char,
  documentado O(n).
- **`toString` implícito**: todo tipo tiene una representación derivada
  automáticamente (structs como `Point { x: 1.0, y: 2.0 }`, enums como
  `Circle(1.0)`), usada por `puts`, la interpolación y el `Printable`
  nativo. Definir `toString` propio en un struct la reemplaza. Los floats
  siempre muestran el punto decimal (`1.0`, nunca `1`) — la separación
  int/float se mantiene visible.

## `if` y `match` como expresiones

- `match` es expresión siempre; exhaustivo siempre; todas las ramas deben
  tipar igual (o el match completo se usa como sentencia y tipa `unit`).
- `if` es expresión cuando hay `else` y las ramas coinciden; si no, `unit`.

## Exhaustividad y flujo

- El checker de exhaustividad de `match` entiende enums, bools, tuplas y
  patrones anidados; para `int`/`string` exige `_`.
- `return`, `throw`, `break`, `continue` tipan como `never` (compatible con
  todo), así `let x = if ok then v else return end` funciona.

## Option

- `Option<T>` es un enum normal de la stdlib con azúcar (`?.`, `??`) y
  pattern matching. No existe `nil`, ni referencia nula, ni default
  implícito. Un campo de struct sin valor posible se declara `Option<T>` y
  el constructor obliga a pasarlo (`None` explícito).

## Genéricos: modelo de ejecución

El chequeo es completamente estático, pero la VM ejecuta una representación
uniforme (valores tagged), así que **no hay monomorfización**: `first<T>`
es una sola función en bytecode. Consecuencias:

- Sin explosión de código ni costo de compilación por instanciación.
- El dispatch de métodos vía constraint se resuelve en el checker (que ya
  sabe el tipo concreto en cada call site) y se emite como llamada directa
  cuando es posible, o mediante una tabla de métodos del valor cuando no.
- Nada de esto es observable en el lenguaje; es detalle de implementación
  con libertad para cambiar.
