# Brasa — bytecode design

> Status: normative. This document defines the bytecode and VM contract;
> the conformance corpus (`crates/brasa_vm/tests/conformance.rs`) checks
> the implementation against it. A disagreement is a test or
> implementation bug, not authority to override this specification.
> Before BRS-108, the M1/M2 tree walker served as the behavior oracle;
> its expectations were recorded in the corpus when it was retired.
> HIR→bytecode compilation
> (BRS-27), the VM loop (BRS-28), and GC plus string interning (BRS-29)
> build on this design; the container types live in
> `crates/brasa_bytecode`.

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
| Dispatch | Plain `match` over `Op` in the VM loop (BRS-28) | The criterion contract (below) was walker-vs-VM, not VM-vs-Lua; threaded dispatch is a later optimization if ever needed |

These operand widths are the compiler's limits, not just its encoding:
an `argc`/`arity` operand caps a call and a parameter list at 255, and
the `u16` operands cap literal element counts, struct fields, enum
variants, local slots, globals, and captures at 65535 each. A program
that does not fit is rejected at compile time with a `C` diagnostic
(`06-diagnostics.md`), never truncated and never run.

### Module execution

A whole program — one file or many — compiles to a single `Module`
(format below). The module graph is flattened at compile time: function,
struct, enum and global indices are program-wide, so the VM has no module
concept and no loader. Entry convention:

- `functions[0]` is the synthetic `<toplevel>` function: every module's
  top-level statements and top-`let` initializers compiled in source
  order, imported modules first, in the loader's post-order DFS. That
  ordering is what makes a module's top level run once, the first time it
  is imported, with its dependencies already initialized
  (`docs/spec/01-syntax.md`, modules and entry point). Top-`let`s store
  into global slots.
- After `<toplevel>` returns, the driver calls `Module::entry` if it is
  set. The code generator names it — the executed file's top-level `def
  main`, if it declares one — rather than the VM finding it by name: an
  imported module may define its own `main`, and only the executed
  file's is an entry point.
- Global slots start **unset**. Loading an unset global is a fatal
  runtime error ("used before initialization")
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
     + argc .. argc+captures  the closure's captures, copied in
     + .. locals              remaining local slots, one per LocalId
     + locals ..              operand temporaries
```

- The resolver gives every binding site a unique `LocalId`; BRS-27 maps
  each function's `LocalId`s to dense frame slots (shadowing needs no
  runtime support — distinct `LocalId`s get distinct slots).
- Each `Function` records `arity`, `captures`, and `locals` (total slot
  count). BRS-27 also computes the maximum operand depth and records it
  as the function's `max_stack`, so the VM can reserve stack space on
  entry without checking per push.
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

### Dispatch through a generic constraint

Generics are not monomorphized (`03-types.md`, "Generics: execution
model"): one bytecode function serves every instantiation. A member call
on a receiver the checker types as a generic parameter therefore has no
single static target, and compiles to `call_method_dyn` (or
`bind_method_dyn` for a bare member read), which resolves the name
against the runtime value in this order:

1. the receiver's struct shape — either a declared method or a field
   holding a callable,
2. the universal derived `toString`,
3. the builtin method table (a builtin type may satisfy a user
   interface structurally).

A struct's fields and its methods are **one member namespace**: a method
may not repeat a field name, and the resolver rejects the collision at
its declaration site (`R006`, `06-diagnostics.md`). Step 1 therefore has
at most one candidate. Each op still mirrors its non-generic counterpart
— `call_method_dyn` inspects methods before fields and `bind_method_dyn`
fields before methods — but with the collision illegal that order is not
observable: a call and a bare read of the same member always reach the
same target. A callable field with no same-named method satisfies a
constraint method structurally, on both paths —
pinned by `generic_receivers_reach_struct_fields_holding_callables` in
`crates/brasa_vm/tests/conformance.rs`.

A missing member on a struct receiver is fatal (`unknown member`); on
any other receiver the builtin table's own `unknown builtin method` is.
Both are unreachable in checked programs.

`call_method_dyn` enters the resolved frame in place, exactly like
`call`, so recursion through a constraint method is bounded by the same
call-depth guard rather than by host stack depth.

When the concrete receiver type is statically known, no dynamic op is
emitted: struct receivers compile to `call`, and `toString` compiles to
`to_string`.

### Closures

A closure captures the **binding**, not a snapshot of its value
(`01-syntax.md`, BRS-106). `make_closure` takes the captures off the
stack into the closure object, and at call time they are copied into
the frame's capture slots — so a capture is still an ordinary frame
slot after the parameters, and there is still no `load_capture` op.
`self` inside a lambda is captured like any other slot.

Sharing a binding across two frames needs one indirection, because the
closure outlives the frame that declared the binding and its capture is
a different slot in a different frame. A shared binding therefore lives
in a **binding cell** that both slots point at:

- `make_binding s` binds slot `s` to a fresh cell holding the popped
  value. Every binding site compiles to it — a `let`, a pattern
  binding, a `catch` binding, a parameter prologue — so re-executing
  the site (a `let` in a loop body) makes a NEW cell, and a closure
  from an earlier iteration keeps the binding it captured.
- `load_binding s` / `store_binding s` read and write through the cell.
  A rebinding is a `store_binding`, so every capture of that binding
  observes it, whichever frame issued it.
- `make_closure` captures the cell itself, which IS the sharing.

Whether a given binding is a cell is not observable, and the code
generator is free to skip one where nothing can tell: a binding no
scope ever rebinds holds one value for its whole life, so capturing
that value is indistinguishable from capturing a cell. BRS-27 boxes
only bindings that are both captured and rebound; that is a
representation decision documented in `brasa_codegen`, and the rule
above is the whole of the semantics.

A binding cell is mutable and can therefore close a reference cycle,
which is why it is an arena kind (GC, below).

### Signals

The walker's `Signal` enum, which the VM inherited its unwinding
vocabulary from, maps on as follows:

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
| Rust `enum Value` with inline scalars + typed GC handles for heap kinds | **Chosen for v1.** 24 bytes per value, exhaustive matches, no `unsafe` in the representation, GC tracing is a `match`. Payloads wider than a `Range` are boxed rather than inlined (BRS-98), because every operand-stack slot pays for the widest variant. The walker→VM criterion win came from eliminating tree dispatch, `HashMap` frames, and `Rc` traffic — not from packing values into 8 bytes |
| NaN-boxing / pointer tagging | Explicit non-goal for v1 (below). It is an *optimization of* the enum representation, invisible to bytecode and to this document's semantics, so it can land later without respeccing anything |

The VM represents language and native-stdlib values as follows:

| Kind | Representation | Heap / GC | Mutable |
|------|----------------|-----------|---------|
| `int`, `float`, `bool`, `char`, `unit` | Inline in `Value` | no | — |
| `Range` | Inline: `lo`, `hi`, inclusive flag (17 bytes of state, lazy — `docs/spec/03-types.md`) | no | — |
| `Option` | Inline tag; `Some` payload is a heap cell | payload only | no |
| `string` | Heap object | yes | no (all string methods are pure); interning is BRS-29's scope and does not change semantics |
| Tuple | Heap object, fixed slice | yes | no (no element assignment exists) |
| Vector | Heap object, growable | yes | yes |
| Map | Heap object: insertion-ordered pairs plus a hash index over the key, as in the walker | yes | yes |
| Set | Heap object, insertion-ordered, same structure as Map | yes | yes |
| Struct | Heap object: shape index + field slots in declaration order | yes | yes (field assignment) |
| Enum variant | Heap object: shape index, variant index, payload slots | yes | no (no assignment through a variant) |
| Function value | Inline `FuncId` | no | — |
| Closure | Heap object: `FuncId` + captured bindings | yes | no (the capture list is fixed at creation; a shared binding changes inside its own cell) |
| Bound method / bound builtin | Heap object: receiver + target | yes | no |
| Native error | Heap object: static name + message string | yes | no |
| `Value::ProcOutput` (`proc.Output`) | Shared immutable record: stdout, stderr, exit code | yes | no |
| `Value::Walk` (`fs.Walk`) | Shared record containing `paths` and `unreadable` vectors | yes; traces both vectors | no |
| `Value::Json` (`Json`) | Shared immutable JSON tree | yes | no |
| `Value::CliArgs` (`cli.Args`) | Shared immutable record: flags, options, positional arguments | yes | no |
| `Value::HttpResponse` (`http.Response`) | Shared immutable record: status, body, header pairs | yes | no |

Three **internal** value kinds exist on the operand stack but are never
observable in the language: the caught-signal value (class, tag,
payload, panic stacktrace), loop iterators (`iter_new`'s snapshot
state), and the binding cell a shared capture lives in (closures,
above) — every read of a shared binding yields the cell's contents, so
no language value is ever a cell. All three are GC-scanned like any
stack slot.

Equality is structural (`==` has no identity form), ordering covers the
four comparable primitives, and derived `toString` follows the language
rules over this representation.

Both traversals are cycle-safe, and observably so
(`docs/spec/03-types.md`, cyclic values). Equality is coinductive: past a
shallow guard depth it records the arena-cell pairs on the current
derivation path, and re-entering a recorded pair yields `true`. Below
that depth nothing is recorded, so the acyclic case pays only the depth
counter. `toString` tracks the arena cells on the current path instead
and renders a back-edge as `<cycle>`; it keeps a 10000-level nesting
limit purely to bound the host stack, and that limit reports nesting
depth, never a cycle.

A tuple renders in source form, which means the one-element tuple keeps
its comma: `(1, "a")`, but `(7,)`. Bare parentheses around a single value
mean grouping in expression position (`docs/spec/02-grammar.md`), so
dropping the comma would render a one-element tuple as a scalar.

### GC: precise mark & sweep over the mutable kinds (BRS-29)

Reference cycles ARE constructible in the language: the checker accepts
recursive struct types (`struct S` with a `Vector<S>` field), containers
are shared mutable references (`docs/spec/03-types.md`), so
`s.v.push(s)` closes a cycle — as does storing a closure inside a
container it captures. Plain reference counting therefore leaks, and the
VM collects with mark & sweep. Design, as implemented:

- **Arena scope**: only the kinds that can gain references after
  creation live in the arena behind opaque indices — `Vector`, `Map`,
  `Set`, `Struct`, and the binding cell. The language's post-creation
  mutations are field assignment, index assignment, the mutating
  container builtins, and rebinding a captured name, which writes
  through the cell (`01-syntax.md`, BRS-106). That last one is why the
  cell is here rather than behind `Rc`: `let mut f = || 0` followed by
  `f = || f()` makes the cell point at a closure that captured it.
  Every reference cycle must pass through one of these five kinds. A
  closure is no longer frozen — the cells it holds change under it —
  but it needs no arena slot, because it gains no references itself:
  every cycle through a closure passes through a cell or a container.
  The remaining kinds (strings, tuples, enum payloads, bound methods,
  `Option` payloads, caught signals, iterators) are frozen at
  construction, provably cycle-free, and stay behind `Rc`: for them
  reference counting IS precise. The tracer walks through immutable
  structure to reach arena cells; sweeping an unreachable cell breaks
  its cycle and the `Rc` remainder unwinds.
- **Roots**: precise; the root set is the value stack, the global slots,
  and the native root stack. Frames hold no `Value`s outside the value
  stack (caught signals and loop iterators are ordinary stack values).
  The native root stack covers the one remaining gap: a native call that
  reenters compiled code — a higher-order builtin running its callback,
  `toString` rendering a nested override — first copies values out of
  the heap into host locals the tracer cannot see, and the reentered
  code may drop the container they came from. Those copies (the
  traversal's snapshot, its accumulator, the fields being rendered) are
  parked on the native root stack for the duration, and so is the
  callback. Entering the callback's frame copies its captures into stack
  slots, which is not enough on its own: the callee was popped off the
  value stack before the call, so from that pop until the entered frame
  republishes the captures this handle is the only reference to them.
- **Trigger and pause behavior**: two conditions, both required, tested
  at every instruction boundary, nested dispatch loops included. The
  measure is **bytes, not objects** — a threshold counting cells
  collects constantly under many small values and never under a few
  huge ones. Live bytes must reach a budget (default 1 MiB, and
  `max(initial, 2 × live bytes)` after each collection), and the bytes
  allocated since the last collection must reach a marking allowance of
  `max(initial, (root slots × sizeof(value) + surviving bytes) / 4)`.
  A container's growth after allocation is charged to both, so a vector
  built by `push` is not accounted as free; drift is bounded to one
  cycle either way, because a collection re-measures the whole live set
  exactly. Nested loops are not
  exempt from the boundary: a long-running callback never reaches a
  top-level one, so exempting them would make a builtin traversal hold
  the whole run's garbage. The allowance is what keeps that affordable —
  marking costs one visit per reachable value, and a traversal parks the
  whole receiver as roots, so the live budget alone would re-trace it
  every budget's worth of allocation, quadratic in the receiver's
  length. The
  allowance is measured against live data only, never against the
  garbage it permitted. Once the native root stack is empty again the
  allowance is lowered to the arena's own measure, if that is lower —
  it is never raised, and a traversal nested inside another keeps the
  outer one's allowance in force until both have released. A collection still never
  interrupts an instruction. Pauses are stop-the-world and proportional
  to live data.
- **String interning**: the module constant pool's strings are interned
  once at load into a content-keyed table, so every `const` push shares
  one allocation. Runtime-computed strings (`concat`, `toString`,
  string builtins) are not interned. Interning is invisible: equality
  stays structural, strings have no identity semantics.

The future trigger for widening the arena is any feature that lets a
frozen kind reference a value created after it (lazy or deferred
initialization); recursive types need nothing new — the arena already
covers them. By-reference capture was on that list and is the one that
arrived: BRS-106 widened the arena by exactly one kind, the binding
cell.

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
| `make_binding` | `s` | `v →` | Bind slot `s` to a fresh binding cell holding `v` (closures, above) |
| `load_binding` / `store_binding` | `s` | `→ v` / `v →` | Read / write through the binding cell slot `s` holds; every capture of that binding observes the write |
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
| `eq` | `a b → bool` | Structural equality (`value_eq`): deep on composites, order-insensitive on Map/Set, IEEE on floats, identity fallback for closures, coinductive on reference cycles. `!=` compiles to `eq` + `not` |
| `lt` `le` `gt` `ge` | `a b → bool` | Primitive ordering (int, float, string, char). Any float comparison involving NaN is `false`. A `T: Comparable` parameter compiles to these same ops — the static type is the parameter, not the instantiation — and the VM falls back to the receiver's `cmp`, compared against `0`, when both operands turn out to be structs |

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
| `call_builtin` | `b, argc` | `[recv] args → r` | Native builtin (`puts`, `push`, `len`, `std::math`, …). `argc` counts every pushed operand, the receiver included when the builtin takes one. The builtin registry is a stdlib concern (BRS-28/M4); bytecode only carries the opaque index (the shared `name → id` table lives in `crates/brasa_bytecode`) |
| `call_method_dyn` | `c, argc` | `recv args → r` | Member call whose receiver is statically a generic parameter, so the target is only known from the runtime value (see "Dispatch through a generic constraint"). `c` is the member name in the constant pool; `argc` counts the receiver, pushed first |
| `bind_method` | `f` | `recv → bm` | Method accessed without calling (`p.dist` as a value) |
| `bind_method_dyn` | `c` | `recv → v` | The `call_method_dyn` lookup without calling: a struct field's value, a bound method, or a bound builtin |
| `bind_builtin` | `b` | `recv → bb` | Builtin method as a value (`v.push` as a value) |
| `ret` | | `r →` | Pop the result, pop the frame, push the result in the caller |

### Construction

| Op | Operands | Stack | Semantics |
|----|----------|-------|-----------|
| `make_vector` | `n` | `v… → vec` | Vector literal from the top `n` values |
| `make_map` | `n` | `(k v)… → map` | Map literal from `n` pairs; structural key dedupe, first occurrence keeps its position, last value wins |
| `make_tuple` | `n` | `v… → tup` | Tuple from the top `n` values (`n >= 1`; there are no zero-element tuples). Emitted by a tuple expression `(a, b)` |
| `make_set_from_vector` | | `vec → set` | The `Set(v)` constructor: dedupe by structural equality, first occurrence kept, insertion order preserved |
| `make_struct` | `struct` | `f… → s` | Struct literal; field count and order come from the shape, initializers already evaluated in written order and reordered to declaration order by codegen |
| `make_enum` | `enum, variant, argc` | `p… → e` | Enum variant with `argc` payload values |
| `make_closure` | `f, n` | `caps… → cl` | Move the top `n` captures into a closure over function `f`: the cell for a shared binding, the value for one nothing rebinds |
| `make_range` | `inclusive` | `lo hi → rg` | Lazy int range |

### Strings and iteration

| Op | Operands | Stack | Semantics |
|----|----------|-------|-----------|
| `to_string` | | `v → s` | Derived `toString` (structural rendering; floats always show the decimal point; a back-edge of a reference cycle renders as `<cycle>`). A struct with a user-defined `toString` dispatches to it via the shape. Emitted by interpolation lowering; the checker makes it a no-op on strings |
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
| `max_stack` | Maximum operand-stack depth above the locals boundary, computed by BRS-27; the VM reserves `locals + max_stack` slots on frame entry |
| `chunk` | The code |

### Shapes and globals

- `structs`: per-struct shape — name (the nominal `catch`/`toString`
  tag), field names in declaration order, method `FuncId`s, and the
  optional user `toString` override.
- `enums`: per-enum shape — name plus `(variant name, payload arity)`
  in declaration order.
- `globals`: one named slot per top-`let` item, indexed by
  `store_global`/`load_global`.
- `tests`: `Vec<TestEntry>` in source order when compiled for
  `brasa test`; each entry stores the test's display `name` and its
  compiled `func`. Normal runs leave this vector empty and do not compile
  test bodies.

## Non-goals (v1) and the benchmark contract

Explicitly out of scope for M3, recorded so nobody "helpfully" adds them:

- **No JIT** and no AOT (language-level non-goal, `00-vision.md`).
- **No NaN-boxing / pointer tagging**: the enum representation is the
  contract; packing is a later, invisible optimization.
- **No inline caching**: dispatch is already static almost everywhere
  (typed arithmetic, resolved field indices, direct calls).
- **No bytecode serialization**: compile-and-run in one process; the
  module format is Rust structures, not a file format. `brasa bundle`
  (BRS-111) embeds module SOURCE and compiles at startup for exactly
  this reason: a serialized bundle would pin the opcode set, the value
  representation and the shape tables, none of which are stable — and
  the whole front half of the pipeline costs 1.4ms, invisible against
  process spawn.
- **No monomorphization**: one bytecode function per generic function,
  uniform value representation (`docs/spec/03-types.md`).

Benchmark contract (M3 acceptance, epic BRS-4) — met, and recorded
here as history. It required the tree-walker to stay in-tree as the
reference interpreter, the full golden-program suite to run against
both backends with byte-identical output, and a criterion harness over
a shared set of `.bras` programs covering at least arithmetic loops,
collection traversal, closure-heavy code, and catch-on-the-happy-path,
with acceptance being a statistically significant VM speedup on every
one. The walker was retired in BRS-108 once that gate had been passed;
the workloads survive it unchanged, as the baseline M6 performance
work is measured against.

### Benchmark results (BRS-30, dev machine)

Measured by the criterion harness (0.8), then at
`crates/brasa_vm/benches/backends.rs` and now VM-only at
`crates/brasa_vm/benches/vm.rs`, on a dev machine (Intel i7-10850H, x86_64 Linux); criterion keeps the full
statistical data under `target/criterion/`. Each program compiles once
outside the measured loop; each iteration executes prebuilt artifacts
into a sink writer. Values are criterion point estimates.

| Benchmark | Walker | VM | Speedup |
|-----------|--------|----|---------|
| `arith_loop` | 57.83 ms | 36.64 ms | 1.58x |
| `collections` | 6.79 ms | 3.67 ms | 1.85x |
| `closures` | 68.40 ms | 18.30 ms | 3.74x |
| `catch_happy` | 59.61 ms | 24.14 ms | 2.47x |
| `fib` | 21.63 ms | 7.16 ms | 3.02x |
| `strings` | 1.38 ms | 1.06 ms | 1.30x |

Acceptance held: the VM was faster on every benchmark with
non-overlapping confidence intervals, so it became the CLI default and,
in BRS-108, the only backend — `--backend` is gone and "walker" is no
longer a user-visible concept.

Catch-on-the-happy-path overhead on the VM (`catch_overhead_vm`): the
same loop with a never-taken `catch` measured 24.21 ms vs 23.33 ms
without it (+3.8%, overlapping confidence intervals) — handler tables
keep the happy path effectively free.

Cold start (full pipeline for a small script): frontend only 19.7 µs,
walker 98.1 µs, VM 110.8 µs. The VM's extra ~13 µs is the codegen
phase; execution benchmarks above exclude it by design. It is the one
number where the walker won, and it is why cold start keeps its own
group in the harness.

> Canonical spec. A Spanish reading copy is mirrored in the Atlas workspace 'brasa'.
