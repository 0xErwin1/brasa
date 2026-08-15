//! The dispatch loop: frames, calls, and handler-table unwinding.
//!
//! Execution model (spec: 07 — Diseño del bytecode): one contiguous value
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
use brasa_runtime::offload::{Job, JobId, OffloadPool};
use brasa_runtime::table::{OrderedMap, OrderedSet};

use crate::heap::{GcRef, Heap, Interner};
use crate::value::{
    BoundBuiltin, BoundMethod, Caught, ClosureValue, EnumValue, IterState, PanicValue, TaskState,
    Value, value_cmp, value_eq,
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
    /// A debug session stopped the run (BRS-117). Not catchable, for the
    /// same reason `Exit` is not: handler unwinding tests for
    /// `Error`/`Panic` only, so this passes every `catch` by
    /// construction — which is what lets a session pause with the frame
    /// stack and the operand stack exactly as the program left them.
    Breakpoint,
    /// A blocking builtin suspended the running task
    /// (spec: 08 — Concurrencia estructurada, BRS-133): its job is
    /// submitted, its wait is recorded on the scheduler, and its frames
    /// must survive untouched — so this signal BYPASSES unwinding
    /// entirely and returns straight out of the dispatch loop to the
    /// drive loop that swapped the task in. Raised only when
    /// [`Vm::can_park`] held, which is what guarantees a drive loop is
    /// there to catch it.
    Park,
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

/// One task's execution state: the unit a future scheduler parks and
/// resumes (spec: 08 — structured concurrency). The value stack, the
/// frame stack, and the native root stack belong to a task; the heap,
/// globals, streams, and caches stay on [`Vm`] because every task
/// shares them.
#[derive(Default)]
struct Task {
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
    /// How many nested Rust-stack dispatch loops this task is inside
    /// ([`Vm::call_frames`]): a builtin HOF running its callback, or
    /// rendering running a user `toString`. Parking there would abandon
    /// live Rust frames, so a suspension point at depth > 0 degrades to
    /// its synchronous blocking form — same semantics, no overlap.
    /// Compiled-to-compiled calls stay in one loop and do not count:
    /// a task parks fine at any Brasa call depth.
    reentry_depth: usize,
}

impl Task {
    /// Every value the collector must reach through this task: its
    /// value stack and its native root stack (frames hold no values).
    /// A safepoint that also has parked tasks chains each one's roots
    /// through this same iterator — one loop extension, not a second
    /// rooting scheme.
    fn roots(&self) -> impl Iterator<Item = &Value> {
        self.stack.iter().chain(self.native_roots.iter())
    }
}

pub(crate) struct Vm<'a> {
    module: &'a Module,
    globals: Vec<Option<Value>>,
    /// The running task, held inline so the dispatch loop reaches its
    /// stack and frames at a fixed offset from `self` — exactly what
    /// direct fields cost. A future scheduler keeps parked tasks in a
    /// separate list and swaps them through here; the running task is
    /// never behind an index or a box the fetch path would chase.
    task: Task,
    pub(crate) heap: Heap,
    interner: Interner,
    /// Constants pre-materialized at load: string constants are
    /// interned once here, so every `const` push shares one allocation.
    consts: Vec<Value>,
    pub(crate) out: &'a mut (dyn Write + Send),
    /// Where `io.eprint` writes (spec: 05 — Stdlib de scripting).
    pub(crate) err: &'a mut (dyn Write + Send),
    /// What `io.readLine`/`io.readAll` consume.
    pub(crate) input: &'a mut (dyn std::io::BufRead + Send),
    max_depth: usize,
    /// Per-run cache of compiled regex patterns for the string regex
    /// methods, keyed by the pattern text.
    pub(crate) regex_cache: std::collections::HashMap<String, Rc<regex::Regex>>,
    /// The script's trailing CLI arguments, served by `env.args()`
    /// (BRS-32, spec: 05 — Stdlib de scripting).
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
    /// Present only while a debug session is driving this VM (BRS-117).
    /// `None` on every ordinary run, and the field the dispatch loop
    /// never looks at — see [`Vm::execute_instrumented`].
    pub(crate) debug: Option<crate::debug::DebugState>,
    /// Whether a debugged run has already entered `main`, so a resume
    /// that finishes `<toplevel>` does not run it twice.
    debug_ran_entry: bool,
    /// Present only while a profiled run is executing (BRS-121). Like
    /// `debug`, the ordinary dispatch loop never looks at it.
    pub(crate) profile: Option<crate::profile::Profiler>,
    /// The task scheduler (spec: 08 — Concurrencia estructurada,
    /// BRS-133): parked and runnable tasks, the drivers set aside while
    /// a drive loop runs, and the IO offload pool. Boxed and `None`
    /// until the first task is started, so a run that never spawns
    /// never allocates any of it — the same lazy rule the TLS stack
    /// follows.
    sched: Option<Box<SchedState>>,
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
            task: Task::default(),
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
            debug: None,
            debug_ran_entry: false,
            profile: None,
            sched: None,
        }
    }

    /// A whole run under the profiler, returning what it measured.
    pub(crate) fn run_profiled(&mut self) -> (Outcome, crate::profile::Profile) {
        let result = (|| {
            self.enter_function(FuncId(0), 0, 0)?;
            self.execute_instrumented(1)?;
            self.task.stack.clear();

            if let Some(main) = self.module.entry {
                if self.function(main).arity != 0 {
                    return Err(Signal::Fatal(
                        "brasa: `main` must take no parameters".to_string(),
                    ));
                }
                self.enter_function(main, 0, 0)?;
                self.execute_instrumented(1)?;
                self.task.stack.clear();
            }

            Ok(())
        })();

        let names: Vec<String> = self
            .module
            .functions
            .iter()
            .map(|function| function.name.clone())
            .collect();
        let profiler = self
            .profile
            .take()
            .expect("a profiled run always has a profiler");

        (self.finish(result), profiler.finish(&names))
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
            self.task.stack.clear();
            self.task.frames.clear();
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
        self.task.stack.clear();

        self.finish(result)
    }

    /// Calls one zero-argument function on a cleared stack.
    fn call_entry(&mut self, func: FuncId) -> Result<(), Signal> {
        self.enter_function(func, 0, 0)?;
        self.execute(1)?;
        self.task.stack.clear();

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
        self.task.stack.clear();

        if let Some(main) = self.module.entry {
            if self.function(main).arity != 0 {
                return Err(Signal::Fatal(
                    "brasa: `main` must take no parameters".to_string(),
                ));
            }
            self.enter_function(main, 0, 0)?;
            self.execute(1)?;
            self.task.stack.clear();
        }

        Ok(())
    }

    fn finish(&mut self, result: Result<(), Signal>) -> Outcome {
        match result {
            Ok(()) => Outcome::Success,
            // A session that is still paused never reaches here: the
            // signal is answered as a `Stop`, and only a run that ended
            // is finished. Reaching this means a debug loop escaped its
            // session, which is a bug in the substrate, not a run
            // outcome.
            Err(Signal::Breakpoint) => Outcome::Panic {
                message: "brasa: debug pause escaped its session".to_string(),
            },
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
            // A park is raised only under `can_park`, whose drive loop
            // consumes it before any bounded caller can see it.
            Err(Signal::Park) => Outcome::Panic {
                message: "brasa: a parked task escaped its scheduler".to_string(),
            },
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
        let toplevel = usize::from(
            self.task
                .frames
                .first()
                .is_some_and(|f| f.func == FuncId(0)),
        );
        self.task.frames.len() - toplevel
    }

    /// Active function names, innermost first, excluding `<toplevel>` —
    /// the walker's panic-stacktrace snapshot.
    fn capture_trace(&self) -> Vec<String> {
        self.task
            .frames
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
        self.task
            .stack
            .reserve(floor + function.max_stack as usize - self.task.stack.len());
        self.task.stack.resize(floor, Value::Unit);

        self.task.frames.push(Frame {
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
        let frame = self
            .task
            .frames
            .last()
            .expect("captures need an active frame");
        let start = frame.base + self.function(closure.func).arity as usize;
        for (offset, value) in closure.captures.iter().enumerate() {
            self.task.stack[start + offset] = value.clone();
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

        while self.task.frames.len() >= min_frames {
            if self.heap.should_collect() {
                self.heap
                    .collect(Self::all_roots(&self.task, &self.sched, &self.globals));
            }

            let frame = self.task.frames.last_mut().expect("loop condition holds");
            let func = frame.func;
            let ip = frame.ip;
            frame.ip = ip + 1;

            if current != Some(func) {
                current = Some(func);
                code = self.function(func).chunk.ops();
            }

            if let Err(signal) = self.step(code[ip]) {
                // A park is not an error: the task's frames must
                // survive for its resume, so the signal skips unwinding
                // and returns to the drive loop that swapped it in.
                if matches!(signal, Signal::Park) {
                    return Err(signal);
                }
                self.unwind(signal, min_frames)?;
            }
        }
        Ok(())
    }

    /// The full root set at a safepoint: the running task, every task
    /// the scheduler holds (runnable, parked, and set-aside drivers),
    /// and the globals. An associated function over the fields rather
    /// than a `&self` method so the call can borrow `self.heap`
    /// mutably alongside it.
    fn all_roots<'r>(
        task: &'r Task,
        sched: &'r Option<Box<SchedState>>,
        globals: &'r [Option<Value>],
    ) -> impl Iterator<Item = &'r Value> {
        task.roots()
            .chain(sched.iter().flat_map(|sched| sched.roots()))
            .chain(globals.iter().flatten())
    }

    // --- debug substrate (BRS-117) -------------------------------------

    /// Whether the debug loop should stop before `(func, ip)`.
    ///
    /// Only ever called from [`Vm::execute_instrumented`].
    fn debug_should_stop(&mut self, func: FuncId, ip: usize, depth: usize) -> bool {
        let Some(state) = self.debug.as_mut() else {
            return false;
        };

        // The instruction we are resuming ON is the one we just stopped
        // at. Checking it again would stop forever.
        if state.take_resuming() {
            return false;
        }

        state.should_stop(func, ip, depth)
    }

    /// Enters the debug loop for the first time: `<toplevel>`, then
    /// `main` if the module has one — the same order [`Vm::run_program`]
    /// uses, so a debugged run is the run the user would get.
    pub(crate) fn start_debug(&mut self) -> Result<(), Signal> {
        self.enter_function(FuncId(0), 0, 0)?;
        self.execute_instrumented(1)?;
        self.task.stack.clear();

        if let Some(main) = self.module.entry {
            if self.function(main).arity != 0 {
                return Err(Signal::Fatal(
                    "brasa: `main` must take no parameters".to_string(),
                ));
            }
            self.debug_ran_entry = true;
            self.enter_function(main, 0, 0)?;
            self.execute_instrumented(1)?;
            self.task.stack.clear();
        }

        Ok(())
    }

    /// Re-enters the loop after a pause. The frames are untouched, so
    /// this picks up exactly where the signal left off.
    pub(crate) fn resume_debug(&mut self) -> Result<(), Signal> {
        self.execute_instrumented(1)?;
        self.task.stack.clear();

        // The toplevel finished while paused inside it; `main` is still
        // owed, exactly as `start_debug` would have run it.
        if let Some(main) = self.module.entry
            && !self.debug_ran_entry
        {
            self.debug_ran_entry = true;
            self.enter_function(main, 0, 0)?;
            self.execute_instrumented(1)?;
            self.task.stack.clear();
        }

        Ok(())
    }

    pub(crate) fn finish_debug(&mut self, result: Result<(), Signal>) -> Outcome {
        self.finish(result)
    }

    pub(crate) fn frame_depth(&self) -> usize {
        self.task.frames.len()
    }

    /// The instruction the innermost frame is about to run.
    pub(crate) fn current_position(&self) -> Option<(FuncId, usize)> {
        self.task.frames.last().map(|frame| (frame.func, frame.ip))
    }

    /// Every frame as `(function, ip, slots)`, outermost first.
    ///
    /// A slot beyond what the stack actually holds reads as `None`: a
    /// frame pauses part-way through its own prologue, and reporting a
    /// neighbouring frame's value as a local would be worse than
    /// reporting nothing.
    pub(crate) fn frame_views(&self) -> Vec<(FuncId, usize, Vec<Option<Value>>)> {
        self.task
            .frames
            .iter()
            .map(|frame| {
                let locals = self.function(frame.func).locals as usize;
                let slots = (0..locals)
                    .map(|slot| self.task.stack.get(frame.base + slot).cloned())
                    .collect();

                (frame.func, frame.ip, slots)
            })
            .collect()
    }

    /// A census of the heap at the pause (BRS-120).
    pub(crate) fn heap_view(&self) -> crate::debug::HeapView {
        let stats = self.heap.stats();

        crate::debug::HeapView {
            by_kind: self
                .heap
                .census()
                .into_iter()
                .map(|(kind, count)| (kind.to_string(), count))
                .collect(),
            live_slots: stats.live,
            free_slots: self.heap.free_slots(),
            live_bytes: self.heap.live_bytes(),
            peak_bytes: self.heap.peak_bytes(),
            allocations: stats.allocations,
            collections: stats.collections,
        }
    }

    /// Why an object is still alive: the shortest chain of arena cells
    /// from a root to it (BRS-120).
    pub(crate) fn retention_of(&self, target: crate::GcRef) -> Option<Vec<crate::GcRef>> {
        self.heap.retention_path(
            self.task.roots().chain(self.globals.iter().flatten()),
            target,
        )
    }

    /// One value rendered one level deep (`crate::debug::ValueView`).
    pub(crate) fn value_view(&self, value: &Value) -> crate::debug::ValueView {
        crate::debug::ValueView {
            summary: self.debug_summary(value),
            children: self.debug_children(value),
            cell: match value {
                Value::Vector(r)
                | Value::Map(r)
                | Value::Set(r)
                | Value::Struct(r)
                | Value::Binding(r) => Some(*r),
                _ => None,
            },
        }
    }

    /// A one-line rendering that never runs user code.
    ///
    /// Deliberately NOT `Vm::display`: that dispatches to a struct's own
    /// `toString`, and a debugger must not execute the program to
    /// describe it. Running a method while paused could have side
    /// effects, could throw with nowhere to unwind to, and would report
    /// what the program says about a value rather than what the value
    /// is — which is the one thing a debugger is for.
    fn debug_summary(&self, value: &Value) -> String {
        match value {
            Value::Int(v) => v.to_string(),
            Value::Float(v) => format!("{v:?}"),
            Value::Bool(v) => v.to_string(),
            Value::Char(v) => format!("{v:?}"),
            Value::Unit => "unit".to_string(),
            Value::Str(s) => format!("{:?}", &**s),
            Value::Range { lo, hi, inclusive } => {
                let dots = if *inclusive { "..=" } else { ".." };
                format!("{lo}{dots}{hi}")
            }
            Value::Tuple(items) => format!("tuple of {}", items.len()),
            Value::Vector(r) => format!("Vector of {}", self.heap.vector(*r).borrow().len()),
            Value::Map(r) => format!("Map of {}", self.heap.map(*r).borrow().len()),
            Value::Set(r) => format!("Set of {}", self.heap.set(*r).borrow().len()),
            Value::Option(None) => "None".to_string(),
            Value::Option(Some(_)) => "Some(…)".to_string(),
            Value::Struct(r) => {
                let value = self.heap.struct_value(*r);
                self.module.structs[value.shape.0 as usize].name.clone()
            }
            Value::Enum(e) => format!("enum variant {}", e.variant),
            Value::Func(f) => format!("fn {}", self.function(*f).name),
            Value::Closure(_) => "closure".to_string(),
            Value::BoundMethod(_) => "bound method".to_string(),
            Value::BoundBuiltin(_) => "bound builtin".to_string(),
            other => format!("{other:?}"),
        }
    }

    /// One level of children, summarised. Never recurses: a cyclic
    /// value would not terminate and a large one would cost more than
    /// the question is worth.
    fn debug_children(&self, value: &Value) -> Vec<(String, String)> {
        match value {
            Value::Vector(r) => self
                .heap
                .vector(*r)
                .borrow()
                .iter()
                .enumerate()
                .map(|(ix, item)| (ix.to_string(), self.debug_summary(item)))
                .collect(),
            Value::Tuple(items) => items
                .iter()
                .enumerate()
                .map(|(ix, item)| (ix.to_string(), self.debug_summary(item)))
                .collect(),
            Value::Map(r) => self
                .heap
                .map(*r)
                .borrow()
                .entries()
                .iter()
                .map(|(key, val)| (self.debug_summary(key), self.debug_summary(val)))
                .collect(),
            Value::Set(r) => self
                .heap
                .set(*r)
                .borrow()
                .iter()
                .enumerate()
                .map(|(ix, item)| (ix.to_string(), self.debug_summary(item)))
                .collect(),
            Value::Option(Some(inner)) => vec![("Some".to_string(), self.debug_summary(inner))],
            Value::Struct(r) => {
                let value = self.heap.struct_value(*r);
                let shape = &self.module.structs[value.shape.0 as usize];

                shape
                    .fields
                    .iter()
                    .zip(value.fields.borrow().iter())
                    .map(|(name, field)| (name.clone(), self.debug_summary(field)))
                    .collect()
            }
            _ => Vec::new(),
        }
    }

    /// [`Vm::execute`]'s twin, used by every consumer that needs to
    /// observe execution: a debug session (BRS-117) and the sampling
    /// profiler (BRS-121).
    ///
    /// It is a SEPARATE loop on purpose. The ticket's requirement is
    /// that a breakpoint cost nothing when none is set, and the
    /// original plan was to patch a trap instruction into the chunk so
    /// the one shared loop stayed byte-identical. That does not work
    /// here: the VM runs on a spawned thread for its stack size, so
    /// `Module` must be `Sync`, and interior mutability on the code
    /// would take that away.
    ///
    /// Splitting the loop makes the guarantee structural instead of
    /// argued — `execute` above is untouched, so there is nothing to
    /// re-measure — and it makes the patching unnecessary, since a cold
    /// loop can afford an ordinary lookup. No new opcode, no saved-op
    /// table, and no unpatch-step-repatch dance on resume.
    ///
    /// The profiler rides the same split for the same reason. Counting
    /// instructions in the hot loop would perturb the measurement AND
    /// cost what the loop was tightened to save; here a clock
    /// comparison per instruction is affordable, and the samples stay
    /// time-based so the distribution is fair.
    pub(crate) fn execute_instrumented(&mut self, min_frames: usize) -> Result<(), Signal> {
        while self.task.frames.len() >= min_frames {
            if self.heap.should_collect() {
                // Collector time is measured apart from interpreted
                // time: a script slow because of the collector and one
                // slow because of its own loop want different fixes,
                // and a single total hides which it is.
                let started = std::time::Instant::now();
                self.heap
                    .collect(Self::all_roots(&self.task, &self.sched, &self.globals));

                if let Some(profiler) = self.profile.as_mut() {
                    profiler.add_gc(started.elapsed());
                }
            }

            if self.profile.is_some() {
                let stack: Vec<FuncId> = self.task.frames.iter().map(|frame| frame.func).collect();
                self.profile
                    .as_mut()
                    .expect("checked above")
                    .maybe_sample(|| stack.clone());
            }

            let depth = self.task.frames.len();
            let frame = self.task.frames.last().expect("loop condition holds");
            let (func, ip) = (frame.func, frame.ip);

            if self.debug_should_stop(func, ip, depth) {
                return Err(Signal::Breakpoint);
            }

            let frame = self.task.frames.last_mut().expect("loop condition holds");
            frame.ip = ip + 1;

            let op = self.function(func).chunk.ops()[ip];
            if let Err(signal) = self.step(op) {
                // Same bypass as `execute`: a park keeps its frames.
                if matches!(signal, Signal::Park) {
                    return Err(signal);
                }
                self.unwind(signal, min_frames)?;
            }
        }
        Ok(())
    }

    /// Handler-table unwinding (spec: 07 — Diseño del bytecode): errors and
    /// panics search each frame's table at the faulting `ip`; fatal and
    /// broken-pipe signals never match. Popping below `min_frames`
    /// propagates the signal to the bounded caller.
    fn unwind(&mut self, signal: Signal, min_frames: usize) -> Result<(), Signal> {
        let catchable = matches!(signal, Signal::Error(_) | Signal::Panic(_));

        loop {
            if self.task.frames.len() < min_frames {
                return Err(signal);
            }

            let frame = self.task.frames.last().expect("bounded above min_frames");
            let function = self.function(frame.func);
            let fault = CodeIx((frame.ip - 1) as u32);

            if catchable && let Some(handler) = function.chunk.handler_for(fault) {
                let floor = frame.base + function.locals as usize;
                self.task.stack.truncate(floor + handler.depth as usize);

                let caught = match signal {
                    Signal::Error(value) => Caught::Error(value),
                    Signal::Panic(panic) => Caught::Panic(panic),
                    _ => unreachable!("only catchable signals reach a handler"),
                };
                self.task.stack.push(Value::Caught(Rc::new(caught)));

                let target = handler.target.0 as usize;
                self.task.frames.last_mut().expect("frame still active").ip = target;
                return Ok(());
            }

            let frame = self.task.frames.pop().expect("bounded above min_frames");
            self.task.stack.truncate(frame.ret_base);
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
        let mark = self.task.native_roots.len();
        self.task.native_roots.extend_from_slice(values);

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
        self.task.native_roots.truncate(mark);

        if mark == 0 {
            if self.task.native_roots.capacity() > NATIVE_ROOT_FLOOR {
                self.task.native_roots.shrink_to(NATIVE_ROOT_FLOOR);
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
        let mark = self.task.native_roots.len();
        self.task.native_roots.push(recv.clone());

        let base = self.task.native_roots.len();
        self.task.native_roots.extend(snapshot);
        let end = self.task.native_roots.len();

        for ix in base..end {
            let item = self.task.native_roots[ix].clone();
            match step(self, item) {
                Ok(Some(kept)) => self.task.native_roots.push(kept),
                Ok(None) => {}
                Err(signal) => {
                    self.unroot(mark);
                    return Err(signal);
                }
            }
        }

        let kept = self.task.native_roots.split_off(end);
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
        let mark = self.task.native_roots.len();
        self.task.native_roots.push(recv.clone());

        let base = self.task.native_roots.len();
        self.task.native_roots.extend(snapshot);
        let end = self.task.native_roots.len();

        let mut found = None;
        for ix in base..end {
            let item = self.task.native_roots[ix].clone();
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
        let mark = self.task.native_roots.len();
        self.task.native_roots.push(recv.clone());

        let base = self.task.native_roots.len();
        self.task
            .native_roots
            .extend(snapshot.into_iter().flat_map(|(a, b)| [a, b]));
        let end = self.task.native_roots.len();

        for ix in (base..end).step_by(2) {
            let (left, right) = (
                self.task.native_roots[ix].clone(),
                self.task.native_roots[ix + 1].clone(),
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
        let mark = self.task.native_roots.len();
        self.task.native_roots.push(recv.clone());

        let base = self.task.native_roots.len();
        self.task.native_roots.extend(snapshot);
        let end = self.task.native_roots.len();

        for ix in base..end {
            let item = self.task.native_roots[ix].clone();
            match key_of(self, &item) {
                Ok(key) => self.task.native_roots.push(key),
                Err(signal) => {
                    self.unroot(mark);
                    return Err(signal);
                }
            }
        }

        let keys = self.task.native_roots.split_off(end);
        let items = self.task.native_roots.split_off(base);
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
        let mark = self.task.native_roots.len();
        self.task.native_roots.push(init);
        self.task.native_roots.push(recv.clone());

        let base = self.task.native_roots.len();
        self.task.native_roots.extend(snapshot);
        let end = self.task.native_roots.len();

        for ix in base..end {
            let item = self.task.native_roots[ix].clone();
            let carried = self.task.native_roots[mark].clone();
            match step(self, carried, item) {
                Ok(next) => self.task.native_roots[mark] = next,
                Err(signal) => {
                    self.unroot(mark);
                    return Err(signal);
                }
            }
        }

        let folded = self.task.native_roots[mark].clone();
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
        self.task.stack.push(value);
    }

    #[inline(always)]
    fn pop(&mut self) -> Value {
        self.task.stack.pop().expect("operand stack underflow")
    }

    fn pop_n(&mut self, n: usize) -> Vec<Value> {
        self.task.stack.split_off(self.task.stack.len() - n)
    }

    #[inline(always)]
    fn peek(&self) -> &Value {
        self.task.stack.last().expect("operand stack underflow")
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
        self.task.frames.last_mut().expect("active frame").ip = target.0 as usize;
    }

    #[inline(always)]
    fn frame_base(&self) -> usize {
        self.task.frames.last().expect("active frame").base
    }

    /// The binding cell a frame slot holds.
    ///
    /// Code generation pairs every `make_binding` with the
    /// `load_binding` / `store_binding` that read it, and a slot is
    /// never reused for a second binding, so anything else here is a
    /// broken compiler rather than a program error — reported as a
    /// fatal for the same reason an uninitialized global is.
    #[inline(always)]
    fn binding_ref(&self, slot: brasa_bytecode::SlotIx) -> Result<crate::heap::GcRef, Signal> {
        match self.task.stack[self.frame_base() + slot.0 as usize] {
            Value::Binding(r) => Ok(r),
            _ => Err(Self::fatal("brasa: slot does not hold a binding")),
        }
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
                let value = self.task.stack[self.frame_base() + slot.0 as usize].clone();
                self.push(value);
            }
            Op::StoreLocal(slot) => {
                let value = self.pop();
                let base = self.frame_base();
                self.task.stack[base + slot.0 as usize] = value;
            }
            Op::MakeBinding(slot) => {
                let value = self.pop();
                let binding = self.heap.alloc_binding(value);
                let base = self.frame_base();
                self.task.stack[base + slot.0 as usize] = binding;
            }
            Op::LoadBinding(slot) => {
                let value = self.binding_ref(slot)?;
                let value = self.heap.binding(value).borrow().clone();
                self.push(value);
            }
            Op::StoreBinding(slot) => {
                let value = self.pop();
                let binding = self.binding_ref(slot)?;
                *self.heap.binding(binding).borrow_mut() = value;
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

            Op::EqInt => self.int_cmp(|a, b| a == b)?,
            Op::LtInt => self.int_cmp(|a, b| a < b)?,
            Op::LeInt => self.int_cmp(|a, b| a <= b)?,
            Op::GtInt => self.int_cmp(|a, b| a > b)?,
            Op::GeInt => self.int_cmp(|a, b| a >= b)?,
            Op::EqFloat => self.float_cmp(|a, b| a == b)?,
            Op::LtFloat => self.float_cmp(|a, b| a < b)?,
            Op::LeFloat => self.float_cmp(|a, b| a <= b)?,
            Op::GtFloat => self.float_cmp(|a, b| a > b)?,
            Op::GeFloat => self.float_cmp(|a, b| a >= b)?,
            Op::EqBool => {
                let b = self.pop_bool().map_err(Self::bad_compare)?;
                let a = self.pop_bool().map_err(Self::bad_compare)?;
                self.push(Value::Bool(a == b));
            }

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
                let base = self.task.stack.len() - argc as usize;
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
                let frame = self.task.frames.pop().expect("ret needs an active frame");
                self.task.stack.truncate(frame.ret_base);
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

    /// A typed comparison opcode (BRS-99): the checker proved both
    /// operands, so unlike [`Vm::ordering`] there is no dynamic dispatch
    /// and no struct-`cmp` fallback to reach. The tag test stays — the
    /// same defensive posture as [`Vm::int_arith`] — but its failure arm
    /// is unreachable from checked code.
    #[inline(always)]
    fn int_cmp(&mut self, f: fn(i64, i64) -> bool) -> Result<(), Signal> {
        let b = self.pop_int().map_err(Self::bad_compare)?;
        let a = self.pop_int().map_err(Self::bad_compare)?;
        self.push(Value::Bool(f(a, b)));
        Ok(())
    }

    /// IEEE semantics fall out of the raw operator: any comparison
    /// involving NaN is `false`, and `NaN != NaN` under [`Op::EqFloat`],
    /// matching `value_eq`/`value_cmp` exactly.
    #[inline(always)]
    fn float_cmp(&mut self, f: fn(f64, f64) -> bool) -> Result<(), Signal> {
        let b = self.pop();
        let a = self.pop();
        match (a, b) {
            (Value::Float(a), Value::Float(b)) => {
                self.push(Value::Bool(f(a, b)));
                Ok(())
            }
            _ => Err(Self::fatal("brasa: invalid operands for comparison")),
        }
    }

    #[cold]
    #[inline(never)]
    fn bad_compare(_: Signal) -> Signal {
        Self::fatal("brasa: invalid operands for comparison")
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
        let base = self.task.stack.len() - argc;

        let Value::Struct(s) = self.task.stack[base] else {
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
            self.task.stack[base] = self.heap.struct_value(s).fields.borrow()[ix].clone();
            return self.call_value_op(argc - 1);
        }

        if name == "toString" && argc == 1 {
            let recv = self.pop();
            let text = self.display(&recv)?;
            self.push(Value::str(text));
            return Ok(());
        }

        self.task.stack.truncate(base);
        Err(Self::fatal(format!("brasa: unknown member `{name}`")))
    }

    /// `bind_method_dyn c`: the same lookup without calling.
    ///
    /// Methods before fields, in the same order as the call path. The
    /// two used to disagree, each mirroring one of the walker's own
    /// paths, and it never mattered: a struct's fields and its methods
    /// are ONE member namespace (spec: 06 — Diagnósticos, R006),
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
            // spec: 05 — Stdlib de scripting): a missing member, an
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
        let callee_ix = self.task.stack.len() - argc - 1;
        let callee = self.task.stack[callee_ix].clone();

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
                self.task.stack.insert(callee_ix + 1, bound.recv.clone());
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
                // captures alive across it. The callee was popped off
                // the value stack before the call, so from that pop
                // until the entered frame republishes the captures into
                // its slots, this handle is the only reference to them
                // — and a collection can run in any nested dispatch
                // loop (BRS-62). Rooting here makes that reachability
                // independent of where the safepoints happen to fall.
                //
                // A capture the callee REBINDS no longer detaches from
                // the closure the way it once did: rebinding writes
                // through the shared binding cell (BRS-106), so the
                // frame slot and the closure's capture stay the same
                // cell for the whole call, and the collector reaches
                // the new contents through it.
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
        self.call_prep(func, closure, args)?;

        // The nested loop lives on the Rust stack, so parking anywhere
        // inside it is forbidden — see `Task::reentry_depth`.
        self.task.reentry_depth += 1;
        let bounded = self.execute(self.task.frames.len());
        self.task.reentry_depth -= 1;
        bounded?;

        Ok(self.pop())
    }

    /// The frame-pushing half of [`Vm::call_frames`]: arguments on the
    /// stack, frame entered, captures written — everything but the
    /// nested loop. The scheduler starts a task through this so the
    /// block's frames belong to the task's own stack and run under the
    /// DRIVER's loop, not a nested one.
    fn call_prep(
        &mut self,
        func: FuncId,
        closure: Option<&ClosureValue>,
        args: Vec<Value>,
    ) -> Result<(), Signal> {
        let ret_base = self.task.stack.len();
        self.task.stack.extend(args);

        if let Err(signal) = self.enter_function(func, ret_base, ret_base) {
            self.task.stack.truncate(ret_base);
            return Err(signal);
        }
        if let Some(closure) = closure {
            self.write_captures(closure);
        }

        Ok(())
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
    /// do, and spec: 05 — Stdlib de scripting requires every one of them to
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
            Value::ConcurrentScope(_) => "Scope".to_string(),
            Value::Task(_) => "Task".to_string(),
            Value::Func(_) | Value::Closure(_) | Value::BoundMethod(_) | Value::BoundBuiltin(_) => {
                "function".to_string()
            }
            Value::Caught(_) | Value::Iter(_) | Value::Binding(_) => {
                unreachable!("internal values never reach nominal_tag")
            }
        }
    }
}

// --- the task scheduler (BRS-133 slice B) ------------------------------
//
// Real parking (spec: 08 — Concurrencia estructurada). The suspension
// points — scope settling and `value()` — DRIVE the scheduler: the
// current task is set aside on the drivers stack, runnable tasks are
// swapped through `Vm::task` one at a time, and a task that reaches a
// blocking builtin at reentry depth zero parks — its job goes to the
// offload pool, its frames stay in its `Task`, and the next runnable
// task takes the slot. When nothing is runnable, the VM blocks on the
// pool's condvar or the nearest sleep deadline: the event loop's idle
// state, without an event loop.
//
// Determinism: runnable tasks run in FIFO order, which is spawn order
// for a settling scope; only IO completion order is outside the
// language's control. A `value()` read runs its target first when the
// target is runnable, so code whose tasks never park observes exactly
// the pre-scheduler execution order.

/// What one blocked task is waiting for.
pub(crate) enum Wait {
    Job { id: JobId, resume: JobResume },
    Until(std::time::Instant),
}

/// How to turn a finished job back into the value or error its builtin
/// would have produced synchronously — the per-call data the sync path
/// would have kept in Rust locals across the blocking call.
pub(crate) enum JobResume {
    Http,
    Proc { strict: bool, shown: String },
}

/// What a drive loop is waiting to see settled.
#[derive(Clone, Copy)]
pub(crate) enum DriveGoal {
    Task(GcRef),
    /// Every task of the scope. With `stop_on_failure`, an unobserved
    /// task failure also ends the drive — settling reacts to it by
    /// cancelling the scope (spec: 08); the cancellation wait then
    /// drives WITHOUT it, or a failure during teardown would spin the
    /// settle loop forever.
    Scope {
        scope: GcRef,
        stop_on_failure: bool,
    },
}

/// One driver set aside while a drive loop runs on its Rust stack: its
/// execution state, the heap cell it was scheduled under (`None` for
/// the program's root task), and whether it was already cancelled.
struct Driver {
    task: Task,
    cell: Option<GcRef>,
    cancelled: bool,
}

/// One task the scheduler holds: its heap cell (as a rooted handle),
/// the scope it belongs to (what an exiting scope purges by), and its
/// swapped-out execution state.
struct ScheduledTask {
    handle: Value,
    scope: Value,
    exec: Task,
    wait: Option<Wait>,
    /// What a finished job (or a cancellation) left for the task's
    /// suspension point: a result to push, or an error to rethrow
    /// there through ordinary handler unwinding.
    resume: Option<Result<Value, Value>>,
    /// Set when the scope cancelled this task: its suspension points
    /// raise `concurrent.Cancelled` instead of blocking or parking,
    /// so it can only run toward its end (spec: 08 — cancellation is
    /// cooperative and never interrupts code between suspension
    /// points).
    cancelled: bool,
}

impl ScheduledTask {
    fn cell(&self) -> GcRef {
        let Value::Task(cell) = &self.handle else {
            unreachable!("a scheduled task's handle is a task value");
        };
        *cell
    }

    fn scope_cell(&self) -> GcRef {
        let Value::ConcurrentScope(scope) = &self.scope else {
            unreachable!("a scheduled task's scope is a scope value");
        };
        *scope
    }

    fn roots(&self) -> impl Iterator<Item = &Value> {
        let resume = self
            .resume
            .iter()
            .map(|resume| resume.as_ref().unwrap_or_else(|error| error));

        self.exec
            .roots()
            .chain([&self.handle, &self.scope])
            .chain(resume)
    }
}

#[derive(Default)]
struct SchedState {
    runnable: std::collections::VecDeque<ScheduledTask>,
    parked: Vec<ScheduledTask>,
    /// Drivers set aside while a drive loop runs on their Rust stack,
    /// innermost last. Held here rather than in drive-loop locals so
    /// safepoint rooting reaches them.
    drivers: Vec<Driver>,
    /// The heap cell of the scheduled task now occupying `Vm::task`,
    /// while a drive loop is running one.
    current_cell: Option<GcRef>,
    /// Whether the scheduled task now occupying `Vm::task` was
    /// cancelled — what suspension points consult to raise
    /// `concurrent.Cancelled` instead of blocking.
    current_cancelled: bool,
    /// How many drive loops are on the Rust stack — what arms
    /// [`Vm::can_park`]: a park without a drive loop to catch it would
    /// abandon the program.
    driving: usize,
    /// The wait the parking builtin recorded for the `Signal::Park` in
    /// flight, consumed by the drive loop when the signal arrives.
    pending_park: Option<Wait>,
    /// Monotonic failure sequence: settling rethrows the EARLIEST
    /// unobserved failure by this order (spec: 08 — occurrence order,
    /// which parking decouples from spawn order).
    failures: u64,
    pool: Option<OffloadPool>,
}

impl SchedState {
    /// Every value the collector must reach through the scheduler:
    /// each held task's stacks plus its cell and scope handles.
    fn roots(&self) -> impl Iterator<Item = &Value> {
        self.runnable
            .iter()
            .chain(self.parked.iter())
            .flat_map(ScheduledTask::roots)
            .chain(self.drivers.iter().flat_map(|driver| driver.task.roots()))
    }
}

impl<'a> Vm<'a> {
    fn sched_mut(&mut self) -> &mut SchedState {
        self.sched.get_or_insert_default()
    }

    /// Whether a suspension point may park the running task: a drive
    /// loop must be there to catch the signal, and the task must not be
    /// inside a nested Rust dispatch loop (`Task::reentry_depth`).
    /// When this is `false` the builtin blocks synchronously — same
    /// semantics, no overlap.
    pub(crate) fn can_park(&self) -> bool {
        self.task.reentry_depth == 0 && self.sched.as_ref().is_some_and(|sched| sched.driving > 0)
    }

    /// Records what the parking task waits for and answers the signal
    /// the builtin returns. Only valid when [`Vm::can_park`] held.
    pub(crate) fn park_on(&mut self, wait: Wait) -> Signal {
        let previous = self.sched_mut().pending_park.replace(wait);
        debug_assert!(previous.is_none(), "one park per suspension point");
        Signal::Park
    }

    /// Queues one blocking job on the offload pool, building the pool
    /// on the first call.
    pub(crate) fn submit_job(&mut self, job: Job) -> JobId {
        self.sched_mut().pool.get_or_insert_default().submit(job)
    }

    /// Runs scheduled tasks until `goal` settles. The suspension
    /// points call this: scope settling drives until every task in the
    /// scope settled, `value()` until its target did. Reentrant — a
    /// driven task that reaches its own suspension point drives again,
    /// with the outer task safely on the drivers stack.
    pub(crate) fn drive(&mut self, goal: DriveGoal) -> VmResult<()> {
        if self.goal_settled(goal)? {
            return Ok(());
        }

        // A cancelled task that would have to WAIT here — a `value()`
        // on an unfinished sibling, an inner scope with live tasks —
        // has reached a suspension point: it raises instead of
        // blocking (spec: 08). A read whose result is already settled
        // took the fast path above and is not a suspension point.
        self.check_cancelled()?;

        let task = std::mem::take(&mut self.task);
        let sched = self.sched_mut();
        let cell = sched.current_cell.take();
        let cancelled = std::mem::take(&mut sched.current_cancelled);
        sched.drivers.push(Driver {
            task,
            cell,
            cancelled,
        });
        sched.driving += 1;

        let outcome = self.drive_loop(goal);

        let sched = self.sched_mut();
        sched.driving -= 1;
        let driver = sched.drivers.pop().expect("drive set its driver aside");
        sched.current_cell = driver.cell;
        sched.current_cancelled = driver.cancelled;
        self.task = driver.task;

        outcome
    }

    /// Whether the task now running was cancelled by its scope: the
    /// signal every suspension point turns into `concurrent.Cancelled`.
    pub(crate) fn cancelled_now(&self) -> bool {
        self.sched
            .as_ref()
            .is_some_and(|sched| sched.current_cancelled)
    }

    /// The check every blocking suspension point runs first: a
    /// cancelled task raises `concurrent.Cancelled` there instead of
    /// blocking, parking, or registering more work (spec: 08 — the
    /// closed suspension-point list is where cancellation surfaces).
    pub(crate) fn check_cancelled(&self) -> VmResult<()> {
        if self.cancelled_now() {
            return Err(Signal::Error(Self::cancelled_value()));
        }
        Ok(())
    }

    fn drive_loop(&mut self, goal: DriveGoal) -> VmResult<()> {
        loop {
            self.absorb_completions();

            let Some(next) = self.pick_runnable(goal) else {
                self.await_progress()?;
                continue;
            };
            self.run_scheduled(next)?;

            if self.goal_settled(goal)? {
                return Ok(());
            }
        }
    }

    /// Whether the goal has settled — starting any task still pending
    /// on the way, so a settling block that spawns again is picked up
    /// by the next check. A `value()` read that finds its target
    /// running OUTSIDE the scheduler found it below itself on the Rust
    /// stack: a result that depends on itself.
    fn goal_settled(&mut self, goal: DriveGoal) -> VmResult<bool> {
        match goal {
            DriveGoal::Scope {
                scope,
                stop_on_failure,
            } => {
                let settled = self.sweep_scope(scope)?;
                if settled {
                    return Ok(true);
                }
                Ok(stop_on_failure && self.first_unobserved_failure(scope).is_some())
            }
            DriveGoal::Task(cell) => {
                if matches!(
                    &*self.heap.task(cell).borrow(),
                    TaskState::Done(_) | TaskState::Failed { .. }
                ) {
                    return Ok(true);
                }

                // The whole scope is swept, not just the target: while
                // the target is parked its pending siblings are the
                // work the scheduler should be doing. The target still
                // RUNS first (`Vm::pick_runnable`), so code whose
                // tasks never park observes the pre-scheduler order.
                if let Some(scope) = self.task_scope(cell) {
                    self.sweep_scope(scope)?;
                }

                let unsettled = self.begin_if_pending(cell)?;
                if !unsettled {
                    return Ok(true);
                }
                if self.holds_scheduled(cell) {
                    return Ok(false);
                }
                Err(Self::fatal("brasa: a task's value depends on itself"))
            }
        }
    }

    /// Starts every task of `scope` still pending, in spawn order;
    /// answers whether all of them have settled.
    fn sweep_scope(&mut self, scope: GcRef) -> VmResult<bool> {
        let mut settled = true;
        let mut ix = 0;
        loop {
            let task = match self.heap.scope(scope).borrow().tasks.get(ix) {
                Some(task) => task.clone(),
                None => break,
            };
            ix += 1;

            let Value::Task(cell) = task else {
                unreachable!("a scope holds task values only");
            };
            if self.begin_if_pending(cell)? {
                settled = false;
            }
        }
        Ok(settled)
    }

    /// The scope `cell` belongs to, when the scheduler can still tell:
    /// from its pending state, or from the scheduler entry that carries
    /// it. `None` for a task on the Rust stack — the caller's
    /// self-dependency check reports it.
    fn task_scope(&self, cell: GcRef) -> Option<GcRef> {
        if let TaskState::Pending { scope, .. } = &*self.heap.task(cell).borrow() {
            return Some(*scope);
        }

        let sched = self.sched.as_ref()?;
        sched
            .runnable
            .iter()
            .chain(sched.parked.iter())
            .find(|task| task.cell() == cell)
            .map(|task| {
                let Value::ConcurrentScope(scope) = &task.scope else {
                    unreachable!("a scheduled task's scope is a scope value");
                };
                *scope
            })
    }

    /// Starts `cell` if it is still pending; answers whether it remains
    /// unsettled afterwards.
    fn begin_if_pending(&mut self, cell: GcRef) -> VmResult<bool> {
        if matches!(&*self.heap.task(cell).borrow(), TaskState::Pending { .. }) {
            self.begin_task(cell)?;
        }
        Ok(matches!(
            &*self.heap.task(cell).borrow(),
            TaskState::Running
        ))
    }

    fn holds_scheduled(&self, cell: GcRef) -> bool {
        self.sched.as_ref().is_some_and(|sched| {
            sched
                .runnable
                .iter()
                .chain(sched.parked.iter())
                .any(|task| task.cell() == cell)
        })
    }

    /// Materializes a pending task: takes the block out, prepares its
    /// frames on a fresh execution state, and queues it runnable. A
    /// natively-callable block (a bound builtin) has no frames to
    /// schedule and runs to its outcome here.
    fn begin_task(&mut self, cell: GcRef) -> VmResult<()> {
        let (block, scope) = {
            let mut state = self.heap.task(cell).borrow_mut();
            let TaskState::Pending { .. } = &*state else {
                unreachable!("only a pending task is started");
            };
            let TaskState::Pending { block, scope } =
                std::mem::replace(&mut *state, TaskState::Running)
            else {
                unreachable!("just matched Pending");
            };
            (block, scope)
        };

        // The current task is set aside THROUGH the drivers stack, not
        // a Rust local: preparing a bound builtin can reenter compiled
        // code, and a safepoint there must still root it.
        let task = std::mem::take(&mut self.task);
        let sched = self.sched_mut();
        let saved_cell = sched.current_cell.take();
        let saved_cancelled = std::mem::take(&mut sched.current_cancelled);
        sched.drivers.push(Driver {
            task,
            cell: saved_cell,
            cancelled: saved_cancelled,
        });

        let prepared = self.prepare_block(block);

        let exec = std::mem::take(&mut self.task);
        let sched = self.sched_mut();
        let saved = sched
            .drivers
            .pop()
            .expect("begin set the current task aside");
        sched.current_cell = saved.cell;
        sched.current_cancelled = saved.cancelled;
        self.task = saved.task;

        match prepared {
            Ok(None) => {
                self.sched_mut().runnable.push_back(ScheduledTask {
                    handle: Value::Task(cell),
                    scope: Value::ConcurrentScope(scope),
                    exec,
                    wait: None,
                    resume: None,
                    cancelled: false,
                });
                Ok(())
            }
            Ok(Some(value)) => {
                *self.heap.task(cell).borrow_mut() = TaskState::Done(value);
                Ok(())
            }
            Err(Signal::Error(error)) => {
                self.settle_failed(cell, error);
                Ok(())
            }
            Err(other) => {
                // Same rule as a task dying mid-run in
                // `Vm::run_scheduled`: the cell must not stay
                // `Running` with nothing behind it.
                self.settle_cancelled(cell);
                Err(other)
            }
        }
    }

    /// Caches one task failure with the next occurrence order.
    fn settle_failed(&mut self, cell: GcRef, error: Value) {
        let sched = self.sched_mut();
        let order = sched.failures;
        sched.failures += 1;

        *self.heap.task(cell).borrow_mut() = TaskState::Failed {
            error,
            observed: false,
            order,
        };
    }

    /// [`Vm::call_callable`]'s frame-preparing twin for a spawned
    /// block: same callable shapes, same arity messages, but the frames
    /// go onto the (fresh) current task without a nested loop.
    /// `Some(value)` means the block was native and already ran.
    fn prepare_block(&mut self, block: Value) -> VmResult<Option<Value>> {
        match block {
            Value::Func(func) => {
                let function = self.function(func);
                if function.arity != 0 {
                    return Err(Self::fatal(format!(
                        "brasa: `{}` takes {} argument(s), found 0",
                        function.name, function.arity
                    )));
                }
                self.call_prep(func, None, Vec::new())?;
                Ok(None)
            }
            Value::Closure(closure) => {
                let function = self.function(closure.func);
                if function.arity != 0 {
                    return Err(Self::fatal(format!(
                        "brasa: lambda takes {} argument(s), found 0",
                        function.arity
                    )));
                }
                // No rooting needed between here and the enqueue: the
                // captures are written straight into the fresh task's
                // slots and nothing in between can collect.
                self.call_prep(closure.func, Some(&closure), Vec::new())?;
                Ok(None)
            }
            Value::BoundMethod(bound) => {
                let function = self.function(bound.func);
                let expected = (function.arity as usize).saturating_sub(1);
                if expected != 0 {
                    return Err(Self::fatal(format!(
                        "brasa: `{}` takes {expected} argument(s), found 0",
                        function.name
                    )));
                }
                self.call_prep(bound.func, None, vec![bound.recv.clone()])?;
                Ok(None)
            }
            Value::BoundBuiltin(bound) => {
                // A native block has no frames to park, so any
                // suspension point it reaches must take its synchronous
                // form — the same depth guard nested loops use.
                let args = vec![bound.recv.clone()];
                self.task.reentry_depth += 1;
                let outcome = self.builtin_with_args(bound.builtin, args);
                self.task.reentry_depth -= 1;
                outcome.map(Some)
            }
            _ => Err(Self::fatal("brasa: value is not callable")),
        }
    }

    /// Runs the target first when a `value()` read is driving and its
    /// target is runnable; FIFO otherwise.
    fn pick_runnable(&mut self, goal: DriveGoal) -> Option<ScheduledTask> {
        let sched = self.sched.as_mut()?;

        if let DriveGoal::Task(target) = goal
            && let Some(ix) = sched.runnable.iter().position(|task| task.cell() == target)
        {
            return sched.runnable.remove(ix);
        }

        sched.runnable.pop_front()
    }

    /// Swaps one runnable task into `Vm::task` and runs it to its next
    /// boundary: settled (its outcome cached on its cell), parked (its
    /// state moved to the parked list), or a non-error signal that
    /// propagates out of the whole scope.
    fn run_scheduled(&mut self, mut next: ScheduledTask) -> VmResult<()> {
        let cell = next.cell();
        let resume = next.resume.take();

        debug_assert!(
            self.task.frames.is_empty(),
            "the drive loop owns an empty running slot"
        );
        self.task = std::mem::take(&mut next.exec);
        let sched = self.sched_mut();
        sched.current_cell = Some(cell);
        sched.current_cancelled = next.cancelled;

        // The suspension point the task stopped at gets what its wait
        // produced: its result pushed as if the builtin returned, or
        // its error — a failed job, or the scope's cancellation —
        // rethrown through the ordinary handler search. The frame's ip
        // is still one past the blocking call, so a `catch` around it
        // works exactly as it does on the synchronous path.
        let resumed = match resume {
            Some(Ok(value)) => {
                self.push(value);
                Ok(())
            }
            Some(Err(error)) => self.unwind(Signal::Error(error), 1),
            None => Ok(()),
        };
        let outcome = resumed.and_then(|()| self.execute(1));

        let sched = self.sched_mut();
        sched.current_cell = None;
        sched.current_cancelled = false;

        match outcome {
            Ok(()) => {
                let value = self.pop();
                self.task = Task::default();
                *self.heap.task(cell).borrow_mut() = TaskState::Done(value);
                Ok(())
            }
            Err(Signal::Park) => {
                debug_assert!(
                    !next.cancelled,
                    "a cancelled task's suspension points raise"
                );
                let wait = self
                    .sched_mut()
                    .pending_park
                    .take()
                    .expect("a park recorded its wait");
                next.exec = std::mem::take(&mut self.task);
                next.wait = Some(wait);
                self.sched_mut().parked.push(next);
                Ok(())
            }
            Err(Signal::Error(error)) => {
                self.task = Task::default();
                self.settle_failed(cell, error);
                Ok(())
            }
            Err(other) => {
                // The task died mid-run with a propagating signal (a
                // panic, an exit). Its cell settles as cancelled so
                // the scope's teardown wait sees it finished, and a
                // read through a leaked handle answers `Cancelled`
                // rather than a bogus self-dependency report.
                self.task = Task::default();
                self.settle_cancelled(cell);
                Err(other)
            }
        }
    }

    /// Moves every task whose wait is over back to the runnable queue:
    /// expired sleep deadlines (in park order), then finished jobs (in
    /// completion order), each with its result pushed — or its error
    /// recorded — exactly as its builtin would have left it. A
    /// completion no parked task claims belonged to a purged scope and
    /// is discarded: cancellation is cooperative, never an abort.
    fn absorb_completions(&mut self) {
        let Some(sched) = self.sched.as_mut() else {
            return;
        };
        let now = std::time::Instant::now();

        let mut ix = 0;
        while ix < sched.parked.len() {
            let expired = matches!(sched.parked[ix].wait, Some(Wait::Until(at)) if at <= now);
            if !expired {
                ix += 1;
                continue;
            }

            let mut task = sched.parked.remove(ix);
            task.wait = None;
            task.resume = Some(Ok(Value::Unit));
            sched.runnable.push_back(task);
        }

        let Some(pool) = sched.pool.as_mut() else {
            return;
        };
        for (id, outcome) in pool.drain_completions() {
            let position = sched.parked.iter().position(
                |task| matches!(&task.wait, Some(Wait::Job { id: waiting, .. }) if *waiting == id),
            );
            let Some(position) = position else {
                continue;
            };

            let mut task = sched.parked.remove(position);
            let Some(Wait::Job { resume, .. }) = task.wait.take() else {
                unreachable!("matched a job wait above");
            };

            task.resume = Some(Self::job_value(&resume, outcome));
            sched.runnable.push_back(task);
        }
    }

    /// Blocks until something parked can make progress: the pool's
    /// condvar when jobs are in flight (bounded by the nearest sleep
    /// deadline), a plain sleep when only timers remain. This is the
    /// scheduler's idle state.
    fn await_progress(&mut self) -> VmResult<()> {
        let sched = self.sched_mut();
        if sched.parked.is_empty() {
            return Err(Self::fatal(
                "brasa: the scheduler has nothing runnable and nothing parked",
            ));
        }

        let now = std::time::Instant::now();
        let mut deadline: Option<std::time::Instant> = None;
        let mut jobs = false;
        for parked in &sched.parked {
            match &parked.wait {
                Some(Wait::Until(at)) => {
                    if *at <= now {
                        return Ok(());
                    }
                    deadline = Some(deadline.map_or(*at, |d| d.min(*at)));
                }
                Some(Wait::Job { .. }) => jobs = true,
                None => unreachable!("a parked task recorded its wait"),
            }
        }

        if jobs {
            let pool = sched.pool.as_ref().expect("a parked job implies the pool");
            pool.wait_for_completion(deadline);
        } else {
            let deadline = deadline.expect("a parked task waits on a job or a deadline");
            std::thread::sleep(deadline.saturating_duration_since(now));
        }
        Ok(())
    }

    /// Forgets every scheduler entry belonging to `scope`, called when
    /// the scope exits — with its tasks settled on the ordinary path,
    /// or abandoned mid-park when a panic tore the scope down. An
    /// abandoned task's job completes in the pool and is discarded on
    /// arrival.
    pub(crate) fn purge_scope(&mut self, scope: GcRef) {
        let Some(sched) = self.sched.as_mut() else {
            return;
        };

        let foreign =
            |task: &ScheduledTask| !matches!(&task.scope, Value::ConcurrentScope(s) if *s == scope);
        sched.runnable.retain(foreign);
        sched.parked.retain(foreign);
    }

    // --- cancellation (BRS-133 slice C) --------------------------------

    /// The `concurrent.Cancelled` error value a cancelled task's
    /// suspension points raise.
    pub(crate) fn cancelled_value() -> Value {
        Value::NativeError(Rc::new(crate::value::NativeErrorValue {
            name: brasa_stdlib::concurrent::CANCELLED,
            message: Rc::from("the task was cancelled: its scope is unwinding"),
        }))
    }

    /// Cancels every unsettled task of `scope` (spec: 08 — Concurrencia
    /// estructurada). Cooperative, delivered at suspension points:
    ///
    /// - Never-started tasks (pending, or queued without having run an
    ///   instruction) settle as `Failed(Cancelled)` without running —
    ///   there is no suspension point inside them to deliver at.
    /// - Parked and resumed-but-not-yet-run tasks are still AT their
    ///   suspension point: they wake with `Cancelled` rethrown there,
    ///   run whatever handlers they have, and every later suspension
    ///   point raises again. A parked task's in-flight job is orphaned;
    ///   its completion is discarded on arrival, never aborted
    ///   mid-socket.
    ///
    /// Cancellation outcomes are settled `observed`, so the failure
    /// that triggered the teardown — not a sibling's `Cancelled` — is
    /// what the scope rethrows.
    fn cancel_scope(&mut self, scope: GcRef) {
        // Pending tasks first: settling them keeps the scope sweeps
        // from ever starting them.
        let mut ix = 0;
        loop {
            let task = match self.heap.scope(scope).borrow().tasks.get(ix) {
                Some(task) => task.clone(),
                None => break,
            };
            ix += 1;

            let Value::Task(cell) = task else {
                unreachable!("a scope holds task values only");
            };
            if matches!(&*self.heap.task(cell).borrow(), TaskState::Pending { .. }) {
                self.settle_cancelled(cell);
            }
        }

        let Some(sched) = self.sched.as_mut() else {
            return;
        };

        let mut fresh = Vec::new();
        let owned = |task: &ScheduledTask| task.scope_cell() == scope;

        // A queued task that never ran and holds no resume has produced
        // no side effect: dropping it is indistinguishable from never
        // starting it. Everything else gets `Cancelled` at the
        // suspension point its resume applies to.
        sched.runnable.retain_mut(|task| {
            if !owned(task) {
                return true;
            }
            if task.resume.is_none() {
                fresh.push(task.cell());
                return false;
            }
            task.resume = Some(Err(Self::cancelled_value()));
            task.cancelled = true;
            true
        });

        let mut parked_ix = 0;
        while parked_ix < sched.parked.len() {
            if !owned(&sched.parked[parked_ix]) {
                parked_ix += 1;
                continue;
            }

            let mut task = sched.parked.remove(parked_ix);
            task.wait = None;
            task.resume = Some(Err(Self::cancelled_value()));
            task.cancelled = true;
            sched.runnable.push_back(task);
        }

        for cell in fresh {
            self.settle_cancelled(cell);
        }
    }

    /// Settles a task that will never run (or run further) as failed
    /// with `Cancelled`, already observed: a later `value()` read
    /// through a leaked handle rethrows it, but scope settling never
    /// reports it over the failure that caused the teardown.
    fn settle_cancelled(&mut self, cell: GcRef) {
        let sched = self.sched_mut();
        let order = sched.failures;
        sched.failures += 1;

        *self.heap.task(cell).borrow_mut() = TaskState::Failed {
            error: Self::cancelled_value(),
            observed: true,
            order,
        };
    }

    /// The earliest unobserved failure among `scope`'s tasks, by
    /// occurrence order — the failure scope settling reacts to.
    fn first_unobserved_failure(&self, scope: GcRef) -> Option<GcRef> {
        let tasks = self.heap.scope(scope).borrow().tasks.clone();

        let mut found: Option<(u64, GcRef)> = None;
        for task in &tasks {
            let Value::Task(cell) = task else {
                unreachable!("a scope holds task values only");
            };
            if let TaskState::Failed {
                observed: false,
                order,
                ..
            } = &*self.heap.task(*cell).borrow()
                && found.is_none_or(|(first, _)| *order < first)
            {
                found = Some((*order, *cell));
            }
        }

        found.map(|(_, cell)| cell)
    }

    /// Marks the failure that triggered a teardown observed and answers
    /// the error to rethrow after the cancelled tasks finish.
    fn take_failure(&mut self, cell: GcRef) -> Value {
        let mut state = self.heap.task(cell).borrow_mut();
        let TaskState::Failed {
            error, observed, ..
        } = &mut *state
        else {
            unreachable!("the trigger was found failed");
        };
        *observed = true;
        error.clone()
    }

    /// Cancels `scope`'s live tasks and drives until every one of them
    /// settled: nothing outlives the scope, not even a task busy
    /// cancelling. Failures during the teardown settle into their
    /// cells and are dropped silently — the caller rethrows the
    /// failure that started it; a panic (or any other non-error
    /// signal) in a cancelled task's cleanup still propagates from
    /// here and supersedes it.
    pub(crate) fn cancel_and_settle(&mut self, scope: GcRef) -> VmResult<()> {
        self.cancel_scope(scope);
        self.drive(DriveGoal::Scope {
            scope,
            stop_on_failure: false,
        })
    }

    /// Scope settling's drive (spec: 08): runs every task of `scope`
    /// to settlement. The FIRST unobserved failure — earliest by
    /// occurrence, whether it happened before settling or during it —
    /// cancels the scope's remaining live tasks, waits for them to
    /// finish cancelling, and comes back as the error to rethrow.
    pub(crate) fn drive_settling(&mut self, scope: GcRef) -> VmResult<Option<Value>> {
        if self.first_unobserved_failure(scope).is_none() {
            self.drive(DriveGoal::Scope {
                scope,
                stop_on_failure: true,
            })?;
        }

        let Some(trigger) = self.first_unobserved_failure(scope) else {
            return Ok(None);
        };

        let error = self.take_failure(trigger);
        self.cancel_and_settle(scope)?;
        Ok(Some(error))
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

    /// Compiles `source` through the frontend and runs it on a directly
    /// constructed `Vm`, answering the scheduler and pool fields the
    /// integration harness cannot see.
    fn run_and_inspect(source: &str) -> (bool, bool) {
        let mut sources = brasa_source::SourceMap::new();
        let file = sources.add_file("guard.bras", source.to_string());
        let parsed = brasa_parser::parse(source, file);
        assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
        let lowered = brasa_hir::lower(&parsed.ast, &parsed.roots);
        assert!(lowered.diagnostics.is_empty(), "{:?}", lowered.diagnostics);
        let resolved = brasa_resolver::resolve(&lowered.hir, &lowered.roots);
        assert!(
            resolved.diagnostics.is_empty(),
            "{:?}",
            resolved.diagnostics
        );
        let checked = brasa_typeck::check(
            &lowered.hir,
            &lowered.roots,
            &resolved.resolutions,
            &lowered.sugar_origins,
        );
        assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
        let compiled = brasa_codegen::compile(
            &lowered.hir,
            &lowered.roots,
            &resolved.resolutions,
            &checked.types,
        );
        assert!(
            compiled.diagnostics.is_empty(),
            "{:?}",
            compiled.diagnostics
        );

        let mut out = Vec::new();
        let mut err = Vec::new();
        let mut input: &[u8] = b"";
        let mut vm = Vm::new(
            &compiled.module,
            brasa_runtime::Streams {
                out: &mut out,
                err: &mut err,
                input: &mut input,
            },
            brasa_runtime::DEFAULT_MAX_CALL_DEPTH,
            crate::heap::DEFAULT_GC_BUDGET_BYTES,
            &[],
        );
        vm.run_program().expect("the guard programs run cleanly");

        let sched = vm.sched.is_some();
        let pool = vm.sched.as_ref().is_some_and(|sched| sched.pool.is_some());
        (sched, pool)
    }

    /// The lazy-initialization guard (BRS-134): nothing of the
    /// scheduler exists before the first task starts, and nothing of
    /// the offload pool before the first park submits a job — a run
    /// that never spawns must not pay for either, and cold start must
    /// stay unmoved.
    #[test]
    fn scheduler_and_pool_are_not_built_before_first_use() {
        let (sched, pool) = run_and_inspect("puts 1 + 1\n");
        assert!(!sched, "a plain run must not build the scheduler");
        assert!(!pool);

        let (sched, pool) =
            run_and_inspect("let n = concurrent do |scope|\n  40 + 2\nend\nputs n\n");
        assert!(
            !sched,
            "a scope with no spawns must not build the scheduler"
        );
        assert!(!pool);

        let (sched, pool) = run_and_inspect(
            "let n = concurrent do |scope|\n  let t = scope.spawn do 42 end\n  t.value()\nend\nputs n\n",
        );
        assert!(sched, "a started task lives in the scheduler");
        assert!(!pool, "a task that never parks must not build the pool");
    }
}
