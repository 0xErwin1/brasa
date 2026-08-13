//! Disassembly snapshot tests: modules assembled by hand (no codegen
//! exists yet — that is BRS-27), disassembled with `brasa_bytecode::dump`,
//! and snapshotted so an accidental change to the mnemonics, operand
//! rendering, or section layout is caught.

use brasa_source::Span;

use brasa_bytecode::{
    BuiltinId, Chunk, CodeIx, ConstPool, Constant, EnumId, EnumShape, FuncId, Function, GlobalIx,
    Handler, Module, Op, SlotIx, StructId, StructShape, Variant,
};

fn push_all(chunk: &mut Chunk, ops: &[Op]) {
    for &op in ops {
        chunk.push(op, Span::default());
    }
}

/// Arithmetic, jumps, globals, construction, and a call — the shape of
/// a compiled `<toplevel>` plus one function.
#[test]
fn arithmetic_and_calls() {
    let mut constants = ConstPool::new();
    let one = constants.insert(Constant::Int(1));
    let two = constants.insert(Constant::Int(2));
    let pi = constants.insert(Constant::Float(3.5));
    let hello = constants.insert(Constant::Str("hello".to_string()));

    let mut toplevel = Chunk::new();
    push_all(
        &mut toplevel,
        &[
            Op::Const(one),
            Op::Const(two),
            Op::AddInt,
            Op::StoreGlobal(GlobalIx(0)),
            Op::Const(pi),
            Op::NegFloat,
            Op::Pop,
            Op::Const(hello),
            Op::ToString,
            Op::CallBuiltin {
                builtin: BuiltinId(0),
                argc: 1,
            },
            Op::Pop,
            Op::LoadUnit,
            Op::Ret,
        ],
    );

    let mut double = Chunk::new();
    push_all(
        &mut double,
        &[
            Op::LoadLocal(SlotIx(0)),
            Op::Const(two),
            Op::MulInt,
            Op::Ret,
        ],
    );

    let mut main = Chunk::new();
    push_all(
        &mut main,
        &[
            Op::LoadGlobal(GlobalIx(0)),
            Op::Call {
                func: FuncId(1),
                argc: 1,
            },
            Op::Dup,
            Op::Const(one),
            Op::Lt,
            Op::JumpIfFalse(CodeIx(7)),
            Op::NegInt,
            Op::MakeVector(1),
            Op::Pop,
            Op::LoadUnit,
            Op::Ret,
        ],
    );

    let module = Module {
        constants,
        functions: vec![
            Function {
                name: "<toplevel>".to_string(),
                arity: 0,
                captures: 0,
                locals: 0,
                max_stack: 4,
                chunk: toplevel,
            },
            Function {
                name: "double".to_string(),
                arity: 1,
                captures: 0,
                locals: 1,
                max_stack: 4,
                chunk: double,
            },
            Function {
                name: "main".to_string(),
                arity: 0,
                captures: 0,
                locals: 0,
                max_stack: 4,
                chunk: main,
            },
        ],
        structs: vec![],
        enums: vec![],
        globals: vec!["total".to_string()],
        tests: vec![],
        entry: None,
    };

    insta::assert_snapshot!("arithmetic_and_calls", brasa_bytecode::dump::dump(&module));
}

/// A catch dispatch sequence with a handler table, plus struct/enum
/// shapes, closures, and iteration ops.
#[test]
fn catch_shapes_and_iteration() {
    let mut constants = ConstPool::new();
    let tag = constants.insert(Constant::Str("NetError".to_string()));
    let panic_tag = constants.insert(Constant::Str("panics.IndexOutOfBounds".to_string()));
    let zero = constants.insert(Constant::Int(0));

    // fn1 `risky` throws a struct error.
    let mut risky = Chunk::new();
    push_all(
        &mut risky,
        &[Op::Const(zero), Op::MakeStruct(StructId(0)), Op::Throw],
    );

    // <toplevel>: `risky() catch (e) NetError => 0; panics.IndexOutOfBounds => 0 end`
    // subject at 0..1; dispatch sequence from 3.
    let mut toplevel = Chunk::new();
    push_all(
        &mut toplevel,
        &[
            Op::Call {
                func: FuncId(1),
                argc: 0,
            },
            Op::Jump(CodeIx(15)),
            Op::LoadUnit, // padding between subject and dispatch
            // dispatch: NetError arm
            Op::JumpIfTagNe {
                tag,
                target: CodeIx(8),
            },
            Op::CaughtValue,
            Op::StoreLocal(SlotIx(0)),
            Op::Pop,
            Op::Jump(CodeIx(15)),
            // dispatch: named panic arm
            Op::JumpIfTagNe {
                tag: panic_tag,
                target: CodeIx(13),
            },
            Op::CaughtDetail,
            Op::StoreLocal(SlotIx(0)),
            Op::Pop,
            Op::Jump(CodeIx(15)),
            // wildcard would go here; unmatched -> rethrow
            Op::JumpIfPanic(CodeIx(14)),
            Op::Rethrow,
            // join: iterate a range, make a closure
            Op::Const(zero),
            Op::Const(zero),
            Op::MakeRange { inclusive: false },
            Op::IterNew,
            Op::IterNext(CodeIx(22)),
            Op::Pop,
            Op::Jump(CodeIx(19)),
            Op::MakeClosure {
                func: FuncId(1),
                captures: 0,
            },
            Op::MakeEnum {
                enum_id: EnumId(0),
                variant: 1,
                argc: 0,
            },
            Op::MakeTuple(2),
            Op::Pop,
            Op::LoadUnit,
            Op::Ret,
        ],
    );
    toplevel.push_handler(Handler {
        start: CodeIx(0),
        end: CodeIx(1),
        target: CodeIx(3),
        depth: 0,
    });

    let module = Module {
        constants,
        functions: vec![
            Function {
                name: "<toplevel>".to_string(),
                arity: 0,
                captures: 0,
                locals: 1,
                max_stack: 4,
                chunk: toplevel,
            },
            Function {
                name: "risky".to_string(),
                arity: 0,
                captures: 0,
                locals: 0,
                max_stack: 4,
                chunk: risky,
            },
        ],
        structs: vec![StructShape {
            name: "NetError".to_string(),
            fields: vec!["detail".to_string()],
            methods: vec![],
            to_string: None,
        }],
        enums: vec![EnumShape {
            name: "Shape".to_string(),
            variants: vec![
                Variant {
                    name: "Circle".to_string(),
                    arity: 1,
                },
                Variant {
                    name: "Dot".to_string(),
                    arity: 0,
                },
            ],
        }],
        globals: vec![],
        tests: vec![],
        entry: None,
    };

    insta::assert_snapshot!(
        "catch_shapes_and_iteration",
        brasa_bytecode::dump::dump(&module)
    );
}
