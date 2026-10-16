//! Ordinary-account descendant ownership for the one delivered executable.
//!
//! A guardian runs on a dedicated, long-lived main thread. It retains its writer
//! lease even if the application closes every descriptor, daemonizes, or creates
//! another session. Subreaping, rather than process-group membership, proves exit.
//! Forced guardian loss deliberately leaves its durable lifetime uncertain.

use super::lifecycle::{Layout, LifePhase, ProcessIdentity};
use anyhow::Context as _;
use rustix::process::{Pid, Signal, WaitOptions};
use std::fs::File;
use std::process::Command;
use std::time::{Duration, Instant};
use uuid::Uuid;

/// Permit ordinary descendants time to finish before escalating their signals.
const DRAIN_TIMEOUT: Duration = Duration::from_secs(10);
/// Reaping is bounded per iteration so signals and newly adopted children progress.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// Arm and immediately re-check the actual spawning parent before any data mutation.
/// The caller installs its signal listener before requesting a catchable death signal.
pub(super) fn arm_parent(parent: ProcessIdentity, signal: Signal) -> anyhow::Result<()> {
    anyhow::ensure!(
        ordinary_account()?,
        "internal writers require an ordinary account without elevated capabilities"
    );
    rustix::thread::set_no_new_privs(true)?;
    rustix::process::set_parent_process_death_signal(Some(signal))?;
    anyhow::ensure!(
        rustix::process::getppid().is_some_and(|pid| pid.as_raw_nonzero().get() == parent.pid)
            && ProcessIdentity::read(parent.pid)? == parent,
        "spawning parent exited before writer admission"
    );
    Ok(())
}

/// Inspect the current identity before lowering privileges or allowing internal actors.
pub(super) fn ordinary_account() -> anyhow::Result<bool> {
    if rustix::process::getuid().is_root()
        || rustix::process::getuid() != rustix::process::geteuid()
        || rustix::process::getgid() != rustix::process::getegid()
    {
        return Ok(false);
    }
    let status = std::fs::read_to_string("/proc/self/status")?;
    for name in ["CapEff:", "CapPrm:"] {
        let value = status
            .lines()
            .find_map(|line| line.strip_prefix(name))
            .context("kernel privilege information unavailable")?;
        if u64::from_str_radix(value.trim(), 16)? != 0 {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Spawn only after durable intent, then reap the application and every descendant.
/// The fixed application/controller supplies the command; this is not an IPC command API.
pub(super) fn run(
    layout: &Layout,
    id: Uuid,
    parent: ProcessIdentity,
    lease: File,
    command: &mut Command,
) -> anyhow::Result<i32> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        let interrupt = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
        arm_parent(parent, Signal::TERM)?;
        layout.validate_lease(&lease)?;
        rustix::process::set_child_subreaper(Some(Pid::INIT))?;
        anyhow::ensure!(
            rustix::process::child_subreaper()?.is_some(),
            "subreaper was not armed"
        );
        let mut lifetime = layout.lifetime(id)?;
        anyhow::ensure!(
            lifetime.phase == LifePhase::Launching && lifetime.parent == parent,
            "guardian launch does not match durable intent"
        );
        lifetime.guardian = Some(ProcessIdentity::current()?);
        lifetime.phase = LifePhase::Running;
        layout.save_lifetime(&lifetime)?;
        // No fallible cleanup may publish Stopped before the kernel proves that
        // the subreaper has no children. An exec failure also follows this path.
        let spawned = command.spawn();
        let primary = spawned.as_ref().ok().map(std::process::Child::id);
        let registered = (|| {
            if let Some(pid) = primary {
                lifetime.child = Some(ProcessIdentity::read(i32::try_from(pid)?)?);
                layout.save_lifetime(&lifetime)?;
            }
            Ok::<_, anyhow::Error>(())
        })();
        let result = reap_descendants(
            primary,
            spawned.is_err() || registered.is_err(),
            terminate,
            interrupt,
        )
        .await?;
        // ECHILD proves every ordinary descendant was reaped before the durable
        // completion record or any lease release can permit another writer.
        lifetime.phase = LifePhase::Stopped;
        layout.save_lifetime(&lifetime)?;
        drop(lease);
        registered?;
        Ok(result)
    })
}

/// Keep the sole-spawner thread alive through normal shutdown and subreaping.
async fn reap_descendants(
    primary: Option<u32>,
    spawn_failed: bool,
    mut terminate: tokio::signal::unix::Signal,
    mut interrupt: tokio::signal::unix::Signal,
) -> anyhow::Result<i32> {
    let mut exit = None;
    let mut draining = if spawn_failed {
        Some(Instant::now())
    } else {
        None
    };
    loop {
        if let Some(start) = draining {
            signal_children(if start.elapsed() < DRAIN_TIMEOUT {
                Signal::TERM
            } else {
                Signal::KILL
            })?;
        }
        loop {
            // waitpid(None) means the caller's process group, not every child;
            // a setsid descendant must remain covered by wait(-1).
            match rustix::process::wait(WaitOptions::NOHANG) {
                Ok(Some((pid, status))) => {
                    if primary == Some(u32::try_from(pid.as_raw_nonzero().get())?) {
                        exit = Some(match status.exit_status() {
                            Some(code) => code,
                            None => 128_i32
                                .checked_add(status.terminating_signal().unwrap_or(0_i32))
                                .context("exit signal overflow")?,
                        });
                        let _started = draining.get_or_insert_with(Instant::now);
                    }
                }
                Ok(None) => break,
                Err(rustix::io::Errno::CHILD) => {
                    return Ok(exit.unwrap_or(1));
                }
                Err(rustix::io::Errno::INTR) => {}
                Err(error) => return Err(error.into()),
            }
        }
        tokio::select! {
            _signal = terminate.recv() => { let _started = draining.get_or_insert_with(Instant::now); }
            _signal = interrupt.recv() => { let _started = draining.get_or_insert_with(Instant::now); }
            () = tokio::time::sleep(POLL_INTERVAL) => {}
        }
    }
}

/// A surviving sole-spawner monitor may prove its complete owned subtree exited.
/// Missing PID/lease evidence alone is never substituted for this kernel wait.
pub(super) async fn drain_remaining(
    terminate: tokio::signal::unix::Signal,
    interrupt: tokio::signal::unix::Signal,
) -> anyhow::Result<()> {
    let _exit = reap_descendants(None, true, terminate, interrupt).await?;
    Ok(())
}

/// Signal only this sole-reaper thread's direct/adopted children. Unreaped child
/// PIDs cannot be reused, and no other thread in a guardian waits for children.
fn signal_children(signal: Signal) -> anyhow::Result<()> {
    // Subreaper adoption can attach children to the thread-group leader even
    // when the dedicated long-lived spawner is another thread (as in libtest).
    let mut children = std::collections::HashSet::new();
    for thread in std::fs::read_dir("/proc/self/task")? {
        let path = thread?.path().join("children");
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        for value in text.split_ascii_whitespace() {
            let raw: i32 = value.parse()?;
            let pid = Pid::from_raw(raw).context("invalid adopted child")?;
            let _new_child = children.insert(pid);
        }
    }
    for pid in children {
        match rustix::process::kill_process(pid, signal) {
            Ok(()) | Err(rustix::io::Errno::SRCH) => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}
