//! Full same-executable update controller. The root monitor only supplies kernel lifecycle control.

use super::{
    bootstrap, control, coordinator,
    lifecycle::{Layout, ProcessIdentity},
    native::Context,
    transaction::native::{Engine, Service},
    Phase, Reply, Request,
};
use anyhow::Context as _;
use std::{
    io::{Read as _, Write as _},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

/// A root-selected controller initializes its full policy before reporting readiness.
pub(super) fn run(context: &Context) -> anyhow::Result<()> {
    let _logging = tracing_subscriber::fmt()
        .with_env_filter("info")
        .with_writer(std::io::stderr)
        .try_init();
    crate::config::configure_data_dir(Some(&context.data))?;
    crate::config::configure_port_override(context.port)?;
    drop(rustls::crypto::ring::default_provider().install_default());
    let layout = Layout::prepare(&context.data)?;
    let engine = Arc::new(bootstrap::engine(&layout)?);
    if engine.config.settings_path.try_exists()? {
        engine.validate_layout()?;
    } else {
        anyhow::ensure!(
            !context.data.join("chan.db").try_exists()?,
            "existing data is missing settings"
        );
    }
    let listener = control::listen(&format!("rustchan-controller-{}", context.id))?;
    listener.set_nonblocking(true)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let entered = runtime.enter();
    let listener = tokio::net::UnixListener::from_std(listener)?;
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    drop(entered);
    drop(coordinator::call(
        context,
        coordinator::Request::ControllerReady {
            version: super::VERSION.to_owned(),
            epoch: coordinator::EPOCH,
        },
    )?);
    let recovered = Arc::new(AtomicBool::new(false));
    let busy = Arc::new(AtomicBool::new(false));
    let slots = Arc::new(tokio::sync::Semaphore::new(16));
    runtime.block_on(async {
        let mut activated = false;
        let mut closing = false;
        loop {
            let observed = coordinator::call(context, coordinator::Request::View)?;
            let view = observed.view.context("monitor omitted controller view")?;
            let active = view.controller.as_ref().is_some_and(|current| current.id == context.id);
            if active && !activated {
                activated = true;
                let status = engine.status()?;
                let warm = view.application.as_ref().is_some_and(|application| application.initialized)
                    && !status.phase.active() && status.phase != Phase::FailedManualIntervention;
                if warm {
                    // Complete initialization happened while standby. The canonical terminal
                    // journal already committed this exact controller and ready application.
                    recovered.store(true, Ordering::Release);
                    drop(coordinator::call(context, coordinator::Request::Activated)?);
                } else {
                    start_recovery(Arc::clone(&engine), context.clone(), Arc::clone(&recovered), &busy)?;
                }
            }
            if (!active && activated || closing) && !busy.load(Ordering::Acquire) {
                return Ok::<_, anyhow::Error>(());
            }
            tokio::select! {
                accepted = listener.accept() => {
                    let (socket, _address) = accepted?;
                    let Ok(permit) = Arc::clone(&slots).try_acquire_owned() else { continue; };
                    let worker_engine = Arc::clone(&engine);
                    let worker_context = context.clone();
                    let worker_recovered = Arc::clone(&recovered);
                    let worker_busy = Arc::clone(&busy);
                    drop(tokio::spawn(async move {
                        let _permit = permit;
                        let result = exchange(socket, worker_engine, worker_context, worker_recovered, worker_busy).await;
                        if let Err(error) = result {
                            tracing::warn!(%error, "source controller request rejected");
                        }
                    }));
                }
                _signal = terminate.recv() => { closing = true; recovered.store(false, Ordering::Release); }
                () = tokio::time::sleep(Duration::from_millis(100)) => {}
            }
        }
    })
}

/// A worker lifetime stays visible until its complete journal operation finishes.
struct Activity(Arc<AtomicBool>);
impl Drop for Activity {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

/// Run recovery before any controlled start; full source backups retain the existing engine.
fn start_recovery(
    engine: Arc<Engine>,
    context: Context,
    recovered: Arc<AtomicBool>,
    busy: &Arc<AtomicBool>,
) -> anyhow::Result<()> {
    busy.store(true, Ordering::Release);
    let activity = Activity(Arc::clone(busy));
    let _worker = std::thread::Builder::new()
        .name("rustchan-source-recovery".into())
        .spawn(move || {
            let _activity = activity;
            let service = NativeService { context };
            let result = (|| {
                engine.recover(&service)?;
                let status = engine.status()?;
                anyhow::ensure!(
                    status.phase != Phase::FailedManualIntervention,
                    "manual recovery remains required"
                );
                let view = service.view()?;
                if view.application.is_none() {
                    service.start()?;
                }
                service.health(&status.installed, crate::db::baseline_schema_version())?;
                service.prepare_commit(&status.installed)?;
                service.commit()?;
                Ok::<_, anyhow::Error>(())
            })();
            if let Err(error) = result {
                tracing::error!(%error, "source startup recovery failed; admission remains closed");
            } else {
                recovered.store(true, Ordering::Release);
            }
        })?;
    Ok(())
}

/// Authorize the exact running application and bound full admin IPC independently of core frames.
async fn exchange(
    mut socket: tokio::net::UnixStream,
    engine: Arc<Engine>,
    context: Context,
    recovered: Arc<AtomicBool>,
    busy: Arc<AtomicBool>,
) -> anyhow::Result<()> {
    let peer = socket.peer_cred()?;
    anyhow::ensure!(
        peer.uid() == engine.config.web_uid,
        "controller peer has a different account"
    );
    let pid = peer.pid().context("controller peer omitted PID")?;
    let peer_identity = ProcessIdentity::read(pid)?;
    let view = coordinator::call(&context, coordinator::Request::View)?
        .view
        .context("monitor omitted view")?;
    anyhow::ensure!(
        view.application
            .as_ref()
            .is_some_and(|application| application.identity == peer_identity),
        "controller caller is not the admitted application"
    );
    let mut bytes = Vec::new();
    let _received = tokio::time::timeout(
        Duration::from_secs(5),
        (&mut socket).take(4097).read_to_end(&mut bytes),
    )
    .await??;
    anyhow::ensure!(bytes.len() <= 4096, "controller request exceeds limit");
    let request: Request = serde_json::from_slice(&bytes)?;
    let reply =
        tokio::task::spawn_blocking(move || handle(&engine, &context, &recovered, &busy, request))
            .await??;
    let reply_bytes = serde_json::to_vec(&reply)?;
    anyhow::ensure!(
        reply_bytes.len() <= 512 * 1024,
        "controller reply exceeds limit"
    );
    socket.write_all(&reply_bytes).await?;
    socket.shutdown().await?;
    Ok(())
}

/// Full policy operations are available only in the selected initialized controller.
#[expect(
    clippy::too_many_lines,
    reason = "closed authenticated operations share one controller authorization and bounded reply boundary"
)]
fn handle(
    engine: &Arc<Engine>,
    context: &Context,
    recovered: &Arc<AtomicBool>,
    busy: &Arc<AtomicBool>,
    request: Request,
) -> anyhow::Result<Reply> {
    let service = NativeService {
        context: context.clone(),
    };
    let result = (|| {
        let view = service.view()?;
        anyhow::ensure!(
            view.controller
                .as_ref()
                .is_some_and(|controller| controller.id == context.id),
            "controller is no longer active"
        );
        match request {
            Request::Status => Ok((
                engine.status()?,
                view.public && recovered.load(Ordering::Acquire),
            )),
            Request::Started {
                instance,
                configuration,
            } => {
                anyhow::ensure!(
                    view.application
                        .as_ref()
                        .is_some_and(|application| application.initialized),
                    "application did not initialize"
                );
                Ok((engine.started(instance, &configuration)?, false))
            }
            Request::Check => {
                anyhow::ensure!(
                    recovered.load(Ordering::Acquire) && !busy.load(Ordering::Acquire),
                    "source recovery or installation is active"
                );
                Ok((engine.check()?, false))
            }
            Request::Install {
                approval,
                administrator,
            } => {
                anyhow::ensure!(
                    recovered.load(Ordering::Acquire)
                        && busy
                            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                            .is_ok(),
                    "source recovery or another operation is active"
                );
                let activity = Activity(Arc::clone(busy));
                let update = engine.lock()?;
                let (status, manifest) = engine.approve(&approval, administrator)?;
                let mut worker_status = status.clone();
                let worker_engine = Arc::clone(engine);
                let worker_recovered = Arc::clone(recovered);
                let worker_service = service;
                let _worker = std::thread::Builder::new()
                    .name("rustchan-source-update".into())
                    .spawn(move || {
                        let _activity = activity;
                        let _update = update;
                        std::thread::sleep(Duration::from_secs(1));
                        let result =
                            worker_engine.install(&mut worker_status, &manifest, &worker_service);
                        if let Err(error) = result {
                            worker_recovered.store(false, Ordering::Release);
                            tracing::error!(%error, "source update requires recovery");
                        }
                    })?;
                Ok((status, false))
            }
            Request::Restart {
                instance,
                administrator,
            } => {
                anyhow::ensure!(
                    recovered.load(Ordering::Acquire)
                        && busy
                            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                            .is_ok(),
                    "source recovery or another operation is active"
                );
                let activity = Activity(Arc::clone(busy));
                let (status, update, settings) =
                    engine.approve_restart(instance, administrator, &service)?;
                let mut worker_status = status.clone();
                let worker_engine = Arc::clone(engine);
                let worker_recovered = Arc::clone(recovered);
                let _worker = std::thread::Builder::new()
                    .name("rustchan-source-settings".into())
                    .spawn(move || {
                        let _activity = activity;
                        let _update = update;
                        let _settings = settings;
                        std::thread::sleep(Duration::from_secs(1));
                        let result = worker_engine.restart_settings(&mut worker_status, &service);
                        if let Err(error) = result {
                            worker_recovered.store(false, Ordering::Release);
                            tracing::error!(%error, "source settings restart requires recovery");
                        }
                    })?;
                Ok((status, false))
            }
            Request::Ready { version: _ } => {
                anyhow::bail!("startup admission belongs to the fixed root monitor")
            }
        }
    })();
    match result {
        Ok((status, ready)) => Ok(Reply {
            status,
            ready,
            error: None,
        }),
        Err(error) => {
            tracing::warn!(%error, "source controller operation failed");
            Ok(Reply { status: engine.status()?, ready: false,
                error: Some("Operation could not proceed. Recovery or another operation may be active; check controller logs.".into()) })
        }
    }
}

/// Full controller delegates all spawn/reap/admission decisions to the dedicated root thread.
struct NativeService {
    /// Startup-authenticated creating monitor and durable controller lifetime.
    context: Context,
}
impl NativeService {
    /// Read the small lifecycle view from the exact creating monitor.
    fn view(&self) -> anyhow::Result<coordinator::View> {
        coordinator::call(&self.context, coordinator::Request::View)?
            .view
            .context("monitor omitted view")
    }

    /// Bind bounded HTTP readiness to the actual admitted instance/configuration observation.
    fn probe(&self, version: &str, previous: Option<uuid::Uuid>) -> anyhow::Result<()> {
        let layout = Layout::prepare(&self.context.data)?;
        let client = reqwest::blocking::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            // Only fixed loopback readiness is queried. Locally generated and
            // public-host certificates need not authenticate 127.0.0.1; the
            // admitted process's private instance/version still bind the reply.
            .danger_accept_invalid_certs(true)
            .timeout(Duration::from_secs(2))
            .build()?;
        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(60) {
            let checked = (|| {
                let view = self.view()?;
                let application = view.application.context("application is not running")?;
                anyhow::ensure!(
                    application.version == version && application.initialized,
                    "application is not initialized"
                );
                let engine = bootstrap::engine(&layout)?;
                let configuration = bootstrap::configuration(&layout)?;
                let (scheme, port) = if configuration.tls.enabled {
                    ("https", configuration.tls.port)
                } else {
                    ("http", engine.config.health_port)
                };
                let response = client
                    .get(format!("{scheme}://127.0.0.1:{port}/readyz"))
                    .send()?
                    .error_for_status()?;
                let instance = response
                    .headers()
                    .get("x-rustchan-instance")
                    .and_then(|value| value.to_str().ok())
                    .and_then(|value| uuid::Uuid::parse_str(value).ok())
                    .context("readiness instance is missing")?;
                let running = engine.restart_store().running()?;
                anyhow::ensure!(
                    running.instance == instance && previous.is_none_or(|old| old != instance),
                    "readiness instance mismatch"
                );
                let mut bytes = Vec::new();
                response
                    .take(4097)
                    .read_to_end(&mut bytes)
                    .map(|_read| ())?;
                anyhow::ensure!(bytes.len() <= 4096, "readiness response exceeds limit");
                let observed: serde_json::Value = serde_json::from_slice(&bytes)?;
                anyhow::ensure!(
                    observed.get("status").and_then(serde_json::Value::as_str) == Some("ready")
                        && observed.get("version").and_then(serde_json::Value::as_str)
                            == Some(version),
                    "readiness version mismatch"
                );
                Ok::<_, anyhow::Error>(())
            })();
            if checked.is_ok() {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        anyhow::bail!("application failed bounded readiness/version validation")
    }
}

impl Service for NativeService {
    fn preflight(&self) -> anyhow::Result<()> {
        let view = self.view()?;
        anyhow::ensure!(
            view.controller
                .as_ref()
                .is_some_and(|controller| controller.id == self.context.id),
            "full controller is no longer active"
        );
        Ok(())
    }
    fn stop(&self) -> anyhow::Result<()> {
        coordinator::call(&self.context, coordinator::Request::Stop).map(|_ack| ())
    }
    fn start(&self) -> anyhow::Result<()> {
        coordinator::call(&self.context, coordinator::Request::Start).map(|_ack| ())
    }
    fn health(&self, version: &str, _schema: &str) -> anyhow::Result<()> {
        self.probe(version, None)
    }
    fn health_instance(&self, version: &str, previous: uuid::Uuid) -> anyhow::Result<()> {
        self.probe(version, Some(previous))
    }
    fn prepare_commit(&self, version: &str) -> anyhow::Result<()> {
        coordinator::call(
            &self.context,
            coordinator::Request::Prepare {
                version: version.to_owned(),
            },
        )
        .map(|_ack| ())
    }
    fn commit(&self) -> anyhow::Result<()> {
        coordinator::call(&self.context, coordinator::Request::Commit).map(|_ack| ())
    }
}

/// Send full controller requests only through the startup-authenticated root locator.
pub(super) async fn request(request: &Request) -> anyhow::Result<Reply> {
    let context = super::native::application_context()?;
    let request = request.clone();
    tokio::task::spawn_blocking(move || {
        if let Request::Ready { version } = request {
            drop(coordinator::call(
                &context,
                coordinator::Request::CanStart { version },
            )?);
            return Ok(Reply {
                status: super::Status::default(),
                ready: true,
                error: None,
            });
        }
        let layout = Layout::prepare(&context.data)?;
        let view = coordinator::call(&context, coordinator::Request::View)?
            .view
            .context("monitor omitted controller view")?;
        let controller = view
            .controller
            .context("full controller is not initialized")?;
        let mut socket = control::connect(&controller.endpoint, layout.uid, controller.identity)?;
        socket.set_read_timeout(Some(Duration::from_secs(45)))?;
        socket.write_all(&serde_json::to_vec(&request)?)?;
        socket.shutdown(std::net::Shutdown::Write)?;
        let mut bytes = Vec::new();
        socket
            .take(512 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map(|_read| ())?;
        anyhow::ensure!(bytes.len() <= 512 * 1024, "controller reply exceeds limit");
        Ok::<_, anyhow::Error>(serde_json::from_slice(&bytes)?)
    })
    .await?
}
