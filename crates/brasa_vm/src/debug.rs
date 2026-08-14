//! The debug substrate (BRS-117): breakpoints, stepping, and reading a
//! paused frame.
//!
//! This is the API the debugger front-ends call — `brasa debug`, a DAP
//! adapter, a heap view. It has no protocol, no rendering and no
//! timing, on purpose: everything above it is a shell over this.
//!
//! # A breakpoint costs nothing when none is set
//!
//! Because the dispatch loop a normal run uses never learns about any
//! of this. [`crate::vm::Vm::execute_debug`] is a second loop, entered
//! only through a [`Session`], and the hot one is untouched — the
//! guarantee is structural rather than argued, so there is nothing to
//! re-measure.
//!
//! # Pausing does not disturb the program
//!
//! Stopping raises `Signal::Breakpoint`, which handler unwinding never
//! matches (it tests for `Error`/`Panic` only). So the signal
//! propagates out of the loop with the frame stack and the operand
//! stack exactly as the program left them, and resuming is just
//! re-entering the loop.

use std::collections::HashSet;

use brasa_bytecode::{CodeIx, FuncId, Module};
use brasa_source::{FileId, Span};

use crate::value::Value;

/// What a paused session is waiting to do next.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum StepMode {
    /// Run until a breakpoint.
    #[default]
    Run,
    /// Stop at the next instruction, wherever it is — into calls.
    In,
    /// Stop at the next instruction at this depth or shallower, so a
    /// call runs to completion.
    Over(usize),
    /// Stop once the frame that was current has returned.
    Out(usize),
}

/// The session's state, parked on the VM while it is attached.
pub(crate) struct DebugState {
    breakpoints: HashSet<(FuncId, usize)>,
    mode: StepMode,
    /// Set immediately after a stop so the very next check does not fire
    /// again at the instruction we are resuming ON. Without it every
    /// resume would stop where it just stopped, forever.
    resuming: bool,
}

impl DebugState {
    fn new() -> DebugState {
        DebugState {
            breakpoints: HashSet::new(),
            mode: StepMode::Run,
            resuming: false,
        }
    }

    /// Consumes the one-shot resume grace. Returns whether this check
    /// should be skipped.
    pub(crate) fn take_resuming(&mut self) -> bool {
        std::mem::take(&mut self.resuming)
    }

    /// Whether to stop before `(func, ip)` at frame depth `depth`.
    ///
    /// A breakpoint wins over the step mode: a user who set one wants
    /// it, and reaching it while stepping over is still reaching it.
    pub(crate) fn should_stop(&self, func: FuncId, ip: usize, depth: usize) -> bool {
        if self.breakpoints.contains(&(func, ip)) {
            return true;
        }

        match self.mode {
            StepMode::Run => false,
            StepMode::In => true,
            // A call pushed a frame, so anything deeper is inside the
            // call this step is meant to pass over.
            StepMode::Over(start) => depth <= start,
            StepMode::Out(start) => depth < start,
        }
    }
}

/// Where a run stopped.
#[derive(Debug, Clone, PartialEq)]
pub enum Stop {
    /// Paused before executing the instruction at this position.
    Paused { func: FuncId, ip: usize, span: Span },
    /// The program ran to completion; nothing is paused any more.
    Finished(crate::Outcome),
}

/// One frame of a paused call stack, innermost last.
#[derive(Debug, Clone)]
pub struct FrameView {
    pub func: FuncId,
    /// The function's name as the disassembler and stacktraces spell it.
    pub name: String,
    /// The instruction about to run, and where it came from in source.
    pub ip: usize,
    pub span: Span,
    /// Every slot of the frame, in slot order. A slot the function has
    /// not written yet reads as `None` rather than as a stale value.
    pub locals: Vec<Option<ValueView>>,
}

/// A value, rendered one level deep (BRS-117).
///
/// One level because a debugger must not force an object graph to
/// answer "what is this": a cyclic structure would not terminate, and a
/// large one would cost more than the question is worth. Children are
/// summarised, and asking about one of them is another call.
#[derive(Debug, Clone, PartialEq)]
pub struct ValueView {
    /// A one-line rendering of the value itself.
    pub summary: String,
    /// The arena cell behind this value, when it has one.
    ///
    /// Present so [`Session::retention`] has something to ask about:
    /// without it that method would be unreachable from this API, and
    /// a method nobody can call is not a feature.
    pub cell: Option<crate::GcRef>,
    /// Named children, one level down: a struct's fields, a vector's
    /// elements, a map's entries. Empty for scalars.
    pub children: Vec<(String, String)>,
}

/// The heap at a pause (BRS-120).
///
/// The one view an editor's debug panels have no vocabulary for. DAP
/// describes a paused frame; nothing in it can say how many vectors
/// are alive, how much the arena is holding, or whether collection is
/// keeping up with allocation.
pub struct HeapView {
    /// Live arena slots by kind.
    pub by_kind: Vec<(String, usize)>,
    pub live_slots: usize,
    /// Holes the sweeper left, reusable by the next allocation.
    /// Reported apart from live slots because an arena that is mostly
    /// holes and one that is mostly live say different things about
    /// whether collection is keeping up.
    pub free_slots: usize,
    pub live_bytes: usize,
    pub peak_bytes: usize,
    pub allocations: u64,
    pub collections: u64,
}

impl HeapView {
    pub fn report(&self) -> String {
        let mut out = format!(
            "{} live slots, {} free — {} bytes live, {} peak\n{} allocations over {} collections\n",
            self.live_slots,
            self.free_slots,
            self.live_bytes,
            self.peak_bytes,
            self.allocations,
            self.collections,
        );

        if self.by_kind.is_empty() {
            out.push_str("\nthe arena is empty");
            return out;
        }

        out.push_str("\n  count  kind\n");
        for (kind, count) in &self.by_kind {
            out.push_str(&format!("  {count:>5}  {kind}\n"));
        }

        out.trim_end().to_string()
    }
}

/// A debug session driving one VM.
///
/// The VM is owned here and stays alive across stops, which is what
/// makes a stateful debugger possible: pausing returns control without
/// unwinding anything, so `frames()` reads the real stack rather than a
/// snapshot taken on the way out.
pub struct Session<'a> {
    vm: crate::vm::Vm<'a>,
    module: &'a Module,
    started: bool,
}

impl<'a> Session<'a> {
    /// Attaches to `module`, ready to run but not yet started.
    pub fn new(module: &'a Module, streams: brasa_runtime::Streams<'a>, args: &[String]) -> Self {
        let mut vm = crate::vm::Vm::new(
            module,
            streams,
            crate::DEFAULT_MAX_CALL_DEPTH,
            crate::DEFAULT_GC_BUDGET_BYTES,
            args,
        );
        vm.debug = Some(DebugState::new());

        Session {
            vm,
            module,
            started: false,
        }
    }

    /// Every instruction whose span covers `offset` in `file`, as
    /// `(function, instruction)` pairs.
    ///
    /// A source position maps to more than one instruction — an
    /// expression compiles to several — so a breakpoint resolves to the
    /// FIRST of them in code order, which is where execution reaches
    /// that source first.
    pub fn resolve(&self, file: FileId, offset: u32) -> Option<(FuncId, usize)> {
        let mut best: Option<(FuncId, usize, u32)> = None;

        for (ix, function) in self.module.functions.iter().enumerate() {
            let func = FuncId(ix as u32);

            for ip in 0..function.chunk.len() {
                let span = function.chunk.span_at(CodeIx(ip as u32));
                if span.file != file || span.start.0 > offset || offset > span.end.0 {
                    continue;
                }

                // The tightest span wins, and among equals the earliest
                // instruction: a statement's own instructions are
                // inside the function's, and stopping at the outermost
                // would stop at the wrong line.
                let width = span.end.0.saturating_sub(span.start.0);
                if best.is_none_or(|(_, _, w)| width < w) {
                    best = Some((func, ip, width));
                }
            }
        }

        best.map(|(func, ip, _)| (func, ip))
    }

    /// The first instruction anywhere in `start..end`.
    ///
    /// This is what a `file:line` breakpoint actually needs: a line
    /// normally begins with indentation, which no instruction's span
    /// covers, so resolving its first byte alone finds nothing. Both
    /// front-ends want the same answer, so the scan lives here — a
    /// second copy in each of them would drift, and the failure is a
    /// breakpoint that silently never binds.
    pub fn resolve_range(&self, file: FileId, start: u32, end: u32) -> Option<(FuncId, usize)> {
        (start..end.max(start + 1)).find_map(|offset| self.resolve(file, offset))
    }

    /// Sets a breakpoint at a resolved position. Returns whether it was
    /// new.
    pub fn set_breakpoint(&mut self, func: FuncId, ip: usize) -> bool {
        self.state().breakpoints.insert((func, ip))
    }

    pub fn clear_breakpoint(&mut self, func: FuncId, ip: usize) -> bool {
        self.state().breakpoints.remove(&(func, ip))
    }

    pub fn breakpoints(&self) -> Vec<(FuncId, usize)> {
        let mut all: Vec<_> = self
            .vm
            .debug
            .as_ref()
            .expect("a session always has debug state")
            .breakpoints
            .iter()
            .copied()
            .collect();
        all.sort_by_key(|(func, ip)| (func.0, *ip));
        all
    }

    /// Runs until a breakpoint or the end of the program.
    pub fn resume(&mut self) -> Stop {
        self.run_with(StepMode::Run)
    }

    /// One instruction, following calls.
    pub fn step_in(&mut self) -> Stop {
        self.run_with(StepMode::In)
    }

    /// One instruction, letting any call it makes run to completion.
    pub fn step_over(&mut self) -> Stop {
        let depth = self.vm.frame_depth();
        self.run_with(StepMode::Over(depth))
    }

    /// Until the current function returns.
    pub fn step_out(&mut self) -> Stop {
        let depth = self.vm.frame_depth();
        self.run_with(StepMode::Out(depth))
    }

    fn run_with(&mut self, mode: StepMode) -> Stop {
        self.state().mode = mode;
        self.state().resuming = self.started;

        let result = if self.started {
            self.vm.resume_debug()
        } else {
            self.started = true;
            self.vm.start_debug()
        };

        match result {
            Err(crate::vm::Signal::Breakpoint) => {
                let (func, ip) = self.vm.current_position().expect("a pause has a frame");
                Stop::Paused {
                    func,
                    ip,
                    span: self.span_of(func, ip),
                }
            }
            other => Stop::Finished(self.vm.finish_debug(other)),
        }
    }

    /// The paused call stack, outermost first.
    pub fn frames(&self) -> Vec<FrameView> {
        self.vm
            .frame_views()
            .into_iter()
            .map(|(func, ip, locals)| FrameView {
                func,
                name: self.module.functions[func.0 as usize].name.clone(),
                ip,
                span: self.span_of(func, ip),
                locals: locals
                    .into_iter()
                    .map(|slot| slot.map(|value| self.view(&value)))
                    .collect(),
            })
            .collect()
    }

    /// The heap as it stands at this pause.
    pub fn heap(&self) -> HeapView {
        self.vm.heap_view()
    }

    /// The shortest chain of arena cells from a root to `target`, or
    /// `None` when nothing keeps it alive.
    pub fn retention(&self, target: crate::GcRef) -> Option<Vec<crate::GcRef>> {
        self.vm.retention_of(target)
    }

    /// One value, one level deep.
    pub fn view(&self, value: &Value) -> ValueView {
        self.vm.value_view(value)
    }

    fn span_of(&self, func: FuncId, ip: usize) -> Span {
        let chunk = &self.module.functions[func.0 as usize].chunk;
        // A frame parked at the end of its code (mid-return) has no
        // instruction to point at; the last one is where it is.
        let ix = ip.min(chunk.len().saturating_sub(1));
        chunk.span_at(CodeIx(ix as u32))
    }

    fn state(&mut self) -> &mut DebugState {
        self.vm
            .debug
            .as_mut()
            .expect("a session always has debug state")
    }
}
