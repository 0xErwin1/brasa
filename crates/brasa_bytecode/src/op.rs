//! The instruction set.
//!
//! Word-code (spec: 07 — Diseño del bytecode, execution model): each
//! instruction is one enum value with inline operands, jump targets are
//! absolute [`CodeIx`] values. Stack effects below read top-on-the-right
//! (`a b -> c` pops `b` then `a`, pushes `c`).

use crate::{BuiltinId, CodeIx, ConstId, EnumId, FuncId, GlobalIx, SlotIx, StructId};

/// One VM instruction. Semantics are normative in
/// spec: 07 — Diseño del bytecode; the doc comments here are summaries.
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum Op {
    // --- constants and slots -------------------------------------------
    /// `-> v`: push constant pool entry (int, float, string, char).
    Const(ConstId),
    /// `-> unit`: push `unit` (frequent enough to skip the pool).
    LoadUnit,
    /// `-> true`.
    LoadTrue,
    /// `-> false`.
    LoadFalse,
    /// `-> None`: push `Option::None`.
    LoadNone,
    /// `v ->`: discard the top of the stack.
    Pop,
    /// `v -> v v`: duplicate the top (scrutinee tests in decision trees).
    Dup,
    /// `-> v`: read frame slot.
    LoadLocal(SlotIx),
    /// `v ->`: write frame slot.
    StoreLocal(SlotIx),
    /// `v ->`: bind a slot to a fresh heap binding cell holding `v`.
    ///
    /// A binding a closure captures and some scope rebinds lives in a
    /// cell instead of directly in the slot, so the capture and the
    /// enclosing frame share one binding (spec: 07 — Diseño del bytecode,
    /// closures). Re-executing a binding site makes a NEW cell, which
    /// is what keeps one loop iteration's binding distinct from the
    /// next one's.
    MakeBinding(SlotIx),
    /// `-> v`: read through the binding cell a slot holds.
    LoadBinding(SlotIx),
    /// `v ->`: write through the binding cell a slot holds; every
    /// capture of that binding observes the write.
    StoreBinding(SlotIx),
    /// `-> v`: read a global slot; loading an unset slot is fatal
    /// ("used before initialization", mirroring the tree-walker).
    LoadGlobal(GlobalIx),
    /// `v ->`: write a global slot (top-`let` init or assignment).
    StoreGlobal(GlobalIx),
    /// `-> fn`: push a function-table entry as a value.
    LoadFunc(FuncId),

    // --- int arithmetic (checked: overflow panics) ---------------------
    /// `a b -> r`: checked; overflow raises `panics.IntegerOverflow`.
    AddInt,
    /// `a b -> r`: checked subtraction.
    SubInt,
    /// `a b -> r`: checked multiplication.
    MulInt,
    /// `a b -> r`: zero divisor raises `panics.DivisionByZero`;
    /// `MIN / -1` raises `panics.IntegerOverflow`.
    DivInt,
    /// `a b -> r`: zero divisor raises `panics.DivisionByZero`.
    RemInt,
    /// `a b -> r`: negative exponent raises `panics.AssertionFailed`;
    /// overflow raises `panics.IntegerOverflow`.
    PowInt,
    /// `a -> r`: checked negation (`MIN` overflows).
    NegInt,

    // --- float arithmetic (IEEE 754, never panics) ---------------------
    /// `a b -> r`: IEEE addition.
    AddFloat,
    /// `a b -> r`: IEEE subtraction.
    SubFloat,
    /// `a b -> r`: IEEE multiplication.
    MulFloat,
    /// `a b -> r`: IEEE division (`1.0 / 0.0` is `inf`).
    DivFloat,
    /// `a b -> r`: IEEE remainder.
    RemFloat,
    /// `a b -> r`: `powf`.
    PowFloat,
    /// `a -> r`: IEEE negation.
    NegFloat,

    /// `a b -> s`: string concatenation (`+` on strings, interpolation).
    Concat,
    /// `a -> b`: boolean negation.
    Not,

    // --- comparison ----------------------------------------------------
    /// `a b -> bool`: structural equality (`value_eq` semantics); `!=`
    /// compiles to `Eq` + `Not`.
    Eq,
    /// `a b -> bool`: primitive ordering; NaN comparisons are `false`.
    Lt,
    /// `a b -> bool`.
    Le,
    /// `a b -> bool`.
    Gt,
    /// `a b -> bool`.
    Ge,

    // --- jumps ---------------------------------------------------------
    /// Unconditional jump.
    Jump(CodeIx),
    /// `bool ->`: pop; jump when false (`if`, `while`, guards).
    JumpIfFalse(CodeIx),
    /// `&&`: jump keeping the value when false, else pop and continue.
    JumpIfFalseOrPop(CodeIx),
    /// `||`: jump keeping the value when true, else pop and continue.
    JumpIfTrueOrPop(CodeIx),
    /// Peek an enum value; jump unless it is `variant` (decision-tree
    /// primitive for BRS-27 match compilation).
    JumpIfVariantNe { variant: u16, target: CodeIx },
    /// Peek an `Option`; jump when it is `None`.
    JumpIfNone(CodeIx),

    // --- Option and aggregate access -----------------------------------
    /// `v -> Some(v)`: the checker's `Wrap` decision for `?.`.
    WrapSome,
    /// `v -> opt`: deferred wrap decision — pass an `Option` through,
    /// wrap anything else (the tree-walker's dynamic fallback).
    WrapSomeDynamic,
    /// `Some(v) -> v`: codegen always guards with [`Op::JumpIfNone`];
    /// `None` here is a VM invariant break.
    UnwrapSome,
    /// `v -> f`: read tuple element `i`.
    TupleField(u16),
    /// `v -> f`: read enum payload `i`.
    EnumField(u16),
    /// `r -> f`: read struct field `i` (declaration order; the checker
    /// resolves names to indices statically).
    GetField(u16),
    /// `r v ->`: write struct field `i`.
    SetField(u16),
    /// `r i -> v`: Vector: bounds-checked read, out of range raises
    /// `panics.IndexOutOfBounds`. Map: structural lookup yielding
    /// `Option` (missing key is `None`).
    GetIndex,
    /// `r i v ->`: Vector: bounds-checked write. Map: upsert (an
    /// existing key keeps its position).
    SetIndex,

    // --- calls ---------------------------------------------------------
    /// `args -> r`: direct call to a function-table entry (top-level
    /// functions and struct methods; the receiver is arg 0).
    Call { func: FuncId, argc: u8 },
    /// `callee args -> r`: indirect call — function value, closure,
    /// bound method, or bound builtin.
    CallValue { argc: u8 },
    /// `[recv] args -> r`: native builtin (`puts`, `push`, `len`, ...).
    CallBuiltin { builtin: BuiltinId, argc: u8 },
    /// `recv args -> r`: member call whose receiver is statically a
    /// generic parameter, so the target is only known from the runtime
    /// value's method table (`argc` counts the receiver). Declared
    /// struct methods first, then a struct field holding a callable,
    /// then the universal `toString`, then the builtin method table.
    CallMethodDyn { name: ConstId, argc: u8 },
    /// `recv -> v`: the same lookup as [`Op::CallMethodDyn`] without
    /// calling — a struct field's value, a bound method, or a bound
    /// builtin.
    BindMethodDyn(ConstId),
    /// `recv -> bm`: struct method accessed as a value (`p.dist`).
    BindMethod(FuncId),
    /// `recv -> bb`: builtin method accessed as a value (`v.push`).
    BindBuiltin(BuiltinId),
    /// `r ->`: pop the result, pop the frame, push the result in the
    /// caller. Functions typed `unit` compile `LoadUnit` before `Ret`.
    Ret,

    // --- construction --------------------------------------------------
    /// `v... -> vec`: vector literal from the top `n` values.
    MakeVector(u16),
    /// `(k v)... -> map`: map literal from `n` pairs; structural key
    /// dedupe, first occurrence keeps its position, last value wins.
    MakeMap(u16),
    /// `v... -> tup`: tuple from the top `n` values.
    MakeTuple(u16),
    /// `vec -> set`: the `Set(v)` constructor — dedupe by structural
    /// equality, first occurrence kept, insertion order preserved.
    MakeSetFromVector,
    /// `f... -> s`: struct literal; field count and order come from the
    /// shape (values already in declaration order).
    MakeStruct(StructId),
    /// `p... -> e`: enum variant with `argc` payload values.
    MakeEnum {
        enum_id: EnumId,
        variant: u16,
        argc: u8,
    },
    /// `caps... -> cl`: move the top `captures` values into a closure
    /// over `func`. A captured binding that some scope rebinds is a
    /// binding cell ([`Op::MakeBinding`]), so the closure and the
    /// creating frame share it; one that is never rebound is the value
    /// itself, which is indistinguishable from sharing a cell.
    MakeClosure { func: FuncId, captures: u16 },
    /// `lo hi -> rg`: lazy int range.
    MakeRange { inclusive: bool },

    // --- strings and iteration -----------------------------------------
    /// `v -> s`: derived `toString` (depth-capped structural rendering;
    /// a struct with a user `toString` dispatches to it via the shape).
    ToString,
    /// `v -> it`: iterator over a Range (lazy), Vector/Map/Set
    /// (snapshot at loop entry), or string (chars). Map yields
    /// key/value tuples. Iterators are VM-internal values.
    IterNew,
    /// `it -> it v`, or jump to the target with the iterator popped
    /// when exhausted.
    IterNext(CodeIx),

    // --- errors --------------------------------------------------------
    /// `v ->`: raise `v` as an error signal; handler-table unwinding
    /// begins (spec: 07 — Diseño del bytecode, throw/catch).
    Throw,
    /// Peek the caught signal; jump when it is a panic (`_` arms never
    /// catch panics).
    JumpIfPanic(CodeIx),
    /// Peek the caught signal; jump unless its nominal tag equals the
    /// string constant.
    JumpIfTagNe { tag: ConstId, target: CodeIx },
    /// `sig -> sig v`: push the caught error value (user arms and `_`).
    CaughtValue,
    /// `sig -> sig s`: push the detail/message string (arms naming a
    /// panic or a native error).
    CaughtDetail,
    /// `sig ->`: pop the caught signal and resignal it unchanged
    /// (non-exhaustive `catch` propagates what it does not handle).
    Rethrow,
}
