//! Process-group lifecycle support for bounded media subprocesses.

use anyhow::{Context as _, Result};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

/// Maximum diagnostic bytes retained from each media-subprocess output pipe.
///
/// Pipes are still drained to EOF after this limit so a verbose parser cannot
/// block on a full pipe or force `RustChan` to retain unbounded attacker-driven
/// output in memory.
pub(crate) const MEDIA_SUBPROCESS_OUTPUT_LIMIT_BYTES: usize = 64 * 1024;

/// Places a standard-library command in a fresh process group on Unix.
pub(crate) fn configure_std_command(command: &mut Command) -> &mut Command {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        let _configured_command = command.process_group(0);
    }
    command
}

/// Runs a blocking subprocess with captured output and a hard deadline.
pub(crate) fn run_std_command_with_timeout(
    command: &mut Command,
    timeout: Duration,
    timeout_label: &str,
    spawn_context: impl FnOnce() -> String,
    io_context: impl Fn() -> String,
) -> Result<Output> {
    let _configured_command = configure_std_command(command);
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(spawn_context)?;
    let mut process_group = ProcessGroupGuard::new(Some(child.id()));
    let stdout = child
        .stdout
        .take()
        .context("media subprocess stdout pipe was not captured")?;
    let stderr = child
        .stderr
        .take()
        .context("media subprocess stderr pipe was not captured")?;
    let stdout_reader = std::thread::spawn(move || read_pipe(stdout));
    let stderr_reader = std::thread::spawn(move || read_pipe(stderr));
    let started = Instant::now();

    loop {
        if let Some(status) = child.try_wait()? {
            process_group.terminate_remaining();
            let stdout_bytes = join_pipe_reader(stdout_reader).with_context(&io_context)?;
            let stderr_bytes = join_pipe_reader(stderr_reader).with_context(&io_context)?;
            return Ok(Output {
                status,
                stdout: stdout_bytes,
                stderr: stderr_bytes,
            });
        }
        if started.elapsed() >= timeout {
            process_group.terminate_remaining();
            // Kill the direct child as a non-Unix fallback and as protection
            // against a rare process-group setup race.
            drop(child.kill());
            drop(child.wait());
            anyhow::bail!("{timeout_label} timed out after {}s", timeout.as_secs());
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// Drains one captured child pipe so subprocess output cannot fill its buffer.
fn read_pipe(mut pipe: impl std::io::Read) -> std::io::Result<Vec<u8>> {
    const READ_BUFFER_BYTES: usize = 8 * 1024;

    let mut bytes = Vec::with_capacity(READ_BUFFER_BYTES);
    let mut chunk = [0_u8; READ_BUFFER_BYTES];
    loop {
        let read = std::io::Read::read(&mut pipe, &mut chunk)?;
        if read == 0 {
            break;
        }

        let remaining = MEDIA_SUBPROCESS_OUTPUT_LIMIT_BYTES.saturating_sub(bytes.len());
        let retain = remaining.min(read);
        let retained = chunk.get(..retain).ok_or_else(|| {
            std::io::Error::other("media output retention exceeded its read buffer")
        })?;
        bytes.extend_from_slice(retained);
    }
    Ok(bytes)
}

/// Resolves one pipe reader without panicking if the reader thread failed.
fn join_pipe_reader(
    reader: std::thread::JoinHandle<std::io::Result<Vec<u8>>>,
) -> std::io::Result<Vec<u8>> {
    reader.join().map_err(|panic| {
        std::io::Error::other(format!(
            "media output reader panicked: {}",
            panic_message(panic.as_ref())
        ))
    })?
}

/// Recover a thread panic message while preserving ordinary string payloads.
pub(crate) fn panic_message(payload: &(dyn std::any::Any + Send)) -> &str {
    payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("non-string panic payload")
}

/// Owns the process-group cleanup obligation for one spawned command.
pub(crate) struct ProcessGroupGuard {
    /// Process-group leader identifier, cleared after cleanup is attempted.
    pid: Option<u32>,
}

impl ProcessGroupGuard {
    /// Creates a cleanup guard for the spawned process group.
    pub(crate) const fn new(pid: Option<u32>) -> Self {
        Self { pid }
    }

    /// Sends a hard termination signal to the group at most once.
    pub(crate) fn terminate_remaining(&mut self) {
        let Some(pid) = self.pid.take() else {
            return;
        };
        if let Err(error) = terminate_process_group(pid) {
            tracing::warn!(pid, error = %error, "failed to terminate media process group");
        }
    }
}

impl Drop for ProcessGroupGuard {
    fn drop(&mut self) {
        self.terminate_remaining();
    }
}

#[cfg(unix)]
/// Terminates the Unix process group whose leader has `pid`.
fn terminate_process_group(pid: u32) -> std::io::Result<()> {
    terminate_process_group_using(pid, std::path::Path::new("/bin/kill"))
}

#[cfg(unix)]
/// Uses the external kill program, falling back to the shell only if it is absent.
fn terminate_process_group_using(pid: u32, kill_program: &std::path::Path) -> std::io::Result<()> {
    let group = format!("-{pid}");
    match Command::new(kill_program)
        // GNU kill requires `--` before a negative process-group identifier;
        // BSD kill accepts the same portable form.
        .args(["-KILL", "--", &group])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
    {
        Ok(_status) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            // Minimal Unix images can provide only the POSIX shell builtin.
            // The script is constant; the derived numeric group is a separate
            // positional argument and is never interpolated into shell code.
            let _status = Command::new("/bin/sh")
                // The builtin needs no environment; imported shell functions
                // or startup hooks must not replace the intended command.
                .env_clear()
                .args([
                    "-c",
                    "kill -s KILL -- \"$1\"",
                    "rustchan-process-group",
                    &group,
                ])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()?;
        }
        Err(error) => return Err(error),
    }
    // A nonzero result normally means the group already exited between wait
    // and cleanup. Direct-child termination remains the portable fallback.
    Ok(())
}

#[cfg(not(unix))]
/// Provides a no-op process-group cleanup fallback on non-Unix platforms.
fn terminate_process_group(_pid: u32) -> std::io::Result<()> {
    Ok(())
}
