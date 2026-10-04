//! Same-executable Linux writer entry and ordinary foreground supervision.
//!
//! Internal roles are kernel-parent authenticated before any application side
//! effects. A parent keeps stdin/stdout/stderr intact and transfers only the
//! fixed close-on-exec writer lease over private abstract IPC.

use super::{
    control, guardian,
    lifecycle::{self, Layout, LifePhase, ProcessIdentity, WriterRole},
};
use anyhow::Context as _;
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File},
    io::Write as _,
    os::unix::fs::DirBuilderExt as _,
    os::unix::net::{UnixListener, UnixStream},
    process::Command,
    sync::OnceLock,
    time::{Duration, Instant},
};
use uuid::Uuid;

/// Startup-only context; no browser operation can construct an internal role.
pub(super) const INTERNAL: &str = "RUSTCHAN_INTERNAL_WRITER";
/// Bound environment/context and private monitor metadata independently of application data.
const MAX_CONTEXT: u64 = 16 * 1024;
/// Internal spawn/handshake deadline.
const START_TIMEOUT: Duration = Duration::from_secs(5);
/// Keep the signal/spawn thread alive while checking IPC and owned process completion.
const POLL_INTERVAL: Duration = Duration::from_millis(25);
/// Installed only after authenticating an admitted application process.
static APPLICATION: OnceLock<Context> = OnceLock::new();

/// An authenticated internal application retains its original fixed data root.
pub(super) fn data_dir() -> Option<&'static std::path::Path> {
    APPLICATION.get().map(|context| context.data.as_path())
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
/// Closed same-binary execution roles.
pub(super) enum Role {
    /// Own and reap the entire admitted writer subtree.
    Guardian,
    /// Execute the original fixed server/administration arguments.
    Application,
    /// Execute a selected full update controller without parsing public CLI arguments.
    Controller,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
/// Parent-created context bound to one durable launch, never an HTTP-selected path.
pub(super) struct Context {
    /// Fixed internal role.
    pub role: Role,
    /// Canonical data directory selected by the original launcher.
    pub data: std::path::PathBuf,
    /// Durable lifetime identifier and private handshake binding.
    pub id: Uuid,
    /// Actual creating process identity.
    pub parent: ProcessIdentity,
    /// The long-lived monitor identity.
    pub monitor: ProcessIdentity,
    /// Private abstract endpoint created before spawn.
    pub endpoint: String,
    /// A source-owned root provides built-in control; protected manual invocations do not.
    pub automatic: bool,
    /// Startup-only launcher port override, never browser-selected.
    pub port: Option<u16>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
/// Private cooperative admission metadata for independent administration CLI callers.
pub(super) struct Monitor {
    /// Version of the persisted small monitor contract.
    pub format: u32,
    /// Real kernel boot identity, never an environment override.
    pub boot: Uuid,
    /// Exact owning process start identity.
    pub identity: ProcessIdentity,
    /// Private abstract endpoint, authenticated against identity above.
    pub endpoint: String,
    /// Running package compatibility for independent CLI operations.
    pub version: String,
    /// Detect accidental same-version source rebuilds before CLI writer admission.
    pub program_sha256: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields, tag = "operation", rename_all = "snake_case")]
/// This writer-admission protocol cannot supply commands or arbitrary paths.
pub(super) enum Message {
    /// Closed operations of the process monitor, authenticated to exact primaries.
    Core {
        /// Fixed lifecycle operation, never an arbitrary execution request.
        request: super::coordinator::Request,
    },
    /// Guardian requests the one already-approved lifetime lease.
    Guardian {
        /// Parent-issued durable invocation identifier.
        id: Uuid,
    },
    /// The exact child reports completion of normal application initialization.
    Initialized {
        /// Parent-issued durable invocation identifier.
        id: Uuid,
    },
    /// An independent CLI requests a shared lease before spawning its own guardian.
    Administrator {
        /// Caller-owned durable pre-spawn intent.
        id: Uuid,
        /// Exact running package compatibility for the CLI.
        version: String,
        /// Exact invoking CLI executable bytes, bound to the running application.
        program_sha256: String,
    },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
/// Closed acknowledgment, with a lease only on successful writer admission.
pub(super) struct Granted {
    /// Bind the reply to the exact launch intent.
    pub id: Uuid,
}

/// Register catchable termination before publishing any launch intent.
pub(super) struct Signals {
    /// Current-thread runtime keeps the dedicated spawner alive.
    pub runtime: tokio::runtime::Runtime,
    /// Normal service shutdown.
    pub terminate: tokio::signal::unix::Signal,
    /// Foreground keyboard shutdown.
    pub interrupt: tokio::signal::unix::Signal,
}

impl Signals {
    /// Install listeners before launch/fsync/spawn, closing the normal startup-stop gap.
    fn prepare() -> anyhow::Result<Self> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let entered = runtime.enter();
        let terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        let interrupt = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
        drop(entered);
        Ok(Self {
            runtime,
            terminate,
            interrupt,
        })
    }
}

/// Handle internal roles before CLI/configuration initialization.
pub(super) fn entry() -> anyhow::Result<bool> {
    let Some(bytes) = std::env::var_os(INTERNAL) else {
        return Ok(false);
    };
    let text = bytes
        .to_str()
        .context("invalid internal context encoding")?;
    anyhow::ensure!(
        u64::try_from(text.len())? <= MAX_CONTEXT,
        "internal context exceeds bound"
    );
    let context: Context = serde_json::from_str(text)?;
    anyhow::ensure!(!context.id.is_nil(), "invalid internal launch identity");
    // A noncatchable signal covers the pre-handshake interval. A guardian
    // installs its catchable TERM listener before replacing this death signal.
    guardian::arm_parent(context.parent, rustix::process::Signal::KILL)?;
    let layout = Layout::prepare(&context.data)?;
    match context.role {
        Role::Guardian => {
            let mut socket = control::connect(&context.endpoint, layout.uid, context.monitor)?;
            control::send(&mut socket, &Message::Guardian { id: context.id }, None)?;
            let (granted, lease): (Granted, _) = control::receive(&mut socket, true)?;
            anyhow::ensure!(
                granted.id == context.id,
                "guardian lease belongs to a different launch"
            );
            let mut child_context = context.clone();
            child_context.role = if layout.lifetime(context.id)?.role == WriterRole::Controller {
                Role::Controller
            } else {
                Role::Application
            };
            child_context.parent = ProcessIdentity::current()?;
            let mut command = Command::new(std::env::current_exe()?);
            let _arguments = command
                .args(std::env::args_os().skip(1))
                .env(INTERNAL, serde_json::to_string(&child_context)?)
                .env("RUSTCHAN_SPAWNED", "1");
            let code = guardian::run(
                &layout,
                context.id,
                context.parent,
                lease.context("missing guardian lease")?,
                &mut command,
            )?;
            anyhow::ensure!(code == 0, "admitted writer exited unsuccessfully ({code})");
            Ok(true)
        }
        Role::Application | Role::Controller => {
            let started = Instant::now();
            loop {
                let record = layout.lifetime(context.id)?;
                anyhow::ensure!(
                    record.phase == LifePhase::Running
                        && record.guardian == Some(context.parent)
                        && record.parent == context.monitor,
                    "application is not owned by its admitted guardian"
                );
                if record.child == Some(ProcessIdentity::current()?) {
                    break;
                }
                anyhow::ensure!(
                    started.elapsed() < START_TIMEOUT,
                    "guardian did not register this application"
                );
                std::thread::sleep(POLL_INTERVAL);
            }
            if matches!(context.role, Role::Controller) {
                super::controller::run(&context)?;
                return Ok(true);
            }
            APPLICATION.set(context).map_err(|_rejected_context| {
                anyhow::anyhow!("application admission was already registered")
            })?;
            Ok(false)
        }
    }
}

/// Launch a durable guardian before logs, configuration generation, migration or CLI writes.
#[expect(
    clippy::too_many_lines,
    reason = "cooperative admission, cancellation and bootstrap must share the pre-write authorization boundary"
)]
pub(super) fn before_startup(administrator: bool) -> anyhow::Result<bool> {
    if let Some(context) = APPLICATION.get() {
        anyhow::ensure!(
            crate::config::data_dir().canonicalize()? == context.data,
            "admitted application data directory changed"
        );
        return Ok(false);
    }
    if super::container_managed() || super::managed() || rustix::process::getuid().is_root() {
        return Ok(false);
    }
    anyhow::ensure!(
        guardian::ordinary_account()?,
        "native supervision requires an ordinary account without elevated capabilities"
    );
    anyhow::ensure!(
        rustix::thread::gettid() == rustix::process::getpid(),
        "native supervision must run on the long-lived main spawner thread"
    );
    rustix::thread::set_no_new_privs(true)?;
    let signals = Signals::prepare()?;
    let data = crate::config::data_dir();
    if !data.try_exists()? {
        let mut ancestor = data.as_path();
        while !ancestor.try_exists()? {
            ancestor = ancestor.parent().context("data has no existing parent")?;
        }
        lifecycle::owned_ancestors(ancestor, rustix::process::getuid().as_raw())?;
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&data)?;
    }
    let layout = Layout::prepare(&data)?;
    let role = if administrator {
        WriterRole::Administrator
    } else {
        WriterRole::Server
    };
    let admission = layout.admission();
    let (admission, lease) = match admission {
        Ok(admission) => (Some(admission), layout.exclusive_quiescent()?),
        Err(error) if role == WriterRole::Administrator => {
            let monitor: Monitor = serde_json::from_slice(&lifecycle::read_private(
                &layout.state.join("monitor.json"),
                layout.uid,
                MAX_CONTEXT,
            )?)?;
            anyhow::ensure!(
                monitor.format == lifecycle::FORMAT
                    && monitor.boot == lifecycle::kernel_boot_id()?
                    && monitor.version == super::VERSION,
                "active monitor is incompatible; stop RustChan before using this CLI"
            );
            let program_sha256 = super::bootstrap::digest(&std::env::current_exe()?)?;
            anyhow::ensure!(program_sha256 == monitor.program_sha256,
                "administration CLI differs from the running source build; use its matching executable");
            let mut socket = control::connect(&monitor.endpoint, layout.uid, monitor.identity)
                .with_context(|| format!("active writer admission is unavailable: {error}"))?;
            let record = layout.launch(role)?;
            let granted = (|| {
                control::send(
                    &mut socket,
                    &Message::Administrator {
                        id: record.id,
                        version: super::VERSION.to_owned(),
                        program_sha256,
                    },
                    None,
                )?;
                let (reply, lease): (Granted, _) = control::receive(&mut socket, true)?;
                anyhow::ensure!(
                    reply.id == record.id,
                    "administration lease belongs to another launch"
                );
                lease.context("missing administration lease")
            })();
            match granted {
                Ok(lease) => return supervise(&layout, record, None, lease, signals),
                Err(grant_error) => {
                    // This process has not called spawn. No descendant could
                    // have been admitted, even if the grant acknowledgment was lost.
                    let mut cancelled = record;
                    cancelled.phase = LifePhase::Stopped;
                    layout.save_lifetime(&cancelled)?;
                    return Err(grant_error);
                }
            }
        }
        Err(error) => return Err(error),
    };
    if role == WriterRole::Server {
        // Retain the exact local executable before permitting application-side
        // changes. This preflight alone does not open update installation.
        if super::bootstrap::prepare(&layout)?.is_some() {
            return super::coordinator::run(
                &layout,
                admission.context("source root must own admission")?,
                lease,
                signals,
            );
        }
    } else {
        super::bootstrap::admit_cli(&layout)?;
    }
    let record = layout.launch(role)?;
    supervise(&layout, record, admission, lease, signals)
}

/// Own one foreground invocation on the same long-lived signal/spawn thread.
fn supervise(
    layout: &Layout,
    mut record: lifecycle::Lifetime,
    admission: Option<File>,
    lease: File,
    signals: Signals,
) -> anyhow::Result<bool> {
    let endpoint = format!("rustchan-writer-{}", record.id);
    let listener = control::listen(&endpoint)?;
    listener.set_nonblocking(true)?;
    let context = Context {
        role: Role::Guardian,
        data: layout.data.clone(),
        id: record.id,
        parent: ProcessIdentity::current()?,
        monitor: ProcessIdentity::current()?,
        endpoint: endpoint.clone(),
        automatic: false,
        port: crate::config::launcher_port(),
    };
    rustix::process::set_child_subreaper(Some(rustix::process::Pid::INIT))?;
    let mut command = Command::new(std::env::current_exe()?);
    let _arguments = command
        .args(std::env::args_os().skip(1))
        .env(INTERNAL, serde_json::to_string(&context)?)
        .env("RUSTCHAN_SPAWNED", "1");
    let spawned = command.spawn();
    let mut child = match spawned {
        Ok(child) => child,
        Err(error) => {
            record.phase = LifePhase::Stopped;
            layout.save_lifetime(&record)?;
            return Err(error.into());
        }
    };
    let identity = ProcessIdentity::read(i32::try_from(child.id())?)?;
    let owns_admission = admission.is_some();
    if owns_admission {
        publish_monitor(
            layout,
            &Monitor {
                format: lifecycle::FORMAT,
                boot: lifecycle::kernel_boot_id()?,
                identity: context.monitor,
                endpoint,
                version: super::VERSION.to_owned(),
                program_sha256: super::bootstrap::digest(&std::env::current_exe()?)?,
            },
        )?;
    }
    let Signals {
        runtime,
        mut terminate,
        mut interrupt,
    } = signals;
    let outcome = runtime.block_on(async {
        let mut ready = false;
        let mut stopping = false;
        loop {
            if let Some(status) = child.try_wait()? {
                // The monitor is itself a subreaper. If its guardian was lost,
                // reap all its owned/adopted descendants before clearing intent.
                guardian::drain_remaining(terminate, interrupt).await?;
                let mut stopped = layout.lifetime(record.id)?;
                anyhow::ensure!(
                    stopped.parent == context.monitor,
                    "writer launch parent changed"
                );
                stopped.phase = LifePhase::Stopped;
                layout.save_lifetime(&stopped)?;
                anyhow::ensure!(
                    status.success(),
                    "RustChan writer guardian exited unsuccessfully"
                );
                return Ok::<_, anyhow::Error>(());
            }
            let accepted = accept(&listener)?;
            if let Some(mut socket) = accepted {
                let handled = handle(
                    layout,
                    &record,
                    identity,
                    &lease,
                    &mut socket,
                    &mut ready,
                    stopping,
                );
                if let Err(error) = handled {
                    tracing::warn!(%error, "writer admission rejected");
                }
            }
            tokio::select! {
                _signal = terminate.recv() => { stopping = true; signal(identity)?; }
                _signal = interrupt.recv() => { stopping = true; signal(identity)?; }
                () = tokio::time::sleep(POLL_INTERVAL) => {}
            }
        }
    });
    if owns_admission {
        fs::remove_file(layout.state.join("monitor.json"))?;
        File::open(&layout.state)?.sync_all()?;
    }
    drop(lease);
    drop(admission);
    outcome?;
    Ok(true)
}

/// Nonblocking accept keeps the main spawner responsive to lifecycle signals.
pub(super) fn accept(listener: &UnixListener) -> anyhow::Result<Option<UnixStream>> {
    match listener.accept() {
        Ok((socket, _address)) => Ok(Some(socket)),
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => Ok(None),
        Err(error) => Err(error.into()),
    }
}

/// Inspect only bounded typed messages after authenticating the kernel peer UID.
fn handle(
    layout: &Layout,
    record: &lifecycle::Lifetime,
    guardian: ProcessIdentity,
    lease: &File,
    socket: &mut UnixStream,
    ready: &mut bool,
    stopping: bool,
) -> anyhow::Result<()> {
    let peer = rustix::net::sockopt::socket_peercred(&*socket)?;
    anyhow::ensure!(
        peer.uid.as_raw() == layout.uid,
        "writer admission has a different owner"
    );
    let identity = ProcessIdentity::read(peer.pid.as_raw_nonzero().get())?;
    let (message, _unexpected): (Message, _) = control::receive(socket, false)?;
    match message {
        Message::Core { request: _ } => {
            anyhow::bail!("automatic control is unavailable in manual mode")
        }
        Message::Guardian { id } => {
            control::authorize(socket, layout.uid, guardian)?;
            anyhow::ensure!(
                id == record.id && !stopping,
                "guardian launch is no longer permitted"
            );
            control::send(socket, &Granted { id }, Some(lease))
        }
        Message::Initialized { id } => {
            let current = layout.lifetime(record.id)?;
            anyhow::ensure!(
                id == record.id
                    && current.child == Some(identity)
                    && current.guardian == Some(guardian)
                    && !stopping,
                "initialization belongs to a different writer"
            );
            lease.lock_shared()?;
            *ready = record.role == WriterRole::Server;
            control::send(socket, &Granted { id }, None)
        }
        Message::Administrator {
            id,
            version,
            program_sha256,
        } => {
            anyhow::ensure!(
                *ready && !stopping && version == super::VERSION,
                "administration is unavailable during startup or shutdown"
            );
            let monitor: Monitor = serde_json::from_slice(&lifecycle::read_private(
                &layout.state.join("monitor.json"),
                layout.uid,
                MAX_CONTEXT,
            )?)?;
            anyhow::ensure!(
                monitor.program_sha256 == program_sha256,
                "administration CLI bytes differ from the admitted source build"
            );
            let intent = layout.lifetime(id)?;
            anyhow::ensure!(
                intent.role == WriterRole::Administrator
                    && intent.phase == LifePhase::Launching
                    && intent.parent == identity
                    && intent.guardian.is_none()
                    && intent.child.is_none(),
                "invalid administration launch intent"
            );
            let shared = layout.shared_writer()?;
            control::send(socket, &Granted { id }, Some(&shared))
        }
    }
}

/// Signal only the still-owned, unreaped guardian PID.
pub(super) fn signal(identity: ProcessIdentity) -> anyhow::Result<()> {
    let pid = rustix::process::Pid::from_raw(identity.pid).context("invalid guardian PID")?;
    match rustix::process::kill_process(pid, rustix::process::Signal::TERM) {
        Ok(()) | Err(rustix::io::Errno::SRCH) => Ok(()),
        Err(error) => Err(error.into()),
    }
}

/// Persist private monitor metadata without replacing any lock inode.
pub(super) fn publish_monitor(layout: &Layout, monitor: &Monitor) -> anyhow::Result<()> {
    let path = layout.state.join("monitor.json");
    if path.try_exists()? {
        drop(lifecycle::read_private(&path, layout.uid, MAX_CONTEXT)?);
    }
    let mut temporary = tempfile::NamedTempFile::new_in(layout.state.join("journal-staging"))?;
    temporary.write_all(&serde_json::to_vec(monitor)?)?;
    temporary.as_file().sync_all()?;
    drop(temporary.persist(path).map_err(|error| error.error)?);
    File::open(layout.state.join("journal-staging"))?.sync_all()?;
    File::open(&layout.state)?.sync_all()?;
    Ok(())
}

/// Report real listener/database initialization through the admitted parent channel.
pub(super) async fn initialized() -> anyhow::Result<()> {
    let Some(context) = APPLICATION.get().cloned() else {
        return Ok(());
    };
    tokio::task::spawn_blocking(move || {
        let layout = Layout::prepare(&context.data)?;
        let mut socket = control::connect(&context.endpoint, layout.uid, context.monitor)?;
        control::send(&mut socket, &Message::Initialized { id: context.id }, None)?;
        let (granted, _unexpected): (Granted, _) = control::receive(&mut socket, false)?;
        anyhow::ensure!(
            granted.id == context.id,
            "initialization acknowledgment belongs to another writer"
        );
        Ok::<_, anyhow::Error>(())
    })
    .await?
}

/// Only an admitted application can use built-in source control.
pub(super) fn automatic() -> bool {
    APPLICATION.get().is_some_and(|context| context.automatic)
}

/// Get the authenticated fixed context for application/controller IPC.
pub(super) fn application_context() -> anyhow::Result<Context> {
    APPLICATION
        .get()
        .cloned()
        .context("application has no admitted source root")
}
