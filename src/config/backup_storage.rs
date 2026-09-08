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

/// Validate and prepare an absolute private backup root and legacy subdirectories.
///
/// # Errors
/// Rejects invalid paths, overlap with live data, unsafe legacy subdirectories,
/// and directory creation, permission, write, sync, or probe removal failures.
pub fn prepare_backup_directory(path: &Path, config: &Config) -> anyhow::Result<PathBuf> {
    let root = resolve_backup_directory(path, config)?;
    // Check all existing child paths before creating or changing permissions.
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
    let root = prepare_backup_directory(path, &super::CONFIG)?;
    let value = root.to_str().context("backup_directory must be UTF-8")?;
    super::update_settings_file_entries_result(
        &[("backup_directory", super::toml_quote(value))],
        None,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::ensure;

    #[test]
    fn backup_directory_creates_private_nested_storage_without_probe_files() -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let path = temp.path().join("disk/nested/backups");
        let root = prepare_backup_directory(&path, &crate::config::tests::valid_config())?;
        ensure!(root == path.canonicalize()?);
        ensure!(root.join("full").is_dir() && root.join("boards").is_dir());
        ensure!(std::fs::read_dir(&root)?.count() == 2);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            ensure!(std::fs::metadata(root)?.permissions().mode() & 0o777 == 0o700);
        }
        Ok(())
    }

    #[test]
    fn backup_directory_rejects_invalid_paths_before_mutation() -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let config = crate::config::tests::valid_config();
        for path in [
            "",
            " ",
            "relative/backups",
            "/",
            "/tmp/../backups",
            "/bad\0path",
            "/bad\npath",
        ] {
            ensure!(prepare_backup_directory(Path::new(path), &config).is_err());
        }
        let file = temp.path().join("file");
        std::fs::write(&file, b"keep")?;
        ensure!(prepare_backup_directory(&file, &config).is_err());
        ensure!(prepare_backup_directory(&file.join("child"), &config).is_err());
        ensure!(std::fs::read(&file)? == b"keep");
        let mut config = config;
        config.upload_dir = temp.path().join("uploads").display().to_string();
        ensure!(
            prepare_backup_directory(&Path::new(&config.upload_dir).join("backups"), &config)
                .is_err()
        );
        ensure!(!Path::new(&config.upload_dir).exists());
        ensure!(prepare_backup_directory(&crate::config::data_dir(), &config).is_err());
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn backup_directory_rejects_symlink_children_and_live_data_aliases() -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let config = crate::config::tests::valid_config();
        let root = temp.path().join("backup");
        let outside = temp.path().join("outside");
        std::fs::create_dir(&root)?;
        std::fs::create_dir(&outside)?;
        std::os::unix::fs::symlink(&outside, root.join("full"))?;
        ensure!(prepare_backup_directory(&root, &config).is_err());
        ensure!(!root.join("boards").exists());
        let mut config = config;
        config.upload_dir = outside.display().to_string();
        let alias = temp.path().join("alias");
        std::os::unix::fs::symlink(&outside, &alias)?;
        ensure!(prepare_backup_directory(&alias, &config).is_err());
        std::os::unix::fs::symlink(temp.path().join("missing"), temp.path().join("dangling"))?;
        ensure!(prepare_backup_directory(&temp.path().join("dangling"), &config).is_err());
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn backup_directory_reports_unwritable_parent() -> anyhow::Result<()> {
        use std::os::unix::fs::PermissionsExt as _;
        let temp = tempfile::tempdir()?;
        let parent = temp.path().join("read-only");
        std::fs::create_dir(&parent)?;
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o500))?;
        // Privileged test runners may bypass mode bits; detect that without a
        // machine-specific path or changing the process's credentials.
        let probe = tempfile::tempfile_in(&parent);
        let result = prepare_backup_directory(
            &parent.join("backups"),
            &crate::config::tests::valid_config(),
        );
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o700))?;
        if probe.is_err() {
            let error = result.err().context("unwritable parent was accepted")?;
            ensure!(format!("{error:#}").contains("grant the RustChan service user"));
        }
        Ok(())
    }
}
