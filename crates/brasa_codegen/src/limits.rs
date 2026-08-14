//! The limits the instruction set imposes, and the diagnostics that
//! report them.
//!
//! Every limit here is inherent to the normative encoding in
//! spec: 07 — Diseño del bytecode: an `argc`/`arity` operand is a `u8`, and
//! slot, count, field, and variant operands are `u16`. Nothing is
//! arbitrary and nothing is a policy choice, so each message states the
//! exact limit the program has to fit into.
//!
//! Limits whose operand is a `u32` (the constant pool, the function
//! table, struct/enum tables, and code indices) are not checked: filling
//! one needs upwards of four billion entries, which exhausts memory long
//! before the index does, and no source file can reach it.
//!
//! Reporting a limit does not stop code generation — the narrowed value
//! is clamped and lowering continues, so one run reports every limit the
//! program breaks. The module built alongside those diagnostics is
//! discarded by [`crate::compile`] and never executed.

use brasa_diagnostics::{Diagnostic, Severity};
use brasa_source::Span;

/// Arguments per call, receiver included: the `argc` operand of `call`,
/// `call_value`, `call_builtin`, `call_method_dyn`, and `make_enum`.
pub(crate) const MAX_ARGS: usize = u8::MAX as usize;

/// Parameters per function, method, lambda, and enum variant: a frame's
/// `arity`.
pub(crate) const MAX_PARAMS: usize = u8::MAX as usize;

/// Elements per vector/tuple literal and pairs per map literal: the
/// count operand of `make_vector`, `make_map`, and `make_tuple`.
pub(crate) const MAX_ELEMENTS: usize = u16::MAX as usize;

/// Fields per struct and variants per enum: the operands of
/// `get_field`/`set_field`/`tuple_field`/`enum_field` and
/// `jump_if_variant_ne`.
pub(crate) const MAX_MEMBERS: usize = u16::MAX as usize;

/// Local slots per frame (`SlotIx`), module globals (`GlobalIx`), and
/// values captured by one closure (`make_closure`'s `captures`).
pub(crate) const MAX_BINDINGS: usize = u16::MAX as usize;

/// Operand-stack slots one frame may need above its locals: a
/// function's `max_stack`.
pub(crate) const MAX_OPERAND_STACK: u32 = u16::MAX as u32;

pub(crate) fn error(code: &str, message: String, label: String, span: Span) -> Diagnostic {
    Diagnostic::new(Severity::Error, message, code.to_string(), span).with_label(span, label)
}
