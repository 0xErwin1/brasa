//! The sampling profiler (BRS-121): where a run spends its time.
//!
//! Split from the debugger on purpose. "Which instructions are hot" and
//! "stop here and show me the locals" sound adjacent and share almost
//! nothing: one pauses an exact instruction and must be exact, the
//! other samples on a timer and must be statistically fair.
//!
//! # Why sampling, and why it does not cost the hot loop
//!
//! Counting every instruction would be the wrong instrument twice: it
//! perturbs the measurement it takes, and it costs in the dispatch loop
//! that was tightened precisely to be tight.
//!
//! So a profiled run uses the instrumented loop — the same cold path a
//! debug session uses ([`crate::vm::Vm::execute_instrumented`]) — and
//! the ordinary loop is untouched. The check per instruction is a clock
//! comparison, which is affordable there and absent everywhere else.
//!
//! The SAMPLES are time-based, not instruction-based, so what comes out
//! is a fair distribution over wall time rather than a count weighted
//! by how many instructions a construct happens to compile to.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use brasa_bytecode::FuncId;

/// How often the stack is sampled. Small enough that a script of a few
/// hundred milliseconds still yields a usable distribution, large
/// enough that the clock read is not the thing being measured.
pub const DEFAULT_INTERVAL: Duration = Duration::from_micros(500);

/// Collected while a profiled run executes.
pub(crate) struct Profiler {
    interval: Duration,
    last: Instant,
    /// One entry per sample: the frame stack, outermost first.
    stacks: Vec<Vec<FuncId>>,
    /// Time spent inside the collector, measured separately.
    gc: Duration,
    started: Instant,
}

impl Profiler {
    pub(crate) fn new(interval: Duration) -> Profiler {
        Profiler {
            interval,
            last: Instant::now(),
            stacks: Vec::new(),
            gc: Duration::ZERO,
            started: Instant::now(),
        }
    }

    /// Records a sample if the interval has elapsed.
    pub(crate) fn maybe_sample(&mut self, stack: impl Fn() -> Vec<FuncId>) {
        if self.last.elapsed() < self.interval {
            return;
        }

        self.last = Instant::now();
        self.stacks.push(stack());
    }

    pub(crate) fn add_gc(&mut self, elapsed: Duration) {
        self.gc += elapsed;
    }

    pub(crate) fn finish(self, names: &[String]) -> Profile {
        let total = self.stacks.len();

        let mut self_samples: HashMap<FuncId, usize> = HashMap::new();
        let mut total_samples: HashMap<FuncId, usize> = HashMap::new();
        let mut paths: HashMap<Vec<FuncId>, usize> = HashMap::new();

        for stack in &self.stacks {
            if let Some(innermost) = stack.last() {
                *self_samples.entry(*innermost).or_default() += 1;
            }

            // A function appears once per stack however many frames it
            // has there: a recursive call would otherwise count its own
            // depth as time spent.
            let mut seen: Vec<FuncId> = Vec::new();
            for func in stack {
                if !seen.contains(func) {
                    seen.push(*func);
                    *total_samples.entry(*func).or_default() += 1;
                }
            }

            *paths.entry(stack.clone()).or_default() += 1;
        }

        let name_of = |func: FuncId| {
            names
                .get(func.0 as usize)
                .cloned()
                .unwrap_or_else(|| format!("<fn {}>", func.0))
        };

        let mut functions: Vec<FunctionProfile> = total_samples
            .keys()
            .map(|func| FunctionProfile {
                name: name_of(*func),
                self_samples: self_samples.get(func).copied().unwrap_or(0),
                total_samples: total_samples[func],
            })
            .collect();

        // Self time first: that is what a reader is looking for, and
        // total time is dominated by whatever `main` calls.
        functions.sort_by(|a, b| {
            b.self_samples
                .cmp(&a.self_samples)
                .then_with(|| b.total_samples.cmp(&a.total_samples))
                .then_with(|| a.name.cmp(&b.name))
        });

        let mut call_paths: Vec<CallPath> = paths
            .into_iter()
            .map(|(stack, samples)| CallPath {
                frames: stack.iter().map(|func| name_of(*func)).collect(),
                samples,
            })
            .collect();
        call_paths.sort_by(|a, b| {
            b.samples
                .cmp(&a.samples)
                .then_with(|| a.frames.join(";").cmp(&b.frames.join(";")))
        });

        Profile {
            samples: total,
            elapsed: self.started.elapsed(),
            gc: self.gc,
            functions,
            call_paths,
        }
    }
}

/// What one profiled run measured.
pub struct Profile {
    pub samples: usize,
    pub elapsed: Duration,
    /// Time inside the collector. Reported apart from interpreted time
    /// because a script that is slow because of the collector and one
    /// that is slow because of its own loop want different fixes, and a
    /// single total hides which it is.
    pub gc: Duration,
    /// By function, self-time first.
    pub functions: Vec<FunctionProfile>,
    /// Whole stacks, hottest first.
    pub call_paths: Vec<CallPath>,
}

pub struct FunctionProfile {
    pub name: String,
    /// Samples where this function was the innermost frame — its own
    /// work, not its callees'.
    pub self_samples: usize,
    /// Samples where it was anywhere on the stack.
    pub total_samples: usize,
}

pub struct CallPath {
    /// Outermost first, as a flamegraph reads.
    pub frames: Vec<String>,
    pub samples: usize,
}

impl Profile {
    /// The collapsed-stack format the existing flamegraph tooling eats,
    /// so this ships an instrument rather than a viewer.
    pub fn collapsed(&self) -> String {
        self.call_paths
            .iter()
            .map(|path| format!("{} {}", path.frames.join(";"), path.samples))
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn percent(&self, samples: usize) -> f64 {
        if self.samples == 0 {
            return 0.0;
        }
        samples as f64 * 100.0 / self.samples as f64
    }

    /// A path with consecutive repeats folded: `fib (x18)`.
    ///
    /// For the REPORT only. Recursion turns a stack into the same name
    /// twenty times, and ten near-identical rows of it is noise rather
    /// than an answer. The collapsed output keeps the raw frames,
    /// because flamegraph tooling folds recursion its own way and a
    /// pre-folded stack would lie to it.
    fn fold(frames: &[String]) -> String {
        let mut out: Vec<String> = Vec::new();
        let mut run: Option<(&str, usize)> = None;

        for frame in frames {
            match run {
                Some((name, count)) if name == frame => run = Some((name, count + 1)),
                Some((name, count)) => {
                    out.push(Self::run_label(name, count));
                    run = Some((frame, 1));
                }
                None => run = Some((frame, 1)),
            }
        }
        if let Some((name, count)) = run {
            out.push(Self::run_label(name, count));
        }

        out.join(" -> ")
    }

    fn run_label(name: &str, count: usize) -> String {
        if count == 1 {
            name.to_string()
        } else {
            format!("{name} (x{count})")
        }
    }

    /// The flat table plus the hot paths.
    pub fn report(&self) -> String {
        if self.samples == 0 {
            return "no samples: the program finished faster than the sampling interval"
                .to_string();
        }

        let mut out = format!(
            "{} samples over {:.1?} — {:.1?} in the collector ({:.1}%)\n\n",
            self.samples,
            self.elapsed,
            self.gc,
            self.gc.as_secs_f64() * 100.0 / self.elapsed.as_secs_f64().max(f64::EPSILON),
        );

        out.push_str("  self%   total%  function\n");
        for function in &self.functions {
            out.push_str(&format!(
                "  {:>5.1}   {:>6.1}  {}\n",
                self.percent(function.self_samples),
                self.percent(function.total_samples),
                function.name,
            ));
        }

        out.push_str("\nhot call paths:\n");
        for path in self.call_paths.iter().take(10) {
            out.push_str(&format!(
                "  {:>5.1}%  {}\n",
                self.percent(path.samples),
                Self::fold(&path.frames),
            ));
        }

        out.trim_end().to_string()
    }
}
