//! A function's code: instructions, span side table, handler table.
//!
//! The span table is the debug information (`docs/spec/07-bytecode.md`,
//! chunk format): one `Span` per instruction, so runtime error
//! locations and panic stacktrace lines resolve without a separate line
//! map. Handler entries implement `catch` as static tables — zero
//! happy-path cost, nesting by innermost-first order.

use brasa_source::Span;

use crate::{CodeIx, Op};

/// One `catch` handler entry. `start..end` (half-open) covers the
/// compiled subject expression only — a throw inside an arm or guard
/// belongs to the enclosing handler.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Handler {
    pub start: CodeIx,
    pub end: CodeIx,
    /// Entry point of the dispatch sequence; the unwinder jumps here
    /// after truncating the operand stack and pushing the caught signal.
    pub target: CodeIx,
    /// Operand depth (relative to the frame's locals boundary) to
    /// restore before entering the dispatch sequence.
    pub depth: u16,
}

impl Handler {
    pub fn covers(&self, ix: CodeIx) -> bool {
        self.start <= ix && ix < self.end
    }
}

/// The code container behind every [`crate::Function`].
#[derive(Debug, Default)]
pub struct Chunk {
    code: Vec<Op>,
    spans: Vec<Span>,
    handlers: Vec<Handler>,
}

impl Chunk {
    pub fn new() -> Chunk {
        Chunk::default()
    }

    /// Appends an instruction with its source span; returns its index
    /// (the value jump targets and handler ranges refer to).
    pub fn push(&mut self, op: Op, span: Span) -> CodeIx {
        let ix = CodeIx(u32::try_from(self.code.len()).expect("chunk overflow"));
        self.code.push(op);
        self.spans.push(span);
        ix
    }

    /// Retargets the jump instruction at `at`. Panics if `at` does not
    /// hold a jump: that is a code-generator bug, not a runtime state.
    pub fn patch_jump(&mut self, at: CodeIx, target: CodeIx) {
        let op = &mut self.code[at.0 as usize];

        match op {
            Op::Jump(t)
            | Op::JumpIfFalse(t)
            | Op::JumpIfFalseOrPop(t)
            | Op::JumpIfTrueOrPop(t)
            | Op::JumpIfVariantNe { target: t, .. }
            | Op::JumpIfNone(t)
            | Op::IterNext(t)
            | Op::JumpIfPanic(t)
            | Op::JumpIfTagNe { target: t, .. } => *t = target,
            other => panic!("patch_jump on non-jump instruction {other:?}"),
        }
    }

    /// Registers a handler entry. Callers append innermost first: the
    /// unwinder takes the first covering entry.
    pub fn push_handler(&mut self, handler: Handler) {
        self.handlers.push(handler);
    }

    pub fn ops(&self) -> &[Op] {
        &self.code
    }

    pub fn span_at(&self, ix: CodeIx) -> Span {
        self.spans[ix.0 as usize]
    }

    pub fn handlers(&self) -> &[Handler] {
        &self.handlers
    }

    /// The innermost handler covering `ix`, if any (first match in
    /// innermost-first order).
    pub fn handler_for(&self, ix: CodeIx) -> Option<&Handler> {
        self.handlers.iter().find(|h| h.covers(ix))
    }

    pub fn len(&self) -> usize {
        self.code.len()
    }

    pub fn is_empty(&self) -> bool {
        self.code.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use brasa_source::{BytePosition, Span};

    use super::*;
    use crate::{ConstId, SlotIx};

    fn span(start: u32, end: u32) -> Span {
        Span {
            start: BytePosition(start),
            end: BytePosition(end),
            ..Span::default()
        }
    }

    #[test]
    fn push_round_trip() {
        let mut chunk = Chunk::new();

        let a = chunk.push(Op::Const(ConstId(0)), span(0, 2));
        let b = chunk.push(Op::StoreLocal(SlotIx(1)), span(3, 8));

        assert_eq!(a, CodeIx(0));
        assert_eq!(b, CodeIx(1));
        assert_eq!(
            chunk.ops(),
            &[Op::Const(ConstId(0)), Op::StoreLocal(SlotIx(1))]
        );
        assert_eq!(chunk.span_at(a), span(0, 2));
        assert_eq!(chunk.span_at(b), span(3, 8));
    }

    #[test]
    fn patch_jump_round_trip() {
        let mut chunk = Chunk::new();

        let jump = chunk.push(Op::JumpIfFalse(CodeIx(0)), span(0, 1));
        chunk.push(Op::LoadUnit, span(1, 2));
        let end = chunk.push(Op::Ret, span(2, 3));

        chunk.patch_jump(jump, end);

        assert_eq!(chunk.ops()[0], Op::JumpIfFalse(CodeIx(2)));
    }

    #[test]
    #[should_panic(expected = "patch_jump on non-jump instruction")]
    fn patch_jump_rejects_non_jumps() {
        let mut chunk = Chunk::new();
        let at = chunk.push(Op::LoadUnit, span(0, 1));
        chunk.patch_jump(at, CodeIx(0));
    }

    #[test]
    fn handler_lookup_is_innermost_first() {
        let mut chunk = Chunk::new();
        for i in 0..10 {
            chunk.push(Op::LoadUnit, span(i, i + 1));
        }

        let inner = Handler {
            start: CodeIx(2),
            end: CodeIx(5),
            target: CodeIx(8),
            depth: 1,
        };
        let outer = Handler {
            start: CodeIx(0),
            end: CodeIx(7),
            target: CodeIx(9),
            depth: 0,
        };
        chunk.push_handler(inner);
        chunk.push_handler(outer);

        assert_eq!(chunk.handler_for(CodeIx(3)), Some(&inner));
        assert_eq!(chunk.handler_for(CodeIx(6)), Some(&outer));
        assert_eq!(
            chunk.handler_for(CodeIx(5)),
            Some(&outer),
            "end is exclusive"
        );
        assert_eq!(chunk.handler_for(CodeIx(8)), None);
    }
}
