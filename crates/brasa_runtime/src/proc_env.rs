//! Backend-agnostic OS glue for `std::proc` and `std::env` (BRS-32,
//! spec: 05 — Stdlib de scripting), shared by the walker and the VM so the
//! process-spawning behavior and every observable message can never
//! drift between backends. Value construction stays in each backend's
//! own builtin table, like the rest of the stdlib.

use std::collections::HashMap;
use std::io::Write;

/// Everything observed from one finished child process.
pub struct RawOutput {
    pub stdout: String,
    pub stderr: String,
    pub code: i64,
}

/// The `/bin/sh -c` argv for `proc.shell` (spec: 05 — Stdlib de scripting:
/// the explicit opt-in to shell interpretation).
pub fn shell_argv(line: &str) -> Vec<String> {
    vec!["/bin/sh".to_string(), "-c".to_string(), line.to_string()]
}

/// Spawns `argv[0]` with the remaining arguments, the parent
/// environment plus `overlay`, and optional piped stdin; captures both
/// output streams fully (decoded as lossy UTF-8). `Err` carries the
/// `proc.SpawnError` message. Stdin is written from a helper thread so
/// a child that fills its output pipes before draining stdin can never
/// deadlock the run.
pub fn run_command(
    argv: &[String],
    stdin: Option<&str>,
    overlay: &HashMap<String, String>,
) -> Result<RawOutput, String> {
    let [program, rest @ ..] = argv else {
        return Err("empty command".to_string());
    };

    let mut command = std::process::Command::new(program);
    command
        .args(rest)
        .envs(overlay)
        .stdin(if stdin.is_some() {
            std::process::Stdio::piped()
        } else {
            std::process::Stdio::null()
        })
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    let mut child = command
        .spawn()
        .map_err(|err| format!("cannot run `{program}`: {err}"))?;

    let writer = stdin.map(|text| {
        let mut handle = child.stdin.take().expect("stdin was requested piped");
        let text = text.to_string();
        std::thread::spawn(move || {
            // A child that never reads its stdin closes the pipe; the
            // resulting write error is not a failure of the run.
            let _ = handle.write_all(text.as_bytes());
        })
    });

    let output = child
        .wait_with_output()
        .map_err(|err| format!("cannot run `{program}`: {err}"))?;
    if let Some(writer) = writer {
        let _ = writer.join();
    }

    Ok(RawOutput {
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        code: exit_code(output.status),
    })
}

/// The default concurrency for [`run_all`]: the machine's parallelism,
/// like `xargs -P0`.
fn default_limit() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
}

/// Runs every command, at most `limit` at a time, and returns their
/// results **in input order**.
///
/// This is the whole of the parallelism story, and it is deliberately
/// not a concurrency model. No Brasa value crosses a thread: the
/// commands arrive as argv arrays of plain `String`s and the results
/// come back as plain data after every child has exited. The VM and the
/// collector stay single-threaded and no interleaving is observable in
/// the language (spec: 00 — Visión y alcance: concurrency is out of v1).
///
/// `limit` is clamped to at least one, and `None` means the machine's
/// parallelism. An unbounded fan-out is never offered: the caller of
/// this member is processing a list whose length it does not control,
/// which is exactly the shape that turns into a fork bomb.
pub fn run_all(
    commands: &[Vec<String>],
    overlay: &HashMap<String, String>,
    limit: Option<usize>,
) -> Vec<Result<RawOutput, String>> {
    if commands.is_empty() {
        return Vec::new();
    }

    let workers = limit
        .unwrap_or_else(default_limit)
        .max(1)
        .min(commands.len());

    let next = std::sync::atomic::AtomicUsize::new(0);
    let slots: Vec<std::sync::Mutex<Option<Result<RawOutput, String>>>> = (0..commands.len())
        .map(|_| std::sync::Mutex::new(None))
        .collect();

    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| {
                loop {
                    let index = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let Some(command) = commands.get(index) else {
                        return;
                    };

                    // Stdin is never piped here: a shared input would
                    // have to be split or duplicated across children,
                    // and neither is a decision this member should make
                    // on the caller's behalf.
                    let result = run_command(command, None, overlay);
                    *slots[index].lock().expect("a worker panicked") = Some(result);
                }
            });
        }
    });

    slots
        .into_iter()
        .map(|slot| {
            slot.into_inner()
                .expect("a worker panicked")
                .expect("every slot is filled before the scope ends")
        })
        .collect()
}

/// The child's exit code; a signal-terminated child reports
/// `128 + signal`, the Unix shell convention
/// (spec: 05 — Stdlib de scripting).
pub fn exit_code(status: std::process::ExitStatus) -> i64 {
    if let Some(code) = status.code() {
        return i64::from(code);
    }

    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            return 128 + i64::from(signal);
        }
    }

    -1
}

/// The `proc.NonZeroExit` message: command, exit code, and the child's
/// trimmed stderr when non-empty — the v1 stand-in for a structured
/// payload (spec: 05 — Stdlib de scripting, recorded limitation).
pub fn non_zero_exit_message(shown: &str, output: &RawOutput) -> String {
    let mut message = format!("command `{shown}` exited with code {}", output.code);

    let stderr = output.stderr.trim();
    if !stderr.is_empty() {
        message.push_str(": ");
        message.push_str(stderr);
    }

    message
}

/// One process environment variable; `None` for unset or non-UTF-8
/// values, and for names `std::env::var` would reject (empty, `=`, or
/// NUL — such variables cannot exist).
pub fn env_lookup(key: &str) -> Option<String> {
    if !valid_env_name(key) {
        return None;
    }
    std::env::var(key).ok()
}

/// Whether `key` can name an environment variable: non-empty, no `=`,
/// no NUL.
pub fn valid_env_name(key: &str) -> bool {
    !key.is_empty() && !key.contains('=') && !key.contains('\0')
}

/// The merged environment for `env.vars`: process variables (decoded
/// lossily) overridden by the overlay, sorted by name for
/// deterministic iteration (spec: 05 — Stdlib de scripting).
pub fn merged_env(overlay: &HashMap<String, String>) -> Vec<(String, String)> {
    let mut merged: HashMap<String, String> = std::env::vars_os()
        .map(|(key, value)| {
            (
                key.to_string_lossy().into_owned(),
                value.to_string_lossy().into_owned(),
            )
        })
        .collect();
    for (key, value) in overlay {
        merged.insert(key.clone(), value.clone());
    }

    let mut entries: Vec<(String, String)> = merged.into_iter().collect();
    entries.sort_by(|(a, _), (b, _)| a.cmp(b));
    entries
}

#[cfg(test)]
mod tests {
    use super::*;

    fn echo(text: &str) -> Vec<String> {
        vec!["sh".to_string(), "-c".to_string(), format!("echo {text}")]
    }

    fn sleep_then_echo(text: &str) -> Vec<String> {
        vec![
            "sh".to_string(),
            "-c".to_string(),
            format!("sleep 0.2; echo {text}"),
        ]
    }

    #[test]
    fn results_come_back_in_input_order_not_completion_order() {
        // The first command is the slowest, so completion order is the
        // reverse of input order and a naive collector would show it.
        let commands = vec![
            vec![
                "sh".to_string(),
                "-c".to_string(),
                "sleep 0.3; echo first".to_string(),
            ],
            echo("second"),
            echo("third"),
        ];

        let results = run_all(&commands, &HashMap::new(), Some(3));

        let stdout: Vec<String> = results
            .into_iter()
            .map(|r| r.expect("every command starts").stdout.trim().to_string())
            .collect();

        assert_eq!(stdout, vec!["first", "second", "third"]);
    }

    #[test]
    fn an_empty_batch_runs_nothing() {
        assert!(run_all(&[], &HashMap::new(), None).is_empty());
    }

    /// The cap is the whole point: without it this member is a fork
    /// bomb over a list whose length the caller does not control.
    #[test]
    fn the_concurrency_cap_is_respected() {
        let commands: Vec<Vec<String>> = (0..4).map(|i| sleep_then_echo(&i.to_string())).collect();

        let serial = std::time::Instant::now();
        run_all(&commands, &HashMap::new(), Some(1));
        let serial = serial.elapsed();

        let parallel = std::time::Instant::now();
        run_all(&commands, &HashMap::new(), Some(4));
        let parallel = parallel.elapsed();

        assert!(
            parallel * 2 < serial,
            "four 0.2s commands at a cap of 4 must beat a cap of 1 by more than 2x: \
             {parallel:?} against {serial:?}"
        );
    }

    /// A non-positive cap is clamped rather than rejected, and never
    /// means "unbounded".
    #[test]
    fn a_non_positive_cap_still_runs_every_command() {
        let commands = vec![echo("a"), echo("b")];

        let results = run_all(&commands, &HashMap::new(), Some(0));

        assert_eq!(results.len(), 2);
        assert!(results.iter().all(|r| r.is_ok()));
    }

    /// A command that cannot start is a failure of that command alone;
    /// the rest of the batch still ran and is still reported.
    #[test]
    fn a_command_that_cannot_start_fails_only_its_own_slot() {
        let commands = vec![
            echo("before"),
            vec!["definitely-not-a-real-binary-xyzzy".to_string()],
            echo("after"),
        ];

        let results = run_all(&commands, &HashMap::new(), Some(3));

        assert!(results[0].is_ok());
        assert!(results[1].is_err(), "the missing binary must fail");
        assert!(
            results[2].is_ok(),
            "a neighbour's spawn failure must not lose this result"
        );
    }

    #[test]
    fn the_environment_overlay_reaches_every_child() {
        let mut overlay = HashMap::new();
        overlay.insert("BRASA_RUNALL_PROBE".to_string(), "seen".to_string());

        let commands: Vec<Vec<String>> = (0..3)
            .map(|_| {
                vec![
                    "sh".to_string(),
                    "-c".to_string(),
                    "printf %s \"$BRASA_RUNALL_PROBE\"".to_string(),
                ]
            })
            .collect();

        let results = run_all(&commands, &overlay, Some(3));

        for result in results {
            assert_eq!(result.expect("every command starts").stdout, "seen");
        }
    }
}
