#[cfg(unix)]
use super::transaction::native::{Config, Engine, Service};
#[cfg(unix)]
use anyhow::Context as _;
use std::path::Path;

/// Runs the operator-configured updater. Linux only; never invoked by HTTP.
///
/// # Errors
/// Rejects unsupported deployments, invalid ownership/trust material, and IPC/service failures.
pub async fn run(config_path: &Path) -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        anyhow::ensure!(cfg!(target_os = "linux"), "rustchan-updater requires Linux");
        anyhow::ensure!(
            !super::container_managed(),
            "containers remain deployment-managed"
        );
        linux::run(config_path).await
    }
    #[cfg(not(unix))]
    {
        let _ = config_path;
        anyhow::bail!("rustchan-updater supports managed native Linux installations only")
    }
}

#[cfg(unix)]
/// Unix implementation of the Linux-only managed updater.
mod linux {
    use super::super::{Reply, Request, SOCKET};
    use super::*;
    use std::fs;
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
    use std::process::{Command, Stdio};
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };
    use std::time::{Duration, Instant};
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    #[derive(Default)]
    /// Recovery/startup and installation admission shared with IPC workers.
    struct Admission {
        /// Set only after boot recovery has finished durably.
        recovered: AtomicBool,
        /// Allows only the controlled service start to pass boot admission.
        starting: AtomicBool,
        /// Tracks the installation worker lifetime.
        installing: AtomicBool,
    }
    impl Admission {
        /// Authorize startup only in a phase that has safe persistent state.
        fn may_start(&self, phase: super::super::Phase) -> bool {
            (self.recovered.load(Ordering::Acquire) || self.starting.load(Ordering::Acquire))
                && phase.may_start()
        }
    }

    /// Validate operator-owned configuration and trust material before binding the socket.
    fn configured_engine(config_path: &Path) -> anyhow::Result<Engine> {
        super::super::transaction::native::protected_ancestors(config_path, u32::MAX)?;
        let metadata = fs::symlink_metadata(config_path)?;
        anyhow::ensure!(
            metadata.is_file() && metadata.uid() == 0 && metadata.mode() & 0o022 == 0,
            "updater configuration must be a root-owned non-writable regular file"
        );
        let config: Config = toml::from_str(&fs::read_to_string(config_path)?)
            .map_err(|_| anyhow::anyhow!("invalid updater configuration"))?;
        super::super::transaction::native::protected_ancestors(&config.public_key, config.web_uid)?;
        let metadata = fs::symlink_metadata(&config.public_key)?;
        anyhow::ensure!(
            metadata.is_file() && metadata.uid() == 0 && metadata.mode() & 0o022 == 0,
            "release trust key must be root owned and non-writable"
        );
        let key_text = fs::read_to_string(&config.public_key)?;
        let key_text = key_text.trim();
        anyhow::ensure!(
            key_text.len() == 64 && key_text.is_ascii(),
            "release key must be 32 bytes encoded as hexadecimal"
        );
        let key = hex::decode(key_text)?;
        anyhow::ensure!(
            fs::metadata(&config.data_dir)?.uid() == config.web_uid,
            "managed data directory must belong to the configured web identity"
        );
        let (identity, _peer) = tokio::net::UnixStream::pair()?;
        let updater_uid = identity.peer_cred()?.uid();
        anyhow::ensure!(
            updater_uid != 0 && updater_uid != config.web_uid,
            "updater must run as a dedicated unprivileged identity"
        );
        for directory in [&config.install_dir, &config.state_dir] {
            anyhow::ensure!(
                fs::metadata(directory)?.uid() == updater_uid,
                "managed installation/state must belong to the updater identity"
            );
        }
        let engine = Engine { config, key };
        engine.validate()?;
        super::super::transaction::native::protected_ancestors(
            Path::new(SOCKET)
                .parent()
                .context("missing socket parent")?,
            engine.config.web_uid,
        )?;
        Ok(engine)
    }

    /// Run the separate operator-configured Linux updater daemon.
    pub(super) async fn run(config_path: &Path) -> anyhow::Result<()> {
        let engine = Arc::new(configured_engine(config_path)?);
        // Exclusive daemon lock prevents two listeners/recovery workers. File
        // locks release on death, unlike pid files or lock directories.
        let daemon_lock = fs::File::options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(engine.config.state_dir.join("daemon.lock"))?;
        daemon_lock
            .try_lock()
            .context("updater daemon already running")?;
        match fs::symlink_metadata(SOCKET) {
            Ok(metadata) => {
                use std::os::unix::fs::FileTypeExt as _;
                anyhow::ensure!(
                    metadata.file_type().is_socket(),
                    "updater socket path has unexpected type"
                );
                fs::remove_file(SOCKET)?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        let listener = tokio::net::UnixListener::bind(SOCKET)?;
        // Shared socket group grants connection access; peer UID still restricts callers.
        std::os::unix::fs::chown(
            SOCKET,
            None,
            Some(fs::metadata(&engine.config.data_dir)?.gid()),
        )?;
        let socket_parent = Path::new(SOCKET)
            .parent()
            .context("missing socket parent")?;
        std::os::unix::fs::chown(
            socket_parent,
            None,
            Some(fs::metadata(&engine.config.data_dir)?.gid()),
        )?;
        fs::set_permissions(SOCKET, fs::Permissions::from_mode(0o660))?;
        // Listen while recovering: starting the application needs Ready IPC.
        let admission = Arc::new(Admission::default());
        let recovery_admission = Arc::clone(&admission);
        let recovery_engine = Arc::clone(&engine);
        tokio::task::spawn_blocking(move || {
            let service = SystemService {
                port: recovery_engine.config.health_port,
                admission: Arc::clone(&recovery_admission),
            };
            let recovery = recovery_engine.recover(&service);
            if let Err(error) = recovery {
                tracing::error!(error = %error, "updater journal recovery failed; application startup is blocked");
            } else {
                recovery_admission.recovered.store(true, Ordering::Release);
            }
        });
        let slots = Arc::new(tokio::sync::Semaphore::new(16));
        loop {
            let (mut socket, _) = listener.accept().await?;
            if !peer_authorized(&socket, engine.config.web_uid)? {
                continue;
            }
            let Ok(permit) = Arc::clone(&slots).try_acquire_owned() else {
                continue;
            };
            let engine = Arc::clone(&engine);
            let admission = Arc::clone(&admission);
            tokio::spawn(async move {
                let _permit = permit;
                let result = async {
                    let mut bytes = Vec::new();
                    tokio::time::timeout(
                        Duration::from_secs(5),
                        (&mut socket).take(4097).read_to_end(&mut bytes),
                    )
                    .await??;
                    anyhow::ensure!(bytes.len() <= 4096, "updater request exceeds limit");
                    let request: Request = serde_json::from_slice(&bytes)?;
                    let reply =
                        tokio::task::spawn_blocking(move || handle(&engine, &admission, request))
                            .await??;
                    socket.write_all(&serde_json::to_vec(&reply)?).await?;
                    socket.shutdown().await?;
                    Ok::<_, anyhow::Error>(())
                }
                .await;
                if let Err(error) = result {
                    tracing::warn!(error = %error, "updater IPC request rejected");
                }
            });
        }
    }

    /// Verify the kernel-provided peer UID rather than trusting any protocol field.
    fn peer_authorized(socket: &tokio::net::UnixStream, uid: u32) -> std::io::Result<bool> {
        Ok(socket.peer_cred()?.uid() == uid)
    }

    /// Dispatch a closed operation after peer authorization and bound request parsing.
    fn handle(
        engine: &Arc<Engine>,
        admission: &Arc<Admission>,
        request: Request,
    ) -> anyhow::Result<Reply> {
        let result = match request {
            Request::Status => engine.status().map(|status| {
                let writable = admission.recovered.load(Ordering::Acquire)
                    && !status.phase.blocks_writes()
                    && status.phase != super::super::Phase::FailedManualIntervention;
                (status, writable)
            }),
            Request::Ready { version } => engine.status().map(|status| {
                let ready = admission.may_start(status.phase)
                    && engine
                        .current_version()
                        .is_ok_and(|current| current == version);
                (status, ready)
            }),
            Request::Check => {
                anyhow::ensure!(
                    admission.recovered.load(Ordering::Acquire),
                    "recovery is incomplete"
                );
                engine.check().map(|status| (status, false))
            }
            Request::Install {
                approval,
                administrator,
            } => (|| {
                anyhow::ensure!(
                    admission.recovered.load(Ordering::Acquire),
                    "recovery is incomplete"
                );
                let lock = engine.lock()?;
                let (status, manifest) = engine.approve(&approval, administrator)?;
                admission.installing.store(true, Ordering::Release);
                let mut worker_status = status.clone();
                let worker_engine = Arc::clone(engine);
                let worker_admission = Arc::clone(admission);
                let spawned = std::thread::Builder::new().name("rustchan-update".into()).spawn(move || {
                    let worker_lock = lock;
                    let service = SystemService { port: worker_engine.config.health_port, admission: Arc::clone(&worker_admission) };
                    if let Err(error) = worker_engine.install(&mut worker_status, &manifest, &service) {
                        worker_admission.recovered.store(false, Ordering::Release);
                        tracing::error!(error = %error, "failed to persist update result; recovery required");
                    }
                    drop(worker_lock);
                    worker_admission.installing.store(false, Ordering::Release);
                });
                if let Err(error) = spawned {
                    let mut failed = status;
                    engine.save(
                        &mut failed,
                        super::super::Phase::Failed,
                        "Update worker could not start; previous version retained.",
                    )?;
                    admission.installing.store(false, Ordering::Release);
                    return Err(error.into());
                }
                Ok((status, false))
            })(),
        };
        match result {
            Ok((status, ready)) => Ok(Reply {
                status,
                error: None,
                ready,
            }),
            Err(error) => {
                tracing::warn!(error = %error, "updater operation failed");
                Ok(Reply { status: engine.status()?, error: Some("Update operation could not proceed. Another operation may be running, or preflight/verification failed. Check updater logs.".into()), ready: false })
            }
        }
    }

    #[derive(Debug, serde::Deserialize)]
    /// Minimal public readiness fields used for version validation.
    struct ReadyHealth {
        /// Aggregate application readiness status.
        status: String,
        /// Running stable package version.
        version: String,
    }

    /// Fixed service controller and bounded loopback readiness probe.
    struct SystemService {
        /// Operator-configured loopback HTTP port.
        port: u16,
        /// Recovery barrier coordinated with controlled starts.
        admission: Arc<Admission>,
    }
    /// Permit only start/stop argv for the fixed `RustChan` systemd unit.
    fn control(verb: &str) -> anyhow::Result<()> {
        anyhow::ensure!(
            matches!(verb, "start" | "stop"),
            "unsupported service action"
        );
        bounded_command(
            "/usr/bin/systemctl",
            &["--no-ask-password", verb, "rustchan.service"],
        )
    }
    /// Execute fixed operator tooling with no inherited environment and a deadline.
    fn bounded_command(program: &str, arguments: &[&str]) -> anyhow::Result<()> {
        let mut child = Command::new(program)
            .env_clear()
            .args(arguments)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        let deadline = Instant::now() + Duration::from_secs(90);
        loop {
            if let Some(status) = child.try_wait()? {
                anyhow::ensure!(status.success(), "RustChan service control failed");
                return Ok(());
            }
            if Instant::now() >= deadline {
                child.kill()?;
                child.wait()?;
                anyhow::bail!("RustChan service control timed out");
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    impl Service for SystemService {
        fn preflight(&self) -> anyhow::Result<()> {
            let pid = std::process::id().to_string();
            for verb in ["start", "stop"] {
                bounded_command(
                    "/usr/bin/pkcheck",
                    &[
                        "--action-id",
                        "org.freedesktop.systemd1.manage-units",
                        "--process",
                        &pid,
                        "--detail",
                        "unit",
                        "rustchan.service",
                        "--detail",
                        "verb",
                        verb,
                    ],
                )?;
            }
            Ok(())
        }
        fn stop(&self) -> anyhow::Result<()> {
            control("stop")
        }
        fn start(&self) -> anyhow::Result<()> {
            // Only this controlled start may bypass the daemon boot barrier.
            // Recovery reaches it only after restore/reactivation has completed.
            self.admission.starting.store(true, Ordering::Release);
            control("start")
        }
        fn health(&self, version: &str, _schema: &str) -> anyhow::Result<()> {
            struct Reset<'a>(&'a AtomicBool);
            impl Drop for Reset<'_> {
                fn drop(&mut self) {
                    self.0.store(false, Ordering::Release);
                }
            }
            let _starting = Reset(&self.admission.starting);
            let client = reqwest::blocking::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(2))
                .build()?;
            let deadline = Instant::now() + Duration::from_secs(60);
            while Instant::now() < deadline {
                let checked = (|| {
                    use std::io::Read as _;
                    let response = client
                        .get(format!("http://127.0.0.1:{}/readyz", self.port))
                        .send()?
                        .error_for_status()?;
                    let mut bytes = Vec::new();
                    response.take(4097).read_to_end(&mut bytes)?;
                    anyhow::ensure!(bytes.len() <= 4096, "readiness response exceeds limit");
                    let health: ReadyHealth = serde_json::from_slice(&bytes)?;
                    anyhow::ensure!(
                        health.version == version && health.status == "ready",
                        "RustChan health or version mismatch"
                    );
                    Ok::<_, anyhow::Error>(())
                })();
                if checked.is_ok() {
                    return Ok(());
                }
                std::thread::sleep(Duration::from_millis(500));
            }
            anyhow::bail!("RustChan failed the bounded health check window")
        }
    }
    #[cfg(test)]
    /// Recovery admission and actual Unix peer-credential tests.
    mod tests {
        use super::*;

        /// Persisted restart phases cannot authorize a boot until recovery reaches a controlled start.
        #[test]
        fn admission_requires_completed_recovery_or_controlled_start() {
            let gate = Admission::default();
            for phase in [
                super::super::super::Phase::Idle,
                super::super::super::Phase::Restarting,
                super::super::super::Phase::RestartingPrevious,
            ] {
                assert!(
                    !gate.may_start(phase),
                    "persisted phases must not bypass boot recovery"
                );
            }
            gate.starting.store(true, Ordering::Release);
            assert!(
                gate.may_start(super::super::super::Phase::RestartingPrevious),
                "controlled recovery start must be admitted"
            );
            assert!(
                !gate.may_start(super::super::super::Phase::RollingBack),
                "restore cannot admit startup"
            );
        }

        /// The actual socket peer must match the configured web UID.
        #[tokio::test]
        async fn socket_rejects_different_peer_uid() -> anyhow::Result<()> {
            let dir = tempfile::tempdir()?;
            let socket = dir.path().join("control.sock");
            let listener = tokio::net::UnixListener::bind(&socket)?;
            let client = tokio::net::UnixStream::connect(&socket).await?;
            let (accepted, _) = listener.accept().await?;
            let uid = accepted.peer_cred()?.uid();
            anyhow::ensure!(
                peer_authorized(&accepted, uid)?,
                "matching UID should be accepted"
            );
            anyhow::ensure!(
                !peer_authorized(&accepted, uid + 1)?,
                "wrong UID must be rejected"
            );
            drop(client);
            Ok(())
        }
    }
}
