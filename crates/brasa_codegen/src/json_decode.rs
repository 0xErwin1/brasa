//! `json.decode` (BRS-144): one synthesized decoder function per target
//! struct.
//!
//! The type a decode must produce is known at the CALL SITE and nowhere
//! else — it is the expected type, which the checker resolved and left
//! in `expr_types`. That is exactly the information a runtime lacks, so
//! it is spent here: the generator walks the target's fields and emits a
//! function that reads a document into that struct, out of instructions
//! that already exist. No opcode is added for it, because an opcode is
//! permanent surface across the generator, the VM, the disassembler and
//! the pinned id registry, and this needs none of that.
//!
//! A decoder takes `(node, path)` and answers the struct:
//!
//! - `node` is the document member being decoded, as the `Option<Json>`
//!   an object lookup yields. The `Json` accessors flatten through
//!   `Option`, so an absent member needs no separate test — it simply
//!   fails every accessor.
//! - `path` is where that member sits in the document (`users[3]`), so
//!   a failure can say where it happened. It is empty at the root, and
//!   the message renders that as `<document>`.
//!
//! Recursion works by reserve-then-fill: the decoder's id is reserved
//! and cached BEFORE its body is emitted, so a field that refers back to
//! its own struct compiles to a call to an id that already exists. The
//! cache is keyed by the struct's declaration, which is sound because a
//! generic struct is not a decodable target (`T036`).
//!
//! A synthesized function has no `LocalId`s, so its frame is addressed
//! by raw slot indices, the way `struct_lit`'s reordering scratch is.

use brasa_bytecode::{Constant, FuncId, Function, Op, SlotIx, builtin_id};
use brasa_hir::{ExprId, ItemId};
use brasa_source::Span;
use brasa_typeck::Type;

use crate::context::Cx;
use crate::expr::compile_expr;
use crate::func::{FnKind, FuncCx, PLACEHOLDER};

/// The document member a decoder was handed.
const NODE_SLOT: SlotIx = SlotIx(0);
/// Where that member sits in the document.
const PATH_SLOT: SlotIx = SlotIx(1);
/// How many slots the two parameters occupy before any scratch.
const PARAM_COUNT: u16 = 2;

/// Compiles `json.decode(text)`: parse the text, then run the decoder
/// for the type the call site asked for, starting at the document root.
///
/// Parsing goes through the ordinary `json.parse` builtin, so a
/// malformed document raises `json.ParseError` exactly as it would have
/// if the caller had spelled the two steps out.
pub(crate) fn compile_call(f: &mut FuncCx, id: ExprId, args: &[ExprId], span: Span) {
    let discard_args = |f: &mut FuncCx| {
        for &arg in args {
            compile_expr(f, arg);
            f.emit(Op::Pop, span);
        }
    };

    let [text] = args else {
        discard_args(f);
        f.emit_fatal("brasa: `json.decode` takes exactly 1 argument", span);
        return;
    };

    // Both fatals are unreachable in a checked program: `T036` refuses
    // a target that is not a decodable struct before code generation
    // runs. They are defence in depth, not behaviour.
    let Some(Type::Struct(item, _)) = f.cx.types.expr_types.get(&id).cloned() else {
        discard_args(f);
        f.emit_fatal("brasa: `json.decode` has no target type", span);
        return;
    };

    let Some(decoder) = decoder_of(&mut *f.cx, item, span) else {
        discard_args(f);
        f.emit_fatal("brasa: `json.decode` target has no decoder", span);
        return;
    };

    let parse = builtin_id("json.parse").expect("`json.parse` is registered");

    compile_expr(f, *text);
    f.emit(
        Op::CallBuiltin {
            builtin: parse,
            argc: 1,
        },
        span,
    );
    f.emit_const(Constant::Str(String::new()), span);
    f.emit(
        Op::Call {
            func: decoder,
            argc: 2,
        },
        span,
    );
}

/// The decoder of one target struct, emitted on first demand.
///
/// The id is reserved and cached before the body is emitted, which is
/// what makes a recursive or mutually recursive type terminate: the
/// nested call finds the id in the cache and compiles to a direct call
/// while the body it belongs to is still being written.
fn decoder_of(cx: &mut Cx, item: ItemId, span: Span) -> Option<FuncId> {
    if let Some(&func) = cx.decoder_of.get(&item) {
        return Some(func);
    }

    let plan = decode_plan(cx, item)?;

    let func = cx.reserve_function();
    cx.decoder_of.insert(item, func);

    let function = emit_decoder(cx, &plan, span);
    cx.define_function(func, function);

    Some(func)
}

/// Everything emitting one decoder needs: the shape it builds and the
/// name and type of each field, paired in declaration order.
struct DecodePlan {
    name: String,
    struct_id: brasa_bytecode::StructId,
    fields: Vec<(String, Type)>,
}

/// Pairs the collected struct shape with the field types the checker
/// recorded while it proved the target decodable, or `None` when the
/// two do not describe the same struct — which a checked program cannot
/// produce.
fn decode_plan(cx: &Cx, item: ItemId) -> Option<DecodePlan> {
    let struct_id = cx.struct_of_item.get(&item).copied()?;
    let shape = cx.structs.get(struct_id.0 as usize)?;
    let types = cx.types.decode_fields.get(&item)?;

    if shape.fields.len() != types.len() {
        return None;
    }

    Some(DecodePlan {
        name: shape.name.clone(),
        struct_id,
        fields: shape
            .fields
            .iter()
            .cloned()
            .zip(types.iter().cloned())
            .collect(),
    })
}

/// Emits one decoder body: prove the node is an object, then read each
/// declared field out of it in declaration order and build the struct.
///
/// A member the document carries but the struct does not declare is
/// never looked at, which is the point: rejecting unknown members would
/// break every decoder the day a provider adds a field.
fn emit_decoder(cx: &mut Cx, plan: &DecodePlan, span: Span) -> Function {
    let mut f = FuncCx::new(
        cx,
        FnKind::Func {
            returns_value: true,
        },
    );
    f.reserve_slot_floor(PARAM_COUNT);

    let object = f.alloc_slot();
    let prefix = f.alloc_slot();
    let field_path = f.alloc_slot();
    let member = f.alloc_slot();

    emit_unwrap_or_raise(&mut f, NODE_SLOT, "asObject", "object", PATH_SLOT, span);
    f.emit(Op::StoreLocal(object), span);

    emit_prefix(&mut f, prefix, span);

    for (name, ty) in &plan.fields {
        f.emit(Op::LoadLocal(prefix), span);
        f.emit_const(Constant::Str(name.clone()), span);
        f.emit(Op::Concat, span);
        f.emit(Op::StoreLocal(field_path), span);

        f.emit(Op::LoadLocal(object), span);
        f.emit_const(Constant::Str(name.clone()), span);
        f.emit(Op::GetIndex, span);
        f.emit(Op::StoreLocal(member), span);

        emit_decode(&mut f, ty, member, field_path, span);
    }

    f.emit(Op::MakeStruct(plan.struct_id), span);
    f.emit(Op::Ret, span);

    let name = format!("<json-decode:{}>", plan.name);
    f.finish(name, 2, 0, span)
}

/// Computes the prefix every field path in this object shares: nothing
/// at the document root, and `"a.b."` under a member.
///
/// Once per call rather than once per field, and a runtime test rather
/// than a compile-time one because one decoder serves both positions —
/// the same struct is the root of one document and a member of another.
fn emit_prefix(f: &mut FuncCx, prefix: SlotIx, span: Span) {
    let len = builtin_id("len").expect("`len` is registered");

    f.emit(Op::LoadLocal(PATH_SLOT), span);
    f.emit(
        Op::CallBuiltin {
            builtin: len,
            argc: 1,
        },
        span,
    );
    f.emit_const(Constant::Int(0), span);
    f.emit(Op::EqInt, span);
    let nested = f.emit(Op::JumpIfFalse(PLACEHOLDER), span);

    f.emit_const(Constant::Str(String::new()), span);
    f.emit(Op::StoreLocal(prefix), span);
    let done = f.emit(Op::Jump(PLACEHOLDER), span);

    let at_nested = f.here();
    f.patch(nested, at_nested);
    f.emit(Op::LoadLocal(PATH_SLOT), span);
    f.emit_const(Constant::Str(".".to_string()), span);
    f.emit(Op::Concat, span);
    f.emit(Op::StoreLocal(prefix), span);

    let end = f.here();
    f.patch(done, end);
}

/// Reads one value of type `ty` out of the member in `value`, leaving
/// it on the stack.
///
/// `path` names that member for the failure messages this may raise.
fn emit_decode(f: &mut FuncCx, ty: &Type, value: SlotIx, path: SlotIx, span: Span) {
    match ty {
        Type::Int => emit_unwrap_or_raise(f, value, "asInt", "int", path, span),
        Type::Float => emit_unwrap_or_raise(f, value, "asFloat", "float", path, span),
        Type::Bool => emit_unwrap_or_raise(f, value, "asBool", "bool", path, span),
        Type::String => emit_unwrap_or_raise(f, value, "asString", "string", path, span),
        Type::Option(inner) => emit_optional(f, inner, value, path, span),
        Type::Vector(elem) => emit_vector(f, elem, value, path, span),
        Type::Struct(item, _) => match decoder_of(&mut *f.cx, *item, span) {
            Some(decoder) => {
                f.emit(Op::LoadLocal(value), span);
                f.emit(Op::LoadLocal(path), span);
                f.emit(
                    Op::Call {
                        func: decoder,
                        argc: 2,
                    },
                    span,
                );
            }
            None => f.emit_fatal("brasa: `json.decode` field struct has no decoder", span),
        },
        // Refused by `T036` before code generation ran; the fatal is
        // defence in depth, not behaviour.
        _ => f.emit_fatal("brasa: `json.decode` reached an undecodable field", span),
    }
}

/// Applies a `Json` accessor and unwraps it, raising `json.DecodeError`
/// when the member is absent or holds a different JSON kind.
///
/// The accessors answer `Option`, and they flatten through the
/// `Option<Json>` an object lookup yields, so one test covers both
/// failures: an absent member and a member of the wrong kind arrive
/// here identically, and the raiser tells them apart from the value
/// itself.
///
/// Net stack effect: one value. The failing branch pushes the raiser's
/// result, which nothing observes — the call raises before it returns.
fn emit_unwrap_or_raise(
    f: &mut FuncCx,
    value: SlotIx,
    accessor: &str,
    expected: &str,
    path: SlotIx,
    span: Span,
) {
    let accessor = builtin_id(accessor).expect("the `Json` accessors are registered");

    f.emit(Op::LoadLocal(value), span);
    f.emit(
        Op::CallBuiltin {
            builtin: accessor,
            argc: 1,
        },
        span,
    );
    let wrong = f.emit(Op::JumpIfNone(PLACEHOLDER), span);
    f.emit(Op::UnwrapSome, span);
    let done = f.emit(Op::Jump(PLACEHOLDER), span);

    let at_wrong = f.here();
    f.patch(wrong, at_wrong);
    f.emit(Op::Pop, span);
    emit_raise(f, path, expected, value, span);

    let end = f.here();
    f.patch(done, end);
}

/// An `Option<T>` field: absent is `None`, an explicit JSON `null` is
/// `None`, and anything else is decoded as `T` and wrapped.
///
/// The two `None` cases are separate tests because `null?` answers
/// `false` for an absent member — deliberately, since an absent member
/// is not an explicit `null` — so it cannot detect absence on its own.
fn emit_optional(f: &mut FuncCx, inner: &Type, value: SlotIx, path: SlotIx, span: Span) {
    let is_null = builtin_id("null?").expect("`null?` is registered");

    f.emit(Op::LoadLocal(value), span);
    let absent = f.emit(Op::JumpIfNone(PLACEHOLDER), span);
    f.emit(Op::Pop, span);

    f.emit(Op::LoadLocal(value), span);
    f.emit(
        Op::CallBuiltin {
            builtin: is_null,
            argc: 1,
        },
        span,
    );
    let present = f.emit(Op::JumpIfFalse(PLACEHOLDER), span);
    let null = f.emit(Op::Jump(PLACEHOLDER), span);

    let at_present = f.here();
    f.patch(present, at_present);
    emit_decode(f, inner, value, path, span);
    f.emit(Op::WrapSome, span);
    let done = f.emit(Op::Jump(PLACEHOLDER), span);

    let at_absent = f.here();
    f.patch(absent, at_absent);
    f.emit(Op::Pop, span);

    let at_null = f.here();
    f.patch(null, at_null);
    f.emit(Op::LoadNone, span);

    let end = f.here();
    f.patch(done, end);
}

/// A `Vector<T>` field: prove the member is an array, then decode every
/// element into a fresh vector.
///
/// The loop counts rather than iterating because the index is part of
/// the answer: an element that fails has to report `tags[2]`, and an
/// iterator would have thrown that number away.
fn emit_vector(f: &mut FuncCx, elem: &Type, value: SlotIx, path: SlotIx, span: Span) {
    let len = builtin_id("len").expect("`len` is registered");
    let push = builtin_id("push").expect("`push` is registered");

    let items = f.alloc_slot();
    let decoded = f.alloc_slot();
    let index = f.alloc_slot();
    let count = f.alloc_slot();
    let item = f.alloc_slot();
    let item_path = f.alloc_slot();

    emit_unwrap_or_raise(f, value, "asArray", "array", path, span);
    f.emit(Op::StoreLocal(items), span);

    f.emit(Op::MakeVector(0), span);
    f.emit(Op::StoreLocal(decoded), span);
    f.emit_const(Constant::Int(0), span);
    f.emit(Op::StoreLocal(index), span);
    f.emit(Op::LoadLocal(items), span);
    f.emit(
        Op::CallBuiltin {
            builtin: len,
            argc: 1,
        },
        span,
    );
    f.emit(Op::StoreLocal(count), span);

    let head = f.here();
    f.emit(Op::LoadLocal(index), span);
    f.emit(Op::LoadLocal(count), span);
    f.emit(Op::LtInt, span);
    let exit = f.emit(Op::JumpIfFalse(PLACEHOLDER), span);

    f.emit(Op::LoadLocal(items), span);
    f.emit(Op::LoadLocal(index), span);
    f.emit(Op::GetIndex, span);
    f.emit(Op::StoreLocal(item), span);

    emit_index_path(f, path, index, item_path, span);

    f.emit(Op::LoadLocal(decoded), span);
    emit_decode(f, elem, item, item_path, span);
    f.emit(
        Op::CallBuiltin {
            builtin: push,
            argc: 2,
        },
        span,
    );
    f.emit(Op::Pop, span);

    f.emit(Op::LoadLocal(index), span);
    f.emit_const(Constant::Int(1), span);
    f.emit(Op::AddInt, span);
    f.emit(Op::StoreLocal(index), span);
    f.emit(Op::Jump(head), span);

    let end = f.here();
    f.patch(exit, end);
    f.emit(Op::LoadLocal(decoded), span);
}

/// `path[i]`: an index appends with no separator, which is what makes a
/// reported path read the way the document is written.
fn emit_index_path(f: &mut FuncCx, path: SlotIx, index: SlotIx, out: SlotIx, span: Span) {
    f.emit(Op::LoadLocal(path), span);
    f.emit_const(Constant::Str("[".to_string()), span);
    f.emit(Op::Concat, span);
    f.emit(Op::LoadLocal(index), span);
    f.emit(Op::ToString, span);
    f.emit(Op::Concat, span);
    f.emit_const(Constant::Str("]".to_string()), span);
    f.emit(Op::Concat, span);
    f.emit(Op::StoreLocal(out), span);
}

/// Calls the internal raiser with the path, the JSON kind the declared
/// type wanted, and the member found there. Net stack effect: one value
/// pushed, which nothing observes.
fn emit_raise(f: &mut FuncCx, path: SlotIx, expected: &str, found: SlotIx, span: Span) {
    let raiser = builtin_id("<json-decode-failed>").expect("the decode raiser is registered");

    f.emit(Op::LoadLocal(path), span);
    f.emit_const(Constant::Str(expected.to_string()), span);
    f.emit(Op::LoadLocal(found), span);
    f.emit(
        Op::CallBuiltin {
            builtin: raiser,
            argc: 3,
        },
        span,
    );
}
