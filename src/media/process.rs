//! Process-group lifecycle support for bounded media subprocesses.

use anyhow::{Context as _, Result};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

/// Places a standard-library command in a fresh process group on Unix.
pub(crate) fn configure_std_command(command: &mut Command) -> &mut Command {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        command.process_group(0);
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
    configure_std_command(command);
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
            let stdout = join_pipe_reader(stdout_reader).with_context(&io_context)?;
            let stderr = join_pipe_reader(stderr_reader).with_context(&io_context)?;
            return Ok(Output {
                status,
                stdout,
                stderr,
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
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut pipe, &mut bytes)?;
    Ok(bytes)
}

/// Resolves one pipe reader without panicking if the reader thread failed.
fn join_pipe_reader(
    reader: std::thread::JoinHandle<std::io::Result<Vec<u8>>>,
) -> std::io::Result<Vec<u8>> {
    reader
        .join()
        .map_err(|_| std::io::Error::other("media output reader panicked"))?
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
    let group = format!("-{pid}");
    let status = Command::new("/bin/kill")
        // GNU kill requires `--` before a negative process-group identifier;
        // BSD kill accepts the same portable form.
        .args(["-KILL", "--", &group])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    // A nonzero result normally means the group already exited between wait
    // and cleanup. Direct-child termination remains the portable fallback.
    let _ = status;
    Ok(())
}

#[cfg(not(unix))]
/// Provides a no-op process-group cleanup fallback on non-Unix platforms.
fn terminate_process_group(_pid: u32) -> std::io::Result<()> {
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::run_std_command_with_timeout;
    use crate::workers::{wait_for_ffmpeg_output, AsyncWaitOutcome};
    use anyhow::{Context as _, Result};
    use std::os::unix::fs::PermissionsExt as _;
    use std::path::{Path, PathBuf};
    use std::time::{Duration, Instant};
    use tokio_util::sync::CancellationToken;

    const WRAPPER: &str = "#!/bin/sh\n\
sleep 60 &\n\
child=$!\n\
printf '%s %s\\n' \"$$\" \"$child\" > \"$1\"\n\
wait \"$child\"\n";

    fn process_wrapper() -> Result<(tempfile::TempDir, PathBuf, PathBuf)> {
        let temp_dir = tempfile::tempdir().context("create wrapper directory")?;
        let wrapper = temp_dir.path().join("spawn-child.sh");
        let pid_file = temp_dir.path().join("pids.txt");
        std::fs::write(&wrapper, WRAPPER).context("write process wrapper")?;
        std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o700))
            .context("make process wrapper executable")?;
        Ok((temp_dir, wrapper, pid_file))
    }

    fn read_wrapper_pids(pid_file: &Path) -> Result<(i32, i32)> {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if let Ok(contents) = std::fs::read_to_string(pid_file) {
                let mut parts = contents.split_whitespace();
                let parent = parts
                    .next()
                    .context("wrapper omitted parent PID")?
                    .parse::<i32>()
                    .context("parse wrapper parent PID")?;
                let child = parts
                    .next()
                    .context("wrapper omitted child PID")?
                    .parse::<i32>()
                    .context("parse wrapper child PID")?;
                return Ok((parent, child));
            }
            anyhow::ensure!(Instant::now() < deadline, "wrapper did not record PIDs");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn process_exists(pid: i32) -> bool {
        std::process::Command::new("/bin/kill")
            .args(["-0", &pid.to_string()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    }

    fn assert_processes_gone(parent: i32, child: i32) -> Result<()> {
        let deadline = Instant::now() + Duration::from_secs(2);
        while (process_exists(parent) || process_exists(child)) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        anyhow::ensure!(
            !process_exists(parent),
            "wrapper parent {parent} survived cleanup"
        );
        anyhow::ensure!(
            !process_exists(child),
            "wrapper child {child} survived cleanup"
        );
        Ok(())
    }

    #[test]
    fn blocking_timeout_kills_parent_and_descendant() -> Result<()> {
        let (_temp_dir, wrapper, pid_file) = process_wrapper()?;
        let mut command = std::process::Command::new("/bin/sh");
        command.args([wrapper.as_os_str(), pid_file.as_os_str()]);

        let error = run_std_command_with_timeout(
            &mut command,
            Duration::from_millis(100),
            "test wrapper",
            || "spawn test wrapper".to_owned(),
            || "wait for test wrapper".to_owned(),
        )
        .err()
        .context("wrapper unexpectedly completed")?;
        let (parent, child) = read_wrapper_pids(&pid_file)?;

        anyhow::ensure!(error.to_string().contains("timed out"));
        assert_processes_gone(parent, child)
    }

    #[tokio::test]
    async fn async_timeout_kills_parent_and_descendant() -> Result<()> {
        let (_temp_dir, wrapper, pid_file) = process_wrapper()?;
        let mut command = tokio::process::Command::new("/bin/sh");
        command
            .args([wrapper.as_os_str(), pid_file.as_os_str()])
            .kill_on_drop(true);
        command.process_group(0);
        let child = command.spawn().context("spawn async test wrapper")?;

        let outcome =
            wait_for_ffmpeg_output(child, Duration::from_millis(100), CancellationToken::new())
                .await?;
        let (parent, child) = read_wrapper_pids(&pid_file)?;

        anyhow::ensure!(matches!(outcome, AsyncWaitOutcome::TimedOut));
        assert_processes_gone(parent, child)
    }

    #[tokio::test]
    async fn cancellation_kills_parent_and_descendant() -> Result<()> {
        let (_temp_dir, wrapper, pid_file) = process_wrapper()?;
        let mut command = tokio::process::Command::new("/bin/sh");
        command
            .args([wrapper.as_os_str(), pid_file.as_os_str()])
            .kill_on_drop(true);
        command.process_group(0);
        let child = command.spawn().context("spawn cancellable test wrapper")?;
        let cancel = CancellationToken::new();
        let cancel_trigger = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            cancel_trigger.cancel();
        });

        let outcome = wait_for_ffmpeg_output(child, Duration::from_secs(10), cancel).await?;
        let (parent, child) = read_wrapper_pids(&pid_file)?;

        anyhow::ensure!(matches!(outcome, AsyncWaitOutcome::Cancelled));
        assert_processes_gone(parent, child)
    }

    #[tokio::test]
    async fn repeated_timeouts_do_not_accumulate_descendants() -> Result<()> {
        let (_temp_dir, wrapper, _) = process_wrapper()?;

        for attempt in 1..=3 {
            let pid_file = wrapper.with_file_name(format!("pids-{attempt}.txt"));
            let mut command = tokio::process::Command::new("/bin/sh");
            command
                .args([wrapper.as_os_str(), pid_file.as_os_str()])
                .kill_on_drop(true);
            command.process_group(0);
            let child = command.spawn().context("spawn retry test wrapper")?;

            let outcome =
                wait_for_ffmpeg_output(child, Duration::from_millis(100), CancellationToken::new())
                    .await?;
            let (parent, child) = read_wrapper_pids(&pid_file)?;

            anyhow::ensure!(matches!(outcome, AsyncWaitOutcome::TimedOut));
            assert_processes_gone(parent, child)?;
        }
        Ok(())
    }
}
