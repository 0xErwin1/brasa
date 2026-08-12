//! Backend-agnostic OS glue for `std::proc` and `std::env` (BRS-32,
//! `docs/spec/05-stdlib.md`), shared by the walker and the VM so the
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

/// The `/bin/sh -c` argv for `proc.shell` (`docs/spec/05-stdlib.md`:
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

/// The child's exit code; a signal-terminated child reports
/// `128 + signal`, the Unix shell convention
/// (`docs/spec/05-stdlib.md`).
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
/// payload (`docs/spec/05-stdlib.md`, recorded limitation).
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
/// deterministic iteration (`docs/spec/05-stdlib.md`).
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
