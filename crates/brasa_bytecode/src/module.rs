//! The compiled module: what BRS-27 emits and BRS-28 executes.
//!
//! Entry convention (`docs/spec/07-bytecode.md`, module execution):
//! `functions[0]` is the synthetic `<toplevel>` function — every
//! module's top-level statements and top-`let` initializers, imported
//! modules first — and the driver then calls [`Module::entry`] if the
//! executed file defines `main`.

use crate::{Chunk, ConstPool, FuncId};

/// A whole compiled program. In-memory only: bytecode is never
/// serialized (spec non-goal).
///
/// A multi-file program compiles to one of these, not one per file: the
/// module graph is flattened at compile time, so function, struct, enum
/// and global indices are program-wide and the VM never needs a module
/// concept.
#[derive(Debug, Default)]
pub struct Module {
    pub constants: ConstPool,
    /// `functions[0]` is `<toplevel>`.
    pub functions: Vec<Function>,
    pub structs: Vec<StructShape>,
    pub enums: Vec<EnumShape>,
    /// Global slot names (one per top-`let` item), indexed by
    /// [`crate::GlobalIx`]. Names exist for diagnostics and the
    /// disassembler; slots start unset at runtime.
    pub globals: Vec<String>,
    /// The `main` to call after `<toplevel>` returns, when the executed
    /// file defines one. Named by the code generator rather than found
    /// by name at run time: an imported module may define its own
    /// `main`, and only the executed file's is an entry point
    /// (`docs/spec/01-syntax.md`).
    pub entry: Option<FuncId>,
}

/// One function-table entry: top-level function, struct method, lambda,
/// or the synthetic `<toplevel>`.
#[derive(Debug)]
pub struct Function {
    /// `<toplevel>`, `<lambda>`, or the declared name — stacktraces and
    /// the disassembler.
    pub name: String,
    /// Parameter count; methods count `self` (slot 0).
    pub arity: u8,
    /// Capture slot count (0 for non-lambdas). Captures are copied into
    /// the frame slots after the parameters at call time.
    pub captures: u16,
    /// Total frame slot count, parameters and captures included.
    pub locals: u16,
    /// Maximum operand-stack depth above the locals boundary, computed
    /// by the code generator so the VM can reserve stack space on frame
    /// entry without per-push checks (`docs/spec/07-bytecode.md`,
    /// function table).
    pub max_stack: u16,
    pub chunk: Chunk,
}

/// Runtime shape of a struct: the nominal tag `catch` and `toString`
/// use, plus the field/method layout construction and dispatch need.
#[derive(Debug)]
pub struct StructShape {
    pub name: String,
    /// Field names in declaration order; [`crate::Op::GetField`]
    /// indices point here.
    pub fields: Vec<String>,
    /// Declared methods, in declaration order, as function-table
    /// entries.
    pub methods: Vec<FuncId>,
    /// A user-defined `toString` override, dispatched by
    /// [`crate::Op::ToString`] instead of the derived rendering.
    pub to_string: Option<FuncId>,
}

/// Runtime shape of an enum: nominal tag plus variant names/arities for
/// construction, matching, and derived `toString`.
#[derive(Debug)]
pub struct EnumShape {
    pub name: String,
    pub variants: Vec<Variant>,
}

#[derive(Debug)]
pub struct Variant {
    pub name: String,
    /// Payload count.
    pub arity: u8,
}
