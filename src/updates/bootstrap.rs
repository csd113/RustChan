//! Retained same-account controller identity, independent of the active trial link.
//!
//! This preparation never makes installation available by itself. The full
//! controller handoff must open that admission after verifying initialization.

use super::{
    lifecycle::{self, Layout},
    transaction::native::{Config, Engine, Ownership},
};
use anyhow::Context as _;
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File},
    io::{Read as _, Write as _},
    os::unix::fs::{MetadataExt as _, PermissionsExt as _},
    path::{Path, PathBuf},
};

/// Bounded controller identity, separate from application configuration secrets.
const MAX_IDENTITY: u64 = 4096;

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
/// The first authorized source executable, retained before any trial can replace it.
struct Origin {
    /// Stable layout contract.
    format: u32,
    /// Original startup executable path; never supplied by the browser.
    program: PathBuf,
    /// Stable package version of this saved source build.
    version: String,
    /// Exact executable bytes, including local source changes.
    sha256: String,
    /// Initial copy may be replayed before any application writer is launched.
    phase: OriginPhase,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
/// Original retention commits only after executable and pointer directory fsync.
enum OriginPhase {
    /// Intent is durable; interrupted preparation may safely retry the exact original bytes.
    Retaining,
    /// The retained original is durable and may be selected as a full controller.
    Ready,
}

/// Ordinary source ownership gives no authority to replace protected installations.
pub(super) fn replaceable(program: &Path, uid: u32) -> bool {
    let inspected = (|| {
        lifecycle::owned_ancestors(program, uid)?;
        let metadata = fs::symlink_metadata(program)?;
        anyhow::ensure!(
            metadata.is_file()
                && metadata.uid() == uid
                && metadata.nlink() == 1
                && metadata.mode() & 0o7022 == 0
                && metadata.mode() & 0o500 == 0o500,
            "source executable ownership or access is unsafe"
        );
        let parent = program
            .parent()
            .context("source executable has no parent")?;
        let parent_metadata = fs::metadata(parent)?;
        anyhow::ensure!(
            parent_metadata.uid() == uid && parent_metadata.mode() & 0o300 == 0o300,
            "source installation directory cannot be replaced by this account"
        );
        Ok::<_, anyhow::Error>(())
    })();
    inspected.is_ok()
}

/// Retain and validate the original full controller before consulting an active trial.
/// Protected installations return check/manual mode without changing permissions.
pub(super) fn prepare(layout: &Layout) -> anyhow::Result<Option<Engine>> {
    let program = std::env::current_exe()?;
    if !replaceable(&program, layout.uid) {
        return Ok(None);
    }
    let origin_path = layout.state.join("origin.json");
    if !origin_path.try_exists()? {
        retain_origin(layout, &program, &origin_path)?;
    }
    let mut origin: Origin = serde_json::from_slice(&lifecycle::read_private(
        &origin_path,
        layout.uid,
        MAX_IDENTITY,
    )?)?;
    anyhow::ensure!(
        origin.format == lifecycle::FORMAT && origin.program == program,
        "source executable differs from the retained installation; manual adoption is required"
    );
    if origin.phase == OriginPhase::Retaining {
        finish_origin(layout, &mut origin, &origin_path)?;
    }
    // The known controller is verified before Engine::validate examines current,
    // which may point at an interrupted non-executable or corrupt trial.
    drop(known_controller(layout, &origin)?);
    let engine = engine(layout)?;
    let status = engine.status()?;
    if !status.phase.active() {
        let known = version_binary(layout, &status.installed)?;
        let actual = digest(&program)?;
        let previous_matches = status.previous_version.as_ref().is_some_and(|previous| {
            version_binary(layout, previous)
                .and_then(|path| digest(&path))
                .is_ok_and(|old| old == actual)
        });
        anyhow::ensure!(actual == digest(&known)? || previous_matches,
            "source build changed after retention; controlled source adoption must complete before startup (the new source executable is preserved)");
    }
    if engine.config.settings_path.try_exists()? {
        engine.validate_layout()?;
    }
    Ok(Some(engine))
}

/// An offline CLI cannot migrate data owned by a different committed application version.
pub(super) fn admit_cli(layout: &Layout) -> anyhow::Result<()> {
    if !layout.state.join("origin.json").try_exists()? {
        // First source adoption remains an explicitly local operation; older
        // uncooperative binaries must already be stopped by the operator.
        return Ok(());
    }
    let path = layout.state.join("status.json");
    let selected = if path.try_exists()? {
        let status: super::Status =
            serde_json::from_slice(&lifecycle::read_private(&path, layout.uid, 512 * 1024)?)?;
        anyhow::ensure!(
            !status.phase.active() && status.phase != super::Phase::FailedManualIntervention,
            "updater recovery must complete before an offline administration command"
        );
        status.installed
    } else {
        let origin: Origin = serde_json::from_slice(&lifecycle::read_private(
            &layout.state.join("origin.json"),
            layout.uid,
            MAX_IDENTITY,
        )?)?;
        anyhow::ensure!(
            origin.phase == OriginPhase::Ready,
            "initial source retention is incomplete"
        );
        origin.version
    };
    anyhow::ensure!(selected == super::VERSION,
        "administration CLI differs from the committed application; use the matching executable after recovery");
    let origin: Origin = serde_json::from_slice(&lifecycle::read_private(
        &layout.state.join("origin.json"),
        layout.uid,
        MAX_IDENTITY,
    )?)?;
    anyhow::ensure!(known_controller(layout, &origin)? == selected
        && digest(&std::env::current_exe()?)? == digest(&version_binary(layout, &selected)?)?,
        "administration CLI bytes differ from the retained application; controlled source adoption is required");
    Ok(())
}

/// Build the fixed same-account layout with the embedded public verification key.
pub(super) fn engine(layout: &Layout) -> anyhow::Result<Engine> {
    let path = layout.data.join("settings.toml");
    let bytes = if path.try_exists()? {
        crate::restart::Store::read(&path, 4 * 1024 * 1024)?
    } else {
        Vec::new()
    };
    let config = crate::config::admin::resolve_file(
        std::str::from_utf8(&bytes)?,
        &crate::config::Environment::Process,
    )?;
    Ok(Engine {
        config: Config {
            install_dir: layout.install.clone(),
            state_dir: layout.state.clone(),
            data_dir: layout.data.clone(),
            settings_path: path,
            health_port: crate::config::launcher_port().unwrap_or(config.port),
            web_uid: layout.uid,
            public_key: None,
            retention: 3,
        },
        key: super::trust::official_public_key()?,
        ownership: Ownership::SameAccount,
    })
}

/// Copy original executable bytes and publish identity only after all files are durable.
fn retain_origin(layout: &Layout, program: &Path, origin_path: &Path) -> anyhow::Result<()> {
    let directory = layout.install.join("versions").join(super::VERSION);
    anyhow::ensure!(
        fs::symlink_metadata(directory)
            .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
            && fs::symlink_metadata(layout.install.join("current"))
                .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound),
        "source installation exists without its durable origin identity"
    );
    let mut origin = Origin {
        format: lifecycle::FORMAT,
        program: program.to_owned(),
        version: super::VERSION.to_owned(),
        sha256: digest(program)?,
        phase: OriginPhase::Retaining,
    };
    publish_origin(layout, &origin, origin_path)?;
    finish_origin(layout, &mut origin, origin_path)
}

/// Replay only the source retention whose fixed initial intent was fsynced before any writer.
fn finish_origin(layout: &Layout, origin: &mut Origin, origin_path: &Path) -> anyhow::Result<()> {
    anyhow::ensure!(
        origin.phase == OriginPhase::Retaining
            && origin.version == super::VERSION
            && replaceable(&origin.program, layout.uid)
            && origin.sha256 == digest(&origin.program)?,
        "source executable changed during unfinished controller retention"
    );
    let directory = layout.install.join("versions").join(&origin.version);
    match fs::create_dir(&directory) {
        Ok(()) => fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))?,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.into()),
    }
    lifecycle::owned_ancestors(&directory, layout.uid)?;
    let directory_metadata = fs::symlink_metadata(&directory)?;
    anyhow::ensure!(
        directory_metadata.is_dir()
            && directory_metadata.uid() == layout.uid
            && directory_metadata.mode().trailing_zeros() >= 6,
        "initial controller directory is unsafe"
    );
    let executable = directory.join("rustchan-cli");
    match fs::symlink_metadata(&executable) {
        Ok(metadata) => {
            anyhow::ensure!(
                metadata.is_file() && metadata.uid() == layout.uid && metadata.nlink() == 1,
                "initial controller file is unsafe"
            );
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let mut output = tempfile::NamedTempFile::new_in(&directory)?;
    let mut input = File::open(&origin.program)?;
    std::io::copy(&mut input, &mut output).map(|_bytes_copied| ())?;
    output
        .as_file()
        .set_permissions(fs::Permissions::from_mode(0o700))?;
    output.as_file().sync_all()?;
    anyhow::ensure!(
        digest(output.path())? == origin.sha256 && digest(&origin.program)? == origin.sha256,
        "source executable changed during retention"
    );
    drop(output.persist(&executable).map_err(|error| error.error)?);
    File::open(&directory)?.sync_all()?;
    File::open(layout.install.join("versions"))?.sync_all()?;
    let pointer = layout.install.join("current");
    let target = Path::new("versions").join(&origin.version);
    match fs::symlink_metadata(&pointer) {
        Ok(metadata) => {
            anyhow::ensure!(
                metadata.file_type().is_symlink()
                    && metadata.uid() == layout.uid
                    && fs::read_link(&pointer)? == target,
                "initial controller pointer is unsafe"
            );
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            std::os::unix::fs::symlink(&target, &pointer)?;
        }
        Err(error) => return Err(error.into()),
    }
    File::open(&layout.install)?.sync_all()?;
    origin.phase = OriginPhase::Ready;
    publish_origin(layout, origin, origin_path)
}

/// Persist initial identity through the same bounded private staging root as writer intent.
fn publish_origin(layout: &Layout, origin: &Origin, origin_path: &Path) -> anyhow::Result<()> {
    let mut identity = tempfile::NamedTempFile::new_in(layout.state.join("journal-staging"))?;
    identity.write_all(&serde_json::to_vec(origin)?)?;
    identity.as_file().sync_all()?;
    drop(identity.persist(origin_path).map_err(|error| error.error)?);
    File::open(layout.state.join("journal-staging"))?.sync_all()?;
    File::open(&layout.state)?.sync_all()?;
    Ok(())
}

/// The canonical terminal journal selects software; current alone cannot select a trial.
fn known_controller(layout: &Layout, origin: &Origin) -> anyhow::Result<String> {
    anyhow::ensure!(
        origin.phase == OriginPhase::Ready,
        "source retention has not committed"
    );
    let status_path = layout.state.join("status.json");
    let selected = if status_path.try_exists()? {
        let status: super::Status = serde_json::from_slice(&lifecycle::read_private(
            &status_path,
            layout.uid,
            512 * 1024,
        )?)?;
        status.installed
    } else {
        origin.version.clone()
    };
    super::release::stable_version(&selected).map(|_stable_version| ())?;
    if selected != origin.version {
        drop(version_binary(layout, &selected)?);
        return Ok(selected);
    }
    let directory = layout.install.join("versions").join(&selected);
    lifecycle::owned_ancestors(&directory, layout.uid)?;
    let executable = directory.join("rustchan-cli");
    let metadata = fs::symlink_metadata(&executable)?;
    anyhow::ensure!(
        metadata.is_file()
            && metadata.uid() == layout.uid
            && metadata.nlink() == 1
            && metadata.mode() & 0o7022 == 0
            && metadata.mode() & 0o500 == 0o500
            && digest(&executable)? == origin.sha256,
        "retained controller failed ownership, execution or byte verification"
    );
    Ok(selected)
}

/// Validate one immutable namespace against the original source or approved release identity.
pub(super) fn version_binary(layout: &Layout, version: &str) -> anyhow::Result<PathBuf> {
    let parsed = super::release::stable_version(version)?;
    anyhow::ensure!(
        parsed.to_string() == version,
        "noncanonical controller version"
    );
    let origin: Origin = serde_json::from_slice(&lifecycle::read_private(
        &layout.state.join("origin.json"),
        layout.uid,
        MAX_IDENTITY,
    )?)?;
    let expected = if origin.version == version {
        anyhow::ensure!(
            origin.phase == OriginPhase::Ready,
            "source retention is incomplete"
        );
        origin.sha256
    } else {
        let manifest: super::release::Manifest = serde_json::from_slice(&lifecycle::read_private(
            &layout
                .state
                .join("programs")
                .join(format!("{version}.json")),
            layout.uid,
            16 * 1024,
        )?)?;
        anyhow::ensure!(
            manifest.version == version
                && Some(manifest.target.as_str()) == super::platform_target(),
            "retained release identity mismatch"
        );
        manifest.executable_sha256
    };
    let path = layout
        .install
        .join("versions")
        .join(version)
        .join("rustchan-cli");
    lifecycle::owned_ancestors(&path, layout.uid)?;
    let metadata = fs::symlink_metadata(&path)?;
    anyhow::ensure!(
        metadata.is_file()
            && metadata.uid() == layout.uid
            && metadata.nlink() == 1
            && metadata.mode() & 0o7022 == 0
            && metadata.mode() & 0o500 == 0o500
            && digest(&path)? == expected,
        "retained controller failed byte or permission verification"
    );
    Ok(path)
}

/// Enroll only the staged executable named by the signature-verified approved discovery.
pub(super) fn enroll_candidate(layout: &Layout, version: &str) -> anyhow::Result<()> {
    if version_binary(layout, version).is_ok() {
        return Ok(());
    }
    let status = engine(layout)?.status()?;
    let Some(super::Discovery::Available(release)) = status.discovery else {
        anyhow::bail!("candidate has no approved signed identity");
    };
    let manifest = release.manifest.context("candidate is unverified")?;
    anyhow::ensure!(
        manifest.version == version
            && status.target_version.as_deref() == Some(version)
            && super::release::stable_version(&manifest.minimum_updater)?
                >= super::release::stable_version("1.6.6")?,
        "candidate requires single-binary protocol adoption"
    );
    let path = layout
        .install
        .join("versions")
        .join(version)
        .join("rustchan-cli");
    lifecycle::owned_ancestors(&path, layout.uid)?;
    anyhow::ensure!(
        digest(&path)? == manifest.executable_sha256
            && fs::metadata(&path)?.len() == manifest.executable_size,
        "staged candidate differs from signed executable identity"
    );
    let directory = layout.state.join("programs");
    crate::config::ensure_private_dir(&directory)?;
    super::transaction::native::atomic_write(
        &directory.join(format!("{version}.json")),
        &serde_json::to_vec(&manifest)?,
    )?;
    drop(version_binary(layout, version)?);
    Ok(())
}

/// The startup-only source path remains fixed even after its old mapped inode is replaced.
pub(super) fn original_program(layout: &Layout) -> anyhow::Result<PathBuf> {
    let origin: Origin = serde_json::from_slice(&lifecycle::read_private(
        &layout.state.join("origin.json"),
        layout.uid,
        MAX_IDENTITY,
    )?)?;
    anyhow::ensure!(
        replaceable(&origin.program, layout.uid),
        "source installation is no longer replaceable"
    );
    Ok(origin.program)
}

/// Publication follows canonical code/data commit; unexpected local rebuilds are preserved.
pub(super) fn publish_original(
    layout: &Layout,
    version: &str,
    expected: &str,
) -> anyhow::Result<String> {
    let program = original_program(layout)?;
    anyhow::ensure!(
        digest(&program)? == expected,
        "source executable changed during the update; new local bytes are preserved"
    );
    let source = version_binary(layout, version)?;
    let target_digest = digest(&source)?;
    if target_digest == expected {
        return Ok(target_digest);
    }
    let mut temporary =
        tempfile::NamedTempFile::new_in(program.parent().context("source parent missing")?)?;
    std::io::copy(&mut File::open(source)?, &mut temporary).map(|_bytes_copied| ())?;
    temporary
        .as_file()
        .set_permissions(fs::metadata(&program)?.permissions())?;
    temporary.as_file().sync_all()?;
    anyhow::ensure!(
        digest(temporary.path())? == target_digest && digest(&program)? == expected,
        "source executable changed during publication"
    );
    drop(temporary.persist(&program).map_err(|error| error.error)?);
    File::open(program.parent().context("source parent missing")?)?.sync_all()?;
    Ok(target_digest)
}

/// Stream executable identity without buffering or interpreting executable contents.
pub(super) fn digest(path: &Path) -> anyhow::Result<String> {
    use sha2::{Digest as _, Sha256};
    let mut file = File::open(path)?;
    let mut digest = Sha256::new();
    let mut bytes = [0_u8; 16 * 1024];
    loop {
        let count = file.read(&mut bytes)?;
        if count == 0 {
            break;
        }
        digest.update(
            bytes
                .get(..count)
                .context("invalid executable byte count")?,
        );
    }
    Ok(hex::encode(digest.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A pending broken trial never becomes the selected retained controller.
    #[test]
    fn retained_origin_precedes_non_executable_trial() -> anyhow::Result<()> {
        let temporary = tempfile::tempdir()?;
        let layout = Layout::prepare(temporary.path())?;
        let program = temporary.path().join("source");
        fs::write(&program, b"saved source modifications")?;
        fs::set_permissions(&program, fs::Permissions::from_mode(0o700))?;
        let identity = layout.state.join("origin.json");
        retain_origin(&layout, &program, &identity)?;
        let origin: Origin = serde_json::from_slice(&lifecycle::read_private(
            &identity,
            layout.uid,
            MAX_IDENTITY,
        )?)?;
        let candidate = layout.install.join("versions/1.6.7");
        fs::create_dir(&candidate)?;
        fs::write(candidate.join("rustchan-cli"), b"non-executable trial")?;
        fs::remove_file(layout.install.join("current"))?;
        std::os::unix::fs::symlink("versions/1.6.7", layout.install.join("current"))?;
        let status = super::super::Status {
            installed: super::super::VERSION.to_owned(),
            target_version: Some("1.6.7".into()),
            phase: super::super::Phase::HealthChecking,
            ..super::super::Status::default()
        };
        crate::config::admin::atomic_replace(
            &layout.state.join("status.json"),
            &serde_json::to_string(&status)?,
        )?;
        anyhow::ensure!(known_controller(&layout, &origin)? == super::super::VERSION);
        let retained = layout
            .install
            .join("versions")
            .join(super::super::VERSION)
            .join("rustchan-cli");
        fs::set_permissions(&retained, fs::Permissions::from_mode(0o600))?;
        anyhow::ensure!(known_controller(&layout, &origin).is_err());
        fs::set_permissions(&retained, fs::Permissions::from_mode(0o700))?;
        fs::write(&retained, b"corrupt controller")?;
        anyhow::ensure!(known_controller(&layout, &origin).is_err());
        anyhow::ensure!(fs::read(program)? == b"saved source modifications");
        Ok(())
    }

    /// Even an executable owned by this UID needs a replaceable parent and safe ancestors.
    #[test]
    fn source_replaceability_rejects_links_and_shared_write() -> anyhow::Result<()> {
        let temporary = tempfile::tempdir()?;
        let uid = rustix::process::getuid().as_raw();
        let program = temporary.path().join("source");
        fs::write(&program, b"ordinary executable")?;
        fs::set_permissions(&program, fs::Permissions::from_mode(0o700))?;
        anyhow::ensure!(replaceable(&program, uid));
        let link = temporary.path().join("linked");
        std::os::unix::fs::symlink(&program, &link)?;
        anyhow::ensure!(!replaceable(&link, uid));
        fs::set_permissions(&program, fs::Permissions::from_mode(0o720))?;
        anyhow::ensure!(!replaceable(&program, uid));
        Ok(())
    }

    /// A cold retry can complete initial retention after the copy/pointer became durable.
    #[test]
    fn initial_retention_replays_exact_source_without_touching_data() -> anyhow::Result<()> {
        let temporary = tempfile::tempdir()?;
        let layout = Layout::prepare(temporary.path())?;
        let program = temporary.path().join("source");
        fs::write(&program, b"exact original source")?;
        fs::set_permissions(&program, fs::Permissions::from_mode(0o700))?;
        fs::write(temporary.path().join("settings.toml"), b"port = 8080\n")?;
        fs::write(
            temporary.path().join("chan.db"),
            b"existing database sentinel",
        )?;
        let identity = layout.state.join("origin.json");
        retain_origin(&layout, &program, &identity)?;
        let mut origin: Origin = serde_json::from_slice(&lifecycle::read_private(
            &identity,
            layout.uid,
            MAX_IDENTITY,
        )?)?;
        origin.phase = OriginPhase::Retaining;
        publish_origin(&layout, &origin, &identity)?;
        anyhow::ensure!(known_controller(&layout, &origin).is_err());
        finish_origin(&layout, &mut origin, &identity)?;
        anyhow::ensure!(known_controller(&layout, &origin)? == super::super::VERSION);
        engine(&layout)?.validate()?;
        anyhow::ensure!(
            fs::read(temporary.path().join("chan.db"))? == b"existing database sentinel"
                && fs::read(temporary.path().join("settings.toml"))? == b"port = 8080\n"
                && fs::read(program)? == b"exact original source"
        );
        Ok(())
    }
}
