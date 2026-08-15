//! The blocking-IO offload pool behind task parking
//! (spec: 08 — Concurrencia estructurada, BRS-133).
//!
//! Decisions recorded here (mirrored in the spec):
//!
//! - **Plain data only.** A job carries `String`s and maps, an outcome
//!   carries the glue crates' raw records — no language value ever
//!   crosses a thread, the same boundary [`crate::proc_env::run_all`]
//!   drew first. The VM converts on its own thread, before submitting
//!   and after collecting.
//! - **std only.** A `Mutex` + two `Condvar`s are the whole
//!   synchronization story; a channel crate would be a dependency for
//!   what fifteen lines of std express.
//! - **Nothing initializes before the first job.** The pool is built by
//!   the first park, so a script that never parks never pays for it —
//!   the same lazy rule the TLS stack follows in `http_glue`.
//! - **Workers spawn on demand** up to [`WORKER_CAP`]. The jobs are
//!   IO-bound waits, not CPU work, so the cap is not the machine's
//!   parallelism: a script that parks 30 tasks on 30 requests wants 30
//!   sockets in flight, not `nproc`.
//! - **Dropping the pool abandons in-flight jobs.** Shutdown flips a
//!   flag and notifies; workers exit when they next look for work, and
//!   nobody joins them — joining could hold process exit hostage to a
//!   request timeout. Their results are discarded (cooperative
//!   cancellation: the request is never aborted mid-socket).

use std::collections::HashMap;
use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Instant;

use crate::http_glue;
use crate::proc_env;

/// The most workers the pool ever spawns. Each one is a blocked wait on
/// a socket or a child process, so the bound exists to cap fan-out
/// mistakes, not to match cores.
const WORKER_CAP: usize = 64;

/// Identifies one submitted job across the thread boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JobId(u64);

/// One blocking operation, as plain data.
pub enum Job {
    HttpGet {
        url: String,
        headers: HashMap<String, String>,
        timeout_ms: Option<i64>,
    },
    HttpPost {
        url: String,
        body: String,
        headers: HashMap<String, String>,
        timeout_ms: Option<i64>,
    },
    ProcRun {
        argv: Vec<String>,
        stdin: Option<String>,
        overlay: HashMap<String, String>,
    },
}

/// What one finished job observed, as plain data.
pub enum JobOutcome {
    Http(Result<http_glue::RawResponse, String>),
    Proc(Result<proc_env::RawOutput, String>),
}

fn run_job(job: Job) -> JobOutcome {
    match job {
        Job::HttpGet {
            url,
            headers,
            timeout_ms,
        } => JobOutcome::Http(http_glue::get(&url, &headers, timeout_ms)),
        Job::HttpPost {
            url,
            body,
            headers,
            timeout_ms,
        } => JobOutcome::Http(http_glue::post(&url, &body, &headers, timeout_ms)),
        Job::ProcRun {
            argv,
            stdin,
            overlay,
        } => JobOutcome::Proc(proc_env::run_command(&argv, stdin.as_deref(), &overlay)),
    }
}

struct State {
    queue: VecDeque<(JobId, Job)>,
    done: Vec<(JobId, JobOutcome)>,
    /// Workers currently blocked on `work_ready` — what `submit`
    /// consults to spawn a new worker only when none would pick the
    /// job up.
    idle: usize,
    workers: usize,
    shutdown: bool,
}

struct Shared {
    state: Mutex<State>,
    work_ready: Condvar,
    done_ready: Condvar,
}

/// The pool. One per VM, built lazily by the first park.
pub struct OffloadPool {
    shared: Arc<Shared>,
    next_job: u64,
}

impl OffloadPool {
    pub fn new() -> OffloadPool {
        OffloadPool {
            shared: Arc::new(Shared {
                state: Mutex::new(State {
                    queue: VecDeque::new(),
                    done: Vec::new(),
                    idle: 0,
                    workers: 0,
                    shutdown: false,
                }),
                work_ready: Condvar::new(),
                done_ready: Condvar::new(),
            }),
            next_job: 0,
        }
    }

    /// Queues one job and answers its id; a worker is spawned when no
    /// idle one would take it and the cap allows another.
    pub fn submit(&mut self, job: Job) -> JobId {
        let id = JobId(self.next_job);
        self.next_job += 1;

        let spawn_worker = {
            let mut state = self.shared.state.lock().expect("a pool worker panicked");
            state.queue.push_back((id, job));

            let starved = state.idle == 0 && state.workers < WORKER_CAP;
            if starved {
                state.workers += 1;
            }
            starved
        };
        self.shared.work_ready.notify_one();

        if spawn_worker {
            let shared = Arc::clone(&self.shared);
            std::thread::Builder::new()
                .name("brasa-offload".to_string())
                .spawn(move || worker_loop(&shared))
                .expect("cannot spawn an offload worker");
        }

        id
    }

    /// Every job finished so far, in completion order. Never blocks.
    pub fn drain_completions(&mut self) -> Vec<(JobId, JobOutcome)> {
        let mut state = self.shared.state.lock().expect("a pool worker panicked");
        std::mem::take(&mut state.done)
    }

    /// Blocks until at least one job finishes or `deadline` passes;
    /// `None` waits without a bound. The completions themselves come
    /// from the next [`OffloadPool::drain_completions`] call.
    pub fn wait_for_completion(&self, deadline: Option<Instant>) {
        let mut state = self.shared.state.lock().expect("a pool worker panicked");

        while state.done.is_empty() {
            match deadline {
                None => {
                    state = self
                        .shared
                        .done_ready
                        .wait(state)
                        .expect("a pool worker panicked");
                }
                Some(deadline) => {
                    let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                        return;
                    };
                    let (guard, _) = self
                        .shared
                        .done_ready
                        .wait_timeout(state, remaining)
                        .expect("a pool worker panicked");
                    state = guard;
                }
            }
        }
    }
}

impl Default for OffloadPool {
    fn default() -> OffloadPool {
        OffloadPool::new()
    }
}

impl Drop for OffloadPool {
    fn drop(&mut self) {
        let mut state = self.shared.state.lock().expect("a pool worker panicked");
        state.shutdown = true;
        state.queue.clear();
        drop(state);

        self.shared.work_ready.notify_all();
    }
}

fn worker_loop(shared: &Shared) {
    loop {
        let job = {
            let mut state = shared.state.lock().expect("a pool worker panicked");

            loop {
                if state.shutdown {
                    state.workers -= 1;
                    return;
                }
                if let Some(job) = state.queue.pop_front() {
                    break job;
                }

                state.idle += 1;
                state = shared
                    .work_ready
                    .wait(state)
                    .expect("the pool owner panicked");
                state.idle -= 1;
            }
        };

        let (id, job) = job;
        let outcome = run_job(job);

        let mut state = shared.state.lock().expect("a pool worker panicked");
        if state.shutdown {
            state.workers -= 1;
            return;
        }
        state.done.push((id, outcome));
        drop(state);

        shared.done_ready.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn echo_job(text: &str) -> Job {
        Job::ProcRun {
            argv: vec!["sh".to_string(), "-c".to_string(), format!("echo {text}")],
            stdin: None,
            overlay: HashMap::new(),
        }
    }

    #[test]
    fn a_submitted_job_completes_and_is_drained_once() {
        let mut pool = OffloadPool::new();
        let id = pool.submit(echo_job("hello"));

        let mut drained = pool.drain_completions();
        while drained.is_empty() {
            pool.wait_for_completion(None);
            drained = pool.drain_completions();
        }

        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0].0, id);
        let JobOutcome::Proc(Ok(output)) = &drained[0].1 else {
            panic!("echo must succeed");
        };
        assert_eq!(output.stdout.trim(), "hello");

        assert!(pool.drain_completions().is_empty());
    }

    #[test]
    fn independent_jobs_overlap() {
        let mut pool = OffloadPool::new();

        let sleep = |secs: &str| Job::ProcRun {
            argv: vec!["sleep".to_string(), secs.to_string()],
            stdin: None,
            overlay: HashMap::new(),
        };

        let started = Instant::now();
        for _ in 0..4 {
            pool.submit(sleep("0.2"));
        }

        let mut seen = 0;
        while seen < 4 {
            pool.wait_for_completion(None);
            seen += pool.drain_completions().len();
        }

        assert!(
            started.elapsed() < std::time::Duration::from_millis(600),
            "four 0.2s jobs must overlap: {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn a_deadline_bounds_the_wait_when_nothing_completes() {
        let pool = OffloadPool::new();

        let started = Instant::now();
        pool.wait_for_completion(Some(Instant::now() + std::time::Duration::from_millis(50)));

        assert!(started.elapsed() < std::time::Duration::from_millis(500));
    }
}
