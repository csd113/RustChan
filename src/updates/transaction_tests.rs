// Fault-injected native transactions against disposable filesystem/SQLite fixtures.

use super::*;
use std::cell::Cell;
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

/// A complete disposable managed deployment, never a host service.
struct Fixture {
    /// Own all fixture files until the test finishes.
    _temp: tempfile::TempDir,
    /// Transaction engine with a distinct simulated web UID.
    engine: Engine,
    /// Synthetic but correctly structured signed-package payload metadata.
    manifest: Manifest,
    /// Deterministic synthetic ELF release archive.
    archive: Vec<u8>,
}

/// Minimal ELF header used to test architecture checks without executing binaries.
fn executable(machine: u16) -> anyhow::Result<Vec<u8>> {
    let mut bytes = vec![0; 128];
    bytes
        .get_mut(..7)
        .context("ELF prefix")?
        .copy_from_slice(b"\x7fELF\x02\x01\x01");
    bytes
        .get_mut(16..18)
        .context("ELF type")?
        .copy_from_slice(&2u16.to_le_bytes());
    bytes
        .get_mut(18..20)
        .context("ELF machine")?
        .copy_from_slice(&machine.to_le_bytes());
    Ok(bytes)
}

/// Build raw tar entries, including malicious names that tar's safe builder would reject.
fn archive(entries: &[(&str, u8)], binary: &[u8]) -> anyhow::Result<Vec<u8>> {
    let mut raw = Vec::new();
    for (name, kind) in entries {
        let mut header = tar::Header::new_ustar();
        header.set_size(u64::try_from(binary.len())?);
        header.set_mode(0o755);
        header.set_entry_type(tar::EntryType::new(*kind));
        let name_bytes = header
            .as_mut_bytes()
            .get_mut(..name.len())
            .context("tar name length")?;
        name_bytes.copy_from_slice(name.as_bytes());
        header.set_cksum();
        raw.extend_from_slice(header.as_bytes());
        raw.extend_from_slice(binary);
        raw.resize(
            raw.len()
                .div_ceil(512)
                .checked_mul(512)
                .context("tar padding length overflow")?,
            0,
        );
    }
    raw.resize(
        raw.len()
            .checked_add(1024)
            .context("tar end padding length overflow")?,
        0,
    );
    let mut gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    gzip.write_all(&raw)?;
    Ok(gzip.finish()?)
}

/// Create old software, configuration, media, runtime identity and database.
fn fixture() -> anyhow::Result<Fixture> {
    let temp = tempfile::tempdir()?;
    let base = temp.path().canonicalize()?;
    let install = base.join("install");
    let state = base.join("state");
    let data = base.join("data");
    for dir in [
        install.join("versions/1.5.0"),
        state.join("backups"),
        data.join("boards/pub"),
        data.join("runtime/private"),
    ] {
        fs::create_dir_all(dir)?;
    }
    fs::write(
        install.join("versions/1.5.0/rustchan-cli"),
        b"old executable",
    )?;
    fs::set_permissions(
        install.join("versions/1.5.0/rustchan-cli"),
        fs::Permissions::from_mode(0o755),
    )?;
    std::os::unix::fs::symlink("versions/1.5.0", install.join("current"))?;
    let settings_path = data.join("settings.toml");
    fs::write(
        &settings_path,
        "site_name = 'Original'\ncookie_secret = 'keep-private-key'\n",
    )?;
    fs::write(data.join("boards/pub/media.bin"), b"old media")?;
    fs::write(data.join("runtime/private/identity"), b"old identity")?;
    let public_key = base.join("public-key.hex");
    fs::write(&public_key, "00".repeat(32))?;
    let conn = Connection::open(data.join("chan.db"))?;
    conn.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE schema_version(version TEXT NOT NULL); INSERT INTO schema_version VALUES ('1.5.0'); CREATE TABLE posts(body TEXT); INSERT INTO posts VALUES ('keep every post');")?;
    drop(conn);
    let binary = executable(62)?;
    let archive = archive(&[("rustchan-cli", b'0')], &binary)?;
    let manifest = Manifest {
        format: 1,
        version: "1.6.0".to_owned(),
        release_id: 100,
        target: "x86_64-unknown-linux-gnu".to_owned(),
        filename: "rustchan-update-x86_64-unknown-linux-gnu.tar.gz".to_owned(),
        size: u64::try_from(archive.len())?,
        sha256: release::digest(&archive),
        executable_sha256: release::digest(&binary),
        executable_size: u64::try_from(binary.len())?,
        schema: "1.6.0".to_owned(),
        minimum_schema: "1.5.0".to_owned(),
        minimum_updater: "1.5.0".to_owned(),
    };
    let web_uid = fs::metadata(&install)?
        .uid()
        .checked_add(1)
        .context("fixture web UID overflow")?;
    let engine = Engine {
        config: Config {
            install_dir: install,
            state_dir: state,
            data_dir: data,
            settings_path,
            health_port: 8080,
            web_uid,
            public_key,
            retention: 2,
        },
        key: vec![0; 32],
    };
    Ok(Fixture {
        _temp: temp,
        engine,
        manifest,
        archive,
    })
}

/// Start a durable synthetic approved transaction before fetching its fixture artifact.
fn job(engine: &Engine) -> anyhow::Result<Status> {
    let mut status = Status {
        installed: "1.5.0".to_owned(),
        previous_version: Some("1.5.0".to_owned()),
        target_version: Some("1.6.0".to_owned()),
        job: Some(Uuid::new_v4().to_string()),
        administrator: Some(1),
        ..Status::default()
    };
    engine.save(&mut status, Phase::Downloading, "approved fixture release")?;
    Ok(status)
}

/// Failure injected after the new release has modified persistent state.
#[derive(Clone, Copy)]
enum Fault {
    /// No injected failure.
    None,
    /// Migration writes followed by an error.
    Migration,
    /// Process startup failure after migration.
    Startup,
    /// New application does not become ready.
    Health,
}

/// Service simulation; no host process or systemd operations occur.
struct FakeService<'a> {
    /// Filesystem fixture controlled by the engine.
    engine: &'a Engine,
    /// Injected post-activation failure.
    fault: Fault,
    /// Whether the simulated service is running.
    running: Cell<bool>,
}
impl Service for FakeService<'_> {
    fn stop(&self) -> anyhow::Result<()> {
        self.running.set(false);
        Ok(())
    }
    fn start(&self) -> anyhow::Result<()> {
        if self.engine.current_version()? == "1.6.0" {
            Connection::open(self.engine.config.data_dir.join("chan.db"))?.execute_batch("UPDATE schema_version SET version='1.6.0'; DELETE FROM posts; CREATE TABLE upgraded(value TEXT);")?;
            fs::write(
                &self.engine.config.settings_path,
                "site_name = 'Upgraded'\n",
            )?;
            fs::write(
                self.engine.config.data_dir.join("boards/pub/media.bin"),
                b"new media",
            )?;
            fs::write(
                self.engine.config.data_dir.join("runtime/private/identity"),
                b"new identity",
            )?;
            if matches!(self.fault, Fault::Migration | Fault::Startup) {
                anyhow::bail!("injected startup or migration failure");
            }
        }
        self.running.set(true);
        Ok(())
    }
    fn health(&self, version: &str, schema: &str) -> anyhow::Result<()> {
        anyhow::ensure!(self.running.get(), "service is stopped");
        anyhow::ensure!(
            self.engine.current_version()? == version
                && verify_database(&self.engine.config.data_dir.join("chan.db"))? == schema,
            "version/schema mismatch"
        );
        anyhow::ensure!(
            !(version == "1.6.0" && matches!(self.fault, Fault::Health)),
            "injected readiness timeout"
        );
        Ok(())
    }
}

/// Bounded fixture artifact source avoids GitHub and never executes synthetic ELF files.
struct Source<'a>(&'a [u8]);
impl ArtifactSource for Source<'_> {
    fn fetch(&self, _manifest: &Manifest) -> anyhow::Result<Vec<u8>> {
        Ok(self.0.to_vec())
    }
}

/// Confirm binary, schema, posts, settings, media and identity all roll back together.
fn assert_restored(f: &Fixture, status: &Status) -> anyhow::Result<()> {
    anyhow::ensure!(
        status.phase == Phase::RolledBack,
        "failure must commit only the restored state"
    );
    anyhow::ensure!(
        f.engine.current_version()? == "1.5.0",
        "old executable must be active"
    );
    anyhow::ensure!(
        fs::read(f.engine.config.install_dir.join("current/rustchan-cli"))? == b"old executable",
        "previous executable must remain unchanged"
    );
    anyhow::ensure!(
        verify_database(&f.engine.config.data_dir.join("chan.db"))? == "1.5.0",
        "old schema must be restored"
    );
    let conn = Connection::open(f.engine.config.data_dir.join("chan.db"))?;
    let body: String = conn.query_row("SELECT body FROM posts", [], |row| row.get(0))?;
    anyhow::ensure!(
        body == "keep every post",
        "all acknowledged posts must survive rollback"
    );
    anyhow::ensure!(
        fs::read_to_string(&f.engine.config.settings_path)?.contains("Original"),
        "configuration must roll back"
    );
    anyhow::ensure!(
        fs::read(f.engine.config.data_dir.join("boards/pub/media.bin"))? == b"old media",
        "startup media changes must roll back"
    );
    anyhow::ensure!(
        fs::read(f.engine.config.data_dir.join("runtime/private/identity"))? == b"old identity",
        "private identity changes must roll back"
    );
    anyhow::ensure!(
        status
            .backup
            .as_ref()
            .is_some_and(|backup| backup.verified && backup.persistent_sha256.is_some()),
        "rollback must use a complete verified backup"
    );
    Ok(())
}

/// A successful transaction cannot commit until readiness and target schema pass.
#[test]
fn successful_install_requires_complete_backup_and_health() -> anyhow::Result<()> {
    let f = fixture()?;
    let mut status = job(&f.engine)?;
    let service = FakeService {
        engine: &f.engine,
        fault: Fault::None,
        running: Cell::new(true),
    };
    f.engine
        .install_with_source(&mut status, &f.manifest, &service, &Source(&f.archive))?;
    anyhow::ensure!(
        status.phase == Phase::Succeeded,
        "verified healthy install should succeed"
    );
    anyhow::ensure!(
        status
            .backup
            .as_ref()
            .is_some_and(|backup| backup.persistent_sha256.is_some()),
        "pre-upgrade persistent snapshot must exist"
    );
    Ok(())
}

/// Exercise every critical activation failure with complete state restoration.
#[test]
fn migration_startup_and_health_failures_restore_every_component() -> anyhow::Result<()> {
    for fault in [Fault::Migration, Fault::Startup, Fault::Health] {
        let f = fixture()?;
        let mut status = job(&f.engine)?;
        let service = FakeService {
            engine: &f.engine,
            fault,
            running: Cell::new(true),
        };
        f.engine
            .install_with_source(&mut status, &f.manifest, &service, &Source(&f.archive))?;
        assert_restored(&f, &status)?;
    }
    Ok(())
}

/// Corrupt backup settings must fail before changing the current version.
#[test]
fn backup_verification_failure_prevents_activation() -> anyhow::Result<()> {
    let f = fixture()?;
    fs::write(&f.engine.config.settings_path, "invalid = [")?;
    let mut status = job(&f.engine)?;
    let service = FakeService {
        engine: &f.engine,
        fault: Fault::None,
        running: Cell::new(true),
    };
    f.engine
        .install_with_source(&mut status, &f.manifest, &service, &Source(&f.archive))?;
    anyhow::ensure!(
        status.phase == Phase::Failed,
        "malformed configuration must abort before activation"
    );
    anyhow::ensure!(
        f.engine.current_version()? == "1.5.0",
        "old binary must remain selected"
    );
    Ok(())
}

/// Every recovery phase after activation intent must restore the complete old state.
#[test]
fn interrupted_activation_recovery_is_durable_and_idempotent() -> anyhow::Result<()> {
    for phase in [
        Phase::Activating,
        Phase::Restarting,
        Phase::HealthChecking,
        Phase::RollingBack,
        Phase::RestartingPrevious,
    ] {
        let f = fixture()?;
        let mut status = job(&f.engine)?;
        f.engine.snapshot(&mut status, &f.manifest, true)?;
        f.engine.save(&mut status, phase, "interrupted fixture")?;
        let stage = f.engine.config.install_dir.join("versions/1.6.0");
        fs::create_dir(&stage)?;
        stage_archive(&f.archive, &f.manifest, &stage)?;
        f.engine.activate("1.6.0")?;
        let service = FakeService {
            engine: &f.engine,
            fault: Fault::None,
            running: Cell::new(false),
        };
        service.start()?;
        f.engine.recover(&service)?;
        assert_restored(&f, &f.engine.status()?)?;
        f.engine.recover(&service)?;
        assert_restored(&f, &f.engine.status()?)?;
    }
    Ok(())
}

/// Archive verification rejects hidden metadata, duplicate entries, traversal and link types.
#[test]
fn unsafe_archive_layouts_never_stage() -> anyhow::Result<()> {
    let f = fixture()?;
    for entries in [
        vec![("../rustchan-cli", b'0')],
        vec![("/rustchan-cli", b'0')],
        vec![("extra", b'0')],
        vec![("rustchan-cli", b'2')],
        vec![("rustchan-cli", b'1')],
        vec![("rustchan-cli", b'x')],
        vec![("rustchan-cli", b'g')],
        vec![("rustchan-cli", b'L')],
        vec![("rustchan-cli", b'0'), ("rustchan-cli", b'0')],
    ] {
        let bytes = archive(&entries, &executable(62)?)?;
        let mut manifest = f.manifest.clone();
        manifest.size = u64::try_from(bytes.len())?;
        manifest.sha256 = release::digest(&bytes);
        let dir = tempfile::tempdir()?;
        anyhow::ensure!(
            stage_archive(&bytes, &manifest, dir.path()).is_err(),
            "unsafe archive must be rejected: {entries:?}"
        );
    }
    Ok(())
}

/// Artifact and executable identities are checked separately before staging.
#[test]
fn invalid_hash_size_and_architecture_fail_closed() -> anyhow::Result<()> {
    let f = fixture()?;
    for change in 0_i32..3_i32 {
        let mut manifest = f.manifest.clone();
        match change {
            0_i32 => manifest.sha256 = "00".repeat(32),
            1_i32 => manifest.size += 1,
            _ => manifest.target = "aarch64-unknown-linux-gnu".to_owned(),
        }
        anyhow::ensure!(
            stage_archive(&f.archive, &manifest, tempfile::tempdir()?.path()).is_err(),
            "artifact identity mismatch must be rejected"
        );
    }
    Ok(())
}

/// File locks reject concurrent installation/check/recovery and release on owner drop.
#[test]
fn concurrent_transaction_is_rejected() -> anyhow::Result<()> {
    let f = fixture()?;
    let lock = f.engine.lock()?;
    anyhow::ensure!(
        f.engine.lock().is_err(),
        "a second update cannot acquire the lock"
    );
    drop(lock);
    anyhow::ensure!(
        f.engine.lock().is_ok(),
        "lock must release after transaction ends"
    );
    Ok(())
}

/// Installation permission cannot be reused or consumed with an expired check.
#[test]
fn approvals_expire_and_are_consumed_durably() -> anyhow::Result<()> {
    let f = fixture()?;
    let mut status = Status {
        installed: "1.5.0".to_owned(),
        approval: Some(Uuid::new_v4().to_string()),
        checked_at: Some((chrono::Utc::now() - chrono::Duration::minutes(11)).to_rfc3339()),
        discovery: Some(Discovery::Available(Box::new(release::Release {
            id: 100,
            version: "1.6.0".to_owned(),
            published_at: String::new(),
            notes: String::new(),
            manifest: Some(f.manifest.clone()),
            size: Some(f.manifest.size),
            verification: String::new(),
            compatible: true,
        }))),
        ..Status::default()
    };
    f.engine
        .save(&mut status, Phase::Idle, "approval fixture")?;
    let approval = status.approval.clone().context("fixture approval")?;
    anyhow::ensure!(
        f.engine.approve(&approval, 1).is_err(),
        "expired approval must be rejected"
    );
    // Target validation remains platform-specific; consumption is tested on actual native Linux.
    if cfg!(target_os = "linux") {
        let target = release::platform_target().context("supported Linux fixture target")?;
        if let Some(Discovery::Available(candidate)) = &mut status.discovery {
            let manifest = candidate.manifest.as_mut().context("fixture manifest")?;
            manifest.target = target.to_owned();
            manifest.filename = format!("rustchan-update-{target}.tar.gz");
        }
        status.checked_at = Some(chrono::Utc::now().to_rfc3339());
        f.engine.save(&mut status, Phase::Idle, "fresh approval")?;
        f.engine
            .approve(&approval, 1)
            .map(|_operation_summary| ())?;
        anyhow::ensure!(
            f.engine.approve(&approval, 1).is_err(),
            "consumed approval must never be reusable"
        );
    }
    Ok(())
}

/// Web-writable program directories violate the isolated updater boundary.
#[test]
fn unsafe_install_permissions_fail_preflight() -> anyhow::Result<()> {
    let f = fixture()?;
    fs::set_permissions(
        &f.engine.config.install_dir,
        fs::Permissions::from_mode(0o777),
    )?;
    anyhow::ensure!(
        f.engine.validate().is_err(),
        "writable installation root must be rejected"
    );
    Ok(())
}

/// Authenticate the complete rollback inventory before touching live database/configuration.
#[test]
fn corrupt_persistent_backup_cannot_partially_restore_live_state() -> anyhow::Result<()> {
    let f = fixture()?;
    let mut status = job(&f.engine)?;
    f.engine.snapshot(&mut status, &f.manifest, true)?;
    let id = status.job.as_deref().context("missing fixture job")?;
    fs::write(
        f.engine
            .config
            .state_dir
            .join("backups")
            .join(id)
            .join("persistent/boards/pub/media.bin"),
        b"corrupt",
    )?;
    fs::write(
        &f.engine.config.settings_path,
        b"forum_name = 'new settings'\n",
    )?;
    let conn = Connection::open(f.engine.config.data_dir.join("chan.db"))?;
    conn.execute("UPDATE posts SET body = 'new state'", [])
        .map(|_affected_rows| ())?;
    anyhow::ensure!(
        f.engine.restore(&status).is_err(),
        "corrupt persistent backup must be rejected"
    );
    let body: String = conn.query_row("SELECT body FROM posts", [], |row| row.get(0))?;
    anyhow::ensure!(
        body == "new state"
            && fs::read_to_string(&f.engine.config.settings_path)?.contains("new settings"),
        "verification failure must leave all live state untouched"
    );
    Ok(())
}

/// Native updates refuse mutable certificate state outside the protected rollback roots.
#[test]
fn external_acme_state_blocks_managed_backup() -> anyhow::Result<()> {
    let f = fixture()?;
    let mut status = job(&f.engine)?;
    fs::write(
        &f.engine.config.settings_path,
        "[tls.acme]\nenabled = true\ncache_dir = '/tmp/external-acme'\n",
    )?;
    anyhow::ensure!(
        f.engine.snapshot(&mut status, &f.manifest, false).is_err(),
        "external mutable state must block installation"
    );
    fs::write(
        &f.engine.config.settings_path,
        "[tls.acme]\nenabled = true\ncache_dir = 'runtime/tls/acme'\n",
    )?;
    f.engine.snapshot(&mut status, &f.manifest, false)?;
    Ok(())
}

/// A malformed durable snapshot identifier must never select a path outside updater storage.
#[test]
fn journal_backup_paths_are_validated_before_restore() -> anyhow::Result<()> {
    let f = fixture()?;
    let mut status = job(&f.engine)?;
    f.engine.snapshot(&mut status, &f.manifest, true)?;
    status.backup.as_mut().context("fixture backup")?.id = "../../outside".to_owned();
    f.engine
        .save(&mut status, Phase::Activating, "invalid restored journal")?;
    anyhow::ensure!(
        f.engine.status().is_err() && f.engine.restore(&status).is_err(),
        "journal path traversal must fail before restore"
    );
    Ok(())
}

/// The web identity must be able to execute a single immutable current binary.
#[test]
fn private_or_hardlinked_current_binary_is_rejected() -> anyhow::Result<()> {
    let f = fixture()?;
    let binary = f
        .engine
        .config
        .install_dir
        .join("versions/1.5.0/rustchan-cli");
    fs::set_permissions(&binary, fs::Permissions::from_mode(0o700))?;
    anyhow::ensure!(
        f.engine.current_version().is_err(),
        "updater-only executable must not be activated for another web UID"
    );
    fs::set_permissions(&binary, fs::Permissions::from_mode(0o755))?;
    fs::hard_link(&binary, f.engine.config.install_dir.join("other-link"))?;
    anyhow::ensure!(
        f.engine.current_version().is_err(),
        "current executable must have no hardlink aliases"
    );
    Ok(())
}

/// Configuration-only supervisor mock, with no actual service execution.
struct RestartService<'a> {
    /// Disposable deployment under test.
    engine: &'a Engine,
    /// Number of replacement starts; the second is rollback recovery.
    starts: Cell<u32>,
    /// Inject a failure in the first startup rather than launching host processes.
    fail_start: bool,
    /// Fail the trial health check, preserving all database writes.
    fail_health: bool,
    /// Simulate the old process falsely answering health after service control.
    old_instance: Option<Uuid>,
}
impl Service for RestartService<'_> {
    fn stop(&self) -> anyhow::Result<()> {
        Ok(())
    }
    fn start(&self) -> anyhow::Result<()> {
        self.starts.set(
            self.starts
                .get()
                .checked_add(1)
                .context("fixture restart count overflow")?,
        );
        if self.starts.get() == 1 && self.fail_start {
            anyhow::bail!("injected settings startup failure");
        }
        if self.starts.get() == 1 && self.old_instance.is_some() {
            return Ok(());
        }
        let bytes =
            crate::restart::Store::read(&self.engine.config.settings_path, 4 * 1024 * 1024)?;
        self.engine
            .started(
                Uuid::new_v4(),
                &crate::restart::configuration_digest(&bytes),
            )
            .map(|_operation_summary| ())?;
        Ok(())
    }
    fn health(&self, _version: &str, _schema: &str) -> anyhow::Result<()> {
        anyhow::ensure!(
            !(self.starts.get() == 1 && self.fail_health),
            "injected readiness timeout"
        );
        Ok(())
    }
    fn health_instance(&self, version: &str, previous: Uuid) -> anyhow::Result<()> {
        self.health(version, "")?;
        anyhow::ensure!(
            self.engine.restart_store().running()?.instance != previous,
            "old process cannot satisfy readiness"
        );
        Ok(())
    }
}

/// Record a healthy initial process and a validated pending startup policy change.
fn pending_restart(engine: &Engine) -> anyhow::Result<Uuid> {
    let instance = Uuid::new_v4();
    let good = format!(
        "enable_tor_support = false\ncookie_secret = '{}'\n",
        "ab".repeat(32)
    )
    .into_bytes();
    fs::write(&engine.config.settings_path, &good)?;
    engine
        .started(instance, &crate::restart::configuration_digest(&good))
        .map(|_operation_summary| ())?;
    let candidate = format!("{}\nrate_limit_gets = 12345\n", std::str::from_utf8(&good)?);
    crate::config::admin::atomic_replace(&engine.config.settings_path, &candidate)?;
    Ok(instance)
}

/// Pending settings survive until replacement readiness; both OS leases exclude all races.
#[test]
fn settings_restart_health_commit_and_update_mutual_exclusion() -> anyhow::Result<()> {
    let fixture = fixture()?;
    let engine = &fixture.engine;
    let previous = pending_restart(engine)?;
    let service = RestartService {
        engine,
        starts: Cell::new(0),
        fail_start: false,
        fail_health: false,
        old_instance: None,
    };
    let (mut status, update, settings) = engine.approve_restart(previous, 1, &service)?;
    anyhow::ensure!(
        status.phase.blocks_writes() && status.phase.active(),
        "restart must close write admission"
    );
    anyhow::ensure!(
        engine.lock().is_err(),
        "software installation must be serialized with restart"
    );
    anyhow::ensure!(
        engine.approve_restart(previous, 2, &service).is_err(),
        "duplicate/concurrent restart must reject"
    );
    anyhow::ensure!(
        crate::config::admin::settings_lease(&engine.config.settings_path).is_err(),
        "saves must not race a restart"
    );
    anyhow::ensure!(
        engine.restart_store().running()?.instance == previous,
        "pending cannot clear merely on request acceptance"
    );
    engine.restart_settings(&mut status, &service)?;
    anyhow::ensure!(
        status.phase == Phase::Succeeded,
        "healthy replacement must commit"
    );
    anyhow::ensure!(
        engine.restart_store().running()?.instance != previous,
        "successful restart must observe a replacement"
    );
    drop(settings);
    drop(update);
    anyhow::ensure!(
        engine.approve_restart(previous, 1, &service).is_err(),
        "replayed process approval must remain consumed"
    );
    Ok(())
}

/// Startup, health timeout and stale process readiness all restore config without reverting data.
#[test]
fn settings_restart_failures_recover_configuration_only() -> anyhow::Result<()> {
    for (fail_start, fail_health, old_process) in [
        (true, false, false),
        (false, true, false),
        (false, false, true),
    ] {
        let fixture = fixture()?;
        let engine = &fixture.engine;
        let previous = pending_restart(engine)?;
        let original = crate::restart::Store::read(
            &engine.restart_store().directory.join("known-good.toml"),
            4 * 1024 * 1024,
        )?;
        fs::write(
            engine.config.data_dir.join("boards/pub/media.bin"),
            b"new content must survive",
        )?;
        let service = RestartService {
            engine,
            starts: Cell::new(0),
            fail_start,
            fail_health,
            old_instance: old_process.then_some(previous),
        };
        let (mut status, _update, _settings) = engine.approve_restart(previous, 1, &service)?;
        engine.restart_settings(&mut status, &service)?;
        anyhow::ensure!(
            status.phase == Phase::RolledBack && service.starts.get() == 2,
            "failed trial must verify previous config recovery"
        );
        anyhow::ensure!(
            fs::read(&engine.config.settings_path)? == original,
            "known-good settings must be restored"
        );
        anyhow::ensure!(
            fs::read(engine.config.data_dir.join("boards/pub/media.bin"))?
                == b"new content must survive",
            "configuration rollback must not revert media/database"
        );
    }
    Ok(())
}

/// An interrupted transaction enters the existing daemon recovery before startup admission.
#[test]
fn interrupted_settings_restart_reuses_updater_recovery() -> anyhow::Result<()> {
    let fixture = fixture()?;
    let engine = &fixture.engine;
    let previous = pending_restart(engine)?;
    let service = RestartService {
        engine,
        starts: Cell::new(1),
        fail_start: false,
        fail_health: false,
        old_instance: None,
    };
    let (mut status, update, settings) = engine.approve_restart(previous, 1, &service)?;
    engine.save(
        &mut status,
        Phase::HealthChecking,
        "interrupted settings startup",
    )?;
    drop(settings);
    drop(update);
    engine.recover(&service)?;
    anyhow::ensure!(
        engine.status()?.phase == Phase::RolledBack,
        "existing recovery must handle configuration-only transactions"
    );
    Ok(())
}

/// Invalid candidate data never gets durable restart approval or touches a service.
#[test]
fn invalid_settings_cannot_start_restart_transaction() -> anyhow::Result<()> {
    let fixture = fixture()?;
    let engine = &fixture.engine;
    let previous = pending_restart(engine)?;
    let service = RestartService {
        engine,
        starts: Cell::new(0),
        fail_start: false,
        fail_health: false,
        old_instance: None,
    };
    fs::write(&engine.config.settings_path, "port = 0\n")?;
    anyhow::ensure!(
        engine.approve_restart(previous, 1, &service).is_err(),
        "invalid configuration must reject before service work"
    );
    anyhow::ensure!(
        service.starts.get() == 0 && !engine.status()?.phase.active(),
        "invalid config must not start a restart"
    );
    Ok(())
}

/// A timed-out stop must close admission without restoring beneath an unconfirmed process.
#[test]
fn failed_settings_shutdown_cannot_restore_under_an_unconfirmed_process() -> anyhow::Result<()> {
    struct UnresponsiveService {
        stops: Cell<u32>,
        starts: Cell<u32>,
    }
    impl Service for UnresponsiveService {
        fn stop(&self) -> anyhow::Result<()> {
            self.stops.set(self.stops.get() + 1);
            anyhow::bail!("shutdown timed out")
        }
        fn start(&self) -> anyhow::Result<()> {
            self.starts.set(
                self.starts
                    .get()
                    .checked_add(1)
                    .context("fixture restart count overflow")?,
            );
            anyhow::bail!("must not start after unconfirmed stop")
        }
        fn health(&self, _version: &str, _schema: &str) -> anyhow::Result<()> {
            anyhow::bail!("must not check health after unconfirmed stop")
        }
    }
    let fixture = fixture()?;
    let engine = &fixture.engine;
    let previous = pending_restart(engine)?;
    let candidate = fs::read(&engine.config.settings_path)?;
    let service = UnresponsiveService {
        stops: Cell::new(0),
        starts: Cell::new(0),
    };
    let (mut status, _update, _settings) = engine.approve_restart(previous, 1, &service)?;
    engine.restart_settings(&mut status, &service)?;
    anyhow::ensure!(
        service.stops.get() == 1 && service.starts.get() == 0,
        "an unconfirmed stop must not trigger further service control"
    );
    anyhow::ensure!(
        status.phase == Phase::FailedManualIntervention
            && fs::read(&engine.config.settings_path)? == candidate,
        "unconfirmed stop must retain configuration and close admission"
    );
    anyhow::ensure!(
        engine.restart_store().running()?.instance == previous,
        "failed shutdown cannot verify a replacement"
    );
    Ok(())
}

/// Two administrator requests compete for the same OS-owned transaction lease.
#[test]
fn concurrent_settings_restarts_accept_exactly_one_request() -> anyhow::Result<()> {
    let fixture = fixture()?;
    let engine = &fixture.engine;
    let previous = pending_restart(engine)?;
    let barrier = std::sync::Barrier::new(2);
    let outcomes = std::thread::scope(|scope| {
        let first = scope.spawn(|| {
            let service = RestartService {
                engine,
                starts: Cell::new(0),
                fail_start: false,
                fail_health: false,
                old_instance: None,
            };
            let result = engine.approve_restart(previous, 1, &service);
            let _barrier_state = barrier.wait();
            result.is_ok()
        });
        let second = scope.spawn(|| {
            let service = RestartService {
                engine,
                starts: Cell::new(0),
                fail_start: false,
                fail_health: false,
                old_instance: None,
            };
            let result = engine.approve_restart(previous, 2, &service);
            let _barrier_state = barrier.wait();
            result.is_ok()
        });
        Ok::<_, anyhow::Error>((
            first.join().map_err(|panic_payload| {
                anyhow::anyhow!(
                    "first restart worker failed: {}",
                    crate::media::process::panic_message(panic_payload.as_ref())
                )
            })?,
            second.join().map_err(|panic_payload| {
                anyhow::anyhow!(
                    "second restart worker failed: {}",
                    crate::media::process::panic_message(panic_payload.as_ref())
                )
            })?,
        ))
    })?;
    anyhow::ensure!(
        outcomes.0 != outcomes.1 && engine.status()?.phase == Phase::Stopping,
        "exactly one concurrent restart may be durable"
    );
    Ok(())
}
