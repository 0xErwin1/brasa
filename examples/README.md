# Brasa examples

Target programs for the language as specified in `docs/spec/`. Nothing
runs yet (the interpreter lands in M1); these files serve three purposes:

1. A human-readable tour of the language.
2. Parser food: they should parse cleanly as soon as BRS-10/11 land.
3. The seed of the golden test suite (`tests/programs/`) once the
   tree-walker exists — each file will get an expected-output twin.

| File | Exercises |
|------|-----------|
| `hello.brs` | puts, interpolation, shebang |
| `fib.brs` | functions, recursion, inline `then` if, while |
| `fizzbuzz.brs` | elsif chains, ranges, `for` |
| `shapes.brs` | structs, methods, enums, match, Option |
| `errors.brs` | throw, catch, catch_all, panics, throws clause |
| `pipeline.brs` | vectors, maps, lambdas, pipe operator |
| `strings.brs` | string methods, raw strings, chars |
| `modules/` | file imports, `pub`, `def main()` entry point |
| `stars.brs` | stdlib preview: fs, json, proc (needs M4) |
