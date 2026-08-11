//! Bytecode container types for the Brasa VM (M3).
//!
//! Normative design: `docs/spec/07-bytecode.md`. This crate owns the
//! instruction set ([`Op`]), the per-function [`Chunk`] (code, span side
//! table, handler table), the interned [`ConstPool`], the [`Module`]
//! format (function table, struct/enum shapes, global slots), and a
//! deterministic disassembler ([`dump::dump`]).
//!
//! It deliberately contains NO code generation (BRS-27) and NO
//! execution (BRS-28): only the shared vocabulary those units target.

pub mod chunk;
pub mod constant;
pub mod dump;
pub mod module;
pub mod op;

pub use chunk::{Chunk, Handler};
pub use constant::{ConstPool, Constant};
pub use module::{EnumShape, Function, Module, StructShape, Variant};
pub use op::Op;

/// Index into a module's constant pool.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct ConstId(pub u32);

/// Index into a module's function table.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct FuncId(pub u32);

/// Index into a module's struct-shape table.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct StructId(pub u32);

/// Index into a module's enum-shape table.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct EnumId(pub u32);

/// Opaque index into the native builtin registry (BRS-28/M4 scope).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct BuiltinId(pub u16);

/// Absolute instruction index into a chunk's code (word-code: one `Op`
/// per index, jump targets are absolute).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CodeIx(pub u32);

/// Frame-local slot index (parameters, captures, then remaining locals;
/// layout in `docs/spec/07-bytecode.md`).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct SlotIx(pub u16);

/// Module global slot index (one per top-`let` item).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct GlobalIx(pub u16);
