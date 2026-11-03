//! Validation for administrator-selected backup storage.

use super::{Config, Path, PathBuf};
use anyhow::Context as _;
use std::io::Write as _;

/// Resolve a storage path without creating it, rejecting unsafe overlap.
fn resolve_backup_directory(path: &Path, config: &Config) -> anyhow::Result<PathBuf> {
    let text = path
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("backup_directory must be UTF-8"))?;
    anyhow::ensure!(
        !text.trim().is_empty() && !text.chars().any(char::is_control),
        "backup_directory must be a nonempty absolute path without control characters"
    );
    let resolved = super::resolve_storage_dir(path, "backup_directory")?;
    let data = absolute_resolved_path(&super::data_dir())?;
    anyhow::ensure!(
        !data.starts_with(&resolved),
        "backup_directory must not be the data directory or one of its parents"
    );
    for protected in [
        PathBuf::from(&config.upload_dir),
        super::runtime_dir(),
        super::logs_dir(),
        PathBuf::from(&config.database_path),
    ] {
        let protected = absolute_resolved_path(&protected)?;
        anyhow::ensure!(
            !resolved.starts_with(&protected) && !protected.starts_with(&resolved),
            "backup_directory must not overlap live uploads, runtime state, logs, or the database: {}",
            protected.display()
        );
    }
    Ok(resolved)
}

/// Resolve an existing file or possibly missing directory for overlap checks.
fn absolute_resolved_path(path: &Path) -> anyhow::Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    if absolute.is_file() {
        return absolute
            .canonicalize()
            .context("resolve protected file path");
    }
    super::resolve_storage_dir(&absolute, "protected storage path")
}

/// Check saved backup paths without creating directories or changing permissions.
pub(super) fn validate_backup_directory(path: &Path, config: &Config) -> anyhow::Result<()> {
    validate_backup_children(&resolve_backup_directory(path, config)?)
}

/// Validate and prepare an absolute private backup root and legacy subdirectories.
///
/// # Errors
/// Rejects invalid paths, overlap with live data, unsafe legacy subdirectories,
/// and directory creation, permission, write, sync, or probe removal failures.
pub fn prepare_backup_directory(path: &Path, config: &Config) -> anyhow::Result<PathBuf> {
    let root = resolve_backup_directory(path, config)?;
    validate_backup_children(&root)?;
    for dir in [&root, &root.join("full"), &root.join("boards")] {
        prepare_private_storage(dir).with_context(|| {
            format!(
                "backup_directory '{}' is unusable; check the mount and grant the RustChan service user directory access, permission to set private modes, and read/write/delete access",
                dir.display()
            )
        })?;
    }
    Ok(root)
}

/// Inspect all legacy children before creating or changing any directory.
fn validate_backup_children(root: &Path) -> anyhow::Result<()> {
    for child in [root.join("full"), root.join("boards")] {
        match std::fs::symlink_metadata(&child) {
            Ok(metadata) => anyhow::ensure!(
                metadata.is_dir() && !metadata.file_type().is_symlink(),
                "backup_directory child must be a real directory, not a file or symlink: {}",
                child.display()
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error).context("inspect backup_directory child"),
        }
    }
    Ok(())
}

/// Exercise private directory access with a unique, automatically cleaned probe.
fn prepare_private_storage(path: &Path) -> anyhow::Result<()> {
    super::ensure_private_dir(path)?;
    let _entries = std::fs::read_dir(path)?;
    let mut probe = tempfile::Builder::new()
        .prefix(".backup-write-probe-")
        .tempfile_in(path)?;
    probe.write_all(b"RustChan backup storage probe")?;
    probe.as_file().sync_all()?;
    probe.close()?;
    Ok(())
}

/// Persist the backup root for the next restart using the existing atomic writer.
///
/// # Errors
/// Returns validation or settings-file persistence failures without changing the
/// running process's backup root. Passing the default path explicitly resets it.
pub fn update_settings_file_backup_directory(path: &Path) -> anyhow::Result<()> {
    prepare_and_persist(path, &super::CONFIG, |root| {
        let value = root.to_str().context("backup_directory must be UTF-8")?;
        super::update_settings_file_entries_result(
            &[("backup_directory", super::toml_quote(value))],
            None,
        )
    })
}

/// Remove only newly prepared, still-empty directories if preparation or saving fails.
fn prepare_and_persist(
    path: &Path,
    config: &Config,
    persist: impl FnOnce(&Path) -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    let root = resolve_backup_directory(path, config)?;
    validate_backup_children(&root)?;
    let mut created = Vec::new();
    for target in [root.clone(), root.join("full"), root.join("boards")] {
        let mut next = Some(target.as_path());
        while let Some(directory) = next {
            match std::fs::symlink_metadata(directory) {
                Ok(_) => break,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    if !created.iter().any(|entry| entry == directory) {
                        created.push(directory.to_path_buf());
                    }
                    next = directory.parent();
                }
                Err(error) => return Err(error).context("inspect backup preparation path"),
            }
        }
    }
    // Record actual successful creates rather than assuming a path still belongs
    // to us after the preflight; another operator may create it concurrently.
    created.sort_by_key(|directory| directory.components().count());
    let mut prepared = Vec::new();
    let result = created
        .iter()
        .try_for_each(|directory| match std::fs::create_dir(directory) {
            Ok(()) => {
                prepared.push(directory.clone());
                Ok(())
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
            Err(error) => Err(error).context("create backup preparation directory"),
        })
        .and_then(|()| prepare_backup_directory(&root, config))
        .and_then(|root| persist(&root));
    if let Err(error) = result {
        prepared.sort_by_key(|directory| std::cmp::Reverse(directory.components().count()));
        for directory in prepared {
            match std::fs::remove_dir(&directory) {
                Ok(()) => {},
                Err(cleanup) if cleanup.kind() == std::io::ErrorKind::NotFound => {},
                Err(cleanup) => return Err(cleanup).with_context(|| format!("Backup directory save failed ({error}); cannot remove newly prepared directory {}; inspect it before retrying", directory.display())),
            }
        }
        return Err(error);
    }
    Ok(())
}
