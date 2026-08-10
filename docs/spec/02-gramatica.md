# Brasa — gramática formal

Notación: EBNF. `*` cero o más, `+` uno o más, `?` opcional, `|`
alternativa. Los tokens van en MAYÚSCULAS, las keywords entre comillas.
`NL` es uno o más saltos de línea (separador de sentencias).

## Léxico

```
IDENT      = [a-zA-Z_][a-zA-Z0-9_]* ("?" | "!")?     # convención: camelCase; pred? y mut!
TYPE_IDENT = [A-Z][a-zA-Z0-9_]*                       # tipos y constructores, PascalCase
INT        = [0-9][0-9_]* | "0x" [0-9a-fA-F_]+ | "0b" [01_]+     # sin octal
FLOAT      = [0-9][0-9_]* "." [0-9]+ ( "e" [+-]? [0-9]+ )?
STRING     = '"' ... '"'            # interpolación #{expr} ANIDABLE a cualquier
                                    # profundidad; escapes \n \t \" \\ \#
RAWSTRING  = '"""' ... '"""'        # multilínea, misma interpolación, SIN escapes
CHAR       = "'" scalar "'"
COMMENT    = "#" hasta fin de línea (fuera de string)
```

Keywords reservadas:

```
def end if then elsif else while for in match enum struct interface import
pub let mut return break continue throw throws catch catch_all never
true false self unit and or not
spawn                                  # reservada, sin semántica en v1
```

Operadores y puntuación:

```
+  -  *  /  %  **        aritmética (** = potencia)
== != < <= > >=          comparación
&& || !                  lógicos (alias: and, or, not)
=  += -= *= /= %=        asignación (solo sobre let mut)
|> ?. ?? .. ..= => ->    pipe, safe-nav, default, rangos, brazo match, retorno lambda tipada
( ) [ ] { } , : :: . | _
```

## Estructura

```
program     = NL? item ( NL item )* NL? EOF
item        = import | func_def | struct_def | enum_def | interface_def
            | top_let | stmt

import      = "import" ( std_path | STRING )
std_path    = IDENT ( "::" IDENT )+        # import std::fs; raíces alias en el futuro
                                            # STRING: path de archivo, relativo al importador

top_let     = "pub"? let_stmt
```

## Definiciones

```
func_def    = "pub"? "def" IDENT generics? "(" params? ")" ret? throws? NL block "end"
params      = param ( "," param )*
param       = "self" | IDENT ":" type
ret         = ":" type
throws      = "throws" ( "never" | TYPE_IDENT ( "|" TYPE_IDENT )* )
generics    = "<" gen_param ( "," gen_param )* ">"
gen_param   = TYPE_IDENT ( ":" constraint )?
constraint  = TYPE_IDENT                              # interface con nombre
            | "{" iface_member ( "," iface_member )* "}"   # interface anónima

struct_def  = "pub"? "struct" TYPE_IDENT generics? NL struct_body "end"
struct_body = ( field NL | func_def NL )*
field       = IDENT ":" type

enum_def    = "pub"? "enum" TYPE_IDENT generics? NL variant+ "end"
variant     = TYPE_IDENT ( "(" field ( "," field )* ")" )? NL

interface_def = "pub"? "interface" TYPE_IDENT generics? NL iface_member+ "end"
iface_member  = "def" IDENT "(" params? ")" ret? NL
```

## Sentencias

```
block       = ( stmt NL )*
stmt        = let_stmt | assign | return_stmt | break | continue
            | if_stmt | while_stmt | for_stmt | throw_stmt | expr

let_stmt    = "let" "mut"? IDENT ( ":" type )? "=" expr
assign      = lvalue assign_op expr
lvalue      = IDENT ( "." IDENT | "[" expr "]" )*
assign_op   = "=" | "+=" | "-=" | "*=" | "/=" | "%="

return_stmt = "return" expr?
throw_stmt  = "throw" expr

if_stmt     = "if" expr NL block
              ( "elsif" expr NL block )*
              ( "else" NL block )? "end"

(* forma inline, expresión de una línea; las ramas son UNA expresión *)
if_inline   = "if" expr "then" expr
              ( "elsif" expr "then" expr )*
              ( "else" expr )? "end"
while_stmt  = "while" expr NL block "end"
for_stmt    = "for" pattern "in" expr NL block "end"
```

## Expresiones

Precedencia de menor a mayor; todos los binarios asocian a izquierda salvo
`**` (derecha) y los rangos (no asociativos):

```
1   |>
2   ?? 
3   || / or
4   && / and
5   == !=
6   < <= > >=
7   .. ..=
8   + -
9   * / %
10  **
11  unarios: - ! not
12  postfix: llamada (), índice [], acceso ., safe-nav ?., catch
```

```
expr        = pipe_expr
pipe_expr   = coalesce ( "|>" pipe_target )*
pipe_target = call | "." IDENT "(" args? ")"          # a |> f(b) ; a |> .m(b)

primary     = INT | FLOAT | STRING | CHAR | "true" | "false" | "unit"
            | IDENT | "self"
            | "(" expr ")"
            | vector_lit | map_lit | struct_lit | lambda
            | if_expr | match_expr
            | TYPE_IDENT ( "(" args? ")" )?           # constructor de enum

postfix     = primary ( "(" args? ")"
                      | "[" expr "]"
                      | "." IDENT
                      | "?." IDENT
                      | catch_clause )*

vector_lit  = "[" ( expr ( "," expr )* )? "]"
map_lit     = "{" ( map_entry ( "," map_entry )* )? "}"
map_entry   = expr ":" expr
struct_lit  = TYPE_IDENT "{" ( IDENT ":" expr ( "," IDENT ":" expr )* )? "}"

lambda      = "|" lparams? "|" expr
            | "do" "|" lparams? "|" NL block "end"
lparams     = lparam ( "," lparam )*
lparam      = IDENT ( ":" type )?

match_expr  = "match" expr NL match_arm+ "end"
match_arm   = pattern ( "if" expr )? "=>" ( expr | NL block ) NL

pattern     = "_" | literal | IDENT
            | TYPE_IDENT ( "(" pattern ( "," pattern )* ")" )?
            | "(" pattern ( "," pattern )* ")"        # tupla
```

```
catch_clause = ( "catch" | "catch_all" ) "(" IDENT ")" NL catch_arm+ "end"
catch_arm    = catch_types ( "if" expr )? "=>" ( expr | NL block ) NL
catch_types  = ( TYPE_IDENT | "_" ) ( "|" TYPE_IDENT )*
```

Semántica completa en [04-errores.md](04-errores.md). `catch` es postfix
sobre expresión con la precedencia más alta del nivel 12; en contextos
donde ligaría mal (p. ej. dentro de un brazo de otro catch) se parentiza.

## Tipos

```
type        = TYPE_IDENT ( "<" type ( "," type )* ">" )?    # Vector<int>, Map<string, T>
            | "(" type ( "," type )* ")"                    # tupla
            | fn_type
fn_type     = "(" ( type ( "," type )* )? ")" "->" type     # (int, int) -> int
```

Primitivos: `int` (i64), `float` (f64), `bool`, `string`, `char`, `unit`.

## Ambigüedades conocidas y su resolución

| Caso | Resolución |
|------|------------|
| `{` de map literal vs struct literal vs bloque `do` | struct lit requiere `TYPE_IDENT {` previo; map lit solo en posición de expresión; bloques usan `do/end`, nunca llaves |
| `|` de lambda vs `||` lógico | `||` se lexea como or; lambda vacía se escribe `| |` → el lexer emite PIPE PIPE solo si hay espacio: **decisión: lambda sin parámetros usa `do ... end` o `|_|`**, se evita el caso |
| `<` de genéricos vs comparación en expresiones | los genéricos solo aparecen tras `def f` / `TYPE_IDENT` en posición de tipo; en expresión, `<` es siempre comparación (no hay turbofish en v1) |
| `if` expresión vs sentencia | mismo nodo; el checker le da tipo cuando todas las ramas coinciden |
| `puts` | no es keyword: es función de stdlib (`io.puts`, re-exportada al prelude) |
| `if` inline vs multilínea | el token después de la condición decide: `then` → forma inline (ramas de una expresión), `NL` → forma de bloques |
| llamada vs campo | los paréntesis son **obligatorios** en llamadas: `v.len()` llama, `p.x` es acceso a campo; no existe llamada sin paréntesis |
| sufijo `?`/`!` de idents vs operadores `?.` / `!=` | el operador gana: `foo?.bar` es SIEMPRE safe-nav (`foo` + `?.`), y `foo!=x` es siempre `foo != x`. El sufijo se absorbe en el ident en cualquier otro contexto (`isDir?`, `isDir?()`). Para encadenar sobre un predicado: `(x.valid?).toString()` |
| escapes en RAWSTRING | los raw strings son crudos de verdad: NO procesan escapes (`\n` es backslash+n literal); solo `#{` y el `"""` de cierre son especiales. Consecuencia: un `#{` literal no puede aparecer en un raw string — usá string normal con `\#{` |

Notas transversales:

- **Trailing commas permitidas** en toda lista separada por comas: args,
  params, literales de Vector/Map/Set, struct literals, genéricos.
- Los rangos producen valores de tipo `Range` (ver doc 03), no son
  sintaxis exclusiva de `for`.
