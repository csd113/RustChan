//! Cleanup ownership for private restore databases and rollback evidence.

use std::path::{Path, PathBuf};

/// Create a secret-bearing file exclusively, with private permissions from its first byte.
///
/// # Errors
/// Returns an error if the file exists or cannot be created privately.
pub(super) fn open_private_file_new(path: &Path) -> std::io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    let _create_options = options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        let _mode_options = options.mode(0o600);
    }
    options.open(path)
}

/// Clean uncommitted extraction data; preserve it once a durable swap references it.
#[derive(Default)]
pub(super) struct StagingDirectories {
    paths: Vec<PathBuf>,
    retained: bool,
}

impl StagingDirectories {
    pub(super) fn track(&mut self, path: &Path) {
        self.paths.push(path.to_path_buf());
    }
    pub(super) const fn retain(&mut self, retain: bool) {
        self.retained = retain;
    }
}

impl Drop for StagingDirectories {
    fn drop(&mut self) {
        if self.retained {
            return;
        }
        for path in &self.paths {
            if let Err(error) = std::fs::remove_dir_all(path) {
                if error.kind() != std::io::ErrorKind::NotFound {
                    tracing::warn!(path = %path.display(), %error, "Could not remove restore staging directory");
                }
            }
        }
    }
}

pub(super) struct FullRestoreTempPaths {
    pub(super) temp_db: PathBuf,
    pub(super) db_snapshot: PathBuf,
}

pub(super) struct FullRestoreTempCleanupGuard {
    db_paths: [PathBuf; 2],
    retain_snapshot: bool,
}

impl FullRestoreTempCleanupGuard {
    pub(super) fn new(paths: &FullRestoreTempPaths) -> Self {
        Self {
            db_paths: [paths.temp_db.clone(), paths.db_snapshot.clone()],
            retain_snapshot: false,
        }
    }
}

impl FullRestoreTempCleanupGuard {
    pub(super) const fn retain_snapshot(&mut self, retain: bool) {
        self.retain_snapshot = retain;
    }
}

impl Drop for FullRestoreTempCleanupGuard {
    fn drop(&mut self) {
        for (index, path) in self.db_paths.iter().enumerate() {
            if index == 1 && self.retain_snapshot {
                tracing::error!(path = %path.display(), "Retained restore safety snapshot for recovery");
                continue;
            }
            remove_sqlite_db_and_sidecars(path);
        }
    }
}

fn sqlite_db_and_sidecar_paths(path: &Path) -> Vec<PathBuf> {
    let mut paths = vec![path.to_path_buf()];
    let Some(file_name) = path.file_name() else {
        return paths;
    };

    for suffix in ["-wal", "-shm", "-journal"] {
        let mut sidecar_name = file_name.to_os_string();
        sidecar_name.push(suffix);
        paths.push(path.with_file_name(sidecar_name));
    }
    paths
}

/// Removes `SQLite` database and sidecars.
fn remove_sqlite_db_and_sidecars(path: &Path) {
    for candidate in sqlite_db_and_sidecar_paths(path) {
        if let Err(error) = std::fs::remove_file(&candidate) {
            if error.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!(
                    path = %candidate.display(),
                    %error,
                    "Failed to remove full restore temporary SQLite file"
                );
            }
        }
    }
}
