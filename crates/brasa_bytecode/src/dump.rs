//! A deterministic text disassembly of a [`Module`].
//!
//! Mirrors `brasa_hir::dump` for the same reason: insta snapshots.
//! Spans are never printed (they would break snapshots on every
//! whitespace-only fixture edit); everything else renders in table
//! order, so two compilations of the same input produce byte-identical
//! dumps. Names resolved through the module (constants, callees,
//! shapes) appear as `; comments` after the operands.

use std::fmt::Write as _;

use crate::{Chunk, Constant, Function, Module, Op};

/// Renders the whole module: constants, shapes, globals, then every
/// function with its code and handler table.
pub fn dump(module: &Module) -> String {
    let mut out = String::new();

    dump_constants(module, &mut out);
    dump_shapes(module, &mut out);
    dump_globals(module, &mut out);

    for (i, func) in module.functions.iter().enumerate() {
        dump_function(module, i, func, &mut out);
    }

    out
}

fn dump_constants(module: &Module, out: &mut String) {
    if module.constants.is_empty() {
        return;
    }

    out.push_str("== constants ==\n");
    for (id, constant) in module.constants.iter() {
        let _ = writeln!(out, "  c{}  {}", id.0, render_constant(constant));
    }
    out.push('\n');
}

fn render_constant(constant: &Constant) -> String {
    match constant {
        Constant::Int(v) => format!("int {v}"),
        Constant::Float(v) => format!("float {v:?}"),
        Constant::Str(v) => format!("str {v:?}"),
        Constant::Char(v) => format!("char {v:?}"),
    }
}

fn dump_shapes(module: &Module, out: &mut String) {
    for shape in &module.structs {
        let fields = shape.fields.join(", ");
        let methods: Vec<String> = shape.methods.iter().map(|f| format!("fn{}", f.0)).collect();
        let to_string = match shape.to_string {
            Some(f) => format!("fn{}", f.0),
            None => "derived".to_string(),
        };
        let _ = writeln!(
            out,
            "== struct {} (fields: [{}]; methods: [{}]; toString: {}) ==\n",
            shape.name,
            fields,
            methods.join(", "),
            to_string
        );
    }

    for shape in &module.enums {
        let _ = writeln!(out, "== enum {} ==", shape.name);
        for variant in &shape.variants {
            let _ = writeln!(out, "  {}/{}", variant.name, variant.arity);
        }
        out.push('\n');
    }
}

fn dump_globals(module: &Module, out: &mut String) {
    if module.globals.is_empty() {
        return;
    }

    out.push_str("== globals ==\n");
    for (i, name) in module.globals.iter().enumerate() {
        let _ = writeln!(out, "  g{i}  {name}");
    }
    out.push('\n');
}

fn dump_function(module: &Module, index: usize, func: &Function, out: &mut String) {
    let _ = writeln!(
        out,
        "== fn{index} {} arity={} captures={} locals={} max_stack={} ==",
        func.name, func.arity, func.captures, func.locals, func.max_stack
    );

    for (ix, op) in func.chunk.ops().iter().enumerate() {
        let (text, comment) = render_op(module, op);
        match comment {
            Some(comment) => {
                let _ = writeln!(out, "  {ix:4}  {text:<28}; {comment}");
            }
            None => {
                let _ = writeln!(out, "  {ix:4}  {text}");
            }
        }
    }

    dump_handlers(&func.chunk, out);
    out.push('\n');
}

fn dump_handlers(chunk: &Chunk, out: &mut String) {
    if chunk.handlers().is_empty() {
        return;
    }

    out.push_str("  handlers:\n");
    for handler in chunk.handlers() {
        let _ = writeln!(
            out,
            "    {}..{} -> {} depth={}",
            handler.start.0, handler.end.0, handler.target.0, handler.depth
        );
    }
}

/// Mnemonic + operands, plus an optional resolved-name comment.
fn render_op(module: &Module, op: &Op) -> (String, Option<String>) {
    let func_name = |f: crate::FuncId| module.functions.get(f.0 as usize).map(|f| f.name.clone());
    let const_text = |c: crate::ConstId| render_constant(module.constants.get(c));
    let builtin_name = |b: crate::BuiltinId| crate::builtin_def(b).map(|def| def.name.to_string());

    match *op {
        Op::Const(c) => (format!("const c{}", c.0), Some(const_text(c))),
        Op::LoadUnit => ("load_unit".to_string(), None),
        Op::LoadTrue => ("load_true".to_string(), None),
        Op::LoadFalse => ("load_false".to_string(), None),
        Op::LoadNone => ("load_none".to_string(), None),
        Op::Pop => ("pop".to_string(), None),
        Op::Dup => ("dup".to_string(), None),
        Op::LoadLocal(s) => (format!("load_local {}", s.0), None),
        Op::StoreLocal(s) => (format!("store_local {}", s.0), None),
        Op::MakeBinding(s) => (format!("make_binding {}", s.0), None),
        Op::LoadBinding(s) => (format!("load_binding {}", s.0), None),
        Op::StoreBinding(s) => (format!("store_binding {}", s.0), None),
        Op::LoadGlobal(g) => (
            format!("load_global g{}", g.0),
            module.globals.get(g.0 as usize).cloned(),
        ),
        Op::StoreGlobal(g) => (
            format!("store_global g{}", g.0),
            module.globals.get(g.0 as usize).cloned(),
        ),
        Op::LoadFunc(f) => (format!("load_func fn{}", f.0), func_name(f)),
        Op::AddInt => ("add_int".to_string(), None),
        Op::SubInt => ("sub_int".to_string(), None),
        Op::MulInt => ("mul_int".to_string(), None),
        Op::DivInt => ("div_int".to_string(), None),
        Op::RemInt => ("rem_int".to_string(), None),
        Op::PowInt => ("pow_int".to_string(), None),
        Op::NegInt => ("neg_int".to_string(), None),
        Op::AddFloat => ("add_float".to_string(), None),
        Op::SubFloat => ("sub_float".to_string(), None),
        Op::MulFloat => ("mul_float".to_string(), None),
        Op::DivFloat => ("div_float".to_string(), None),
        Op::RemFloat => ("rem_float".to_string(), None),
        Op::PowFloat => ("pow_float".to_string(), None),
        Op::NegFloat => ("neg_float".to_string(), None),
        Op::Concat => ("concat".to_string(), None),
        Op::Not => ("not".to_string(), None),
        Op::Eq => ("eq".to_string(), None),
        Op::Lt => ("lt".to_string(), None),
        Op::Le => ("le".to_string(), None),
        Op::Gt => ("gt".to_string(), None),
        Op::Ge => ("ge".to_string(), None),
        Op::Jump(t) => (format!("jump {}", t.0), None),
        Op::JumpIfFalse(t) => (format!("jump_if_false {}", t.0), None),
        Op::JumpIfFalseOrPop(t) => (format!("jump_if_false_or_pop {}", t.0), None),
        Op::JumpIfTrueOrPop(t) => (format!("jump_if_true_or_pop {}", t.0), None),
        Op::JumpIfVariantNe { variant, target } => {
            (format!("jump_if_variant_ne {variant}, {}", target.0), None)
        }
        Op::JumpIfNone(t) => (format!("jump_if_none {}", t.0), None),
        Op::WrapSome => ("wrap_some".to_string(), None),
        Op::WrapSomeDynamic => ("wrap_some_dynamic".to_string(), None),
        Op::UnwrapSome => ("unwrap_some".to_string(), None),
        Op::TupleField(i) => (format!("tuple_field {i}"), None),
        Op::EnumField(i) => (format!("enum_field {i}"), None),
        Op::GetField(i) => (format!("get_field {i}"), None),
        Op::SetField(i) => (format!("set_field {i}"), None),
        Op::GetIndex => ("get_index".to_string(), None),
        Op::SetIndex => ("set_index".to_string(), None),
        Op::Call { func, argc } => (format!("call fn{}, {argc}", func.0), func_name(func)),
        Op::CallValue { argc } => (format!("call_value {argc}"), None),
        Op::CallBuiltin { builtin, argc } => (
            format!("call_builtin b{}, {argc}", builtin.0),
            builtin_name(builtin),
        ),
        Op::CallMethodDyn { name, argc } => (
            format!("call_method_dyn c{}, {argc}", name.0),
            Some(const_text(name)),
        ),
        Op::BindMethodDyn(name) => (
            format!("bind_method_dyn c{}", name.0),
            Some(const_text(name)),
        ),
        Op::BindMethod(f) => (format!("bind_method fn{}", f.0), func_name(f)),
        Op::BindBuiltin(b) => (format!("bind_builtin b{}", b.0), builtin_name(b)),
        Op::Ret => ("ret".to_string(), None),
        Op::MakeVector(n) => (format!("make_vector {n}"), None),
        Op::MakeMap(n) => (format!("make_map {n}"), None),
        Op::MakeTuple(n) => (format!("make_tuple {n}"), None),
        Op::MakeSetFromVector => ("make_set_from_vector".to_string(), None),
        Op::MakeStruct(s) => (
            format!("make_struct s{}", s.0),
            module.structs.get(s.0 as usize).map(|s| s.name.clone()),
        ),
        Op::MakeEnum {
            enum_id,
            variant,
            argc,
        } => {
            let comment = module.enums.get(enum_id.0 as usize).map(|e| {
                match e.variants.get(variant as usize) {
                    Some(v) => format!("{}.{}", e.name, v.name),
                    None => e.name.clone(),
                }
            });
            (
                format!("make_enum e{}, {variant}, {argc}", enum_id.0),
                comment,
            )
        }
        Op::MakeClosure { func, captures } => (
            format!("make_closure fn{}, {captures}", func.0),
            func_name(func),
        ),
        Op::MakeRange { inclusive } => (format!("make_range inclusive={inclusive}"), None),
        Op::ToString => ("to_string".to_string(), None),
        Op::IterNew => ("iter_new".to_string(), None),
        Op::IterNext(t) => (format!("iter_next {}", t.0), None),
        Op::Throw => ("throw".to_string(), None),
        Op::JumpIfPanic(t) => (format!("jump_if_panic {}", t.0), None),
        Op::JumpIfTagNe { tag, target } => (
            format!("jump_if_tag_ne c{}, {}", tag.0, target.0),
            Some(const_text(tag)),
        ),
        Op::CaughtValue => ("caught_value".to_string(), None),
        Op::CaughtDetail => ("caught_detail".to_string(), None),
        Op::Rethrow => ("rethrow".to_string(), None),
    }
}
