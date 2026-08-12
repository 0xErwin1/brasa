//! Expression lowering: every `compile_expr` call pushes exactly one
//! value. Dispatch follows `docs/spec/07-bytecode.md` case by case, on
//! the checker's static types (`expr_types`) rather than on runtime
//! values — the two agree for every checked program.

use brasa_bytecode::{Constant, Op, SlotIx, builtin_def, builtin_id};
use brasa_diagnostics::codes;
use brasa_hir::{BinaryOp, Expr, ExprId, ImportPath, Item, ItemId, LambdaBody, UnaryOp};
use brasa_resolver::{CtorRes, Res, TypeRes};
use brasa_source::Span;
use brasa_typeck::{Type, WrapDecision};

use crate::captures::lambda_captures;
use crate::catch::compile_catch;
use crate::func::{FnKind, FuncCx, PLACEHOLDER};
use crate::limits::MAX_ELEMENTS;
use crate::pattern::compile_match;
use crate::stmt::{block_value, if_value};

pub(crate) fn compile_expr(f: &mut FuncCx, id: ExprId) {
    let span = f.cx.hir.span_of_expr(id);

    match f.cx.hir.expr(id).clone() {
        Expr::Int(v) => {
            f.emit_const(Constant::Int(v), span);
        }
        Expr::Float(v) => {
            f.emit_const(Constant::Float(v), span);
        }
        Expr::Bool(v) => {
            f.emit(if v { Op::LoadTrue } else { Op::LoadFalse }, span);
        }
        Expr::Char(v) => {
            f.emit_const(Constant::Char(v), span);
        }
        Expr::Unit => {
            f.emit(Op::LoadUnit, span);
        }
        Expr::Str(text) => {
            f.emit_const(Constant::Str(text), span);
        }
        Expr::Ident(name) => ident(f, id, &name, span),
        Expr::SelfExpr => f.load_self(span),
        Expr::Call { callee, args } => call(f, callee, &args, span),
        Expr::Field { recv, name } => field(f, recv, &name, span),
        Expr::Index { recv, index } => {
            compile_expr(f, recv);
            compile_expr(f, index);
            f.emit(Op::GetIndex, span);
        }
        Expr::Unary { op, operand } => {
            compile_expr(f, operand);
            match op {
                UnaryOp::Neg => {
                    let op = match f.cx.types.expr_types.get(&operand) {
                        Some(Type::Float) => Op::NegFloat,
                        _ => Op::NegInt,
                    };
                    f.emit(op, span);
                }
                UnaryOp::Not => {
                    f.emit(Op::Not, span);
                }
            }
        }
        Expr::Binary { op, lhs, rhs } => binary(f, id, op, lhs, rhs, span),
        Expr::OptionWrap(inner) => {
            compile_expr(f, inner);
            match f.cx.types.wrap_decisions.get(&id) {
                Some(WrapDecision::Wrap) => {
                    f.emit(Op::WrapSome, span);
                }
                Some(WrapDecision::NoOp) => {}
                None => {
                    f.emit(Op::WrapSomeDynamic, span);
                }
            }
        }
        Expr::ToString(inner) => {
            compile_expr(f, inner);
            f.emit(Op::ToString, span);
        }
        Expr::Lambda { .. } => lambda(f, id, span),
        Expr::If(node) => if_value(f, &node, span),
        Expr::Match { scrutinee, arms } => compile_match(f, scrutinee, &arms, span),
        Expr::VectorLit(elements) => {
            for &element in &elements {
                compile_expr(f, element);
            }
            let n = element_count(f, "vector literal", "elements", elements.len(), span);
            f.emit(Op::MakeVector(n), span);
        }
        Expr::MapLit(pairs) => {
            for &(key, value) in &pairs {
                compile_expr(f, key);
                compile_expr(f, value);
            }
            let n = element_count(f, "map literal", "entries", pairs.len(), span);
            f.emit(Op::MakeMap(n), span);
        }
        Expr::TupleLit(elements) => {
            for &element in &elements {
                compile_expr(f, element);
            }
            let n = element_count(f, "tuple", "elements", elements.len(), span);
            f.emit(Op::MakeTuple(n), span);
        }
        Expr::StructLit { fields, .. } => struct_lit(f, id, &fields, span),
        Expr::Range { lo, hi, inclusive } => {
            compile_expr(f, lo);
            compile_expr(f, hi);
            f.emit(Op::MakeRange { inclusive }, span);
        }
        Expr::Catch { subject, arms, .. } => compile_catch(f, id, subject, &arms, span),
        Expr::EnumCtor { name, args } => enum_ctor(f, id, &name, &args, span),
    }
}

/// Narrows an aggregate literal's element count to the construction
/// instruction's operand, reporting the literal that does not fit.
fn element_count(f: &mut FuncCx, what: &str, unit: &str, count: usize, span: Span) -> u16 {
    u16::try_from(count).unwrap_or_else(|_| {
        f.cx.report(
            codes::C_TOO_MANY_ELEMENTS,
            format!("{what} has {count} {unit}, but the limit is {MAX_ELEMENTS}"),
            &format!("too many {unit}"),
            span,
        );
        u16::MAX
    })
}

fn ident(f: &mut FuncCx, id: ExprId, name: &str, span: Span) {
    match f.cx.res.expr_res.get(&id).copied() {
        Some(Res::Local(local)) => {
            let slot = f.slot_of(local);
            f.emit(Op::LoadLocal(slot), span);
        }
        Some(Res::Item(item)) => match f.cx.hir.item(item) {
            Item::FuncDef(_) => {
                let func = f.cx.func_of_item[&item];
                f.emit(Op::LoadFunc(func), span);
            }
            Item::TopLet(_) => {
                let global = f.cx.global_of_item[&item];
                f.emit(Op::LoadGlobal(global), span);
            }
            _ => f.emit_fatal(&format!("brasa: `{name}` is not a value"), span),
        },
        Some(Res::SelfParam) => f.load_self(span),
        Some(Res::Module(_)) => f.emit_fatal(
            &format!("brasa: module `{name}` is not a value; access members as `{name}.member`"),
            span,
        ),
        Some(Res::Builtin(_)) => f.emit_fatal(
            &format!("brasa: `{name}` cannot be used as a value in M1"),
            span,
        ),
        None => f.emit_fatal(&format!("brasa: unresolved name `{name}`"), span),
    }
}

fn binary(f: &mut FuncCx, id: ExprId, op: BinaryOp, lhs: ExprId, rhs: ExprId, span: Span) {
    match op {
        // Short-circuit forms keep the deciding value on the taken
        // branch (`docs/spec/07-bytecode.md`, jumps).
        BinaryOp::And => {
            compile_expr(f, lhs);
            let jump = f.emit(Op::JumpIfFalseOrPop(PLACEHOLDER), span);
            compile_expr(f, rhs);
            let end = f.here();
            f.patch(jump, end);
        }
        BinaryOp::Or => {
            compile_expr(f, lhs);
            let jump = f.emit(Op::JumpIfTrueOrPop(PLACEHOLDER), span);
            compile_expr(f, rhs);
            let end = f.here();
            f.patch(jump, end);
        }
        BinaryOp::Eq => {
            compile_expr(f, lhs);
            compile_expr(f, rhs);
            f.emit(Op::Eq, span);
        }
        BinaryOp::NotEq => {
            compile_expr(f, lhs);
            compile_expr(f, rhs);
            f.emit(Op::Eq, span);
            f.emit(Op::Not, span);
        }
        BinaryOp::Lt | BinaryOp::LtEq | BinaryOp::Gt | BinaryOp::GtEq => {
            ordering(f, op, lhs, rhs, span);
        }
        BinaryOp::Add
        | BinaryOp::Sub
        | BinaryOp::Mul
        | BinaryOp::Div
        | BinaryOp::Rem
        | BinaryOp::Pow => arithmetic(f, id, op, lhs, rhs, span),
    }
}

fn ordering_op(op: BinaryOp) -> Op {
    match op {
        BinaryOp::Lt => Op::Lt,
        BinaryOp::LtEq => Op::Le,
        BinaryOp::Gt => Op::Gt,
        BinaryOp::GtEq => Op::Ge,
        _ => unreachable!("only ordering operators reach here"),
    }
}

fn ordering(f: &mut FuncCx, op: BinaryOp, lhs: ExprId, rhs: ExprId, span: Span) {
    compile_expr(f, lhs);
    compile_expr(f, rhs);

    // `T: Comparable` compiles to the user's `cmp` plus an int
    // comparison against 0 (`docs/spec/07-bytecode.md`, comparison).
    if let Some(Type::Struct(item, _)) = f.cx.types.expr_types.get(&lhs) {
        let item = *item;
        match struct_method(f, item, "cmp") {
            Some(func) => {
                f.emit(Op::Call { func, argc: 2 }, span);
                f.emit_const(Constant::Int(0), span);
                f.emit(ordering_op(op), span);
            }
            None => {
                f.emit(Op::Pop, span);
                f.emit(Op::Pop, span);
                f.emit_fatal("brasa: unknown member `cmp`", span);
            }
        }
        return;
    }

    f.emit(ordering_op(op), span);
}

fn arithmetic(f: &mut FuncCx, id: ExprId, op: BinaryOp, lhs: ExprId, rhs: ExprId, span: Span) {
    compile_expr(f, lhs);
    compile_expr(f, rhs);

    // The checker resolves operand types statically (no mixing), so the
    // result type picks the typed op; the operand type breaks the tie
    // when the result was deferred.
    let ty = match f.cx.types.expr_types.get(&id) {
        Some(ty) if !ty.is_flexible() => ty,
        _ => f.cx.types.expr_types.get(&lhs).unwrap_or(&Type::Unknown),
    };

    let op = match (ty, op) {
        (Type::String, BinaryOp::Add) => Op::Concat,
        (Type::Float, BinaryOp::Add) => Op::AddFloat,
        (Type::Float, BinaryOp::Sub) => Op::SubFloat,
        (Type::Float, BinaryOp::Mul) => Op::MulFloat,
        (Type::Float, BinaryOp::Div) => Op::DivFloat,
        (Type::Float, BinaryOp::Rem) => Op::RemFloat,
        (Type::Float, BinaryOp::Pow) => Op::PowFloat,
        (_, BinaryOp::Add) => Op::AddInt,
        (_, BinaryOp::Sub) => Op::SubInt,
        (_, BinaryOp::Mul) => Op::MulInt,
        (_, BinaryOp::Div) => Op::DivInt,
        (_, BinaryOp::Rem) => Op::RemInt,
        (_, BinaryOp::Pow) => Op::PowInt,
        _ => unreachable!("only arithmetic operators reach here"),
    };
    f.emit(op, span);
}

/// The declared-method index of `name` on a struct item, as a direct
/// function id.
fn struct_method(f: &FuncCx, item: ItemId, name: &str) -> Option<brasa_bytecode::FuncId> {
    let Item::StructDef(def) = f.cx.hir.item(item) else {
        return None;
    };
    let index = def.methods.iter().position(|m| m.name == name)?;
    f.cx.func_of_method.get(&(item, index)).copied()
}

fn struct_field_index(f: &FuncCx, item: ItemId, name: &str) -> Option<u16> {
    let Item::StructDef(def) = f.cx.hir.item(item) else {
        return None;
    };
    let index = def.fields.iter().position(|field| field.name == name)?;

    // A struct whose field count outruns the operand is reported by
    // `Cx::collect`, before any body is lowered.
    Some(u16::try_from(index).unwrap_or(u16::MAX))
}

pub(crate) fn call(f: &mut FuncCx, callee: ExprId, args: &[ExprId], span: Span) {
    match f.cx.hir.expr(callee).clone() {
        // `puts`/`print` (`docs/spec/05-stdlib.md`).
        Expr::Ident(_) if matches!(f.cx.res.expr_res.get(&callee), Some(Res::Builtin(_))) => {
            let Some(Res::Builtin(builtin)) = f.cx.res.expr_res.get(&callee).copied() else {
                unreachable!("guarded by the match arm");
            };
            if args.len() != 1 {
                for &arg in args {
                    compile_expr(f, arg);
                    f.emit(Op::Pop, span);
                }
                f.emit_fatal("brasa: `puts`/`print` take exactly 1 argument", span);
                return;
            }
            compile_expr(f, args[0]);
            let builtin = builtin_id(builtin.name()).expect("prelude builtins are registered");
            f.emit(Op::CallBuiltin { builtin, argc: 1 }, span);
        }
        Expr::Field { recv, name } => method_call(f, recv, &name, args, span),
        Expr::Ident(_)
            if matches!(
                f.cx.res.expr_res.get(&callee),
                Some(Res::Item(item)) if matches!(f.cx.hir.item(*item), Item::FuncDef(_))
            ) =>
        {
            let Some(Res::Item(item)) = f.cx.res.expr_res.get(&callee).copied() else {
                unreachable!("guarded by the match arm");
            };
            for &arg in args {
                compile_expr(f, arg);
            }
            let func = f.cx.func_of_item[&item];
            let argc = f.cx.argc(args.len(), span);
            f.emit(Op::Call { func, argc }, span);
        }
        _ => {
            compile_expr(f, callee);
            for &arg in args {
                compile_expr(f, arg);
            }
            let argc = f.cx.argc(args.len(), span);
            f.emit(Op::CallValue { argc }, span);
        }
    }
}

fn method_call(f: &mut FuncCx, recv: ExprId, name: &str, args: &[ExprId], span: Span) {
    if let Expr::Ident(_) = f.cx.hir.expr(recv)
        && let Some(Res::Module(item)) = f.cx.res.expr_res.get(&recv).copied()
    {
        module_call(f, item, name, args, span);
        return;
    }

    if let Some(Type::Struct(item, _)) = f.cx.types.expr_types.get(&recv) {
        let item = *item;

        if let Some(func) = struct_method(f, item, name) {
            compile_expr(f, recv);
            for &arg in args {
                compile_expr(f, arg);
            }
            let argc = f.cx.argc(args.len() + 1, span);
            f.emit(Op::Call { func, argc }, span);
            return;
        }

        // A struct field holding a callable: load it, then call the
        // value (the walker's field-before-builtin fallback).
        if let Some(index) = struct_field_index(f, item, name) {
            compile_expr(f, recv);
            f.emit(Op::GetField(index), span);
            for &arg in args {
                compile_expr(f, arg);
            }
            let argc = f.cx.argc(args.len(), span);
            f.emit(Op::CallValue { argc }, span);
            return;
        }

        if name == "toString" && args.is_empty() {
            compile_expr(f, recv);
            f.emit(Op::ToString, span);
            return;
        }

        f.emit_fatal(&format!("brasa: unknown member `{name}`"), span);
        return;
    }

    // The universal derived `toString`: one op, which also dispatches a
    // user struct override through the shape.
    if name == "toString" && args.is_empty() {
        compile_expr(f, recv);
        f.emit(Op::ToString, span);
        return;
    }

    // A receiver typed as a generic parameter: the constraint's method
    // is a different function at every instantiation and the body is
    // shared (no monomorphization, `docs/spec/03-types.md`), so the
    // target comes from the runtime value's method table.
    if matches!(f.cx.types.expr_types.get(&recv), Some(Type::Generic { .. })) {
        compile_expr(f, recv);
        for &arg in args {
            compile_expr(f, arg);
        }
        let name = f.cx.const_str(name);
        let argc = f.cx.argc(args.len() + 1, span);
        f.emit(Op::CallMethodDyn { name, argc }, span);
        return;
    }

    match builtin_id(name).filter(|&id| builtin_def(id).is_some_and(|def| def.has_receiver)) {
        Some(builtin) => {
            compile_expr(f, recv);
            for &arg in args {
                compile_expr(f, arg);
            }
            let argc = f.cx.argc(args.len() + 1, span);
            f.emit(Op::CallBuiltin { builtin, argc }, span);
        }
        None => f.emit_fatal(&format!("brasa: unknown builtin method `{name}`"), span),
    }
}

fn module_call(f: &mut FuncCx, module_item: ItemId, name: &str, args: &[ExprId], span: Span) {
    let Item::Import(import) = f.cx.hir.item(module_item) else {
        f.emit_fatal("brasa: module handle is not an import", span);
        return;
    };

    let is_std = matches!(&import.path, ImportPath::Std(_));
    let module = match &import.path {
        ImportPath::Std(segments) => segments.last().cloned().unwrap_or_default(),
        ImportPath::File(path) => std::path::Path::new(path)
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.clone()),
    };

    if is_std
        && matches!(
            module.as_str(),
            "math" | "proc" | "env" | "fs" | "json" | "io" | "time" | "rand"
        )
    {
        if let Some(builtin) = builtin_id(&format!("{module}.{name}")) {
            for &arg in args {
                compile_expr(f, arg);
            }
            let argc = f.cx.argc(args.len(), span);
            f.emit(Op::CallBuiltin { builtin, argc }, span);
            return;
        }

        // Argument side effects run before the walker's fatal.
        for &arg in args {
            compile_expr(f, arg);
            f.emit(Op::Pop, span);
        }
        f.emit_fatal(
            &format!("brasa: unknown member `{name}` on module `{module}`"),
            span,
        );
        return;
    }

    for &arg in args {
        compile_expr(f, arg);
        f.emit(Op::Pop, span);
    }
    f.emit_fatal(
        &format!("brasa: module `{module}` is not available yet (importing from another file is not implemented)"),
        span,
    );
}

fn field(f: &mut FuncCx, recv: ExprId, name: &str, span: Span) {
    // A member read on a module handle: no module exposes plain values
    // in M1, so this reuses the module-call path with zero arguments,
    // as the spec requires.
    if let Expr::Ident(_) = f.cx.hir.expr(recv)
        && let Some(Res::Module(item)) = f.cx.res.expr_res.get(&recv).copied()
    {
        module_call(f, item, name, &[], span);
        return;
    }

    if let Some(Type::Struct(item, _)) = f.cx.types.expr_types.get(&recv) {
        let item = *item;

        if let Some(index) = struct_field_index(f, item, name) {
            compile_expr(f, recv);
            f.emit(Op::GetField(index), span);
            return;
        }
        if let Some(func) = struct_method(f, item, name) {
            compile_expr(f, recv);
            f.emit(Op::BindMethod(func), span);
            return;
        }
        if name == "toString" {
            compile_expr(f, recv);
            let builtin = builtin_id("toString").expect("toString is registered");
            f.emit(Op::BindBuiltin(builtin), span);
            return;
        }

        f.emit_fatal(&format!("brasa: unknown member `{name}`"), span);
        return;
    }

    // A field read on a native record — the `proc` `Output` (BRS-32) or
    // the `fs` `Walk` (BRS-66) — is a receiver-only builtin call
    // yielding the field value directly, rather than a bound method.
    let native_field = match f.cx.types.expr_types.get(&recv) {
        Some(Type::ProcOutput) => matches!(name, "stdout" | "stderr" | "code"),
        Some(Type::Walk) => matches!(name, "paths" | "unreadable"),
        _ => false,
    };

    if native_field {
        let builtin = builtin_id(name).expect("native record field accessors are registered");
        compile_expr(f, recv);
        f.emit(Op::CallBuiltin { builtin, argc: 1 }, span);
        return;
    }

    // A generic receiver, as in `method_call`: the member is whatever
    // the runtime value carries.
    if matches!(f.cx.types.expr_types.get(&recv), Some(Type::Generic { .. })) {
        compile_expr(f, recv);
        let name = f.cx.const_str(name);
        f.emit(Op::BindMethodDyn(name), span);
        return;
    }

    match builtin_id(name).filter(|&id| builtin_def(id).is_some_and(|def| def.has_receiver)) {
        Some(builtin) => {
            compile_expr(f, recv);
            f.emit(Op::BindBuiltin(builtin), span);
        }
        None => f.emit_fatal(&format!("brasa: unknown builtin method `{name}`"), span),
    }
}

fn lambda(f: &mut FuncCx, id: ExprId, span: Span) {
    let caps = lambda_captures(f.cx.hir, f.cx.res, id);
    let params = f.cx.res.lambda_params.get(&id).cloned().unwrap_or_default();
    let Expr::Lambda { body, .. } = f.cx.hir.expr(id).clone() else {
        unreachable!("lambda lowering is only called on lambdas");
    };

    let func = f.cx.reserve_function();
    let arity = f.cx.arity("lambda", params.len(), span);
    let capture_count =
        f.cx.capture_count(caps.locals.len() + usize::from(caps.uses_self), span);

    // Compile the lambda body into its own function. Frame layout:
    // parameters, then captures (`self` first when captured, then the
    // free locals in ascending LocalId order — the capture order
    // contract), then the remaining locals.
    let function = {
        let mut sub = FuncCx::new(&mut *f.cx, FnKind::Lambda);
        // The position fits the slot operand: a parameter list longer
        // than `MAX_PARAMS` has already been reported just above.
        for (position, &local) in params.iter().enumerate() {
            sub.assign_slot(local, SlotIx(u16::try_from(position).unwrap_or(u16::MAX)));
        }

        if caps.uses_self {
            let slot = sub.alloc_slot();
            sub.self_slot = Some(slot);
        }
        for &local in &caps.locals {
            sub.slot_of(local);
        }

        match &body {
            LambdaBody::Expr(expr) => compile_expr(&mut sub, *expr),
            LambdaBody::Block(block) => block_value(&mut sub, block, span),
        }
        sub.emit(Op::Ret, span);
        sub.finish("<lambda>".to_string(), arity, capture_count, span)
    };
    f.cx.define_function(func, function);

    // Snapshot the captured values from the enclosing frame, in
    // capture order.
    if caps.uses_self {
        f.load_self(span);
    }
    for &local in &caps.locals {
        let slot = f.slot_of(local);
        f.emit(Op::LoadLocal(slot), span);
    }
    f.emit(
        Op::MakeClosure {
            func,
            captures: capture_count,
        },
        span,
    );
}

fn struct_lit(f: &mut FuncCx, id: ExprId, fields: &[(String, ExprId)], span: Span) {
    let Some(TypeRes::Item(item)) = f.cx.res.struct_lit_res.get(&id).copied() else {
        f.emit_fatal("brasa: unresolved struct literal", span);
        return;
    };
    let Some(&struct_id) = f.cx.struct_of_item.get(&item) else {
        f.emit_fatal("brasa: struct literal of a non-struct type", span);
        return;
    };

    let declared = f.cx.structs[struct_id.0 as usize].fields.clone();
    let written_in_order = declared.len() == fields.len()
        && declared
            .iter()
            .zip(fields)
            .all(|(decl, (written, _))| decl == written);

    if written_in_order {
        for &(_, value) in fields {
            compile_expr(f, value);
        }
        f.emit(Op::MakeStruct(struct_id), span);
        return;
    }

    // Initializers evaluate in written order; the values are parked in
    // scratch slots and reloaded in declaration order (the spec's
    // "reordered to declaration order by codegen").
    let mut scratch: Vec<(&str, SlotIx)> = Vec::with_capacity(fields.len());
    for (name, value) in fields {
        compile_expr(f, *value);
        let slot = f.alloc_slot();
        f.emit(Op::StoreLocal(slot), span);
        scratch.push((name, slot));
    }

    for name in &declared {
        match scratch.iter().find(|(written, _)| written == name) {
            Some(&(_, slot)) => {
                f.emit(Op::LoadLocal(slot), span);
            }
            None => f.emit_fatal(&format!("brasa: missing field `{name}`"), span),
        }
    }
    f.emit(Op::MakeStruct(struct_id), span);
}

fn enum_ctor(f: &mut FuncCx, id: ExprId, name: &str, args: &[ExprId], span: Span) {
    let discard_args = |f: &mut FuncCx| {
        for &arg in args {
            compile_expr(f, arg);
            f.emit(Op::Pop, span);
        }
    };

    match f.cx.res.ctor_expr_res.get(&id).copied() {
        Some(CtorRes::OptionSome) => match args {
            [inner] => {
                compile_expr(f, *inner);
                f.emit(Op::WrapSome, span);
            }
            _ => {
                discard_args(f);
                f.emit_fatal("brasa: `Some` takes exactly 1 argument", span);
            }
        },
        Some(CtorRes::OptionNone) => {
            discard_args(f);
            f.emit(Op::LoadNone, span);
        }
        Some(CtorRes::SetCtor) => match args {
            [vector] => {
                compile_expr(f, *vector);
                f.emit(Op::MakeSetFromVector, span);
            }
            _ => {
                discard_args(f);
                f.emit_fatal("brasa: `Set` takes exactly 1 Vector argument", span);
            }
        },
        Some(CtorRes::EnumVariant {
            enum_item,
            variant_index,
        }) => {
            for &arg in args {
                compile_expr(f, arg);
            }
            let enum_id = f.cx.enum_of_item[&enum_item];
            // An enum with more variants than the operand indexes is
            // reported by `Cx::collect`, before any body is lowered.
            let variant = u16::try_from(variant_index).unwrap_or(u16::MAX);
            let argc = f.cx.argc(args.len(), span);
            f.emit(
                Op::MakeEnum {
                    enum_id,
                    variant,
                    argc,
                },
                span,
            );
        }
        None => {
            discard_args(f);
            f.emit_fatal(&format!("brasa: unresolved constructor `{name}`"), span);
        }
    }
}
