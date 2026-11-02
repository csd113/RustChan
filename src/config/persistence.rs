//! Cross-process settings leases and durable atomic file replacement.

use anyhow::{ensure, Context as _};
use std::io::Write as _;
use std::path::Path;

/// Persist private, synced bytes before replacing the original settings file.
#[expect(
    clippy::redundant_pub_crate,
    reason = "crate-visible facade re-exports require this visibility; pub would violate unreachable_pub"
)]
pub(crate) fn atomic_replace(path: &Path, content: &str) -> anyhow::Result<()> {
    let parent = path.parent().context("settings path needs a parent")?;
    let mut temporary = tempfile::Builder::new()
        .prefix(".settings_")
        .suffix(".tmp")
        .tempfile_in(parent)
        .context("create temporary settings file")?;
    super::restrict_private_file_permissions(temporary.path())?;
    temporary
        .write_all(content.as_bytes())
        .context("write temporary settings file")?;
    temporary
        .as_file()
        .sync_all()
        .context("sync temporary settings file")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        match std::fs::symlink_metadata(path) {
            Ok(metadata) => {
                ensure!(
                    metadata.is_file() && metadata.nlink() == 1,
                    "configuration destination must be a regular file without hardlinks"
                );
                let staged = temporary.as_file().metadata()?;
                if (staged.uid(), staged.gid()) != (metadata.uid(), metadata.gid()) {
                    rustix::fs::fchown(
                        temporary.as_file(),
                        Some(rustix::fs::Uid::from_raw(metadata.uid())),
                        Some(rustix::fs::Gid::from_raw(metadata.gid())),
                    )?;
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    drop(
        temporary
            .persist(path)
            .map_err(|e| e.error)
            .context("atomically replace settings.toml")?,
    );
    #[cfg(unix)]
    std::fs::File::open(parent)?.sync_all()?;
    Ok(())
}

/// Owns the settings lock until the configuration operation completes.
#[expect(
    clippy::redundant_pub_crate,
    reason = "crate-visible facade re-exports require this visibility; pub would violate unreachable_pub"
)]
pub(crate) struct SettingsLease(std::fs::File);

impl Drop for SettingsLease {
    fn drop(&mut self) {
        // Closing only this descriptor can leave a Unix lock held by a duplicate
        // inherited during concurrent process creation. Release it explicitly.
        if let Err(error) = self.0.unlock() {
            tracing::error!(target: concat!(env!("CARGO_CRATE_NAME"), "::config::admin"), %error, "failed to release settings lease");
        }
    }
}

/// Lock the fixed configuration resource across web saves and supervisor transactions.
///
/// # Errors
/// Rejects unsafe lock paths and concurrent saves, restarts or configuration recovery.
#[expect(
    clippy::redundant_pub_crate,
    reason = "crate-visible facade re-exports require this visibility; pub would violate unreachable_pub"
)]
pub(crate) fn settings_lease(path: &Path) -> anyhow::Result<SettingsLease> {
    let parent = path
        .parent()
        .context("settings path needs a parent")?
        .canonicalize()?;
    crate::utils::fs_security::reject_symlink_components(&parent)?;
    let lock_path = parent.join(".settings.lock");
    if lock_path.exists() {
        crate::utils::fs_security::assert_regular_file_no_symlink(&lock_path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt as _;
            ensure!(
                std::fs::metadata(&lock_path)?.nlink() == 1,
                "settings lock must not be hardlinked"
            );
        }
    }
    let mut options = std::fs::File::options();
    let _access_options = options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        let _platform_options = options
            .mode(0o600)
            .custom_flags(i32::try_from(rustix::fs::OFlags::NOFOLLOW.bits())?);
    }
    let file = options.open(lock_path)?;
    ensure!(
        file.metadata()?.is_file(),
        "settings lease must be a regular file"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        let metadata = file.metadata()?;
        ensure!(
            metadata.nlink() == 1,
            "settings lease must not be hardlinked"
        );
        // The updater may establish this inode during an upgrade from a pre-restart build.
        // Keep the fixed lock accessible to the web-owned data directory after handoff.
        let owner = std::fs::metadata(&parent)?;
        if (metadata.uid(), metadata.gid()) != (owner.uid(), owner.gid()) {
            rustix::fs::fchown(
                &file,
                Some(rustix::fs::Uid::from_raw(owner.uid())),
                Some(rustix::fs::Gid::from_raw(owner.gid())),
            )?;
        }
    }
    file.try_lock().context(
        "Another configuration save, update or restart is in progress. Retry after it completes.",
    )?;
    Ok(SettingsLease(file))
}
