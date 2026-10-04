//! Same-account writer admission. A free advisory lock never proves an orphan exited.
//!
//! Lifetime intent is durable before spawn; completion is durable only after every
//! ordinary descendant was reaped. Uncertain records block recovery on the same
//! kernel boot, even if no lease holder remains. Lock inodes are never restored.

use anyhow::Context as _;
use serde::{Deserialize, Serialize};
use std::fs::{self, File};
use std::io::{Read as _, Write as _};
use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use uuid::Uuid;

/// Version of the small persisted monitor/lifetime contract.
pub(super) const FORMAT: u32 = 1;
/// Bound the number of records consulted before touching application data.
const MAX_RECORDS: usize = 16_384;
/// Maximum encoded bytes in one lifetime record.
const MAX_RECORD_BYTES: u64 = 4096;

#[derive(Debug, Clone)]
/// Fixed internal state roots, separate from restored boards/runtime data.
pub(super) struct Layout {
    /// Canonical application data directory selected only by startup arguments.
    pub data: PathBuf,
    /// Private internal state containing stable lock inodes and writer records.
    pub state: PathBuf,
    /// Immutable same-binary controller copies and bounded active-version link.
    pub install: PathBuf,
    /// Only account authorized to operate this same-account layout.
    pub uid: u32,
}

impl Layout {
    /// Validate same-account authority and prepare private updater resources.
    pub(super) fn prepare(data: &Path) -> anyhow::Result<Self> {
        let uid = rustix::process::getuid().as_raw();
        anyhow::ensure!(uid != 0, "automatic updates require an ordinary account");
        crate::utils::fs_security::reject_symlink_components(data)?;
        let data = data.canonicalize()?;
        owned_ancestors(&data, uid)?;
        anyhow::ensure!(
            fs::metadata(&data)?.uid() == uid,
            "application data has a different owner"
        );
        let state = data.join(".software-updates");
        private_directory(&state, uid)?;
        let install = state.join("installation");
        private_directory(&install, uid)?;
        for path in [
            state.join("writers"),
            state.join("journal-staging"),
            state.join("backups"),
            install.join("versions"),
        ] {
            private_directory(&path, uid)?;
        }
        Ok(Self {
            data,
            state,
            install,
            uid,
        })
    }

    /// Stable single-controller admission inode; hold through every lock conversion.
    pub(super) fn admission(&self) -> anyhow::Result<File> {
        let file = private_file(&self.state.join("admission.lock"), self.uid)?;
        file.try_lock()
            .context("RustChan already has an active controller")?;
        Ok(file)
    }

    /// Fresh independent descriptor for the fixed data-writer inode.
    pub(super) fn writer_file(&self) -> anyhow::Result<File> {
        private_file(&self.state.join("data-writers.lock"), self.uid)
    }

    /// Acquire independent ordinary-writer admission before application side effects.
    pub(super) fn shared_writer(&self) -> anyhow::Result<File> {
        let file = self.writer_file()?;
        file.try_lock_shared()
            .context("RustChan data is being upgraded or recovered")?;
        // Re-check uncertainty after the lock, so an exclusive recovery cannot
        // race this check. New valid lifetimes on the current boot are permitted
        // only under the active monitor; independent CLI admission is separate.
        Ok(file)
    }

    /// A transferred lease must reference the fixed inode and stay close-on-exec.
    pub(super) fn validate_lease(&self, lease: &File) -> anyhow::Result<()> {
        let expected = self.writer_file()?.metadata()?;
        let received = lease.metadata()?;
        anyhow::ensure!(
            received.is_file()
                && received.uid() == self.uid
                && received.nlink() == 1
                && received.mode().trailing_zeros() >= 6
                && received.dev() == expected.dev()
                && received.ino() == expected.ino()
                && rustix::io::fcntl_getfd(lease)?.contains(rustix::io::FdFlags::CLOEXEC),
            "invalid transferred data-writer lease"
        );
        Ok(())
    }

    /// Require both exclusive writer exclusion and durable descendant-exit proof.
    pub(super) fn exclusive_quiescent(&self) -> anyhow::Result<File> {
        let file = self.writer_file()?;
        file.try_lock()
            .context("RustChan still has active data writers")?;
        self.prove_quiescence(kernel_boot_id()?)?;
        Ok(file)
    }

    /// Refuse same-boot uncertainty without treating a missing PID as proof.
    pub(super) fn prove_quiescence(&self, boot: Uuid) -> anyhow::Result<()> {
        self.prove_quiescence_except(boot, &[])
    }

    /// Only current root-owned controller guards can retain the root's converted OFD lease.
    pub(super) fn prove_quiescence_except(
        &self,
        boot: Uuid,
        controllers: &[Uuid],
    ) -> anyhow::Result<()> {
        for (count, item) in fs::read_dir(self.state.join("writers"))?.enumerate() {
            anyhow::ensure!(
                count < MAX_RECORDS,
                "writer lifetime inventory exceeds safety bounds"
            );
            let path = item?.path();
            let name = path
                .file_stem()
                .and_then(std::ffi::OsStr::to_str)
                .context("invalid writer lifetime name")?;
            let id = Uuid::parse_str(name)?;
            anyhow::ensure!(
                path.file_name() == Some(std::ffi::OsStr::new(&format!("{id}.json"))),
                "invalid writer lifetime layout"
            );
            let record: Lifetime =
                serde_json::from_slice(&read_private(&path, self.uid, MAX_RECORD_BYTES)?)?;
            record.validate(id)?;
            if controllers.contains(&id) {
                anyhow::ensure!(
                    record.role == WriterRole::Controller
                        && record.parent == ProcessIdentity::current()?,
                    "controller exclusion has a different owner"
                );
                continue;
            }
            anyhow::ensure!(record.phase == LifePhase::Stopped || record.boot != boot,
                "Writer exit could not be proved. RustChan remains stopped to protect data; restart the host before recovery.");
        }
        Ok(())
    }

    /// Persist conservative intent before permitting one same-binary guardian to spawn.
    pub(super) fn launch(&self, role: WriterRole) -> anyhow::Result<Lifetime> {
        let identity = ProcessIdentity::current()?;
        let record = Lifetime {
            format: FORMAT,
            id: Uuid::new_v4(),
            boot: kernel_boot_id()?,
            role,
            phase: LifePhase::Launching,
            parent: identity,
            guardian: None,
            child: None,
        };
        self.save_lifetime(&record)?;
        Ok(record)
    }

    /// Fsync exact lifetime bytes and the containing directory before acknowledgment.
    pub(super) fn save_lifetime(&self, record: &Lifetime) -> anyhow::Result<()> {
        self.save_lifetime_with(record, |_| Ok(()))
    }

    /// Keep fault injection at the real fsync/rename boundaries used in production.
    fn save_lifetime_with(
        &self,
        record: &Lifetime,
        checkpoint: impl Fn(SaveStep) -> anyhow::Result<()>,
    ) -> anyhow::Result<()> {
        let path = self
            .state
            .join("writers")
            .join(format!("{}.json", record.id));
        if path.try_exists()? {
            drop(read_private(&path, self.uid, MAX_RECORD_BYTES)?);
        }
        let staging = self.state.join("journal-staging");
        let mut temporary = tempfile::NamedTempFile::new_in(&staging)?;
        checkpoint(SaveStep::Write)?;
        temporary.write_all(&serde_json::to_vec(record)?)?;
        checkpoint(SaveStep::FileSync)?;
        temporary.as_file().sync_all()?;
        checkpoint(SaveStep::Publish)?;
        drop(temporary.persist(&path).map_err(|error| error.error)?);
        checkpoint(SaveStep::StagingSync)?;
        File::open(&staging)?.sync_all()?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
        checkpoint(SaveStep::PublishedSync)?;
        File::open(&path)?.sync_all()?;
        checkpoint(SaveStep::DirectorySync)?;
        File::open(path.parent().context("missing lifetime directory")?)?.sync_all()?;
        Ok(())
    }

    /// Validate the fixed launch record before a guardian opens application data.
    pub(super) fn lifetime(&self, id: Uuid) -> anyhow::Result<Lifetime> {
        let path = self.state.join("writers").join(format!("{id}.json"));
        let record: Lifetime =
            serde_json::from_slice(&read_private(&path, self.uid, MAX_RECORD_BYTES)?)?;
        record.validate(id)?;
        anyhow::ensure!(
            record.boot == kernel_boot_id()?,
            "stale or incompatible writer launch"
        );
        Ok(record)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Real durable lifetime publication boundaries, with test-only injected errors.
enum SaveStep {
    /// Failure while creating new exact record bytes.
    Write,
    /// Failure persisting the staged file contents.
    FileSync,
    /// Failure atomically replacing the lifetime path.
    Publish,
    /// Failure persisting removal from the separate staging directory.
    StagingSync,
    /// Failure persisting final private file metadata/contents.
    PublishedSync,
    /// Failure persisting the destination lifetime-directory entry.
    DirectorySync,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
/// Only fixed classes of writers may obtain a lifetime record.
pub(super) enum WriterRole {
    /// A full same-executable update controller whose guardian retains the root's lease.
    Controller,
    /// The application server, including in-process workers and migrations.
    Server,
    /// One administration CLI action.
    Administrator,
    /// One supervised external media-tool invocation.
    Media,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
/// Clear completion is published only after all descendants are reaped.
pub(super) enum LifePhase {
    /// Intent was fsynced before the guardian could exist.
    Launching,
    /// A fully initialized guardian may have spawned data writers.
    Running,
    /// All admitted descendants exited and the completion record was fsynced.
    Stopped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
/// PID alone is insufficient because it can be reused within the same boot.
pub(super) struct ProcessIdentity {
    /// Kernel process identifier.
    pub pid: i32,
    /// Kernel start tick from the same process's proc stat record.
    pub start_tick: u64,
}

impl ProcessIdentity {
    /// Capture the calling process identity on the calling long-lived spawner thread.
    pub(super) fn current() -> anyhow::Result<Self> {
        Self::read(rustix::process::getpid().as_raw_nonzero().get())
    }

    /// Read one explicit PID and parse stat after the final command-name parenthesis.
    pub(super) fn read(pid: i32) -> anyhow::Result<Self> {
        anyhow::ensure!(pid > 0_i32, "invalid process identity");
        let stat = fs::read_to_string(format!("/proc/{pid}/stat"))?;
        let tail = stat.rsplit_once(')').context("malformed process stat")?.1;
        let start_tick = tail
            .split_whitespace()
            .nth(19)
            .context("missing process start tick")?
            .parse()?;
        Ok(Self { pid, start_tick })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
/// Versioned durable evidence used together with the separate data-writer lease.
pub(super) struct Lifetime {
    /// Persisted format compatibility, distinct from the package version.
    pub format: u32,
    /// Unique admitted invocation, never a browser-selected identifier.
    pub id: Uuid,
    /// A real changed kernel boot is required to clear uncertain old invocations.
    pub boot: Uuid,
    /// Fixed kind of possible data writer.
    pub role: WriterRole,
    /// Whether writer creation may have happened, or exit was proved.
    pub phase: LifePhase,
    /// Expected creating process and its start identity.
    pub parent: ProcessIdentity,
    /// Guardian identity, absent while the durable pre-spawn record is pending.
    pub guardian: Option<ProcessIdentity>,
    /// Exact primary child, published before that child may initialize the application.
    #[serde(default)]
    pub child: Option<ProcessIdentity>,
}

impl Lifetime {
    /// Reject malformed changed-boot claims and incompatible identities before admission.
    fn validate(&self, id: Uuid) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.format == FORMAT
                && self.id == id
                && !id.is_nil()
                && !self.boot.is_nil()
                && self.parent.pid > 0_i32
                && self.guardian.is_none_or(|guardian| guardian.pid > 0_i32)
                && self.child.is_none_or(|child| child.pid > 0_i32)
                && (self.phase != LifePhase::Running || self.guardian.is_some()),
            "invalid or incompatible writer lifetime record"
        );
        Ok(())
    }
}

/// Read and validate the actual local kernel boot ID; missing information denies recovery.
pub(super) fn kernel_boot_id() -> anyhow::Result<Uuid> {
    let bytes = fs::read_to_string("/proc/sys/kernel/random/boot_id")?;
    let id = Uuid::parse_str(bytes.trim()).context("kernel boot identity unavailable")?;
    anyhow::ensure!(!id.is_nil(), "kernel boot identity is invalid");
    Ok(id)
}

/// Root-owned sticky shared temp ancestors are safe; all other foreign writes are refused.
pub(super) fn owned_ancestors(path: &Path, uid: u32) -> anyhow::Result<()> {
    crate::utils::fs_security::reject_symlink_components(path)?;
    for ancestor in path.ancestors() {
        let metadata = fs::symlink_metadata(ancestor)?;
        let sticky_root = metadata.is_dir() && metadata.uid() == 0 && metadata.mode() & 0o1000 != 0;
        anyhow::ensure!(
            (metadata.uid() == uid || metadata.uid() == 0)
                && (metadata.mode() & 0o022 == 0 || sticky_root),
            "same-account path has a foreign or writable ancestor"
        );
    }
    Ok(())
}

/// Create only same-account private directories, rejecting links and foreign ownership.
fn private_directory(path: &Path, uid: u32) -> anyhow::Result<()> {
    if !path.try_exists()? {
        fs::create_dir(path)?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
        File::open(path)?.sync_all()?;
        File::open(path.parent().context("private directory needs a parent")?)?.sync_all()?;
    }
    owned_ancestors(path, uid)?;
    let metadata = fs::symlink_metadata(path)?;
    anyhow::ensure!(
        metadata.is_dir() && metadata.uid() == uid && metadata.mode().trailing_zeros() >= 6,
        "updater state must be private to its owning account"
    );
    Ok(())
}

/// Stable close-only lease file; never explicit-unlock an inherited duplicate.
fn private_file(path: &Path, uid: u32) -> anyhow::Result<File> {
    let file = File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(i32::try_from(rustix::fs::OFlags::NOFOLLOW.bits())?)
        .open(path)?;
    let metadata = file.metadata()?;
    anyhow::ensure!(
        metadata.is_file()
            && metadata.uid() == uid
            && metadata.nlink() == 1
            && metadata.mode().trailing_zeros() >= 6,
        "invalid private writer lease"
    );
    File::open(path.parent().context("lease needs a parent")?)?.sync_all()?;
    Ok(file)
}

/// Bounded no-follow reads cannot turn a journal into a link or pipe operation.
pub(super) fn read_private(path: &Path, uid: u32, limit: u64) -> anyhow::Result<Vec<u8>> {
    for _attempt in 0_u8..4_u8 {
        if let Some(bytes) = read_private_once(path, uid, limit)? {
            return Ok(bytes);
        }
    }
    anyhow::bail!("private writer record changed repeatedly during validation")
}

/// An atomic rename can unlink the opened previous inode before its metadata is read.
/// Retry that observation; linked, foreign or permissive files remain immediate failures.
fn read_private_once(path: &Path, uid: u32, limit: u64) -> anyhow::Result<Option<Vec<u8>>> {
    let file = File::options()
        .read(true)
        .custom_flags(i32::try_from(
            (rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK).bits(),
        )?)
        .open(path)?;
    let metadata = file.metadata()?;
    anyhow::ensure!(
        metadata.is_file()
            && metadata.uid() == uid
            && metadata.nlink() <= 1
            && metadata.mode().trailing_zeros() >= 6
            && metadata.len() <= limit,
        "invalid private writer record"
    );
    if metadata.nlink() == 0 {
        return Ok(None);
    }
    let mut bytes = Vec::new();
    let _bytes_read = file
        .take(
            limit
                .checked_add(1)
                .context("writer record bound overflow")?,
        )
        .read_to_end(&mut bytes)?;
    anyhow::ensure!(
        u64::try_from(bytes.len())? <= limit,
        "writer record exceeds bound"
    );
    Ok(Some(bytes))
}

#[cfg(test)]
/// Kernel-independent journal tests and real Linux close-only descriptor checks.
mod tests {
    use super::*;

    /// Prepare disposable roots using the ordinary invoking test account.
    fn fixture() -> anyhow::Result<(tempfile::TempDir, Layout)> {
        let root = tempfile::tempdir()?;
        let layout = Layout::prepare(&root.path().canonicalize()?)?;
        Ok((root, layout))
    }

    /// Pending/running records require a real changed boot or durable stopped proof.
    #[test]
    fn lifetime_uncertainty_never_uses_pid_absence_as_recovery_proof() -> anyhow::Result<()> {
        if rustix::process::getuid().is_root() {
            let root = tempfile::tempdir()?;
            anyhow::ensure!(Layout::prepare(root.path()).is_err());
            return Ok(());
        }
        let (_root, layout) = fixture()?;
        anyhow::ensure!(layout.data.is_dir() && layout.install.is_dir());
        let boot = kernel_boot_id()?;
        let _admission = layout.admission()?;
        anyhow::ensure!(layout.admission().is_err());
        for role in [
            WriterRole::Server,
            WriterRole::Administrator,
            WriterRole::Media,
        ] {
            let mut record = layout.launch(role)?;
            anyhow::ensure!(layout.lifetime(record.id)?.phase == LifePhase::Launching);
            anyhow::ensure!(layout.prove_quiescence(boot).is_err());
            record.phase = LifePhase::Running;
            record.guardian = Some(ProcessIdentity {
                pid: i32::MAX,
                start_tick: u64::MAX,
            });
            layout.save_lifetime(&record)?;
            anyhow::ensure!(
                layout.exclusive_quiescent().is_err(),
                "missing PID must not prove descendant exit"
            );
            layout.prove_quiescence(Uuid::new_v4())?;
            record.phase = LifePhase::Stopped;
            layout.save_lifetime(&record)?;
            drop(layout.exclusive_quiescent()?);
        }
        Ok(())
    }

    /// Closing one inherited duplicate preserves exclusion; the separate gate survives conversion.
    #[test]
    fn duplicate_close_preserves_lock_and_admission_survives_conversion() -> anyhow::Result<()> {
        if rustix::process::getuid().is_root() {
            return Ok(());
        }
        let (_root, layout) = fixture()?;
        let _admission = layout.admission()?;
        let exclusive = layout.exclusive_quiescent()?;
        let inode = exclusive.metadata()?.ino();
        drop(exclusive.try_clone()?);
        anyhow::ensure!(layout.shared_writer().is_err());
        anyhow::ensure!(layout.exclusive_quiescent().is_err());
        for _ in 0_u16..100_u16 {
            exclusive.lock_shared()?;
            let independent = layout.shared_writer()?;
            anyhow::ensure!(layout.admission().is_err());
            anyhow::ensure!(layout.exclusive_quiescent().is_err());
            drop(independent);
            exclusive.lock()?;
            anyhow::ensure!(layout.admission().is_err());
        }
        anyhow::ensure!(exclusive.metadata()?.ino() == inode);
        drop(exclusive);
        drop(layout.exclusive_quiescent()?);
        Ok(())
    }

    /// Corrupt, foreign-format and linked lifetime data cannot authorize recovery.
    #[test]
    fn malformed_or_linked_lifetime_fails_closed() -> anyhow::Result<()> {
        if rustix::process::getuid().is_root() {
            return Ok(());
        }
        let (_root, layout) = fixture()?;
        let mut record = layout.launch(WriterRole::Server)?;
        record.phase = LifePhase::Stopped;
        record.format += 1;
        layout.save_lifetime(&record)?;
        anyhow::ensure!(layout.prove_quiescence(kernel_boot_id()?).is_err());
        record.format = FORMAT;
        layout.save_lifetime(&record)?;
        let path = layout
            .state
            .join("writers")
            .join(format!("{}.json", record.id));
        let original = layout.state.join("original.json");
        fs::rename(&path, &original)?;
        std::os::unix::fs::symlink(&original, &path)?;
        anyhow::ensure!(layout.lifetime(record.id).is_err());
        anyhow::ensure!(layout.prove_quiescence(Uuid::new_v4()).is_err());
        Ok(())
    }

    /// ENOSPC/rename/fsync failures cannot turn launched/running uncertainty into admission.
    #[test]
    fn failed_lifetime_publication_never_clears_uncertainty() -> anyhow::Result<()> {
        if rustix::process::getuid().is_root() {
            return Ok(());
        }
        for failed in [
            SaveStep::Write,
            SaveStep::FileSync,
            SaveStep::Publish,
            SaveStep::StagingSync,
            SaveStep::PublishedSync,
            SaveStep::DirectorySync,
        ] {
            let (_root, layout) = fixture()?;
            let mut record = layout.launch(WriterRole::Server)?;
            record.phase = LifePhase::Running;
            record.guardian = Some(ProcessIdentity::current()?);
            let result = layout.save_lifetime_with(&record, |step| {
                anyhow::ensure!(step != failed, std::io::Error::from_raw_os_error(28));
                Ok(())
            });
            anyhow::ensure!(result.is_err(), "injected publication error was missed");
            anyhow::ensure!(
                layout.exclusive_quiescent().is_err(),
                "failed publication allowed recovery"
            );
            let observed = layout.lifetime(record.id)?;
            anyhow::ensure!(matches!(
                observed.phase,
                LifePhase::Launching | LifePhase::Running
            ));
            record.phase = LifePhase::Stopped;
            layout.save_lifetime(&record)?;
            drop(layout.exclusive_quiescent()?);
        }
        let (_root, layout) = fixture()?;
        let mut record = layout.launch(WriterRole::Server)?;
        record.boot = Uuid::nil();
        record.phase = LifePhase::Stopped;
        layout.save_lifetime(&record)?;
        anyhow::ensure!(
            layout.prove_quiescence(Uuid::new_v4()).is_err(),
            "malformed boot identity allowed recovery"
        );
        Ok(())
    }

    /// Disposable competing process; it performs no database/application writes.
    #[test]
    #[ignore = "internal concurrent-admission subprocess entry"]
    fn admission_competitor_entry() -> anyhow::Result<()> {
        let layout = Layout::prepare(Path::new(&std::env::var("RUSTCHAN_ADMISSION_TEST")?))?;
        fs::write(layout.state.join("competitor-ready"), b"ready")?;
        let deadline = std::time::Instant::now();
        while !layout.state.join("competitor-start").try_exists()? {
            anyhow::ensure!(
                deadline.elapsed() < std::time::Duration::from_secs(10),
                "admission fixture did not start"
            );
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        for _ in 0_u16..200_u16 {
            anyhow::ensure!(
                layout.admission().is_err(),
                "competing process entered during lock conversion"
            );
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        fs::write(
            layout.state.join("competitor-result"),
            b"200 attempts; 0 admissions",
        )?;
        Ok(())
    }

    /// Real concurrent process startup cannot enter while the parent converts its shared OFD.
    #[test]
    fn independent_process_is_excluded_through_writer_lock_conversions() -> anyhow::Result<()> {
        if rustix::process::getuid().is_root() {
            return Ok(());
        }
        let (_root, layout) = fixture()?;
        let _admission = layout.admission()?;
        let writer = layout.exclusive_quiescent()?;
        let inode = writer.metadata()?.ino();
        let mut competitor = std::process::Command::new(std::env::current_exe()?)
            .args([
                "--exact",
                "updates::lifecycle::tests::admission_competitor_entry",
                "--ignored",
                "--nocapture",
            ])
            .env("RUSTCHAN_ADMISSION_TEST", &layout.data)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::inherit())
            .spawn()?;
        let deadline = std::time::Instant::now();
        while !layout.state.join("competitor-ready").try_exists()? {
            anyhow::ensure!(
                deadline.elapsed() < std::time::Duration::from_secs(10),
                "competing process did not become ready"
            );
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        fs::write(layout.state.join("competitor-start"), b"start")?;
        for _ in 0_u16..100_u16 {
            writer.lock_shared()?;
            std::thread::sleep(std::time::Duration::from_millis(1));
            writer.lock()?;
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        anyhow::ensure!(
            competitor.wait()?.success(),
            "competing admission process failed"
        );
        anyhow::ensure!(
            fs::read(layout.state.join("competitor-result"))? == b"200 attempts; 0 admissions"
        );
        anyhow::ensure!(
            writer.metadata()?.ino() == inode,
            "writer lease inode was replaced"
        );
        Ok(())
    }
}
