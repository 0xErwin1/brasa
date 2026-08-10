# Brasa — sistema de errores

> Estado: cerrado. Referencia: el modelo de BAML (rama `canary`,
> `baml_language/`), que está implementado y testeado — no solo anunciado.
> Donde BAML aún no shippeó (`catch_all`), Brasa decide por su cuenta.

Modelo: los errores son valores comunes que se lanzan (`throw`), los
error-sets de cada función se **infieren** (nunca se declaran), y `catch`
es un match **no exhaustivo** sobre esos errores. No existe `Result<T, E>`,
no existe `unwrap`, no existe el estado intermedio "propagá si falló".

## Filosofía

1. El happy path se escribe como si nada fallara.
2. Manejar un error es un diff mínimo: agregás un `catch`, no reescribís
   firmas ni envolvés retornos.
3. O manejás el error o no lo manejás — y si no, se propaga solo. Sin
   ceremonia intermedia.

## Lanzar

```ruby
struct NetError
  detail: string
end

def fetchPage(ok: bool): string
  if !ok
    throw NetError { detail: "timeout" }
  end
  "<html>"
end
```

- `throw` acepta **cualquier valor**: struct, enum, string, int. No hay
  clase base `Exception` ni jerarquías.
- Convención stdlib: structs con sufijo `Error` y campos con contexto.

## Inferencia del error-set

- El checker calcula para cada función el conjunto de tipos que puede
  lanzar: sus `throw` propios ∪ los error-sets de lo que llama, menos lo
  que atrapa. Es un análisis interprocedural de punto fijo (la recursión
  converge porque los sets solo crecen y son finitos).
- La firma escrita NO cambia: `def fetchPage(ok: bool): string`. El
  error-set es metadato derivado, visible por tooling (LSP hover, doc
  generada), no exigido por sintaxis.
- El set viaja entre módulos: si `a()` llama `b.helper()` que lanza
  `NetError`, el set de `a` incluye `NetError`.

`throws` **opcional y verificado** (adoptado de BAML): una función puede
declarar su contrato y el compilador lo chequea contra el cuerpo:

```ruby
def fetch(url: string): string throws NetError
  ...
end
```

- Si el cuerpo puede lanzar algo no declarado, error de compilación.
- `throws never` afirma que la función no lanza (puede paniquear igual).
- Los métodos de `interface` DEBEN declarar `throws` si lanzan: una
  interface es un contrato y los contratos no se infieren.
- Sin declaración, inferencia pura — el default para scripts.

## Atrapar: `catch` como match

`catch` se adosa a una expresión (típicamente una llamada):

```ruby
let page = fetchPage(ok) catch (e)
  NetError => "recuperado: #{e.detail}"
end
```

- Cada brazo matchea un TIPO de error; `e` queda ligado con ese tipo dentro
  del brazo (como un match sobre un enum anónimo del error-set).
- **No exhaustivo por defecto**: si la expresión puede lanzar 10 errores y
  manejás 1, los otros 9 se relanzan automáticamente.
- Los brazos deben tipar igual que la expresión (el `catch` produce el
  mismo tipo, es una expresión).
- Un brazo que matchea un tipo que la expresión NO puede lanzar es error de
  compilación ("brazo inalcanzable") — esto existe gracias a la inferencia
  del error-set.
- El binding `e` se re-tipa por brazo: dentro del brazo `NetError => ...`,
  `e` ES `NetError` (narrowing, como TS). Los brazos pueden agrupar tipos
  con `|`: `NetError | DnsError => ...` (dentro, `e` solo permite lo común
  a ambos). Esta es la ÚNICA aparición de uniones en el lenguaje, acotada
  a brazos de catch.
- `_ =>` como último brazo atrapa cualquier error restante — pero **nunca
  panics** (regla BAML, adoptada).
- El matching es **nominal**: `catch` distingue por el tipo declarado del
  valor lanzado, no por su forma.
- Re-lanzar envolviendo es un `throw` normal dentro del brazo:

```ruby
let cfg = load(path) catch (e)
  fs.NotFound => throw ConfigError { detail: "sin config", cause: e.toString() }
end
```

`catch` es un operador **postfix de expresión** (como en BAML): se adosa a
cualquier expresión, no existe la forma "bloque try". Para cubrir varias
sentencias, extraé una función — que es exactamente el patrón que el
diseño quiere incentivar.

## Exhaustividad opt-in

```ruby
let page = fetchPage(ok) catch_all (e)
  NetError => "..."
  ParseError => "..."
end
```

- `catch_all`: el compilador exige un brazo (o `_`) por CADA tipo del
  error-set inferido, y prohíbe brazos inalcanzables. Si mañana
  `fetchPage` lanza algo nuevo, este call site deja de compilar — es el
  punto: los bordes del programa (main, handlers) declaran "acá no pasa
  nada sin manejar". Nota: en BAML `catch_all` es keyword reservada aún sin
  semántica shippeada; Brasa la implementa con esta definición propia.

## Panics vs errores

Los panics son una **unión cerrada de la stdlib** (`panics.IndexOutOfBounds`,
`panics.DivisionByZero`, `panics.IntegerOverflow`,
`panics.AssertionFailed`, ...), separada del canal de errores. No aparecen en los error-sets y `_` no los atrapa. La única
forma de atrapar uno es **nombrarlo explícitamente** en un brazo:

```ruby
let x = items[i] catch (e)
  panics.IndexOutOfBounds => 0
end
```

(Esto reemplaza al `catch_all_panics` del diseño original — que en BAML ni
siquiera es keyword. Nombrar el panic exige la misma intencionalidad con un
mecanismo menos.)

| | Error (`throw`) | Panic |
|---|---|---|
| Origen | dominio: red, IO, parseo, validación | bug: índice fuera de rango, div/0, aserción |
| En el error-set | sí, inferido | no |
| Brazo con su tipo | lo atrapa | lo atrapa (opt-in explícito) |
| Brazo `_` | lo atrapa | NO lo atrapa |
| Sin manejar en main | mensaje + exit ≠ 0 | mensaje + stacktrace + exit ≠ 0 |

## Interacción con el resto del lenguaje

- **Lambdas**: su error-set fluye al de quien las invoca (`map(|x| parse(x))`
  hace que el `map` "lance" lo que lanza la lambda). Las funciones de orden
  superior son transparentes a errores.
- **Genéricos**: una función genérica que recibe `(T) -> R` hereda el
  error-set del argumento concreto en cada call site.
- **`for`/pipes**: un `throw` dentro corta la iteración/cadena y propaga,
  como cualquier expresión.
- **Interop con Option**: `Option` es para AUSENCIA esperada (clave que no
  está); `throw` es para FALLA de operación (no se pudo leer el archivo).
  La stdlib es consistente con esa línea.

## Decisiones tomadas con la referencia BAML a la vista

| Pregunta | Decisión | Fundamento |
|----------|----------|------------|
| Binding | `catch (e)` único, re-tipado (narrowed) por brazo | Es lo que BAML implementa y evita sintaxis nueva por brazo |
| ¿Catch sobre bloques? | No: postfix de expresión solamente | Igual que BAML; "extraé una función" es el patrón deseado |
| ¿Nominal o estructural? | Nominal | Los errores son identidad; BAML matchea clases nominalmente |
| `catch_all_panics` | Eliminado; los panics se atrapan nombrándolos | En BAML no existe como keyword; nombrar el tipo ya es opt-in explícito |
| `throws` explícito | Opcional y verificado; obligatorio en interfaces; `throws never` | Adoptado de TYPE_SYSTEM.md de BAML |
| Brazo `_` | Atrapa errores, nunca panics | Regla BAML testeada |

Riesgo conocido: BAML no documenta cómo maneja la recursión en la
inferencia ni la interacción con genéricos — ahí Brasa está sola. El plan
de M2 debe incluir tests de: recursión mutua, funciones de orden superior
con lambdas que lanzan, y genéricos con `(T) -> R` que lanza.
