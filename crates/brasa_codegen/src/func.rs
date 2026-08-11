//! Per-function compilation state: the chunk under construction, the
//! `LocalId` → frame-slot map, the loop stack for `break`/`continue`
//! patching, and the emit helpers shared by every lowering module.

use std::collections::HashMap;

use brasa_bytecode::{Chunk, CodeIx, Constant, Function, Handler, Op, SlotIx, builtin_id};
use brasa_source::Span;

use crate::context::Cx;
use crate::depth;

/// Placeholder target for jumps patched after their target is known. A
/// forgotten patch shows up as an obviously-wrong `4294967295` in the
/// disassembly instead of a silently-valid index 0.
pub(crate) const PLACEHOLDER: CodeIx = CodeIx(u32::MAX);

/// What kind of function body is being compiled; drives `return`
/// lowering and the implicit result.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum FnKind {
    /// The synthetic `<toplevel>`; `return` cannot occur here.
    Toplevel,
    /// A declared function or method. `returns_value` is whether it
    /// declares a return type: without one it always returns `unit`.
    Func { returns_value: bool },
    /// A lambda: always returns its body value.
    Lambda,
}

/// One enclosing loop, for `break`/`continue` lowering.
pub(crate) struct LoopCx {
    /// `continue` target: the `while` condition or the `for` loop's
    /// `iter_next` instruction.
    pub(crate) head: CodeIx,
    /// `break` jumps to patch to the loop exit.
    pub(crate) break_jumps: Vec<CodeIx>,
    /// Whether `break` must pop the loop iterator first (`for` keeps it
    /// on the operand stack while the body runs).
    pub(crate) pops_iterator_on_break: bool,
}

pub(crate) struct FuncCx<'a, 'c> {
    pub(crate) cx: &'c mut Cx<'a>,
    pub(crate) kind: FnKind,
    pub(crate) chunk: Chunk,
    /// Handler entries in innermost-first order (inner `catch` subjects
    /// finish compiling before their enclosing subject registers).
    /// `depth` values are placeholders until [`FuncCx::finish`] runs
    /// the operand-depth pass.
    pub(crate) handlers: Vec<Handler>,
    slots: HashMap<brasa_resolver::LocalId, SlotIx>,
    next_slot: u16,
    /// Where `self` lives: slot 0 in methods, a capture slot in lambdas
    /// that capture it, absent elsewhere.
    pub(crate) self_slot: Option<SlotIx>,
    pub(crate) loops: Vec<LoopCx>,
}

impl<'a, 'c> FuncCx<'a, 'c> {
    pub(crate) fn new(cx: &'c mut Cx<'a>, kind: FnKind) -> FuncCx<'a, 'c> {
        FuncCx {
            cx,
            kind,
            chunk: Chunk::new(),
            handlers: Vec::new(),
            slots: HashMap::new(),
            next_slot: 0,
            self_slot: None,
            loops: Vec::new(),
        }
    }

    pub(crate) fn emit(&mut self, op: Op, span: Span) -> CodeIx {
        self.chunk.push(op, span)
    }

    pub(crate) fn patch(&mut self, at: CodeIx, target: CodeIx) {
        self.chunk.patch_jump(at, target);
    }

    /// The index the next emitted instruction will get: the target for
    /// jumps landing "here".
    pub(crate) fn here(&self) -> CodeIx {
        CodeIx(u32::try_from(self.chunk.len()).expect("chunk overflow"))
    }

    /// Pre-assigns a slot (parameters and capture slots, whose
    /// positions are fixed by the frame layout).
    pub(crate) fn assign_slot(&mut self, local: brasa_resolver::LocalId, slot: SlotIx) {
        self.slots.insert(local, slot);
        self.next_slot = self.next_slot.max(slot.0 + 1);
    }

    /// The frame slot of a local, allocated on first encounter.
    /// Distinct `LocalId`s always get distinct slots.
    pub(crate) fn slot_of(&mut self, local: brasa_resolver::LocalId) -> SlotIx {
        if let Some(&slot) = self.slots.get(&local) {
            return slot;
        }

        let slot = self.alloc_slot();
        self.slots.insert(local, slot);
        slot
    }

    /// Ensures the next allocated slot is at least `floor` (a `self`
    /// parameter occupies a slot but has no `LocalId` to record).
    pub(crate) fn reserve_slot_floor(&mut self, floor: u16) {
        self.next_slot = self.next_slot.max(floor);
    }

    /// A fresh anonymous slot (struct-literal reordering scratch).
    pub(crate) fn alloc_slot(&mut self) -> SlotIx {
        let slot = SlotIx(self.next_slot);
        self.next_slot = self.next_slot.checked_add(1).expect("frame slot overflow");
        slot
    }

    pub(crate) fn emit_const(&mut self, constant: Constant, span: Span) -> CodeIx {
        let id = self.cx.pool.insert(constant);
        self.emit(Op::Const(id), span)
    }

    /// Compiles a statically-known walker fatal: pushes the message and
    /// calls the internal `<fatal>` builtin, which raises an
    /// uncatchable fatal error at runtime. Net stack effect: one value
    /// pushed (never observed — the call raises).
    pub(crate) fn emit_fatal(&mut self, message: &str, span: Span) {
        self.emit_raise("<fatal>", message, span);
    }

    /// Compiles a `panics.AssertionFailed` raise with the given detail
    /// (match fall-through, `for` pattern mismatch). Net stack effect:
    /// one value pushed.
    pub(crate) fn emit_assert_failed(&mut self, detail: &str, span: Span) {
        self.emit_raise("<assert-failed>", detail, span);
    }

    fn emit_raise(&mut self, builtin: &str, message: &str, span: Span) {
        let builtin = builtin_id(builtin).expect("internal builtins are registered");
        self.emit_const(Constant::Str(message.to_string()), span);
        self.emit(Op::CallBuiltin { builtin, argc: 1 }, span);
    }

    /// Pushes the current `self`, or the walker's exact fatal when none
    /// is in scope.
    pub(crate) fn load_self(&mut self, span: Span) {
        match self.self_slot {
            Some(slot) => {
                self.emit(Op::LoadLocal(slot), span);
            }
            None => self.emit_fatal("brasa: `self` outside a method", span),
        }
    }

    /// Runs the operand-depth pass (fixing handler depths and computing
    /// `max_stack`), attaches the handlers, and builds the final
    /// [`Function`].
    pub(crate) fn finish(mut self, name: String, arity: u8, captures: u16) -> Function {
        let max_stack = depth::finalize(self.chunk.ops(), &mut self.handlers, &self.cx.structs);

        for handler in self.handlers.drain(..) {
            self.chunk.push_handler(handler);
        }

        Function {
            name,
            arity,
            captures,
            locals: self.next_slot,
            max_stack,
            chunk: self.chunk,
        }
    }
}
