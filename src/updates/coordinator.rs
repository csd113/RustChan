//! Stable ordinary-account process monitor; full update policy lives in a selected controller.

use super::{
    bootstrap, control, guardian,
    lifecycle::{self, Layout, LifePhase, Lifetime, ProcessIdentity, WriterRole},
    native::{Context, Granted, Message, Monitor, Role, Signals},
};
use anyhow::Context as _;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fs::{self, File},
    os::unix::net::{UnixListener, UnixStream},
    process::{Child, Command},
    time::{Duration, Instant},
};
use uuid::Uuid;

/// Frozen small monitor contract, distinct from the full controller package version.
pub(super) const EPOCH: u32 = 1;
/// Lifecycle drain may exceed a short ordinary IPC exchange.
const DEADLINE: Duration = Duration::from_secs(90);

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields, tag = "action", rename_all = "snake_case")]
/// No operation accepts a path, URL, executable, environment or arbitrary command.
pub(super) enum Request {
    /// Locate the current controller and admitted application.
    View,
    /// Require exact controlled application startup before migration.
    CanStart {
        /// Exact package version of the admitted application.
        version: String,
    },
    /// Check public request admission independently of readiness probing.
    PublicAdmission,
    /// Initialize one root-selected full controller before it can become active.
    ControllerReady {
        /// Exact selected full controller package version.
        version: String,
        /// Frozen small monitor contract understood by this controller.
        epoch: u32,
    },
    /// A prepared controller has entered its full active event loop.
    Activated,
    /// Close admission and prove every application/CLI writer exited.
    Stop,
    /// Spawn only the fixed validated active version.
    Start,
    /// Initialize the selected full controller before canonical data/code commit.
    Prepare {
        /// Fixed retained version to initialize before canonical commit.
        version: String,
    },
    /// Open admission only after the canonical terminal journal is durable.
    Commit,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
/// Kernel identity and fixed endpoint of the active full controller.
pub(super) struct Controller {
    /// Root-created durable guardian lifetime.
    pub id: Uuid,
    /// Registered primary process identity checked against socket credentials.
    pub identity: ProcessIdentity,
    /// Abstract socket derived solely from the durable lifetime identifier.
    pub endpoint: String,
    /// Package version of the selected full controller.
    pub version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
/// Only the root-owned exact application may report startup or use the controller.
pub(super) struct Application {
    /// Registered primary process identity checked against socket credentials.
    pub identity: ProcessIdentity,
    /// Package version of the selected application.
    pub version: String,
    /// Listener/database initialization acknowledged by the owned application.
    pub initialized: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
/// Small monitor facts contain no settings, release notes or application data.
pub(super) struct View {
    /// Fully initialized active controller, absent during its initial launch.
    pub controller: Option<Controller>,
    /// Registered application, absent until the guardian records its primary.
    pub application: Option<Application>,
    /// Whether canonical commit and activation permit external traffic.
    pub public: bool,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
/// Bounded core acknowledgment; larger admin status stays in controller IPC.
pub(super) struct Reply {
    /// Small selected-process facts for locator and lifecycle queries.
    pub view: Option<View>,
    /// Fixed operation acknowledgment or admission decision.
    pub ready: bool,
    /// Bounded failure preventing lifecycle admission.
    pub error: Option<String>,
}

/// A guardian owns all descendants of this root-selected actor.
struct Actor {
    /// Owned unreaped guardian; only the dedicated root thread waits on it.
    child: Child,
    /// Kernel identity authenticated during descriptor handoff.
    guardian: ProcessIdentity,
    /// Durable launch identity and fixed actor class.
    lifetime: Lifetime,
    /// Retained program version selected before spawn.
    version: String,
    /// Application/controller initialization acknowledged by its exact primary.
    initialized: bool,
}

/// A caller retains its connection while the dedicated main thread drains or initializes.
enum Pending {
    /// Wait for all non-controller writers to finish before snapshot/restore.
    Stop {
        /// Retained authenticated caller connection for the final acknowledgment.
        socket: UnixStream,
        /// Absolute bounded-operation start observation.
        started: Instant,
    },
    /// Wait for the fixed selected replacement controller to initialize.
    Prepare {
        /// Retained authenticated caller connection for the final acknowledgment.
        socket: UnixStream,
        /// Root-created replacement controller lifetime.
        id: Uuid,
        /// Absolute bounded-operation start observation.
        started: Instant,
    },
    /// Wait for the prepared full controller to enter its active event loop.
    Commit {
        /// Retained authenticated caller connection for the final acknowledgment.
        socket: UnixStream,
        /// Initialized controller selected by the canonical terminal journal.
        id: Uuid,
        /// Absolute bounded-operation start observation.
        started: Instant,
    },
}

/// Only the main thread creates processes or reaps its owned guard subtrees.
struct Root<'a> {
    /// Canonical owned data, journal and retained executable namespace.
    layout: &'a Layout,
    /// Private fixed core socket owned by this invocation.
    listener: UnixListener,
    /// Abstract address transferred only through authenticated startup context.
    endpoint: String,
    /// Dedicated long-lived main spawner identity.
    identity: ProcessIdentity,
    /// Close-only shared OFD lease, converted only after quiescence proof.
    lease: File,
    /// Root-owned unreaped guardian subtrees.
    actors: HashMap<Uuid, Actor>,
    /// Selected initialized full controller lifetime.
    controller: Option<Uuid>,
    /// Replacement initialized before canonical code/data commit.
    prepared: Option<Uuid>,
    /// Current application guardian lifetime.
    application: Option<Uuid>,
    /// One lifecycle acknowledgment whose proof remains pending.
    pending: Option<Pending>,
    /// External admission opens only after durable commit and activation.
    public: bool,
    /// Normal shutdown prevents further grants or spawns.
    stopping: bool,
    /// Exit after all owned guardians prove complete subtree cleanup.
    exit_requested: bool,
    /// Expected original path bytes; unexpected local rebuilds are preserved.
    original_hash: String,
}

/// Own the one foreground/service invocation and always bootstrap the committed full controller.
pub(super) fn run(
    layout: &Layout,
    admission: File,
    lease: File,
    signals: Signals,
) -> anyhow::Result<bool> {
    let endpoint = format!("rustchan-root-{}", Uuid::new_v4());
    let listener = control::listen(&endpoint)?;
    listener.set_nonblocking(true)?;
    rustix::process::set_child_subreaper(Some(rustix::process::Pid::INIT))?;
    let status = bootstrap::engine(layout)?.status()?;
    let mut root = Root {
        layout,
        listener,
        endpoint,
        identity: ProcessIdentity::current()?,
        lease,
        actors: HashMap::new(),
        controller: None,
        prepared: None,
        application: None,
        pending: None,
        public: false,
        stopping: false,
        exit_requested: false,
        original_hash: bootstrap::digest(&bootstrap::original_program(layout)?)?,
    };
    let current = root.spawn(WriterRole::Controller, &status.installed)?;
    root.controller = Some(current);
    let Signals {
        runtime,
        mut terminate,
        mut interrupt,
    } = signals;
    let result = runtime.block_on(async {
        loop {
            root.reap()?;
            root.advance()?;
            if root.exit_requested && root.actors.is_empty() {
                return Ok::<_, anyhow::Error>(());
            }
            let accepted = super::native::accept(&root.listener)?;
            if let Some(mut socket) = accepted {
                let handled = root.dispatch(&mut socket);
                match handled {
                    Ok(Some(pending)) => root.pending = Some(pending),
                    Ok(None) => {}
                    Err(error) => tracing::warn!(%error, "native core request rejected"),
                }
            }
            tokio::select! {
                _signal = terminate.recv() => root.shutdown()?,
                _signal = interrupt.recv() => root.shutdown()?,
                () = tokio::time::sleep(Duration::from_millis(25)) => {}
            }
        }
    });
    if result.is_err() {
        root.public = false;
        runtime.block_on(guardian::drain_remaining(terminate, interrupt))?;
        // Complete kernel ECHILD proof covers all this root's possible ordinary descendants.
        for actor in root.actors.values() {
            let mut record = layout.lifetime(actor.lifetime.id)?;
            anyhow::ensure!(record.parent == root.identity, "actor ownership changed");
            record.phase = LifePhase::Stopped;
            layout.save_lifetime(&record)?;
        }
    }
    let metadata = layout.state.join("monitor.json");
    if metadata.try_exists()? {
        fs::remove_file(metadata)?;
        File::open(&layout.state)?.sync_all()?;
    }
    drop(root);
    drop(admission);
    result?;
    Ok(true)
}

impl Root<'_> {
    /// Commands derive only from one fixed namespace and the original launcher arguments.
    fn spawn(&mut self, role: WriterRole, version: &str) -> anyhow::Result<Uuid> {
        let program = bootstrap::version_binary(self.layout, version)?;
        let lifetime = self.layout.launch(role)?;
        let context = Context {
            role: Role::Guardian,
            data: self.layout.data.clone(),
            id: lifetime.id,
            parent: self.identity,
            monitor: self.identity,
            endpoint: self.endpoint.clone(),
            automatic: true,
            port: crate::config::launcher_port(),
        };
        let mut command = Command::new(program);
        if role == WriterRole::Controller {
            let _arguments = command.arg("--native-controller");
        } else {
            let _arguments = command.args(std::env::args_os().skip(1));
        }
        let _environment = command
            .env(super::native::INTERNAL, serde_json::to_string(&context)?)
            .env("RUSTCHAN_SPAWNED", "1");
        let child = match command.spawn() {
            Ok(child) => child,
            Err(error) => {
                let mut stopped = lifetime;
                stopped.phase = LifePhase::Stopped;
                self.layout.save_lifetime(&stopped)?;
                return Err(error.into());
            }
        };
        let guardian = ProcessIdentity::read(i32::try_from(child.id())?)?;
        let id = lifetime.id;
        let _previous = self.actors.insert(
            id,
            Actor {
                child,
                guardian,
                lifetime,
                version: version.to_owned(),
                initialized: false,
            },
        );
        Ok(id)
    }

    /// Kernel peer credentials bind every frame to a selected guardian or its exact primary.
    fn dispatch(&mut self, socket: &mut UnixStream) -> anyhow::Result<Option<Pending>> {
        let peer = rustix::net::sockopt::socket_peercred(&*socket)?;
        anyhow::ensure!(
            peer.uid.as_raw() == self.layout.uid,
            "different native core owner"
        );
        let identity = ProcessIdentity::read(peer.pid.as_raw_nonzero().get())?;
        let (message, _unexpected): (Message, _) = control::receive(socket, false)?;
        match message {
            Message::Guardian { id } => {
                let actor = self.actors.get(&id).context("unknown native guardian")?;
                control::authorize(socket, self.layout.uid, actor.guardian)?;
                anyhow::ensure!(!self.stopping, "native shutdown has started");
                control::send(socket, &Granted { id }, Some(&self.lease))?;
            }
            Message::Initialized { id } => {
                let actor = self
                    .actors
                    .get_mut(&id)
                    .context("unknown native application")?;
                let record = self.layout.lifetime(id)?;
                anyhow::ensure!(
                    record.role == WriterRole::Server
                        && record.child == Some(identity)
                        && record.guardian == Some(actor.guardian)
                        && self.application == Some(id),
                    "different initialized application"
                );
                actor.initialized = true;
                self.lease.lock_shared()?;
                control::send(socket, &Granted { id }, None)?;
            }
            Message::Administrator {
                id,
                version,
                program_sha256,
            } => {
                anyhow::ensure!(
                    self.public && !self.stopping && self.pending.is_none(),
                    "administration admission is closed"
                );
                let application = self.application.context("application is unavailable")?;
                let actor = self
                    .actors
                    .get(&application)
                    .context("application is unavailable")?;
                let program = bootstrap::version_binary(self.layout, &actor.version)?;
                anyhow::ensure!(
                    actor.version == version && bootstrap::digest(&program)? == program_sha256,
                    "administration executable differs from the active application"
                );
                let record = self.layout.lifetime(id)?;
                anyhow::ensure!(
                    record.role == WriterRole::Administrator
                        && record.phase == LifePhase::Launching
                        && record.parent == identity
                        && record.guardian.is_none()
                        && record.child.is_none(),
                    "invalid administration intent"
                );
                let shared = self.layout.shared_writer()?;
                control::send(socket, &Granted { id }, Some(&shared))?;
            }
            Message::Core { request } => {
                let id = self.primary(identity)?;
                return self.core(socket, id, request);
            }
        }
        Ok(None)
    }

    /// Find only an owned exact primary identity; missing PIDs never clear lifetime records.
    fn primary(&self, identity: ProcessIdentity) -> anyhow::Result<Uuid> {
        for actor in self.actors.values() {
            let record = self.layout.lifetime(actor.lifetime.id)?;
            if record.child == Some(identity) && record.guardian == Some(actor.guardian) {
                return Ok(record.id);
            }
        }
        anyhow::bail!("core caller is not an admitted primary")
    }

    /// Small fixed core operations; full discovery, signing, snapshots and schema policy stay outside.
    #[expect(
        clippy::too_many_lines,
        reason = "the closed monitor protocol keeps authentication, lifecycle state and acknowledgments in one reviewable dispatch boundary"
    )]
    fn core(
        &mut self,
        socket: &mut UnixStream,
        id: Uuid,
        request: Request,
    ) -> anyhow::Result<Option<Pending>> {
        match request {
            Request::View => Self::reply(socket, Some(self.view()?), true)?,
            Request::CanStart { version } => {
                let actor = self.actors.get(&id).context("missing application")?;
                Self::reply(
                    socket,
                    None,
                    self.application == Some(id) && actor.version == version && !self.stopping,
                )?;
            }
            Request::PublicAdmission => Self::reply(
                socket,
                None,
                self.application == Some(id) && self.public && !self.stopping,
            )?,
            Request::ControllerReady { version, epoch } => {
                let actor = self.actors.get_mut(&id).context("missing controller")?;
                anyhow::ensure!(
                    actor.lifetime.role == WriterRole::Controller
                        && actor.version == version
                        && epoch == EPOCH,
                    "controller monitor contract mismatch"
                );
                actor.initialized = true;
                Self::reply(socket, None, true)?;
            }
            Request::Activated => {
                anyhow::ensure!(
                    self.controller == Some(id)
                        && self.actors.get(&id).is_some_and(|actor| actor.initialized),
                    "controller is not active and initialized"
                );
                let pending = self.pending.take();
                match pending {
                    Some(Pending::Commit {
                        socket: mut committed,
                        id: expected,
                        started: _,
                    }) if expected == id => {
                        self.open_committed()?;
                        Self::reply(&mut committed, None, true)?;
                    }
                    other => self.pending = other,
                }
                Self::reply(socket, None, true)?;
            }
            Request::Stop => {
                self.authorize_controller(id)?;
                anyhow::ensure!(self.pending.is_none(), "another core command is pending");
                self.public = false;
                if let Some(app) = self.application {
                    super::native::signal(
                        self.actors
                            .get(&app)
                            .context("missing application")?
                            .guardian,
                    )?;
                }
                return Ok(Some(Pending::Stop {
                    socket: socket.try_clone()?,
                    started: Instant::now(),
                }));
            }
            Request::Start => {
                self.authorize_controller(id)?;
                anyhow::ensure!(
                    self.application.is_none() && self.pending.is_none() && !self.stopping,
                    "application start is not quiescent"
                );
                self.quiescent()?;
                let version = bootstrap::engine(self.layout)?.current_version()?;
                bootstrap::enroll_candidate(self.layout, &version)?;
                self.application = Some(self.spawn(WriterRole::Server, &version)?);
                Self::reply(socket, None, true)?;
            }
            Request::Prepare { version } => {
                self.authorize_controller(id)?;
                anyhow::ensure!(
                    self.pending.is_none()
                        && bootstrap::digest(&bootstrap::original_program(self.layout)?)?
                            == self.original_hash,
                    "source changed or another core command is pending"
                );
                let app = self
                    .application
                    .and_then(|app| self.actors.get(&app))
                    .context("application is unavailable")?;
                anyhow::ensure!(
                    app.initialized && app.version == version,
                    "candidate did not initialize"
                );
                if self
                    .actors
                    .get(&id)
                    .is_some_and(|actor| actor.version == version && actor.initialized)
                {
                    self.prepared = Some(id);
                    Self::reply(socket, None, true)?;
                } else {
                    bootstrap::enroll_candidate(self.layout, &version)?;
                    let prepared = self.spawn(WriterRole::Controller, &version)?;
                    self.prepared = Some(prepared);
                    return Ok(Some(Pending::Prepare {
                        socket: socket.try_clone()?,
                        id: prepared,
                        started: Instant::now(),
                    }));
                }
            }
            Request::Commit => {
                self.authorize_controller(id)?;
                anyhow::ensure!(self.pending.is_none(), "another core command is pending");
                let status = bootstrap::engine(self.layout)?.status()?;
                anyhow::ensure!(
                    !status.phase.active()
                        && status.phase != super::Phase::FailedManualIntervention,
                    "canonical commit is not terminal"
                );
                let prepared = self
                    .prepared
                    .or(self.controller)
                    .context("controller is unavailable")?;
                let actor = self
                    .actors
                    .get(&prepared)
                    .context("prepared controller is unavailable")?;
                anyhow::ensure!(
                    actor.initialized && actor.version == status.installed,
                    "newest controller is not initialized"
                );
                self.controller = Some(prepared);
                self.prepared = None;
                if prepared == id {
                    self.open_committed()?;
                    Self::reply(socket, None, true)?;
                } else {
                    return Ok(Some(Pending::Commit {
                        socket: socket.try_clone()?,
                        id: prepared,
                        started: Instant::now(),
                    }));
                }
            }
        }
        Ok(None)
    }

    /// Only the initialized active full controller may change controlled lifecycle state.
    fn authorize_controller(&self, id: Uuid) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.controller == Some(id)
                && self.actors.get(&id).is_some_and(|actor| actor.initialized),
            "only the active full controller may control lifecycle"
        );
        Ok(())
    }

    /// Keep root-owned controller guards while proving every other writer completed.
    fn quiescent(&self) -> anyhow::Result<()> {
        self.lease
            .try_lock()
            .context("independent administration writers remain active")?;
        let controllers: Vec<_> = self
            .actors
            .values()
            .filter(|actor| actor.lifetime.role == WriterRole::Controller)
            .map(|actor| actor.lifetime.id)
            .collect();
        self.layout
            .prove_quiescence_except(lifecycle::kernel_boot_id()?, &controllers)
    }

    /// Durable terminal code/data selection precedes source publication and public admission.
    fn open_committed(&mut self) -> anyhow::Result<()> {
        let status = bootstrap::engine(self.layout)?.status()?;
        let app = self
            .application
            .and_then(|id| self.actors.get(&id))
            .context("committed application is unavailable")?;
        anyhow::ensure!(
            app.initialized
                && app.version == status.installed
                && !status.phase.active()
                && status.phase != super::Phase::FailedManualIntervention,
            "committed application mismatch"
        );
        self.original_hash =
            bootstrap::publish_original(self.layout, &status.installed, &self.original_hash)?;
        super::native::publish_monitor(
            self.layout,
            &Monitor {
                format: lifecycle::FORMAT,
                boot: lifecycle::kernel_boot_id()?,
                identity: self.identity,
                endpoint: self.endpoint.clone(),
                version: app.version.clone(),
                program_sha256: self.original_hash.clone(),
            },
        )?;
        self.public = true;
        Ok(())
    }

    /// Poll acknowledgment predicates on the sole spawner/reaper thread.
    fn advance(&mut self) -> anyhow::Result<()> {
        let Some(pending) = self.pending.take() else {
            return Ok(());
        };
        if let Pending::Prepare {
            id,
            started,
            socket: _,
        } = &pending
        {
            if !self.actors.contains_key(id) || started.elapsed() >= DEADLINE {
                if let Pending::Prepare {
                    mut socket,
                    id: _,
                    started: _,
                } = pending
                {
                    self.prepared = None;
                    control::send(
                        &mut socket,
                        &Reply {
                            view: None,
                            ready: false,
                            error: Some("full replacement controller could not initialize".into()),
                        },
                        None,
                    )?;
                    return Ok(());
                }
            }
        }
        let finished = match &pending {
            Pending::Stop { started, socket: _ } => {
                anyhow::ensure!(
                    started.elapsed() < DEADLINE,
                    "writer shutdown remains unproved"
                );
                self.application.is_none() && self.quiescent().is_ok()
            }
            Pending::Prepare {
                id,
                started,
                socket: _,
            } => {
                anyhow::ensure!(
                    started.elapsed() < DEADLINE && self.actors.contains_key(id),
                    "full controller initialization failed"
                );
                self.actors.get(id).is_some_and(|actor| actor.initialized)
            }
            Pending::Commit {
                started,
                id: _,
                socket: _,
            } => {
                anyhow::ensure!(
                    started.elapsed() < DEADLINE,
                    "new controller activation acknowledgment was lost"
                );
                false
            }
        };
        if finished {
            let mut socket = match pending {
                Pending::Stop { socket, started: _ }
                | Pending::Prepare {
                    socket,
                    started: _,
                    id: _,
                }
                | Pending::Commit {
                    socket,
                    started: _,
                    id: _,
                } => socket,
            };
            Self::reply(&mut socket, None, true)?;
        } else {
            self.pending = Some(pending);
        }
        Ok(())
    }

    /// Trust normal completion only with the guardian's durable all-descendant proof.
    fn reap(&mut self) -> anyhow::Result<()> {
        let mut completed = Vec::new();
        for (id, actor) in &mut self.actors {
            if let Some(status) = actor.child.try_wait()? {
                let record = self.layout.lifetime(*id)?;
                anyhow::ensure!(
                    record.phase == LifePhase::Stopped && record.parent == self.identity,
                    "guardian was lost without descendant-exit proof"
                );
                completed.push((*id, status.success()));
            }
        }
        for (id, success) in completed {
            let _actor = self.actors.remove(&id);
            if self.application == Some(id) {
                self.application = None;
                self.public = false;
                if success
                    && self.pending.is_none()
                    && !bootstrap::engine(self.layout)?.status()?.phase.active()
                {
                    self.shutdown()?;
                }
            }
            if self.controller == Some(id) && !self.exit_requested {
                anyhow::bail!("active full controller exited");
            }
        }
        Ok(())
    }

    /// Stop only owned unreaped guard PIDs; their guards prove complete subtree completion.
    fn shutdown(&mut self) -> anyhow::Result<()> {
        self.public = false;
        self.stopping = true;
        self.exit_requested = true;
        for actor in self.actors.values() {
            super::native::signal(actor.guardian)?;
        }
        Ok(())
    }

    /// Return only selected initialized actors bound to actual registered child identities.
    fn view(&self) -> anyhow::Result<View> {
        let controller = self
            .controller
            .and_then(|id| self.actors.get(&id))
            .filter(|actor| actor.initialized)
            .map(|actor| {
                let identity = self
                    .layout
                    .lifetime(actor.lifetime.id)?
                    .child
                    .context("controller primary missing")?;
                Ok::<_, anyhow::Error>(Controller {
                    id: actor.lifetime.id,
                    identity,
                    endpoint: format!("rustchan-controller-{}", actor.lifetime.id),
                    version: actor.version.clone(),
                })
            })
            .transpose()?;
        let application = self
            .application
            .and_then(|id| self.actors.get(&id))
            .map(|actor| {
                let identity = self.layout.lifetime(actor.lifetime.id)?.child;
                Ok::<_, anyhow::Error>(identity.map(|identity| Application {
                    identity,
                    version: actor.version.clone(),
                    initialized: actor.initialized,
                }))
            })
            .transpose()?
            .flatten();
        Ok(View {
            controller,
            application,
            public: self.public,
        })
    }

    /// Send a bounded no-descriptor response while retaining every root lease.
    fn reply(socket: &mut UnixStream, view: Option<View>, ready: bool) -> anyhow::Result<()> {
        control::send(
            socket,
            &Reply {
                view,
                ready,
                error: None,
            },
            None,
        )
    }
}

/// Root calls are authenticated to the creating monitor and never select their endpoint via HTTP.
pub(super) fn call(context: &Context, request: Request) -> anyhow::Result<Reply> {
    let layout = Layout::prepare(&context.data)?;
    let mut socket = control::connect(&context.endpoint, layout.uid, context.monitor)?;
    control::send(&mut socket, &Message::Core { request }, None)?;
    let (reply, _unexpected): (Reply, _) =
        control::receive_with_timeout(&mut socket, false, Duration::from_secs(120))?;
    anyhow::ensure!(
        reply.error.is_none() && reply.ready,
        "native core denied lifecycle operation"
    );
    Ok(reply)
}
