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

#[cfg(test)]
#[cfg(unix)]
mod tests {
    use super::{
        configure_std_command, run_std_command_with_timeout, terminate_process_group_using,
        ProcessGroupGuard, MEDIA_SUBPROCESS_OUTPUT_LIMIT_BYTES,
    };
    use crate::workers::{wait_for_ffmpeg_output, AsyncWaitOutcome};
    use anyhow::{Context as _, Result};
    use std::os::unix::fs::PermissionsExt as _;
    use std::os::unix::process::ExitStatusExt as _;
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
        let deadline = Instant::now()
            .checked_add(Duration::from_secs(2))
            .context("process test deadline overflow")?;
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
        let deadline = Instant::now()
            .checked_add(Duration::from_secs(2))
            .context("process test deadline overflow")?;
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

    /// A missing kill executable still terminates the group under hostile shell inheritance.
    #[test]
    fn missing_kill_executable_terminates_parent_and_descendant() -> Result<()> {
        const CHILD_MARKER: &str = "RUSTCHAN_PROCESS_FALLBACK_ENVIRONMENT_TEST_CHILD";
        if std::env::var_os(CHILD_MARKER).as_deref() != Some(std::ffi::OsStr::new("1")) {
            let (_, test_module) = module_path!()
                .split_once("::")
                .context("fallback test module omitted its crate prefix")?;
            let test_filter =
                format!("{test_module}::missing_kill_executable_terminates_parent_and_descendant");
            // Only the child receives the exported function; the parent test
            // runner and every concurrently executing test keep their environment.
            let output = std::process::Command::new(
                std::env::current_exe().context("locate fallback regression test binary")?,
            )
            .args(["--exact", &test_filter, "--nocapture", "--format=pretty"])
            .env(CHILD_MARKER, "1")
            .env("BASH_FUNC_kill%%", "() { return 0; }")
            .output()
            .context("run fallback regression with an exported shell function")?;
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::ensure!(
                stdout.lines().any(|line| line.trim() == "running 1 test"),
                "child must run exactly the fallback regression; stdout:\n{stdout}\nstderr:\n{stderr}"
            );
            anyhow::ensure!(
                output.status.success()
                    && stdout.lines().any(|line| {
                        line.starts_with("test result: ok. 1 passed; 0 failed; 0 ignored;")
                    }),
                "fallback child failed with {}: stdout:\n{stdout}\nstderr:\n{stderr}",
                output.status
            );
            return Ok(());
        }
        let (temp_dir, wrapper, pid_file) = process_wrapper()?;
        let missing_kill = temp_dir.path().join("missing-kill");
        let mut command = std::process::Command::new("/bin/sh");
        let _configured_command = command.args([wrapper.as_os_str(), pid_file.as_os_str()]);
        let _configured_group = configure_std_command(&mut command);
        let mut child = command.spawn().context("spawn shell-fallback wrapper")?;
        let mut process_group = ProcessGroupGuard::new(Some(child.id()));

        let result = (|| -> Result<()> {
            let (parent, descendant) = read_wrapper_pids(&pid_file)?;
            anyhow::ensure!(
                parent == i32::try_from(child.id()).context("wrapper PID exceeds i32")?,
                "wrapper parent must be the owned process-group leader"
            );
            anyhow::ensure!(
                process_exists(parent) && process_exists(descendant),
                "the parent and descendant must be alive before fallback termination"
            );
            anyhow::ensure!(
                !missing_kill.exists(),
                "fixture kill executable must be absent"
            );
            terminate_process_group_using(child.id(), &missing_kill)?;
            let deadline = Instant::now()
                .checked_add(Duration::from_secs(2))
                .context("shell-fallback exit deadline overflow")?;
            let status = loop {
                if let Some(status) = child.try_wait().context("reap shell-fallback wrapper")? {
                    break status;
                }
                anyhow::ensure!(
                    Instant::now() < deadline,
                    "shell-fallback wrapper did not exit"
                );
                std::thread::sleep(Duration::from_millis(10));
            };
            anyhow::ensure!(status.signal() == Some(9_i32), "fallback must send SIGKILL");
            assert_processes_gone(parent, descendant)
        })();

        // Always terminate and reap this test's owned fixture on an error too.
        process_group.terminate_remaining();
        drop(child.kill());
        drop(child.wait());
        result
    }

    #[test]
    fn blocking_timeout_kills_parent_and_descendant() -> Result<()> {
        let (_temp_dir, wrapper, pid_file) = process_wrapper()?;
        let mut command = std::process::Command::new("/bin/sh");
        let _configured_command = command.args([wrapper.as_os_str(), pid_file.as_os_str()]);

        let error = run_std_command_with_timeout(
            &mut command,
            Duration::from_millis(100),
            "test wrapper",
            || "spawn test wrapper".to_owned(),
            || "wait for test wrapper".to_owned(),
        )
        .err()
        .context("wrapper unexpectedly completed")?;
        let (parent, descendant) = read_wrapper_pids(&pid_file)?;

        anyhow::ensure!(error.to_string().contains("timed out"));
        assert_processes_gone(parent, descendant)
    }

    #[test]
    fn blocking_subprocess_output_is_retained_within_limit() -> Result<()> {
        let mut command = std::process::Command::new("/bin/sh");
        let _configured_command = command.args(["-c", "printf '%131072s' ''"]);

        let output = run_std_command_with_timeout(
            &mut command,
            Duration::from_secs(5),
            "verbose test wrapper",
            || "spawn verbose test wrapper".to_owned(),
            || "wait for verbose test wrapper".to_owned(),
        )?;

        anyhow::ensure!(output.status.success());
        anyhow::ensure!(output.stdout.len() == MEDIA_SUBPROCESS_OUTPUT_LIMIT_BYTES);
        anyhow::ensure!(output.stderr.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn async_timeout_kills_parent_and_descendant() -> Result<()> {
        let (_temp_dir, wrapper, pid_file) = process_wrapper()?;
        let mut command = tokio::process::Command::new("/bin/sh");
        let _configured_command = command
            .args([wrapper.as_os_str(), pid_file.as_os_str()])
            .kill_on_drop(true)
            .process_group(0);
        let child = command.spawn().context("spawn async test wrapper")?;

        let outcome =
            wait_for_ffmpeg_output(child, Duration::from_millis(100), CancellationToken::new())
                .await?;
        let (parent, descendant) = read_wrapper_pids(&pid_file)?;

        anyhow::ensure!(matches!(outcome, AsyncWaitOutcome::TimedOut));
        assert_processes_gone(parent, descendant)
    }

    #[tokio::test]
    async fn async_subprocess_output_is_retained_within_limit() -> Result<()> {
        let mut command = tokio::process::Command::new("/bin/sh");
        let _configured_command = command
            .args(["-c", "printf '%131072s' '' >&2"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .process_group(0);
        let child = command
            .spawn()
            .context("spawn verbose async test wrapper")?;

        let outcome =
            wait_for_ffmpeg_output(child, Duration::from_secs(5), CancellationToken::new()).await?;
        let AsyncWaitOutcome::Exited(output) = outcome else {
            anyhow::bail!("verbose async test wrapper did not exit normally");
        };

        anyhow::ensure!(output.status.success());
        anyhow::ensure!(output.stdout.is_empty());
        anyhow::ensure!(output.stderr.len() == MEDIA_SUBPROCESS_OUTPUT_LIMIT_BYTES);
        Ok(())
    }

    #[tokio::test]
    async fn cancellation_kills_parent_and_descendant() -> Result<()> {
        let (_temp_dir, wrapper, pid_file) = process_wrapper()?;
        let mut command = tokio::process::Command::new("/bin/sh");
        let _configured_command = command
            .args([wrapper.as_os_str(), pid_file.as_os_str()])
            .kill_on_drop(true)
            .process_group(0);
        let child = command.spawn().context("spawn cancellable test wrapper")?;
        let cancel = CancellationToken::new();
        let cancel_trigger = cancel.clone();
        let cancel_task = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            cancel_trigger.cancel();
        });

        let outcome = wait_for_ffmpeg_output(child, Duration::from_secs(10), cancel).await?;
        cancel_task.await.context("join cancellation trigger")?;
        let (parent, descendant) = read_wrapper_pids(&pid_file)?;

        anyhow::ensure!(matches!(outcome, AsyncWaitOutcome::Cancelled));
        assert_processes_gone(parent, descendant)
    }

    #[tokio::test]
    async fn repeated_timeouts_do_not_accumulate_descendants() -> Result<()> {
        let (_temp_dir, wrapper, _) = process_wrapper()?;

        for attempt in 1_i32..=3_i32 {
            let pid_file = wrapper.with_file_name(format!("pids-{attempt}.txt"));
            let mut command = tokio::process::Command::new("/bin/sh");
            let _configured_command = command
                .args([wrapper.as_os_str(), pid_file.as_os_str()])
                .kill_on_drop(true)
                .process_group(0);
            let child = command.spawn().context("spawn retry test wrapper")?;

            let outcome =
                wait_for_ffmpeg_output(child, Duration::from_millis(100), CancellationToken::new())
                    .await?;
            let (parent, descendant) = read_wrapper_pids(&pid_file)?;

            anyhow::ensure!(matches!(outcome, AsyncWaitOutcome::TimedOut));
            assert_processes_gone(parent, descendant)?;
        }
        Ok(())
    }
}
