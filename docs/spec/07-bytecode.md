# Brasa — bytecode design

> Status: draft for review (M3, unit BRS-26). The reference semantics are
> the M1/M2 tree-walker (`crates/brasa_interp`): where this document and
> the walker's observable behavior disagree, the walker wins and this
> document has a bug. HIR→bytecode compilation (BRS-27), the VM loop and
> GC (BRS-28), and string interning (BRS-29) build on this design; the
> container types live in `crates/brasa_bytecode`.

## Scope

This document fixes what M3 code generation targets and what the VM
executes: the execution model (frames, calls, unwinding), the runtime
value representation, the instruction set, and the in-memory module
format. It does NOT fix compilation strategy (decision trees, register
allocation of local slots — BRS-27), the dispatch loop or the collector
(BRS-28), or the interning table (BRS-29); it only guarantees the
primitives those units need.

## Execution model

### Word-code, not byte-code

| Question | Decision | Rationale |
|----------|----------|-----------|
| Encoding | **Word-code**: `Vec<Op>` where `Op` is a Rust enum with inline operands, one instruction per element | No byte-level encode/decode layer, no alignment bugs, exhaustive `match` dispatch checked by the compiler. Bytecode is an in-memory compilation artifact, never serialized to disk (non-goal below), so a compact wire format buys nothing |
| Operands | Inline enum fields: `u16` for local slots, fields, globals, and counts; `u32` newtypes for constant-pool, function, and code indices | Keeps `Op` small (`u16`/`u32` payloads) while indices stay typed — mixing a `ConstId` into a jump is a compile error |
| Jumps | Absolute instruction indices (`CodeIx`, an index into the `Vec<Op>`), patched by the code generator | Word-code makes relative offsets pointless; absolute indices make the disassembly and handler tables directly readable |
| Dispatch | Plain `match` over `Op` in the VM loop (BRS-28) | The criterion contract (below) is walker-vs-VM, not VM-vs-Lua; threaded dispatch is a later optimization if ever needed |

### Module execution

A compiled module is a `Module` (format below). Entry convention:

- `functions[0]` is the synthetic `<toplevel>` function: top-level
  statements and top-`let` initializers compiled in source order
  (`docs/spec/01-syntax.md`, entry point). Top-`let`s store into global
  slots.
- After `<toplevel>` returns, the driver calls the module's `main` (a
  regular function-table entry) if the executed file defines one.
- Global slots start **unset**. Loading an unset global is a fatal
  runtime error ("used before initialization"), exactly like the walker
  — this can only happen when a function called from a top-level
  statement reads a top-`let` declared further down.

Exit codes and uncaught-signal rendering are unchanged from M2: they are
the CLI driver's contract, not the VM's.

### Frames and the value stack

One contiguous value stack per VM, shared by all frames (Lua-style). A
frame is three words:

| Field | Meaning |
|-------|---------|
| `func` | Function-table index of the running function |
| `ip` | Next instruction index into that function's chunk |
| `base` | Value-stack index of the frame's slot 0 |

Frame slot layout, from `base` upward:

```
base + 0 .. argc              parameters (methods: self is slot 0)
     + argc .. argc+captures  the closure's captured values, copied in
     + .. locals              remaining local slots, one per LocalId
     + locals ..              operand temporaries
```

- The resolver gives every binding site a unique `LocalId`; BRS-27 maps
  each function's `LocalId`s to dense frame slots (shadowing needs no
  runtime support — distinct `LocalId`s get distinct slots).
- Each `Function` records `arity`, `captures`, and `locals` (total slot
  count). BRS-27 also computes the maximum operand depth so the VM can
  reserve stack space on entry without checking per push.
- Call depth is guarded by a configurable limit (CLI default 4096, as in
  M1); exceeding it raises `panics.StackOverflow`, never a Rust stack
  overflow. The VM loop is iterative, so Rust stack depth is constant.

### Calling convention

- Caller pushes arguments left to right; for method calls the receiver
  is pushed first and becomes slot 0 (`self`).
- `call f argc`: the new frame's `base` is `sp - argc`, so the arguments
  are already in their parameter slots — no copying.
- `call_value argc`: the callee value sits directly below the arguments.
  It may be a function value, a closure, a bound method, or a bound
  builtin. On return the callee slot is replaced by the result.
- `ret` pops the return value, truncates the stack to the frame's base
  (minus the callee slot for `call_value`), pushes the result in the
  caller, and resumes at the caller's saved `ip`. Functions without a
  declared return type compile a `load_unit` before `ret`; there is no
  "void call" form.

### Closures

Capture is **by value at creation time** (M1 decision, `brasa_interp`
module docs): `make_closure` pops the captured values, snapshotted by the
code generator from the visible slots, into the closure object. At call
time they are copied into the frame's capture slots. Consequences,
matching the walker exactly:

- Rebinding a captured `let mut` after capture is not observable.
- Assigning to a captured variable inside the lambda writes the frame's
  copy — local to that invocation, not persisted across calls.
- Heap values stay shared through their references, so interior mutation
  remains visible.

There are therefore **no upvalue cells and no `load_capture` op**:
captures are ordinary frame slots after the parameters. `self` inside a
lambda is captured like any other slot.

### Signals

The walker's `Signal` enum maps onto the VM as follows:

| Signal | VM mechanism |
|--------|--------------|
| `Return` | `ret` (compiled; `return` inside a function is a jump to a shared epilogue or a direct `ret`) |
| `Break` / `Continue` | Compile-time jumps to the loop exit / step label. No runtime signal exists — loops are structured, so BRS-27 always knows the target |
| `Error` (throw) | `throw` + handler-table unwinding (below) |
| `Panic` | Raised by the faulting instruction itself (`div_int`, `get_index`, …); same unwinding machinery, distinct signal class that `_` never matches |
| `Fatal` | Uncatchable: unwinds every frame, no handler consulted; the driver reports and exits |
| `BrokenPipe` | Same as `Fatal` but the driver exits silently with status 0 (Unix convention, M1 decision) |

### Throw, catch, and handler tables

`catch` compiles to **static handler tables**, not runtime enter/exit
instructions. Rationale: the happy path executes zero extra instructions
(principle 1 of `00-vision.md` applied to the runtime), and nesting falls
out of table order instead of a runtime stack of active handlers.

Each `Chunk` carries a list of handler entries:

| Field | Meaning |
|-------|---------|
| `start`, `end` | Half-open instruction range covering the compiled `catch` **subject** (never the arm code — a throw inside an arm or guard belongs to the enclosing handler, if any) |
| `target` | Instruction index of the dispatch sequence |
| `depth` | Operand depth (relative to the frame's `locals` boundary) to restore before entering the dispatch sequence |

Entries are ordered **innermost first**; the unwinder takes the first
entry whose range contains the faulting `ip`.

Unwinding, on `throw` or a panic raised by an instruction:

1. Search the current frame's handler entries for the current `ip`. If
   none matches, pop the frame (recording the function name for the
   panic stacktrace) and retry at the caller's call-site `ip`.
2. On a match: truncate the operand stack to the entry's `depth`, push
   the in-flight signal as a **caught-signal value** (an internal value
   kind, below), and jump to `target`.
3. If no frame has a handler, the driver renders the uncaught error or
   the panic message plus stacktrace, exactly as in M2.

The dispatch sequence is ordinary compiled code operating on the
caught-signal value at the top of the stack:

- `jump_if_panic t` — wildcard arms jump over: `_` catches any error,
  never a panic (`docs/spec/04-errors.md`).
- `jump_if_tag_ne c, t` — nominal matching: compares the signal's tag
  (the walker's `nominal_tag`: declared name for structs/enums, dotted
  name for panics and native errors, type name otherwise) against a
  string constant. Exact equality suffices — user type names never
  contain `.`.
- `caught_value` / `caught_detail` — push the arm binding: the error
  value itself for user arms and `_`, the detail/message string for arms
  naming a panic or a native error (the checker's per-arm narrowing).
  Which one is a compile-time choice — dotted arm names are static.
- Guards run after the binding is stored; a false guard falls through to
  the next arm's tests. The caught-signal value survives guard
  execution because it is an ordinary stack value — a nested catch
  inside a guard cannot clobber it.
- A selected arm `pop`s the caught-signal value before its body.
- `rethrow` at the end of the dispatch sequence pops the caught-signal
  value and resignals it unchanged: non-exhaustive `catch` propagates
  what it does not handle.

## Value representation

### Decision: plain enum, GC handles — no NaN-boxing

| Option | Verdict |
|--------|---------|
| Rust `enum Value` with inline scalars + typed GC handles for heap kinds | **Chosen for v1.** 16 bytes per value, exhaustive matches, no `unsafe` in the representation, GC tracing is a `match`. The walker→VM criterion win comes from eliminating tree dispatch, `HashMap` frames, and `Rc` traffic — not from packing values into 8 bytes |
| NaN-boxing / pointer tagging | Explicit non-goal for v1 (below). It is an *optimization of* the enum representation, invisible to bytecode and to this document's semantics, so it can land later without respeccing anything |

The VM `Value` mirrors the walker's kinds with `Rc<RefCell<…>>` replaced
by GC-managed heap objects:

| Kind | Representation | Heap / GC | Mutable |
|------|----------------|-----------|---------|
| `int`, `float`, `bool`, `char`, `unit` | Inline in `Value` | no | — |
| `Range` | Inline: `lo`, `hi`, inclusive flag (17 bytes of state, lazy — `docs/spec/03-types.md`) | no | — |
| `Option` | Inline tag; `Some` payload is a heap cell | payload only | no |
| `string` | Heap object | yes | no (all string methods are pure); interning is BRS-29's scope and does not change semantics |
| Tuple | Heap object, fixed slice | yes | no (no element assignment exists) |
| Vector | Heap object, growable | yes | yes |
| Map | Heap object; insertion-ordered pairs with structural key lookup, as in the walker (a faster table is a later optimization, invisible to the language) | yes | yes |
| Set | Heap object, insertion-ordered, same rationale as Map | yes | yes |
| Struct | Heap object: shape index + field slots in declaration order | yes | yes (field assignment) |
| Enum variant | Heap object: shape index, variant index, payload slots | yes | no (no assignment through a variant) |
| Function value | Inline `FuncId` | no | — |
| Closure | Heap object: `FuncId` + captured values | yes | no (captures are copied out at call) |
| Bound method / bound builtin | Heap object: receiver + target | yes | no |
| Native error | Heap object: static name + message string | yes | no |

Two **internal** value kinds exist on the operand stack but are never
observable in the language: the caught-signal value (class, tag, payload,
panic stacktrace) and loop iterators (`iter_new`'s snapshot state). Both
are GC-scanned like any stack slot.

Equality is structural (`==` has no identity form), ordering covers the
four comparable primitives, and derived `toString` (recursion-capped at
depth 100) all behave exactly as the walker's `value_eq` / `value_cmp` /
`display`; the VM reuses the same rules over the new representation.

GC contract (design constraint on BRS-28, not specced here): precise and
simple; the root set is the value stack, the global slots, and nothing
else — frames hold no `Value`s outside the stack.

## Instruction set

Stack effects read left to right with the top on the **right**:
`a b → c` pops `b` then `a`, pushes `c`. Operand key: `c` constant-pool
index, `s` local slot, `g` global slot, `f` function index, `t` code
index, `n`/`argc` counts, `b` builtin index.

### Constants and slots

| Op | Operands | Stack | Semantics |
|----|----------|-------|-----------|
| `const` | `c` | `→ v` | Push constant `c` (int, float, string, char) |
| `load_unit` | | `→ unit` | Push `unit` (frequent enough to skip the pool) |
| `load_true` / `load_false` | | `→ bool` | Push a bool |
| `load_none` | | `→ None` | Push `Option::None` |
| `pop` | | `v →` | Discard the top |
| `dup` | | `v → v v` | Duplicate the top (match/scrutinee tests) |
| `load_local` / `store_local` | `s` | `→ v` / `v →` | Read / write frame slot `s` |
| `load_global` / `store_global` | `g` | `→ v` / `v →` | Read / write global slot `g`; loading an unset slot is fatal |
| `load_func` | `f` | `→ fn` | Push function `f` as a value |

### Arithmetic

The checker resolves operand types statically (no mixing, no numeric
interfaces), so arithmetic is **typed**: separate int and float ops with
different failure semantics, per `docs/spec/03-types.md`.

| Op | Stack | Semantics |
|----|-------|-----------|
| `add_int` `sub_int` `mul_int` `pow_int` | `a b → r` | Checked: overflow raises `panics.IntegerOverflow`; `pow_int` with a negative exponent raises `panics.AssertionFailed` |
| `div_int` `rem_int` | `a b → r` | Zero divisor raises `panics.DivisionByZero`; overflow (`MIN / -1`) raises `panics.IntegerOverflow` |
| `neg_int` | `a → r` | Checked negation (`MIN` overflows) |
| `add_float` `sub_float` `mul_float` `div_float` `rem_float` `pow_float` `neg_float` | | IEEE 754: `1.0 / 0.0` is `inf`, never a panic |
| `concat` | `a b → s` | String concatenation (`+` on strings; interpolation pieces) |
| `not` | `a → b` | Boolean negation |

### Comparison

| Op | Stack | Semantics |
|----|-------|-----------|
| `eq` | `a b → bool` | Structural equality (`value_eq`): deep on composites, order-insensitive on Map/Set, IEEE on floats, identity fallback for closures. `!=` compiles to `eq` + `not` |
| `lt` `le` `gt` `ge` | `a b → bool` | Primitive ordering (int, float, string, char). Any float comparison involving NaN is `false`. `T: Comparable` never reaches these ops: the checker compiles it to a `cmp` call plus an int comparison against `0` |

### Jumps

All targets are absolute instruction indices.

| Op | Operands | Stack | Semantics |
|----|----------|-------|-----------|
| `jump` | `t` | | Unconditional |
| `jump_if_false` | `t` | `bool →` | Pop; jump when false (`if`, `while`, guards) |
| `jump_if_false_or_pop` | `t` | see text | `&&`: jump keeping the value when false, else pop and continue |
| `jump_if_true_or_pop` | `t` | see text | `\|\|`: jump keeping the value when true, else pop and continue |
| `jump_if_variant_ne` | `variant, t` | peeks | Peek an enum value; jump unless it is `variant`. Decision-tree primitive for BRS-27 |
| `jump_if_none` | `t` | peeks | Peek an `Option`; jump when `None` |

### Option and aggregate access

| Op | Operands | Stack | Semantics |
|----|----------|-------|-----------|
| `wrap_some` | | `v → Some(v)` | The checker's `WrapDecision::Wrap` for `?.` |
| `wrap_some_dynamic` | | `v → opt` | Deferred wrap decision: pass an `Option` through, wrap anything else (the walker's fallback) |
| `unwrap_some` | | `Some(v) → v` | Extract the payload. Codegen always guards with `jump_if_none`; `None` here is a VM invariant break |
| `tuple_field` / `enum_field` | `i` | `v → f` | Read element/payload `i` (destructuring primitives) |
| `get_field` / `set_field` | `i` | `r → f` / `r v →` | Struct field `i` in declaration order — the checker resolves names to indices statically |
| `get_index` | | `r i → v` | Vector: bounds check, out of range raises `panics.IndexOutOfBounds`. Map: structural key lookup, yields `Option` (missing key is `None`) |
| `set_index` | | `r i v →` | Vector: bounds-checked element write. Map: upsert (existing key keeps its position) |

### Calls

| Op | Operands | Stack | Semantics |
|----|----------|-------|-----------|
| `call` | `f, argc` | `args → r` | Direct call to function-table entry `f` (top-level functions and struct methods; receiver is arg 0) |
| `call_value` | `argc` | `callee args → r` | Indirect call: function value, closure, bound method, or bound builtin |
| `call_builtin` | `b, argc` | `[recv] args → r` | Native builtin (`puts`, `push`, `len`, `std::math`, …). The builtin registry is a stdlib concern (BRS-28/M4); bytecode only carries the opaque index |
| `bind_method` | `f` | `recv → bm` | Method accessed without calling (`p.dist` as a value) |
| `bind_builtin` | `b` | `recv → bb` | Builtin method as a value (`v.push` as a value) |
| `ret` | | `r →` | Pop the result, pop the frame, push the result in the caller |

### Construction

| Op | Operands | Stack | Semantics |
|----|----------|-------|-----------|
| `make_vector` | `n` | `v… → vec` | Vector literal from the top `n` values |
| `make_map` | `n` | `(k v)… → map` | Map literal from `n` pairs; structural key dedupe, first occurrence keeps its position, last value wins |
| `make_tuple` | `n` | `v… → tup` | Tuple from the top `n` values |
| `make_set_from_vector` | | `vec → set` | The `Set(v)` constructor: dedupe by structural equality, first occurrence kept, insertion order preserved |
| `make_struct` | `struct` | `f… → s` | Struct literal; field count and order come from the shape, initializers already evaluated in written order and reordered to declaration order by codegen |
| `make_enum` | `enum, variant, argc` | `p… → e` | Enum variant with `argc` payload values |
| `make_closure` | `f, n` | `caps… → cl` | Snapshot the top `n` capture values into a closure over function `f` |
| `make_range` | `inclusive` | `lo hi → rg` | Lazy int range |

### Strings and iteration

| Op | Operands | Stack | Semantics |
|----|----------|-------|-----------|
| `to_string` | | `v → s` | Derived `toString` (depth-capped structural rendering; floats always show the decimal point). A struct with a user-defined `toString` dispatches to it via the shape. Emitted by interpolation lowering; the checker makes it a no-op on strings |
| `iter_new` | | `v → it` | Iterator over a Range (lazy, ends on `i64` overflow), Vector/Map/Set (snapshot at loop entry, M1 decision), or string (chars). Map yields key/value tuples |
| `iter_next` | `t` | `it → it v` / jump | Push the next element, or jump to `t` with the iterator popped when exhausted |

### Errors

| Op | Operands | Stack | Semantics |
|----|----------|-------|-----------|
| `throw` | | `v →` | Raise `v` as an error signal; unwinding begins |
| `jump_if_panic` | `t` | peeks | Peek the caught signal; jump when it is a panic (wildcard arms) |
| `jump_if_tag_ne` | `c, t` | peeks | Peek the caught signal; jump unless its nominal tag equals string constant `c` |
| `caught_value` | | `sig → sig v` | Push the caught error value (user arms and `_`) |
| `caught_detail` | | `sig → sig s` | Push the detail/message string (arms naming a panic or native error) |
| `rethrow` | | `sig →` | Pop the caught signal and resignal it unchanged |

## Chunk and module format

Everything is an in-memory Rust structure in `crates/brasa_bytecode`; no
serialized form exists (non-goal below).

### Chunk

| Field | Contents |
|-------|----------|
| `code` | `Vec<Op>` |
| `spans` | One `Span` per instruction, parallel to `code`. This is the debug information: runtime error locations, panic stacktrace lines, and future stepping all resolve through it. Spans are file-qualified byte ranges (`brasa_source::Span`) copied from the HIR node that produced each instruction |
| `handlers` | Handler entries (format above), innermost first |

### Constant pool

Per-module, interned: inserting an equal constant returns the existing
index. Kinds: `int`, `float` (interned by bit pattern, so `-0.0` and
`0.0` are distinct entries and NaN payloads are preserved), `string`,
`char`. `unit` and bools have dedicated push ops and never enter the
pool. Full string interning — sharing one heap object per distinct
string at runtime — is BRS-29's scope; the pool only dedupes within a
module's constants.

### Function table

| Field | Contents |
|-------|----------|
| `name` | For stacktraces and the disassembler (`<toplevel>`, `<lambda>`, or the declared name) |
| `arity` | Parameter count (methods count `self`) |
| `captures` | Capture slot count (0 for non-lambdas) |
| `locals` | Total frame slot count, params and captures included |
| `chunk` | The code |

### Shapes and globals

- `structs`: per-struct shape — name (the nominal `catch`/`toString`
  tag), field names in declaration order, method `FuncId`s, and the
  optional user `toString` override.
- `enums`: per-enum shape — name plus `(variant name, payload arity)`
  in declaration order.
- `globals`: one named slot per top-`let` item, indexed by
  `store_global`/`load_global`.

## Non-goals (v1) and the benchmark contract

Explicitly out of scope for M3, recorded so nobody "helpfully" adds them:

- **No JIT** and no AOT (language-level non-goal, `00-vision.md`).
- **No NaN-boxing / pointer tagging**: the enum representation is the
  contract; packing is a later, invisible optimization.
- **No inline caching**: dispatch is already static almost everywhere
  (typed arithmetic, resolved field indices, direct calls).
- **No bytecode serialization**: compile-and-run in one process; the
  module format is Rust structures, not a file format.
- **No monomorphization**: one bytecode function per generic function,
  uniform value representation (`docs/spec/03-types.md`).

Benchmark contract (M3 acceptance, epic BRS-4):

- The tree-walker remains in-tree as the reference interpreter, and the
  full golden-program suite runs against **both** backends with
  byte-identical output.
- A criterion harness runs a shared set of `.brs` benchmark programs on
  both backends. Acceptance is a statistically significant speedup of
  the VM over the walker on every benchmark in the set; the set must
  cover at least arithmetic loops, collection traversal, closure-heavy
  code, and catch-on-the-happy-path (which must be free under handler
  tables).

> Canonical spec. A Spanish reading copy is mirrored in the Atlas workspace 'brasa'.
