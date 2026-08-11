# Brasa — formal grammar

Notation: EBNF. `*` zero or more, `+` one or more, `?` optional, `|`
alternative. Tokens are UPPERCASE, keywords are quoted.
`NL` is one or more line breaks (statement separator).

## Lexicon

```
IDENT      = [a-zA-Z_][a-zA-Z0-9_]* ("?" | "!")?     # convention: camelCase; pred? and mut!
TYPE_IDENT = [A-Z][a-zA-Z0-9_]*                       # types and constructors, PascalCase
INT        = [0-9][0-9_]* | "0x" [0-9a-fA-F_]+ | "0b" [01_]+     # no octal
             (* underscore placement is lenient: 1_, 1__000 accepted;
                0x_/0b_ with no digits is an error *)
FLOAT      = [0-9][0-9_]* "." [0-9]+ ( "e" [+-]? [0-9]+ )?
STRING     = '"' ... '"'            # interpolation #{expr} NESTABLE to any
                                    # depth; escapes \n \t \" \\ \#
RAWSTRING  = '"""' ... '"""'        # multi-line, same interpolation, NO escapes
CHAR       = "'" scalar "'"
COMMENT    = "#" to end of line (outside a string)
```

Reserved keywords:

```
def end if then elsif else while for in do match enum struct interface
import pub let mut return break continue throw throws catch catch_all
never true false self unit and or not
spawn                                  # reserved, no semantics in v1
```

Operators and punctuation:

```
+  -  *  /  %  **        arithmetic (** = power)
== != < <= > >=          comparison
&& || !                  logical (aliases: and, or, not)
=  += -= *= /= %=        assignment (only on let mut)
|> ?. ?? .. ..= => ->    pipe, safe-nav, default, ranges, match arm, typed lambda return
( ) [ ] { } , : :: . | _
```

## Structure

```
program     = NL? item ( NL item )* NL? EOF
item        = import | func_def | struct_def | enum_def | interface_def
            | top_let | stmt

import      = "import" ( std_path | STRING )
std_path    = IDENT ( "::" IDENT )+        # import std::fs; aliased roots in the future
                                            # STRING: file path, relative to the importer

top_let     = "pub"? let_stmt
```

## Definitions

```
func_def    = "pub"? "def" IDENT generics? "(" params? ")" ret? throws? NL block "end"
params      = param ( "," param )*
param       = "self" | IDENT ":" type
ret         = ":" type
throws      = "throws" ( "never" | TYPE_IDENT ( "|" TYPE_IDENT )* )
generics    = "<" gen_param ( "," gen_param )* ">"
gen_param   = TYPE_IDENT ( ":" constraint )?
constraint  = TYPE_IDENT                              # named interface
            | "{" iface_member ( "," iface_member )* "}"   # anonymous interface

struct_def  = "pub"? "struct" TYPE_IDENT generics? NL struct_body "end"
struct_body = ( field NL | func_def NL )*
field       = IDENT ":" type

enum_def    = "pub"? "enum" TYPE_IDENT generics? NL variant+ "end"
variant     = TYPE_IDENT ( "(" field ( "," field )* ")" )? NL

interface_def = "pub"? "interface" TYPE_IDENT generics? NL iface_member+ "end"
iface_member  = "def" IDENT "(" params? ")" ret? throws? NL
```

## Statements

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

(* inline form, single-line expression; branches are ONE expression *)
if_inline   = "if" expr "then" expr
              ( "elsif" expr "then" expr )*
              ( "else" expr )? "end"
while_stmt  = "while" expr NL block "end"
for_stmt    = "for" pattern "in" expr NL block "end"
```

## Expressions

Precedence from lowest to highest; all binary operators are left-associative
except `**` and `??` (right) and ranges (non-associative). `??` associates
to the right so `Option`s chain into a final fallback: `a ?? b ?? 0` ≡
`a ?? (b ?? 0)`, and the desugared `match` then types naturally
(`Option<T> ?? T -> T` at every step):

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
11  unary: - ! not
12  postfix: call (), index [], access ., safe-nav ?., catch
```

```
expr        = pipe_expr
pipe_expr   = coalesce ( "|>" pipe_target )*
pipe_target = postfix                                 # a |> f(b) ; any callable expression

primary     = INT | FLOAT | STRING | CHAR | "true" | "false" | "unit"
            | IDENT | "self"
            | "(" expr ")"
            | vector_lit | map_lit | struct_lit | lambda
            | if_expr | match_expr
            | TYPE_IDENT ( "(" args? ")" )?           # enum constructor

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
match_arm   = pattern ( "if" expr )? "=>" arm_body NL
arm_body    = expr | throw_stmt | return_stmt | "break" | "continue"
            | NL block
            (* a bare statement-keyword body normalizes to a one-statement
               block, mirroring inline-if normalization *)

pattern     = "_" | literal | IDENT
            | TYPE_IDENT ( "(" pattern ( "," pattern )* ")" )?
            | "(" pattern ( "," pattern )* ")"        # tuple

(* literals allowed in patterns; pattern STRINGs do NOT allow
   interpolation — a pattern compares, it does not construct *)
literal     = INT | FLOAT | STRING | CHAR | "true" | "false"
```

```
catch_clause = ( "catch" | "catch_all" ) "(" IDENT ")" NL catch_arm+ "end"
catch_arm    = catch_types ( "if" expr )? "=>" arm_body NL
catch_types  = ( error_type | "_" ) ( "|" error_type )*
error_type   = ( IDENT "." )? TYPE_IDENT     # possibly qualified: fs.NotFound, panics.DivisionByZero
```

Full semantics in [04-errors.md](04-errors.md). `catch` is a postfix
operator on an expression with the highest precedence of level 12; in
contexts where it would bind wrongly (e.g. inside an arm of another catch)
it is parenthesized.

## Types

```
type        = type_name ( "<" type ( "," type )* ">" )?     # Vector<int>, Map<string, T>
type_name   = TYPE_IDENT | IDENT                            # IDENT covers primitives: int, float, ...
            | "(" type ( "," type )* ")"                    # tuple
            | fn_type
fn_type     = "(" ( type ( "," type )* )? ")" "->" type     # (int, int) -> int
```

Primitives: `int` (i64), `float` (f64), `bool`, `string`, `char`, `unit`.

## Known ambiguities and their resolution

| Case | Resolution |
|------|------------|
| `{` for map literal vs struct literal vs `do` block | struct lit requires a preceding `TYPE_IDENT {`; map lit only in expression position; blocks use `do/end`, never braces |
| `|` for lambda vs `||` logical | `||` lexes as or; an empty lambda is written `| |` → the lexer only emits PIPE PIPE if there is a space: **decision: a parameterless lambda uses `do ... end` or `|_|`**, avoiding the case |
| `<` for generics vs comparison in expressions | generics only appear after `def f` / `TYPE_IDENT` in type position; in an expression, `<` is always comparison (no turbofish in v1) |
| `if` expression vs statement | same node; the checker types it when all branches match |
| `puts` | not a keyword: it's a stdlib function (`io.puts`, re-exported to the prelude) |
| inline `if` vs multi-line | the token after the condition decides: `then` → inline form (single-expression branches), `NL` → block form |
| call vs field | parentheses are **mandatory** in calls in expression position: `v.len()` calls, `p.x` is field access. Exceptions: statement-position command calls and trailing `do`-blocks (see below) |
| command calls | at STATEMENT position only, a bare `IDENT` followed by one or more comma-separated expressions on the same line is a call: `puts "hi"`, `puts a, b`. In expression position parentheses remain mandatory (`let x = puts("hi")`). A leading `-` binds as binary subtraction, not as a negative first argument: `puts -x` is `puts - x`; write `puts(-x)` |
| unknown escapes | `\<c>` for any `c` outside the escape set (`\n \t \" \\ \#`) is an ERROR in both string and char literals — never silently dropped or passed through. Raw strings are unaffected (no escapes at all) |
| `?`/`!` ident suffix vs `?.` / `!=` operators | the operator wins: `foo?.bar` is ALWAYS safe-nav (`foo` + `?.`), and `foo!=x` is always `foo != x`. The suffix is absorbed into the ident in any other context (`isDir?`, `isDir?()`). To chain on a predicate: `(x.valid?).toString()` |
| escapes in RAWSTRING | raw strings are truly raw: they do NOT process escapes (`\n` is a literal backslash+n); only `#{` and the closing `"""` are special. Consequence: a literal `#{` cannot appear in a raw string — use a normal string with `\#{` |

| line continuation | a newline run whose next token is `\|>`, `.`, or `?.` continues the current expression instead of terminating the statement (Ruby-style leading-dot chains): `repos NL .filter(...)` is one expression |
| trailing `do`-block | the ONE exception to mandatory call parentheses: a `do \|params\| ... end` block directly after a call or method name appends the lambda as the last argument. `f(a) do \|x\| ... end` ≡ `f(a, lambda)`; `recv.each do \|x\| ... end` ≡ `recv.each(lambda)` |

Cross-cutting notes:

- **Trailing commas are allowed** in every comma-separated list: args,
  params, Vector/Map/Set literals, struct literals, generics.
- Ranges produce values of type `Range` (see doc 03); they are not
  syntax exclusive to `for`.
- Newline tokens are insignificant inside `( )` and `[ ]` delimiters
  (arguments, groupings, vector literals) and after a comma in map and
  struct literals; they are significant only as statement separators and
  around block keywords.

> Canonical spec. A Spanish reading copy is mirrored in the Atlas workspace 'brasa'.
