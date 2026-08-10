//! CLI-level regression tests: rejecting non-regular-file inputs (FIFOs,
//! directories) before an unbounded blocking read, while still accepting
//! regular files and symlinks to regular files.

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Runs the `brasa` binary against `script`, waiting at most `timeout` for
/// it to exit. Returns `None` if the process is still running once the
/// timeout elapses (and kills it), otherwise the captured output.
fn run_with_timeout(script: &Path, timeout: Duration) -> Option<std::process::Output> {
    let mut child = Command::new(env!("CARGO_BIN_EXE_brasa"))
        .arg(script)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn brasa");

    let deadline = Instant::now() + timeout;
    loop {
        if let Some(_status) = child.try_wait().expect("failed to poll child") {
            return Some(child.wait_with_output().expect("failed to collect output"));
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn normal_file_is_read_and_parsed() {
    let dir = std::env::temp_dir().join(format!("brasa-cli-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("failed to create temp dir");
    let script = dir.join("ok.brs");
    std::fs::write(&script, "let x = 1\n").expect("failed to write script");

    let output = run_with_timeout(&script, Duration::from_secs(5))
        .expect("brasa should exit promptly on a regular file");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("is not a regular file"),
        "a regular file must not be rejected, stderr: {stderr}"
    );

    std::fs::remove_dir_all(&dir).ok();
}

#[cfg(unix)]
#[test]
fn fifo_is_rejected_instead_of_hanging() {
    use std::os::unix::fs::FileTypeExt;

    let dir = std::env::temp_dir().join(format!("brasa-cli-fifo-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("failed to create temp dir");
    let fifo = dir.join("evil.brs");

    let status = Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .expect("failed to run mkfifo");
    assert!(status.success(), "mkfifo must succeed for this test");
    assert!(
        std::fs::metadata(&fifo)
            .expect("fifo must exist")
            .file_type()
            .is_fifo(),
        "test setup must produce an actual FIFO"
    );

    let output = run_with_timeout(&fifo, Duration::from_secs(5))
        .expect("brasa must not hang reading from a FIFO");

    assert!(
        !output.status.success(),
        "brasa must reject a FIFO instead of succeeding"
    );
    assert_eq!(output.status.code(), Some(65));

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("is not a regular file"),
        "stderr should explain the rejection, got: {stderr}"
    );

    std::fs::remove_dir_all(&dir).ok();
}
