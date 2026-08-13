//! The dispatch loop: frames, calls, and handler-table unwinding.
//!
//! Execution model (`docs/spec/07-bytecode.md`): one contiguous value
//! stack shared by all frames; a frame is `{func, ip, base}` plus the
//! return-truncation point (`base` minus the callee slot for
//! `call_value`). The loop is iterative — compiled calls push frames,
//! never Rust frames — and the call-depth guard raises
//! `panics.StackOverflow`.
//!
//! Unwinding on `throw` or a faulting instruction: search the current
//! frame's handler entries for the faulting `ip`; on a match truncate
//! the operand stack to the entry's depth (relative to the locals
//! boundary), push the caught-signal value, and jump to the dispatch
//! target; otherwise pop the frame and retry at the caller's call-site
//! `ip`. Fatal and broken-pipe signals unwind everything
//! unconditionally. Panic stacktraces are snapshotted at raise time —
//! all frames are still active then, so this matches the spec's
//! record-while-popping wording and the walker's behavior.

use std::io::Write;
use std::rc::Rc;

use brasa_bytecode::{
    BuiltinId, CodeIx, ConstId, Constant, EnumShape, FuncId, Function, Module, Op, StructShape,
    builtin_def, builtin_id,
};
use brasa_runtime::Outcome;
use brasa_runtime::table::{OrderedMap, OrderedSet};

use crate::heap::{Heap, Interner};
use crate::value::{
    BoundBuiltin, BoundMethod, Caught, ClosureValue, EnumValue, IterState, PanicValue, Value,
    value_cmp, value_eq,
};

pub(crate) const INDEX_OUT_OF_BOUNDS: &str = "panics.IndexOutOfBounds";
pub(crate) const DIVISION_BY_ZERO: &str = "panics.DivisionByZero";
pub(crate) const INTEGER_OVERFLOW: &str = "panics.IntegerOverflow";
pub(crate) const ASSERTION_FAILED: &str = "panics.AssertionFailed";
pub(crate) const STACK_OVERFLOW: &str = "panics.StackOverflow";

/// Native-root-stack capacity kept between traversals; anything a large
/// traversal grew beyond this is released when the stack empties.
const NATIVE_ROOT_FLOOR: usize = 64;

/// Non-local control flow, from the walker's signal classes.
/// `Return`/`Break`/`Continue` do not exist here: they compile away.
#[derive(Debug)]
pub(crate) enum Signal {
    Error(Value),
    /// Boxed: a panic's name, detail, and raise-time call chain are the
    /// widest payload a signal carries, and every instruction returns a
    /// `Result<_, Signal>` whose size this would otherwise set.
    Panic(Box<PanicValue>),
    Fatal(String),
    BrokenPipe,
    /// `env.exit(code)`. Not an error and deliberately not catchable:
    /// handler unwinding tests for `Error`/`Panic`, so a new variant
    /// passes every `catch` by construction.
    Exit(i32),
}

pub(crate) type VmResult<T = Value> = Result<T, Signal>;

/// What one step of an early-exiting rooted traversal decided
/// ([`Vm::find_rooted`]).
pub(crate) enum Step {
    Continue,
    /// Stop the traversal; this is the builtin's result.
    Stop(Value),
}

/// One call frame. `ip` is pre-advanced: it always points at the next
/// instruction, so the faulting/call-site index is uniformly `ip - 1`.
struct Frame {
    func: FuncId,
    ip: usize,
    /// Value-stack index of slot 0.
    base: usize,
    /// Truncation point on return: `base`, or `base - 1` for
    /// `call_value` (the callee slot is replaced by the result).
    ret_base: usize,
}

pub(crate) struct Vm<'a> {
    module: &'a Module,
    globals: Vec<Option<Value>>,
    stack: Vec<Value>,
    frames: Vec<Frame>,
    /// Shadow root stack for values a native call holds only in Rust
    /// locals (BRS-62). Builtin higher-order functions pop their
    /// operands off the value stack and keep receiver, callback,
    /// snapshot, and accumulators in Rust; rendering does the same with
    /// the fields it extracted. Collection runs inside those calls, so
    /// whatever they hold has to be reachable from here or the
    /// collector would sweep it out from under them.
    native_roots: Vec<Value>,
    pub(crate) heap: Heap,
    interner: Interner,
    /// Constants pre-materialized at load: string constants are
    /// interned once here, so every `const` push shares one allocation.
    consts: Vec<Value>,
    pub(crate) out: &'a mut (dyn Write + Send),
    /// Where `io.eprint` writes (`docs/spec/05-stdlib.md`).
    pub(crate) err: &'a mut (dyn Write + Send),
    /// What `io.readLine`/`io.readAll` consume.
    pub(crate) input: &'a mut (dyn std::io::BufRead + Send),
    max_depth: usize,
    /// Per-run cache of compiled regex patterns for the string regex
    /// methods, keyed by the pattern text.
    pub(crate) regex_cache: std::collections::HashMap<String, Rc<regex::Regex>>,
    /// The script's trailing CLI arguments, served by `env.args()`
    /// (BRS-32, `docs/spec/05-stdlib.md`).
    pub(crate) script_args: Vec<String>,
    /// `env.set` overrides (BRS-32): merged over the process
    /// environment by `env.get`/`env.vars` and passed to every child
    /// spawned through `std::proc`. The host process's own environment
    /// block is never mutated.
    pub(crate) env_overlay: std::collections::HashMap<String, String>,
    /// The per-run PRNG behind `std::rand` (BRS-35): entropy-seeded at
    /// startup, reset deterministically by `rand.seed`
    /// (`brasa_runtime::rand_glue`).
    pub(crate) rng: brasa_runtime::rand_glue::Rng,
}

impl<'a> Vm<'a> {
    pub(crate) fn new(
        module: &'a Module,
        streams: brasa_runtime::Streams<'a>,
        max_depth: usize,
        gc_budget_bytes: usize,
        args: &[String],
    ) -> Vm<'a> {
        let mut interner = Interner::default();
        let consts = module
            .constants
            .iter()
            .map(|(_, constant)| match constant {
                Constant::Int(v) => Value::Int(*v),
                Constant::Float(v) => Value::Float(*v),
                Constant::Str(v) => Value::Str(interner.intern(v)),
                Constant::Char(v) => Value::Char(*v),
            })
            .collect();

        Vm {
            module,
            globals: vec![None; module.globals.len()],
            stack: Vec::new(),
            frames: Vec::new(),
            native_roots: Vec::new(),
            heap: Heap::new(gc_budget_bytes),
            interner,
            consts,
            out: streams.out,
            err: streams.err,
            input: streams.input,
            max_depth,
            regex_cache: std::collections::HashMap::new(),
            script_args: args.to_vec(),
            env_overlay: std::collections::HashMap::new(),
            rng: brasa_runtime::rand_glue::Rng::from_entropy(),
        }
    }

    pub(crate) fn run(&mut self) -> Outcome {
        let result = self.run_program();
        self.finish(result)
    }

    /// Runs `<toplevel>` and then every `test` in source order,
    /// reporting how each one ended.
    ///
    /// Isolation falls out of the existing unwinding: a failing
    /// assertion raises `panics.AssertionFailed`, which unwinds to an
    /// `Outcome` without touching the process, so one failed test is one
    /// failed test. Module state is deliberately NOT reset between
    /// them — the top level ran once, exactly as it does for a program,
    /// and a runner that re-ran it would be testing something the
    /// program never does.
    pub(crate) fn run_tests(&mut self) -> (Outcome, Vec<(String, Outcome)>) {
        let setup = self.run_setup();
        if !matches!(setup, Outcome::Success) {
            return (setup, Vec::new());
        }

        let mut results = Vec::with_capacity(self.module.tests.len());
        for test in &self.module.tests {
            let result = self.call_entry(test.func);
            self.stack.clear();
            self.frames.clear();
            results.push((test.name.clone(), self.finish(result)));
        }

        (Outcome::Success, results)
    }

    /// `<toplevel>` alone, for a run that calls its own entry points
    /// afterwards.
    fn run_setup(&mut self) -> Outcome {
        let result = (|| {
            self.enter_function(FuncId(0), 0, 0)?;
            self.execute(1)?;
            Ok(())
        })();
        self.stack.clear();

        self.finish(result)
    }

    /// Calls one zero-argument function on a cleared stack.
    fn call_entry(&mut self, func: FuncId) -> Result<(), Signal> {
        self.enter_function(func, 0, 0)?;
        self.execute(1)?;
        self.stack.clear();

        Ok(())
    }

    pub(crate) fn run_stats(&self) -> crate::RunStats {
        let heap = self.heap.stats();
        crate::RunStats {
            heap_allocations: heap.allocations,
            gc_collections: heap.collections,
            live_heap_objects: heap.live,
            peak_heap_objects: self.heap.arena_slots(),
            live_heap_bytes: self.heap.live_bytes(),
            peak_heap_bytes: self.heap.peak_bytes(),
            interned_strings: self.interner.len(),
            intern_hits: self.interner.hits(),
        }
    }

    fn run_program(&mut self) -> Result<(), Signal> {
        self.enter_function(FuncId(0), 0, 0)?;
        self.execute(1)?;
        self.stack.clear();

        if let Some(main) = self.module.entry {
            if self.function(main).arity != 0 {
                return Err(Signal::Fatal(
                    "brasa: `main` must take no parameters".to_string(),
                ));
            }
            self.enter_function(main, 0, 0)?;
            self.execute(1)?;
            self.stack.clear();
        }

        Ok(())
    }

    fn finish(&mut self, result: Result<(), Signal>) -> Outcome {
        match result {
            Ok(()) => Outcome::Success,
            Err(Signal::Error(value)) => {
                let tag = self.nominal_tag(&value);
                let rendered = self
                    .display(&value)
                    .unwrap_or_else(|_| "<toString failed>".to_string());
                Outcome::Error {
                    message: format!("error: {tag}: {rendered}"),
                }
            }
            Err(Signal::Panic(panic)) => {
                let mut message = format!("panic: {}: {}", panic.name, panic.detail);
                for frame in &panic.stack {
                    message.push_str("\n  in ");
                    message.push_str(frame);
                }
                Outcome::Panic { message }
            }
            Err(Signal::Fatal(message)) => Outcome::Error { message },
            Err(Signal::BrokenPipe) => Outcome::BrokenPipe,
            Err(Signal::Exit(code)) => Outcome::Exit { code },
        }
    }

    // --- frame plumbing ------------------------------------------------

    pub(crate) fn function(&self, id: FuncId) -> &'a Function {
        &self.module.functions[id.0 as usize]
    }

    pub(crate) fn module_struct(&self, id: brasa_bytecode::StructId) -> &'a StructShape {
        &self.module.structs[id.0 as usize]
    }

    pub(crate) fn module_enum(&self, id: brasa_bytecode::EnumId) -> &'a EnumShape {
        &self.module.enums[id.0 as usize]
    }

    /// Active call depth for the guard, excluding the synthetic
    /// `<toplevel>` bottom frame — the walker never counts it.
    fn call_depth(&self) -> usize {
        let toplevel = usize::from(self.frames.first().is_some_and(|f| f.func == FuncId(0)));
        self.frames.len() - toplevel
    }

    /// Active function names, innermost first, excluding `<toplevel>` —
    /// the walker's panic-stacktrace snapshot.
    fn capture_trace(&self) -> Vec<String> {
        self.frames
            .iter()
            .rev()
            .filter(|frame| frame.func != FuncId(0))
            .map(|frame| self.function(frame.func).name.clone())
            .collect()
    }

    pub(crate) fn panic(&self, name: &'static str, detail: impl Into<String>) -> Signal {
        Signal::Panic(Box::new(PanicValue {
            name,
            detail: detail.into(),
            stack: self.capture_trace(),
        }))
    }

    fn fatal(message: impl Into<String>) -> Signal {
        Signal::Fatal(message.into())
    }

    /// Pushes a frame for `func` whose arguments already sit at
    /// `base..`: reserves `locals + max_stack`, filling the non-argument
    /// local slots with `unit`. The depth guard runs before the push,
    /// so the overflow panic's stacktrace is the caller chain.
    fn enter_function(&mut self, func: FuncId, base: usize, ret_base: usize) -> Result<(), Signal> {
        if self.call_depth() >= self.max_depth {
            return Err(self.panic(
                STACK_OVERFLOW,
                format!("recursion limit ({} frames) exceeded", self.max_depth),
            ));
        }

        let function = self.function(func);
        let floor = base + function.locals as usize;
        self.stack
            .reserve(floor + function.max_stack as usize - self.stack.len());
        self.stack.resize(floor, Value::Unit);

        self.frames.push(Frame {
            func,
            ip: 0,
            base,
            ret_base,
        });
        Ok(())
    }

    /// Copies a closure's captured values into the capture slots
    /// (`base + arity ..`) of the just-entered frame.
    fn write_captures(&mut self, closure: &ClosureValue) {
        let frame = self.frames.last().expect("captures need an active frame");
        let start = frame.base + self.function(closure.func).arity as usize;
        for (offset, value) in closure.captures.iter().enumerate() {
            self.stack[start + offset] = value.clone();
        }
    }

    // --- the dispatch loop ---------------------------------------------

    /// Runs until fewer than `min_frames` frames remain: the bottom
    /// bounded frame returned (its result is on the stack) or unwinding
    /// crossed the boundary (the signal propagates to the caller with
    /// the frames below intact).
    ///
    /// Every instruction boundary is a safepoint, nested loops
    /// included: the root set is the value stack, the global slots, and
    /// the native root stack that bounded callers park their Rust-local
    /// values on (BRS-62). A nested loop that did not collect would let
    /// a long-running builtin HOF hold the whole run's garbage, since
    /// its callback never reaches a top-level boundary.
    ///
    /// The running function's code slice is hoisted into a loop local
    /// and refreshed only when the top frame's function changes, which
    /// takes the module-table indirection (function table, chunk,
    /// instruction vector) out of the per-instruction fetch. `ip` stays
    /// in the frame and is pre-advanced there: unwinding reads it to
    /// locate the faulting instruction, and every instruction that can
    /// raise would otherwise have to write it back anyway.
    fn execute(&mut self, min_frames: usize) -> Result<(), Signal> {
        let mut current: Option<FuncId> = None;
        let mut code: &'a [Op] = &[];

        while self.frames.len() >= min_frames {
            if self.heap.should_collect() {
                self.heap.collect(
                    self.stack
                        .iter()
                        .chain(self.globals.iter().flatten())
                        .chain(self.native_roots.iter()),
                );
            }

            let frame = self.frames.last_mut().expect("loop condition holds");
            let func = frame.func;
            let ip = frame.ip;
            frame.ip = ip + 1;

            if current != Some(func) {
                current = Some(func);
                code = self.function(func).chunk.ops();
            }

            if let Err(signal) = self.step(code[ip]) {
                self.unwind(signal, min_frames)?;
            }
        }
        Ok(())
    }

    /// Handler-table unwinding (`docs/spec/07-bytecode.md`): errors and
    /// panics search each frame's table at the faulting `ip`; fatal and
    /// broken-pipe signals never match. Popping below `min_frames`
    /// propagates the signal to the bounded caller.
    fn unwind(&mut self, signal: Signal, min_frames: usize) -> Result<(), Signal> {
        let catchable = matches!(signal, Signal::Error(_) | Signal::Panic(_));

        loop {
            if self.frames.len() < min_frames {
                return Err(signal);
            }

            let frame = self.frames.last().expect("bounded above min_frames");
            let function = self.function(frame.func);
            let fault = CodeIx((frame.ip - 1) as u32);

            if catchable && let Some(handler) = function.chunk.handler_for(fault) {
                let floor = frame.base + function.locals as usize;
                self.stack.truncate(floor + handler.depth as usize);

                let caught = match signal {
                    Signal::Error(value) => Caught::Error(value),
                    Signal::Panic(panic) => Caught::Panic(panic),
                    _ => unreachable!("only catchable signals reach a handler"),
                };
                self.stack.push(Value::Caught(Rc::new(caught)));

                let target = handler.target.0 as usize;
                self.frames.last_mut().expect("frame still active").ip = target;
                return Ok(());
            }

            let frame = self.frames.pop().expect("bounded above min_frames");
            self.stack.truncate(frame.ret_base);
        }
    }

    // --- native roots (BRS-62) -----------------------------------------

    /// Keeps `values` reachable for the duration of `body`.
    ///
    /// A native call that reenters compiled code — a builtin invoking
    /// its callback, rendering invoking a user `toString` — can trigger
    /// a collection while the values it extracted live only in a Rust
    /// local. Holding the container they came from is not enough: the
    /// reentered code may empty it.
    pub(crate) fn with_rooted<R>(
        &mut self,
        values: &[Value],
        body: impl FnOnce(&mut Self) -> VmResult<R>,
    ) -> VmResult<R> {
        let mark = self.native_roots.len();
        self.native_roots.extend_from_slice(values);

        let result = body(self);
        self.unroot(mark);

        result
    }

    /// Pops back to `mark`, releasing the buffer once nothing is parked.
    ///
    /// Truncation alone keeps the capacity, and a traversal parks the
    /// caller's whole collection here — so without this a single
    /// `bigVector.map(f)` would hold that much memory for the rest of
    /// the run. The retained floor keeps the ordinary small traversals
    /// from reallocating on every call.
    fn unroot(&mut self, mark: usize) {
        self.native_roots.truncate(mark);

        if mark == 0 {
            if self.native_roots.capacity() > NATIVE_ROOT_FLOOR {
                self.native_roots.shrink_to(NATIVE_ROOT_FLOOR);
            }
            // The collector's allowance was measured while this stack
            // held the traversal's snapshot. That marking cost is gone
            // now, so a following phase should not hold garbage against
            // it.
            //
            // Deliberately unpinned: the effect is real but too small
            // to assert on, because the residue self-corrects — the
            // next collection is at most one allowance away and
            // recomputes the floor from the roots that remain by then.
            // Measured on a 20k traversal followed by 3k SURVIVING
            // allocations, with and without this call: identical
            // `peak_heap_objects` (5004, set by the traversal itself),
            // 20 collections against 18, 105 fewer objects retained.
            // With 4k DYING allocations instead, the case this is
            // actually for: 505 collections against 503, peak again
            // identical, 3 objects retained against 4.
            self.heap.relax_mark_floor();
        }
    }

    /// Runs `step` once per element of `snapshot`, keeping `recv`, the
    /// whole snapshot, and everything `step` returns reachable for the
    /// whole traversal, and returns the values `step` kept.
    ///
    /// Taking `recv` is what makes these helpers the mechanical guard
    /// for the rule stated on [`Vm::builtin_with_args`]. Every operand
    /// was popped off the value stack before the builtin ran, so a
    /// temporary receiver is swept by the first nested collection; a
    /// future callback-taking builtin that reads its receiver after a
    /// call would then read a recycled slot, and if the slot were
    /// reused by the same heap kind it would read foreign data instead
    /// of tripping the kind-mismatch assertion. Rooting it here costs
    /// one clone per traversal, not per element, and a callback-taking
    /// builtin cannot forget it: every one of them reenters through one
    /// of these five helpers. Rendering reenters too and does not —
    /// `display` roots what it holds itself.
    ///
    /// The callback is rooted too, but by [`Vm::call_callable`] rather
    /// than here — every caller that hands a closure to compiled code
    /// needs that, not just these traversals.
    ///
    /// The snapshot is moved onto the native root stack rather than
    /// copied onto it: these traversals run over the caller's entire
    /// collection, so a second copy would be a second peak.
    pub(crate) fn collect_rooted(
        &mut self,
        recv: &Value,
        snapshot: Vec<Value>,
        mut step: impl FnMut(&mut Self, Value) -> VmResult<Option<Value>>,
    ) -> VmResult<Vec<Value>> {
        let mark = self.native_roots.len();
        self.native_roots.push(recv.clone());

        let base = self.native_roots.len();
        self.native_roots.extend(snapshot);
        let end = self.native_roots.len();

        for ix in base..end {
            let item = self.native_roots[ix].clone();
            match step(self, item) {
                Ok(Some(kept)) => self.native_roots.push(kept),
                Ok(None) => {}
                Err(signal) => {
                    self.unroot(mark);
                    return Err(signal);
                }
            }
        }

        let kept = self.native_roots.split_off(end);
        self.unroot(mark);

        Ok(kept)
    }

    /// Like [`Vm::collect_rooted`] for a traversal that stops at the
    /// first [`Step::Stop`], whose value it returns; `None` means the
    /// traversal ran to the end.
    pub(crate) fn find_rooted(
        &mut self,
        recv: &Value,
        snapshot: Vec<Value>,
        mut step: impl FnMut(&mut Self, Value) -> VmResult<Step>,
    ) -> VmResult<Option<Value>> {
        let mark = self.native_roots.len();
        self.native_roots.push(recv.clone());

        let base = self.native_roots.len();
        self.native_roots.extend(snapshot);
        let end = self.native_roots.len();

        let mut found = None;
        for ix in base..end {
            let item = self.native_roots[ix].clone();
            match step(self, item) {
                Ok(Step::Continue) => {}
                Ok(Step::Stop(value)) => {
                    found = Some(value);
                    break;
                }
                Err(signal) => {
                    self.unroot(mark);
                    return Err(signal);
                }
            }
        }

        self.unroot(mark);
        Ok(found)
    }

    /// Like [`Vm::collect_rooted`] over a snapshot of pairs, which the
    /// root stack carries flattened. Map entries are pairs in the host,
    /// not in the language, so this keeps them out of the `Value`
    /// domain instead of boxing each one in a tuple just to pass
    /// through the rooting API.
    ///
    /// It accumulates nothing: its one caller discards every result,
    /// and an accumulation region above the pairs would put a value at
    /// the index a mispaired read lands on — turning an odd region from
    /// an immediate out-of-bounds panic into a silent misread.
    pub(crate) fn each_pair_rooted(
        &mut self,
        recv: &Value,
        snapshot: Vec<(Value, Value)>,
        mut step: impl FnMut(&mut Self, Value, Value) -> VmResult<()>,
    ) -> VmResult<()> {
        let mark = self.native_roots.len();
        self.native_roots.push(recv.clone());

        let base = self.native_roots.len();
        self.native_roots
            .extend(snapshot.into_iter().flat_map(|(a, b)| [a, b]));
        let end = self.native_roots.len();

        for ix in (base..end).step_by(2) {
            let (left, right) = (
                self.native_roots[ix].clone(),
                self.native_roots[ix + 1].clone(),
            );
            if let Err(signal) = step(self, left, right) {
                self.unroot(mark);
                return Err(signal);
            }
        }

        self.unroot(mark);
        Ok(())
    }

    /// Pairs every element of `snapshot` with the key `key_of` computes
    /// for it, keeping both halves reachable while the remaining keys
    /// are computed. The pairs never become `Value`s.
    pub(crate) fn key_rooted(
        &mut self,
        recv: &Value,
        snapshot: Vec<Value>,
        mut key_of: impl FnMut(&mut Self, &Value) -> VmResult,
    ) -> VmResult<Vec<(Value, Value)>> {
        let mark = self.native_roots.len();
        self.native_roots.push(recv.clone());

        let base = self.native_roots.len();
        self.native_roots.extend(snapshot);
        let end = self.native_roots.len();

        for ix in base..end {
            let item = self.native_roots[ix].clone();
            match key_of(self, &item) {
                Ok(key) => self.native_roots.push(key),
                Err(signal) => {
                    self.unroot(mark);
                    return Err(signal);
                }
            }
        }

        let keys = self.native_roots.split_off(end);
        let items = self.native_roots.split_off(base);
        self.unroot(mark);

        Ok(keys.into_iter().zip(items).collect())
    }

    /// Threads `init` through `step` once per element of `snapshot`,
    /// keeping `recv`, the snapshot, and the carried accumulator
    /// reachable for the whole traversal (see [`Vm::collect_rooted`]).
    pub(crate) fn fold_rooted(
        &mut self,
        recv: &Value,
        snapshot: Vec<Value>,
        init: Value,
        mut step: impl FnMut(&mut Self, Value, Value) -> VmResult,
    ) -> VmResult {
        let mark = self.native_roots.len();
        self.native_roots.push(init);
        self.native_roots.push(recv.clone());

        let base = self.native_roots.len();
        self.native_roots.extend(snapshot);
        let end = self.native_roots.len();

        for ix in base..end {
            let item = self.native_roots[ix].clone();
            let carried = self.native_roots[mark].clone();
            match step(self, carried, item) {
                Ok(next) => self.native_roots[mark] = next,
                Err(signal) => {
                    self.unroot(mark);
                    return Err(signal);
                }
            }
        }

        let folded = self.native_roots[mark].clone();
        self.unroot(mark);

        Ok(folded)
    }

    // --- stack helpers -------------------------------------------------
    //
    // These are the per-instruction primitives, so they are inlined
    // unconditionally: left to its own judgement the optimizer outlines
    // them, and a push or a pop then costs a call and a spill.

    #[inline(always)]
    fn push(&mut self, value: Value) {
        self.stack.push(value);
    }

    #[inline(always)]
    fn pop(&mut self) -> Value {
        self.stack.pop().expect("operand stack underflow")
    }

    fn pop_n(&mut self, n: usize) -> Vec<Value> {
        self.stack.split_off(self.stack.len() - n)
    }

    #[inline(always)]
    fn peek(&self) -> &Value {
        self.stack.last().expect("operand stack underflow")
    }

    #[inline(always)]
    fn pop_int(&mut self) -> VmResult<i64> {
        match self.pop() {
            Value::Int(v) => Ok(v),
            _ => Err(Self::not_an_int()),
        }
    }

    /// Outlined so that the operand check `pop_int` compiles to is a
    /// predicted branch over a call, not an inline `String` build.
    #[cold]
    #[inline(never)]
    fn not_an_int() -> Signal {
        Self::fatal("brasa: expected an int")
    }

    #[inline(always)]
    fn pop_bool(&mut self) -> VmResult<bool> {
        match self.pop() {
            Value::Bool(v) => Ok(v),
            _ => Err(Self::not_a_bool()),
        }
    }

    /// Outlined for the same reason as [`Vm::not_an_int`].
    #[cold]
    #[inline(never)]
    fn not_a_bool() -> Signal {
        Self::fatal("brasa: condition is not a bool")
    }

    #[inline(always)]
    fn jump(&mut self, target: CodeIx) {
        self.frames.last_mut().expect("active frame").ip = target.0 as usize;
    }

    #[inline(always)]
    fn frame_base(&self) -> usize {
        self.frames.last().expect("active frame").base
    }

    // --- one instruction -----------------------------------------------

    /// Inlined into [`Vm::execute`] on purpose: as a separate function
    /// every instruction paid a call, the prologue and epilogue of a
    /// frame sized by the cold arms' string formatting, and a
    /// multi-word `Result` return, none of which the hot arms need.
    #[inline(always)]
    fn step(&mut self, op: Op) -> Result<(), Signal> {
        match op {
            Op::Const(id) => {
                let value = self.consts[id.0 as usize].clone();
                self.push(value);
            }
            Op::LoadUnit => self.push(Value::Unit),
            Op::LoadTrue => self.push(Value::Bool(true)),
            Op::LoadFalse => self.push(Value::Bool(false)),
            Op::LoadNone => self.push(Value::NONE),
            Op::Pop => {
                self.pop();
            }
            Op::Dup => {
                let top = self.peek().clone();
                self.push(top);
            }
            Op::LoadLocal(slot) => {
                let value = self.stack[self.frame_base() + slot.0 as usize].clone();
                self.push(value);
            }
            Op::StoreLocal(slot) => {
                let value = self.pop();
                let base = self.frame_base();
                self.stack[base + slot.0 as usize] = value;
            }
            Op::LoadGlobal(ix) => match &self.globals[ix.0 as usize] {
                Some(value) => {
                    let value = value.clone();
                    self.push(value);
                }
                None => {
                    let name = &self.module.globals[ix.0 as usize];
                    return Err(Self::fatal(format!(
                        "brasa: `{name}` used before initialization"
                    )));
                }
            },
            Op::StoreGlobal(ix) => {
                let value = self.pop();
                self.globals[ix.0 as usize] = Some(value);
            }
            Op::LoadFunc(func) => self.push(Value::Func(func)),

            Op::AddInt => self.int_arith("+", i64::checked_add)?,
            Op::SubInt => self.int_arith("-", i64::checked_sub)?,
            Op::MulInt => self.int_arith("*", i64::checked_mul)?,
            Op::DivInt => {
                let b = self.pop_int()?;
                let a = self.pop_int()?;
                if b == 0 {
                    return Err(self.panic(DIVISION_BY_ZERO, "division by zero"));
                }
                let result = a.checked_div(b).ok_or_else(|| self.overflow("/"))?;
                self.push(Value::Int(result));
            }
            Op::RemInt => {
                let b = self.pop_int()?;
                let a = self.pop_int()?;
                if b == 0 {
                    return Err(self.panic(DIVISION_BY_ZERO, "remainder by zero"));
                }
                let result = a.checked_rem(b).ok_or_else(|| self.overflow("%"))?;
                self.push(Value::Int(result));
            }
            Op::PowInt => {
                let b = self.pop_int()?;
                let a = self.pop_int()?;
                if b < 0 {
                    return Err(self.panic(ASSERTION_FAILED, "negative exponent in integer `**`"));
                }
                let exp = u32::try_from(b).map_err(|_| self.overflow("**"))?;
                let result = a.checked_pow(exp).ok_or_else(|| self.overflow("**"))?;
                self.push(Value::Int(result));
            }
            Op::NegInt => {
                let a = self.pop_int()?;
                let result = a
                    .checked_neg()
                    .ok_or_else(|| self.panic(INTEGER_OVERFLOW, "integer overflow in unary `-`"))?;
                self.push(Value::Int(result));
            }

            Op::AddFloat => self.float_arith(|a, b| a + b)?,
            Op::SubFloat => self.float_arith(|a, b| a - b)?,
            Op::MulFloat => self.float_arith(|a, b| a * b)?,
            Op::DivFloat => self.float_arith(|a, b| a / b)?,
            Op::RemFloat => self.float_arith(|a, b| a % b)?,
            Op::PowFloat => self.float_arith(f64::powf)?,
            Op::NegFloat => match self.pop() {
                Value::Float(v) => self.push(Value::Float(-v)),
                _ => return Err(Self::fatal("brasa: invalid operand for unary operator")),
            },

            Op::Concat => {
                let b = self.pop();
                let a = self.pop();
                match (a, b) {
                    (Value::Str(a), Value::Str(b)) => self.push(Value::str(format!("{a}{b}"))),
                    _ => {
                        return Err(Self::fatal(
                            "brasa: invalid operands for arithmetic operator",
                        ));
                    }
                }
            }
            Op::Not => {
                let a = self
                    .pop_bool()
                    .map_err(|_| Self::fatal("brasa: invalid operand for unary operator"))?;
                self.push(Value::Bool(!a));
            }

            Op::Eq => {
                let b = self.pop();
                let a = self.pop();
                self.push(Value::Bool(value_eq(&self.heap, &a, &b)));
            }
            Op::Lt => self.ordering(|o| o.is_lt())?,
            Op::Le => self.ordering(|o| o.is_le())?,
            Op::Gt => self.ordering(|o| o.is_gt())?,
            Op::Ge => self.ordering(|o| o.is_ge())?,

            Op::Jump(target) => self.jump(target),
            Op::JumpIfFalse(target) => {
                if !self.pop_bool()? {
                    self.jump(target);
                }
            }
            Op::JumpIfFalseOrPop(target) => match self.peek() {
                Value::Bool(false) => self.jump(target),
                Value::Bool(true) => {
                    self.pop();
                }
                _ => return Err(Self::fatal("brasa: condition is not a bool")),
            },
            Op::JumpIfTrueOrPop(target) => match self.peek() {
                Value::Bool(true) => self.jump(target),
                Value::Bool(false) => {
                    self.pop();
                }
                _ => return Err(Self::fatal("brasa: condition is not a bool")),
            },
            Op::JumpIfVariantNe { variant, target } => {
                let Value::Enum(e) = self.peek() else {
                    unreachable!("jump_if_variant_ne peeks an enum value");
                };
                if e.variant != variant as usize {
                    self.jump(target);
                }
            }
            Op::JumpIfNone(target) => {
                let Value::Option(inner) = self.peek() else {
                    unreachable!("jump_if_none peeks an Option");
                };
                if inner.is_none() {
                    self.jump(target);
                }
            }

            Op::WrapSome => {
                let value = self.pop();
                self.push(Value::some(value));
            }
            Op::WrapSomeDynamic => {
                let value = self.pop();
                match value {
                    Value::Option(_) => self.push(value),
                    other => self.push(Value::some(other)),
                }
            }
            Op::UnwrapSome => {
                let Value::Option(Some(inner)) = self.pop() else {
                    unreachable!("unwrap_some is always guarded by jump_if_none");
                };
                self.push((*inner).clone());
            }
            Op::TupleField(ix) => {
                let Value::Tuple(items) = self.pop() else {
                    unreachable!("tuple_field reads a tuple");
                };
                self.push(items[ix as usize].clone());
            }
            Op::EnumField(ix) => {
                let Value::Enum(e) = self.pop() else {
                    unreachable!("enum_field reads an enum payload");
                };
                self.push(e.fields[ix as usize].clone());
            }
            Op::GetField(ix) => {
                let Value::Struct(s) = self.pop() else {
                    unreachable!("get_field reads a struct");
                };
                let value = self.heap.struct_value(s).fields.borrow()[ix as usize].clone();
                self.push(value);
            }
            Op::SetField(ix) => {
                let value = self.pop();
                let Value::Struct(s) = self.pop() else {
                    unreachable!("set_field writes a struct");
                };
                self.heap.struct_value(s).fields.borrow_mut()[ix as usize] = value;
            }
            Op::GetIndex => {
                let index = self.pop();
                let recv = self.pop();
                let value = self.get_index(&recv, &index)?;
                self.push(value);
            }
            Op::SetIndex => {
                let value = self.pop();
                let index = self.pop();
                let recv = self.pop();
                self.set_index(&recv, index, value)?;
            }

            Op::Call { func, argc } => {
                let base = self.stack.len() - argc as usize;
                self.enter_function(func, base, base)?;
            }
            Op::CallValue { argc } => self.call_value_op(argc as usize)?,
            Op::CallBuiltin { builtin, argc } => {
                let result = self.dispatch_builtin(builtin, argc as usize)?;
                self.push(result);
            }
            Op::CallMethodDyn { name, argc } => {
                let name = self.const_str(name);
                self.call_method_dyn(name, argc as usize)?;
            }
            Op::BindMethodDyn(name) => {
                let name = self.const_str(name);
                let recv = self.pop();
                let bound = self.bind_member_by_name(recv, name)?;
                self.push(bound);
            }
            Op::BindMethod(func) => {
                let recv = self.pop();
                self.push(Value::BoundMethod(Rc::new(BoundMethod { recv, func })));
            }
            Op::BindBuiltin(builtin) => {
                let recv = self.pop();
                self.push(Value::BoundBuiltin(Rc::new(BoundBuiltin { recv, builtin })));
            }
            Op::Ret => {
                let result = self.pop();
                let frame = self.frames.pop().expect("ret needs an active frame");
                self.stack.truncate(frame.ret_base);
                self.push(result);
            }

            Op::MakeVector(n) => {
                let items = self.pop_n(n as usize);
                let vector = self.heap.alloc_vector(items);
                self.push(vector);
            }
            Op::MakeMap(n) => {
                let flat = self.pop_n(2 * n as usize);
                let mut entries: OrderedMap<Value> = OrderedMap::with_capacity(n as usize);
                let mut flat = flat.into_iter();
                while let (Some(key), Some(value)) = (flat.next(), flat.next()) {
                    entries.insert(key, value, |a, b| value_eq(&self.heap, a, b));
                }
                let map = self.heap.alloc_map(entries);
                self.push(map);
            }
            Op::MakeTuple(n) => {
                let items = self.pop_n(n as usize);
                self.push(Value::Tuple(Rc::from(items)));
            }
            Op::MakeSetFromVector => {
                let Value::Vector(items) = self.pop() else {
                    return Err(Self::fatal("brasa: `Set` takes exactly 1 Vector argument"));
                };
                let items = self.heap.vector(items).borrow();
                let mut set: OrderedSet<Value> = OrderedSet::new();
                for item in items.iter() {
                    set.add(item.clone(), |a, b| value_eq(&self.heap, a, b));
                }
                drop(items);
                let set = self.heap.alloc_set(set);
                self.push(set);
            }
            Op::MakeStruct(shape) => {
                let field_count = self.module.structs[shape.0 as usize].fields.len();
                let fields = self.pop_n(field_count);
                let strukt = self.heap.alloc_struct(shape, fields);
                self.push(strukt);
            }
            Op::MakeEnum {
                enum_id,
                variant,
                argc,
            } => {
                let fields = self.pop_n(argc as usize);
                self.push(Value::Enum(Rc::new(EnumValue {
                    shape: enum_id,
                    variant: variant as usize,
                    fields,
                })));
            }
            Op::MakeClosure { func, captures } => {
                let captures = self.pop_n(captures as usize);
                self.push(Value::Closure(Rc::new(ClosureValue { func, captures })));
            }
            Op::MakeRange { inclusive } => {
                let hi = self.pop_int()?;
                let lo = self.pop_int()?;
                self.push(Value::Range { lo, hi, inclusive });
            }

            Op::ToString => {
                let value = self.pop();
                let text = self.display(&value)?;
                self.push(Value::str(text));
            }
            Op::IterNew => {
                let value = self.pop();
                let state = self.iter_new(&value)?;
                self.push(Value::Iter(Rc::new(std::cell::RefCell::new(state))));
            }
            Op::IterNext(target) => {
                let Value::Iter(iter) = self.peek() else {
                    unreachable!("iter_next peeks the loop iterator");
                };
                let next = iter.borrow_mut().next();
                match next {
                    Some(item) => self.push(item),
                    None => {
                        self.pop();
                        self.jump(target);
                    }
                }
            }

            Op::Throw => {
                let value = self.pop();
                return Err(Signal::Error(value));
            }
            Op::JumpIfPanic(target) => {
                let Value::Caught(caught) = self.peek() else {
                    unreachable!("jump_if_panic peeks the caught signal");
                };
                if matches!(**caught, Caught::Panic(_)) {
                    self.jump(target);
                }
            }
            Op::JumpIfTagNe { tag, target } => {
                let Constant::Str(tag) = self.module.constants.get(tag) else {
                    unreachable!("jump_if_tag_ne carries a string constant");
                };
                let Value::Caught(caught) = self.peek() else {
                    unreachable!("jump_if_tag_ne peeks the caught signal");
                };
                let signal_tag = match &**caught {
                    Caught::Error(value) => self.nominal_tag(value),
                    Caught::Panic(panic) => panic.name.to_string(),
                };
                if signal_tag != *tag {
                    self.jump(target);
                }
            }
            Op::CaughtValue => {
                let Value::Caught(caught) = self.peek() else {
                    unreachable!("caught_value peeks the caught signal");
                };
                let bound = match &**caught {
                    Caught::Error(value) => value.clone(),
                    // A panic can never arrive here, and it is the
                    // code generator that guarantees it, not this VM
                    // (`brasa_codegen::catch`): a wildcard arm emits
                    // `JumpIfPanic` ahead of the binding, so a panic
                    // jumps past it; a `panics.`-qualified arm is
                    // dotted and emits `CaughtDetail` instead; and any
                    // other named arm is guarded by `JumpIfTagNe` on a
                    // tag no panic carries. One conformance test per
                    // mechanism: `wildcard_never_catches_a_panic`,
                    // `recursion_limit_panic_is_catchable_by_its_named_arm`
                    // (a `panics.`-qualified arm), and
                    // `rethrow_wrapping_replaces_the_original_error` (a
                    // bare struct name). The arm used to bind
                    // the detail string here as well, mirroring the
                    // walker; with one backend, an unreachable case
                    // says so.
                    Caught::Panic(_) => {
                        unreachable!("a panic binding compiles to caught_detail")
                    }
                };
                self.push(bound);
            }
            Op::CaughtDetail => {
                let Value::Caught(caught) = self.peek() else {
                    unreachable!("caught_detail peeks the caught signal");
                };
                let bound = match &**caught {
                    Caught::Error(Value::NativeError(error)) => Value::Str(error.message.clone()),
                    Caught::Error(value) => value.clone(),
                    Caught::Panic(panic) => Value::str(&panic.detail),
                };
                self.push(bound);
            }
            Op::Rethrow => {
                let Value::Caught(caught) = self.pop() else {
                    unreachable!("rethrow pops the caught signal");
                };
                return Err(match Rc::try_unwrap(caught) {
                    Ok(Caught::Error(value)) => Signal::Error(value),
                    Ok(Caught::Panic(panic)) => Signal::Panic(panic),
                    Err(shared) => match &*shared {
                        Caught::Error(value) => Signal::Error(value.clone()),
                        Caught::Panic(panic) => Signal::Panic(panic.clone()),
                    },
                });
            }
        }
        Ok(())
    }

    // --- operator helpers ----------------------------------------------

    #[cold]
    #[inline(never)]
    fn overflow(&self, op: &str) -> Signal {
        self.panic(INTEGER_OVERFLOW, format!("integer overflow in `{op}`"))
    }

    /// Inlined so that `f` resolves to the concrete checked operation
    /// at each of its call sites instead of an indirect call.
    #[inline(always)]
    fn int_arith(&mut self, symbol: &str, f: fn(i64, i64) -> Option<i64>) -> Result<(), Signal> {
        let b = self.pop_int().map_err(Self::bad_arith)?;
        let a = self.pop_int().map_err(Self::bad_arith)?;
        let result = f(a, b).ok_or_else(|| self.overflow(symbol))?;
        self.push(Value::Int(result));
        Ok(())
    }

    fn float_arith(&mut self, f: fn(f64, f64) -> f64) -> Result<(), Signal> {
        let b = self.pop();
        let a = self.pop();
        match (a, b) {
            (Value::Float(a), Value::Float(b)) => {
                self.push(Value::Float(f(a, b)));
                Ok(())
            }
            _ => Err(Self::fatal(
                "brasa: invalid operands for arithmetic operator",
            )),
        }
    }

    #[cold]
    #[inline(never)]
    fn bad_arith(_: Signal) -> Signal {
        Self::fatal("brasa: invalid operands for arithmetic operator")
    }

    /// Primitive ordering, plus the walker's dynamic struct-`cmp`
    /// fallback (`eval_ordering`). The checker only lets
    /// `int`/`float`/`string`/`char` satisfy `Comparable` today, so the
    /// struct branch is unreachable in checked programs — mirrored
    /// anyway so the two backends share one dynamic contract.
    ///
    /// Inlined so that `f` resolves at each comparison opcode; the
    /// fallback stays outlined so the four call sites do not each carry
    /// a copy of a path only a generic struct receiver reaches.
    #[inline(always)]
    fn ordering(&mut self, f: fn(std::cmp::Ordering) -> bool) -> Result<(), Signal> {
        let b = self.pop();
        let a = self.pop();

        let result = match value_cmp(&a, &b) {
            Some(ordering) => f(ordering),
            None => self.ordering_fallback(&a, &b, f)?,
        };
        self.push(Value::Bool(result));
        Ok(())
    }

    /// The operands [`value_cmp`] has no primitive ordering for.
    #[cold]
    #[inline(never)]
    fn ordering_fallback(
        &mut self,
        a: &Value,
        b: &Value,
        f: fn(std::cmp::Ordering) -> bool,
    ) -> VmResult<bool> {
        match (a, b) {
            // IEEE: comparisons involving NaN are all false.
            (Value::Float(_), Value::Float(_)) => Ok(false),
            // A struct satisfying `Comparable` through its own `cmp`,
            // reached from inside a generic function where the operand
            // type is not statically one of the orderable primitives.
            // `a > b` on two struct values written directly is a T004
            // diagnostic, which is why this reads as unreachable and is
            // not: `comparable_structs_order_through_their_cmp` and
            // `comparable_is_satisfied_transitively_through_a_user_constraint`
            // both come through here.
            (Value::Struct(_), Value::Struct(_)) => {
                let cmp = self.call_struct_by_name(a.clone(), "cmp", vec![b.clone()])?;
                match cmp {
                    Value::Int(v) => Ok(f(v.cmp(&0))),
                    _ => Err(Self::fatal("brasa: `cmp` must return an int")),
                }
            }
            _ => Err(Self::fatal("brasa: operands are not comparable")),
        }
    }

    /// Runtime member dispatch on a struct receiver: declared methods
    /// first, then fields holding callables, then the universal
    /// `toString`. Reached from [`Vm::ordering`] for a struct
    /// satisfying `Comparable` through its own `cmp`.
    fn call_struct_by_name(&mut self, recv: Value, name: &str, args: Vec<Value>) -> VmResult {
        let Value::Struct(s) = &recv else {
            unreachable!("struct dispatch needs a struct receiver");
        };
        let s = *s;
        let shape = self.module_struct(self.heap.struct_value(s).shape);

        if let Some(&func) = shape
            .methods
            .iter()
            .find(|&&method| self.function(method).name == name)
        {
            let mut with_recv = Vec::with_capacity(args.len() + 1);
            with_recv.push(recv.clone());
            with_recv.extend(args);
            return self.call_function(func, with_recv);
        }

        if let Some(ix) = shape.fields.iter().position(|field| field == name) {
            let field = self.heap.struct_value(s).fields.borrow()[ix].clone();
            return self.call_callable(field, args);
        }

        if name == "toString" {
            let text = self.display(&recv)?;
            return Ok(Value::str(text));
        }

        Err(Self::fatal(format!("brasa: unknown member `{name}`")))
    }

    /// The string constant behind a dynamic-dispatch operand.
    fn const_str(&self, id: ConstId) -> &'a str {
        let Constant::Str(text) = self.module.constants.get(id) else {
            unreachable!("dynamic member dispatch carries a string constant");
        };
        text
    }

    /// `call_method_dyn c, argc`: the member call behind a receiver the
    /// checker only knows as a generic parameter, mirroring the
    /// walker's `call_method_by_name`. `argc` counts the receiver, which
    /// already sits below the arguments.
    ///
    /// Struct methods and field callables enter their frame in place
    /// rather than through a nested loop, so recursion through a
    /// constraint method is bounded by the same call-depth guard as a
    /// direct call.
    fn call_method_dyn(&mut self, name: &str, argc: usize) -> Result<(), Signal> {
        let base = self.stack.len() - argc;

        let Value::Struct(s) = self.stack[base] else {
            let mut args = self.pop_n(argc);
            let recv = args.remove(0);
            let result = self.method_builtin(name, recv, args)?;
            self.push(result);
            return Ok(());
        };

        let shape = self.module_struct(self.heap.struct_value(s).shape);

        if let Some(&func) = shape
            .methods
            .iter()
            .find(|&&method| self.function(method).name == name)
        {
            return self.enter_function(func, base, base);
        }

        // A struct field holding a callable: it replaces the receiver
        // on the stack, which leaves exactly an indirect-call layout.
        if let Some(ix) = shape.fields.iter().position(|field| field == name) {
            self.stack[base] = self.heap.struct_value(s).fields.borrow()[ix].clone();
            return self.call_value_op(argc - 1);
        }

        if name == "toString" && argc == 1 {
            let recv = self.pop();
            let text = self.display(&recv)?;
            self.push(Value::str(text));
            return Ok(());
        }

        self.stack.truncate(base);
        Err(Self::fatal(format!("brasa: unknown member `{name}`")))
    }

    /// `bind_method_dyn c`: the same lookup without calling.
    ///
    /// Methods before fields, in the same order as the call path. The
    /// two used to disagree, each mirroring one of the walker's own
    /// paths, and it never mattered: a struct's fields and its methods
    /// are ONE member namespace (`docs/spec/06-diagnostics.md`, R006),
    /// so no checked program can hold a name that both would find. Two
    /// orders for one concept is still a defect, whoever implements it.
    fn bind_member_by_name(&mut self, recv: Value, name: &str) -> VmResult {
        if let Value::Struct(s) = recv {
            let shape = self.module_struct(self.heap.struct_value(s).shape);

            if let Some(&func) = shape
                .methods
                .iter()
                .find(|&&method| self.function(method).name == name)
            {
                return Ok(Value::BoundMethod(Rc::new(BoundMethod { recv, func })));
            }
            if let Some(ix) = shape.fields.iter().position(|field| field == name) {
                return Ok(self.heap.struct_value(s).fields.borrow()[ix].clone());
            }
        }

        match builtin_id(name).filter(|&id| builtin_def(id).is_some_and(|def| def.has_receiver)) {
            Some(builtin) => Ok(Value::BoundBuiltin(Rc::new(BoundBuiltin { recv, builtin }))),
            None => Err(Self::fatal(format!(
                "brasa: unknown builtin method `{name}`"
            ))),
        }
    }

    fn get_index(&self, recv: &Value, index: &Value) -> VmResult {
        match (recv, index) {
            (Value::Vector(items), Value::Int(i)) => {
                let items = self.heap.vector(*items).borrow();
                let len = items.len();
                if *i < 0 || *i as usize >= len {
                    return Err(self.panic(
                        INDEX_OUT_OF_BOUNDS,
                        format!("index {i} out of range (len {len})"),
                    ));
                }
                Ok(items[*i as usize].clone())
            }
            (Value::Map(entries), key) => Ok(self
                .heap
                .map(*entries)
                .borrow()
                .get(key, |a, b| value_eq(&self.heap, a, b))
                .map(|v| Value::some(v.clone()))
                .unwrap_or(Value::NONE)),
            // `Json` indexing is total (BRS-34,
            // `docs/spec/05-stdlib.md`): a missing member, an
            // out-of-range position, or a wrong-kind node is `None`,
            // and chains flatten through `Option<Json>` (`None`
            // propagates).
            (Value::Json(tree), index) => Self::json_index_value(tree, index),
            (Value::Option(inner), index) => match inner.as_deref() {
                Some(Value::Json(tree)) => Self::json_index_value(tree, index),
                None => Ok(Value::NONE),
                Some(_) => Err(Self::fatal("brasa: value does not support indexing")),
            },
            _ => Err(Self::fatal("brasa: value does not support indexing")),
        }
    }

    fn json_index_value(tree: &brasa_runtime::json_glue::JsonValue, index: &Value) -> VmResult {
        let subtree = match index {
            Value::Str(key) => brasa_runtime::json_glue::index_key(tree, key),
            Value::Int(position) => brasa_runtime::json_glue::index_position(tree, *position),
            _ => {
                return Err(Self::fatal(
                    "brasa: a Json index must be a string or an int",
                ));
            }
        };

        Ok(subtree
            .map(Value::Json)
            .map(Value::some)
            .unwrap_or(Value::NONE))
    }

    fn set_index(&self, recv: &Value, index: Value, value: Value) -> Result<(), Signal> {
        match recv {
            Value::Vector(items) => {
                let Value::Int(i) = index else {
                    return Err(Self::fatal("brasa: vector index must be an int"));
                };
                let mut items = self.heap.vector(*items).borrow_mut();
                let len = items.len();
                if i < 0 || i as usize >= len {
                    return Err(self.panic(
                        INDEX_OUT_OF_BOUNDS,
                        format!("index {i} out of range (len {len})"),
                    ));
                }
                items[i as usize] = value;
                Ok(())
            }
            Value::Map(entries) => {
                self.heap.edit_map(*entries, |entries| {
                    entries.insert(index, value, |a, b| value_eq(&self.heap, a, b));
                });
                Ok(())
            }
            _ => Err(Self::fatal(
                "brasa: value does not support index assignment",
            )),
        }
    }

    /// Snapshot iteration at loop entry for collections (M1 decision);
    /// ranges stay lazy.
    fn iter_new(&self, value: &Value) -> VmResult<IterState> {
        match value {
            Value::Range { lo, hi, inclusive } => Ok(IterState::Range {
                next: *lo,
                hi: *hi,
                inclusive: *inclusive,
                done: false,
            }),
            Value::Vector(items) => Ok(IterState::Items {
                items: self.heap.vector(*items).borrow().clone(),
                ix: 0,
            }),
            Value::Map(entries) => Ok(IterState::Items {
                items: self
                    .heap
                    .map(*entries)
                    .borrow()
                    .iter()
                    .map(|(k, v)| Value::Tuple(Rc::from(vec![k.clone(), v.clone()])))
                    .collect(),
                ix: 0,
            }),
            Value::Set(items) => Ok(IterState::Items {
                items: self.heap.set(*items).borrow().items().to_vec(),
                ix: 0,
            }),
            Value::Str(s) => Ok(IterState::Items {
                items: s.chars().map(Value::Char).collect(),
                ix: 0,
            }),
            _ => Err(Self::fatal(
                "brasa: `for` iterates `Vector`, `Map`, `Set`, ranges, and `string`",
            )),
        }
    }

    // --- calls ---------------------------------------------------------

    /// `call_value argc`: the callee sits directly below the arguments
    /// and is replaced by the result on return.
    fn call_value_op(&mut self, argc: usize) -> Result<(), Signal> {
        let callee_ix = self.stack.len() - argc - 1;
        let callee = self.stack[callee_ix].clone();

        match callee {
            Value::Func(func) => {
                let function = self.function(func);
                if function.arity as usize != argc {
                    return Err(Self::fatal(format!(
                        "brasa: `{}` takes {} argument(s), found {argc}",
                        function.name, function.arity
                    )));
                }
                self.enter_function(func, callee_ix + 1, callee_ix)
            }
            Value::Closure(closure) => {
                let function = self.function(closure.func);
                if function.arity as usize != argc {
                    return Err(Self::fatal(format!(
                        "brasa: lambda takes {} argument(s), found {argc}",
                        function.arity
                    )));
                }
                self.enter_function(closure.func, callee_ix + 1, callee_ix)?;
                self.write_captures(&closure);
                Ok(())
            }
            Value::BoundMethod(bound) => {
                let function = self.function(bound.func);
                let expected = function.arity as usize - 1;
                if expected != argc {
                    return Err(Self::fatal(format!(
                        "brasa: `{}` takes {expected} argument(s), found {argc}",
                        function.name
                    )));
                }
                // The receiver becomes slot 0, below the arguments.
                self.stack.insert(callee_ix + 1, bound.recv.clone());
                self.enter_function(bound.func, callee_ix + 1, callee_ix)
            }
            Value::BoundBuiltin(bound) => {
                let mut args = self.pop_n(argc);
                self.pop();
                args.insert(0, bound.recv.clone());
                let result = self.builtin_with_args(bound.builtin, args)?;
                self.push(result);
                Ok(())
            }
            _ => Err(Self::fatal("brasa: value is not callable")),
        }
    }

    /// Calls a callable value from native code (builtin HOFs, user
    /// `toString` during rendering) with a nested bounded loop: the
    /// walker recurses in Rust at exactly these points, and the shared
    /// call-depth guard bounds both.
    pub(crate) fn call_callable(&mut self, callee: Value, args: Vec<Value>) -> VmResult {
        match callee {
            Value::Func(func) => {
                let function = self.function(func);
                if function.arity as usize != args.len() {
                    return Err(Self::fatal(format!(
                        "brasa: `{}` takes {} argument(s), found {}",
                        function.name,
                        function.arity,
                        args.len()
                    )));
                }
                self.call_frames(func, None, args)
            }
            Value::Closure(closure) => {
                let function = self.function(closure.func);
                if function.arity as usize != args.len() {
                    return Err(Self::fatal(format!(
                        "brasa: lambda takes {} argument(s), found {}",
                        function.arity,
                        args.len()
                    )));
                }
                // Rooting the closure for the call is what keeps its
                // captures alive across it. Entering the frame copies
                // them into stack slots, but the callee may assign to a
                // capture — the store is frame-local, and the next
                // invocation republishes the original from here — so
                // between the store and that republication this is the
                // only reference to the overwritten value. The callee
                // was popped off the value stack before the call, so
                // without this the collection a nested loop can now run
                // (BRS-62) sweeps the capture and the next invocation
                // republishes a recycled slot.
                let rooted = [Value::Closure(closure.clone())];
                self.with_rooted(&rooted, |this| {
                    this.call_frames(closure.func, Some(&closure), args)
                })
            }
            Value::BoundMethod(bound) => {
                let function = self.function(bound.func);
                let expected = function.arity as usize - 1;
                if expected != args.len() {
                    return Err(Self::fatal(format!(
                        "brasa: `{}` takes {expected} argument(s), found {}",
                        function.name,
                        args.len()
                    )));
                }
                let mut with_recv = Vec::with_capacity(args.len() + 1);
                with_recv.push(bound.recv.clone());
                with_recv.extend(args);
                self.call_frames(bound.func, None, with_recv)
            }
            Value::BoundBuiltin(bound) => {
                let mut with_recv = Vec::with_capacity(args.len() + 1);
                with_recv.push(bound.recv.clone());
                with_recv.extend(args);
                self.builtin_with_args(bound.builtin, with_recv)
            }
            _ => Err(Self::fatal("brasa: value is not callable")),
        }
    }

    /// Calls a compiled function reentrantly: `main`-loop mechanics on
    /// a bounded frame, returning the popped result.
    pub(crate) fn call_function(&mut self, func: FuncId, args: Vec<Value>) -> VmResult {
        self.call_frames(func, None, args)
    }

    fn call_frames(
        &mut self,
        func: FuncId,
        closure: Option<&ClosureValue>,
        args: Vec<Value>,
    ) -> VmResult {
        let ret_base = self.stack.len();
        self.stack.extend(args);

        if let Err(signal) = self.enter_function(func, ret_base, ret_base) {
            self.stack.truncate(ret_base);
            return Err(signal);
        }
        if let Some(closure) = closure {
            self.write_captures(closure);
        }

        self.execute(self.frames.len())?;
        Ok(self.pop())
    }

    /// `call_builtin b argc`: pops every operand (receiver included
    /// when the builtin takes one) and returns the single result.
    fn dispatch_builtin(&mut self, builtin: BuiltinId, argc: usize) -> VmResult {
        let args = self.pop_n(argc);
        self.builtin_with_args(builtin, args)
    }

    /// Runs a builtin over operands `dispatch_builtin` already popped
    /// off the value stack, so they are unreachable from the root set
    /// for the whole dispatch.
    ///
    /// What makes that safe is that no builtin reads a popped operand
    /// after reentering compiled code, which is where a collection can
    /// sweep it. Two families reenter. The callback-taking builtins
    /// do, and `docs/spec/05-stdlib.md` requires every one of them to
    /// traverse a snapshot taken before the first call, so none may
    /// read its receiver again afterwards; the traversal helpers root
    /// the receiver anyway, so a future one cannot get this wrong
    /// silently. Rendering does too, through a user `toString`
    /// override, and there the operand being rendered is pushed as the
    /// override's receiver — the value stack roots it for the call.
    ///
    /// Rooting the operands here instead would cost a clone on every
    /// builtin call (~6% on a builtin-hot loop) to protect reads that
    /// do not happen.
    pub(crate) fn builtin_with_args(&mut self, builtin: BuiltinId, args: Vec<Value>) -> VmResult {
        let def = builtin_def(builtin).expect("compiled builtin ids are registered");

        if def.has_receiver {
            let mut args = args.into_iter();
            let recv = args.next().expect("method builtins carry a receiver");
            self.method_builtin(def.name, recv, args.collect())
        } else {
            self.free_builtin(def.name, args)
        }
    }

    // --- nominal tags --------------------------------------------------

    /// The nominal type tag `catch` matches against: the declared name
    /// for structs and enums, the type name otherwise.
    pub(crate) fn nominal_tag(&self, value: &Value) -> String {
        match value {
            Value::Int(_) => "int".to_string(),
            Value::Float(_) => "float".to_string(),
            Value::Bool(_) => "bool".to_string(),
            Value::Char(_) => "char".to_string(),
            Value::Unit => "unit".to_string(),
            Value::Str(_) => "string".to_string(),
            Value::Range { .. } => "Range".to_string(),
            Value::Tuple(_) => "tuple".to_string(),
            Value::Vector(_) => "Vector".to_string(),
            Value::Map(_) => "Map".to_string(),
            Value::Set(_) => "Set".to_string(),
            Value::Option(_) => "Option".to_string(),
            Value::Struct(s) => self.module.structs[self.heap.struct_value(*s).shape.0 as usize]
                .name
                .clone(),
            Value::Enum(e) => self.module.enums[e.shape.0 as usize].name.clone(),
            Value::NativeError(error) => error.name.to_string(),
            Value::ProcOutput(_) => "Output".to_string(),
            Value::HttpResponse(_) => "Response".to_string(),
            Value::CliArgs(_) => "Args".to_string(),
            Value::Walk(_) => "Walk".to_string(),
            Value::Json(_) => "Json".to_string(),
            Value::Func(_) | Value::Closure(_) | Value::BoundMethod(_) | Value::BoundBuiltin(_) => {
                "function".to_string()
            }
            Value::Caught(_) | Value::Iter(_) => {
                unreachable!("internal values never reach nominal_tag")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every operand-stack slot is a [`Value`] and every instruction
    /// returns a `Result<_, Signal>`, so both widths are dispatch-loop
    /// costs paid per instruction rather than implementation details
    /// (BRS-98). Both are set by their widest variant, and a new
    /// variant carrying its payload inline silently widens every slot
    /// or every return — which is what these figures catch. Raising
    /// either is a deliberate decision, not a diff to wave through:
    /// box the payload instead, as `Value::NativeError` and
    /// `Signal::Panic` do.
    #[test]
    fn the_hot_path_types_stay_narrow() {
        assert_eq!(std::mem::size_of::<Value>(), 24, "Value widened");
        assert_eq!(std::mem::size_of::<Signal>(), 32, "Signal widened");
    }

    /// The resolver validates `panics.`-qualified `catch` arm names
    /// against its own list, and the VM raises by the constants above.
    /// Two lists mean they can drift: a panic the resolver accepts with
    /// no raiser behind it, or a raiser carrying a name no arm may
    /// bind. The walker's suite held this guard until BRS-108 retired
    /// it; it belongs wherever the raising happens.
    ///
    /// What it catches mechanically is the resolver growing a name
    /// nothing raises. The other direction is only as good as this
    /// list: a new constant added above and raised, without being added
    /// here, still passes. Closing that would take one list both crates
    /// read, which is a larger change than this guard.
    #[test]
    fn the_raised_panic_names_are_exactly_the_resolver_union() {
        let mut raised = vec![
            INDEX_OUT_OF_BOUNDS,
            DIVISION_BY_ZERO,
            INTEGER_OVERFLOW,
            ASSERTION_FAILED,
            STACK_OVERFLOW,
        ];
        raised.sort_unstable();

        let mut declared: Vec<&str> = brasa_resolver::PANIC_UNION.to_vec();
        declared.sort_unstable();

        assert_eq!(
            raised, declared,
            "the panic union and the names the VM raises have drifted"
        );
    }
}
