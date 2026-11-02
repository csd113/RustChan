#![expect(
    unused_crate_dependencies,
    reason = "focused integration harness exercises only backup safety dependencies"
)]
//! Regression checks for coordination, recovery evidence, and backup resource limits.

use chan::models;

#[path = "../../../../middleware/data_access.rs"]
pub(crate) mod data_access;
#[expect(
    clippy::redundant_pub_crate,
    reason = "included production helpers retain their owning subsystem visibility in this regression harness"
)]
#[path = "../limits.rs"]
pub(crate) mod limits;
#[expect(
    clippy::redundant_pub_crate,
    reason = "included production helpers retain their owning subsystem visibility in this regression harness"
)]
#[path = "../recovery_files.rs"]
pub(crate) mod recovery_files;
#[expect(
    clippy::redundant_pub_crate,
    reason = "included production helpers retain their owning subsystem visibility in this regression harness"
)]
#[path = "../retention.rs"]
pub(crate) mod retention;

#[cfg(test)]
mod tests {
    use super::{data_access, limits, recovery_files, retention};
    use chan::{models::BackupInfo, pending_fs};
    use std::{io, path::Path, sync::mpsc, time::Duration};

    macro_rules! ensure {
        ($condition:expr_2021 $(, $message:expr_2021)?) => {
            if !$condition {
                return Err(io::Error::other(concat!(
                    "assertion failed: ",
                    stringify!($condition)
                ))
                .into());
            }
        };
    }
    macro_rules! ensure_eq {
        ($actual:expr_2021, $expected:expr_2021) => {
            ensure!($actual == $expected);
        };
    }

    fn unavailable() -> io::Error {
        io::Error::other("expected access lease")
    }

    #[expect(
        clippy::significant_drop_tightening,
        reason = "tests deliberately retain and move leases across cancellation and spawned work"
    )]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancelled_request_does_not_release_detached_blocking_writer(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let gate = data_access::DataAccess::default();
        let lease = gate.try_read().ok_or_else(unavailable)?;
        let (started_tx, started_rx) = mpsc::channel();
        let (finish_tx, finish_rx) = mpsc::channel();
        let request = tokio::spawn(data_access::scope(lease, async move {
            data_access::spawn_blocking(move || {
                let _sent = started_tx.send(());
                let _finished = finish_rx.recv();
            })
            .await
        }));
        started_rx.recv_timeout(Duration::from_secs(2))?;
        request.abort();
        let _cancelled = request.await;
        let exclusive_gate = gate.clone();
        let exclusive = tokio::spawn(async move { exclusive_gate.exclusive().await });
        tokio::time::timeout(Duration::from_secs(2), async {
            while gate.try_read().is_some() {
                tokio::task::yield_now().await;
            }
        })
        .await?;
        ensure!(
            !exclusive.is_finished(),
            "blocking writer must drain before maintenance starts"
        );
        finish_tx.send(())?;
        let exclusive_lease = tokio::time::timeout(Duration::from_secs(2), exclusive)
            .await??
            .ok_or_else(unavailable)?;
        ensure!(gate.try_read().is_none());
        drop(exclusive_lease);
        ensure!(gate.try_read().is_some());
        Ok(())
    }

    #[expect(
        clippy::significant_drop_tightening,
        reason = "tests deliberately retain and move leases across cancellation and spawned work"
    )]
    #[tokio::test]
    async fn detached_async_jobs_inherit_access_and_recovery_closes_admission(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let gate = data_access::DataAccess::default();
        let lease = gate.try_read().ok_or_else(unavailable)?;
        let (finish_tx, finish_rx) = tokio::sync::oneshot::channel();
        let (child,) =
            data_access::scope(lease, async move { (data_access::spawn(finish_rx),) }).await;
        let exclusive_gate = gate.clone();
        let exclusive = tokio::spawn(async move { exclusive_gate.exclusive().await });
        tokio::task::yield_now().await;
        ensure!(!exclusive.is_finished());
        finish_tx.send(()).map_err(|()| unavailable())?;
        child.await??;
        drop(exclusive.await?.ok_or_else(unavailable)?);
        gate.block();
        ensure!(gate.is_blocked());
        ensure!(gate.try_read().is_none());
        ensure!(gate.exclusive().await.is_none());
        Ok(())
    }

    #[test]
    fn aggregate_extraction_budget_is_enforced_before_writes(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut output = Vec::new();
        let mut total = 0;
        let _copied = limits::copy_limited_with_total_budget(
            &mut &[1_u8; 6][..],
            &mut output,
            100,
            &mut total,
            10,
            "archive",
        )?;
        let result = limits::copy_limited_with_total_budget(
            &mut &[2_u8; 6][..],
            &mut output,
            100,
            &mut total,
            10,
            "archive",
        );
        ensure!(result.is_err());
        ensure!(
            output.len() <= 10,
            "failed entries must not exceed the remaining disk budget"
        );
        ensure_eq!(total, 6);
        Ok(())
    }

    #[test]
    fn verified_inventory_can_budget_restores_past_eight_gib(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let previous_bytes = 8_u64 * 1024 * 1024 * 1024;
        let mut total = previous_bytes;
        let mut output = Vec::new();
        let _copied = limits::copy_limited_with_total_budget(
            &mut &[1_u8; 4][..],
            &mut output,
            16,
            &mut total,
            previous_bytes + 4,
            "saved archive",
        )?;
        ensure_eq!(total, previous_bytes + 4);
        ensure_eq!(output, [1_u8; 4]);
        Ok(())
    }

    #[test]
    fn failed_rollback_preserves_snapshot_and_sidecars_but_cleans_input(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let paths = recovery_files::FullRestoreTempPaths {
            temp_db: directory.path().join("input.db"),
            db_snapshot: directory.path().join("before.db"),
        };
        std::fs::write(&paths.temp_db, b"input")?;
        std::fs::write(&paths.db_snapshot, b"safety snapshot")?;
        let sidecar = directory.path().join("before.db-wal");
        std::fs::write(&sidecar, b"snapshot journal")?;
        let mut cleanup = recovery_files::FullRestoreTempCleanupGuard::new(&paths);
        cleanup.retain_snapshot(true);
        drop(cleanup);
        ensure!(!paths.temp_db.exists());
        ensure_eq!(std::fs::read(&paths.db_snapshot)?, b"safety snapshot");
        ensure!(sidecar.exists());
        let mut successful_cleanup = recovery_files::FullRestoreTempCleanupGuard::new(&paths);
        successful_cleanup.retain_snapshot(false);
        drop(successful_cleanup);
        ensure!(!paths.db_snapshot.exists() && !sidecar.exists());
        Ok(())
    }

    #[test]
    fn secret_files_are_private_at_creation_and_never_overwrite_existing_files(
    ) -> Result<(), Box<dyn std::error::Error>> {
        use std::io::Write as _;
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("secret.db");
        let mut file = recovery_files::open_private_file_new(&path)?;
        file.write_all(b"preserve recovery evidence")?;
        file.sync_all()?;
        drop(file);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            // All six group/other permission bits must be clear.
            ensure!(
                std::fs::metadata(&path)?
                    .permissions()
                    .mode()
                    .trailing_zeros()
                    >= 6
            );
        }
        ensure!(recovery_files::open_private_file_new(&path).is_err());
        ensure_eq!(std::fs::read(&path)?, b"preserve recovery evidence");
        Ok(())
    }

    fn backup(id: &str, scope: &str, verified: bool) -> BackupInfo {
        BackupInfo {
            backup_ref: id.into(),
            backup_id: id.into(),
            filename: id.into(),
            size_bytes: 0,
            modified: String::new(),
            modified_epoch: None,
            verified,
            verification_note: String::new(),
            scope: scope.into(),
            mode: "Directory".into(),
            part_count: 0,
            part_filenames: Vec::new(),
            contains_tor_hidden_service_keys: false,
            boards: Vec::new(),
            server_path: id.into(),
            manifest_path: String::new(),
            downloadable_archive: false,
        }
    }

    #[test]
    fn retention_preserves_last_usable_full_backup_despite_newer_other_backups(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let backups = vec![
            backup("selected", "Selected boards", true),
            backup("incomplete", "Full site", false),
            backup("corrupt", "Full site", true),
            backup("good", "Full site", true),
        ];
        let removals =
            retention::removals(backups.clone(), 1, |backup| backup.backup_ref != "corrupt");
        ensure!(removals.is_empty());
        let mut backups = backups;
        backups.push(backup("older-good", "Full site", true));
        let older_removals =
            retention::removals(backups, 1, |backup| backup.backup_ref != "corrupt");
        ensure_eq!(
            older_removals
                .iter()
                .map(|backup| backup.backup_ref.as_str())
                .collect::<Vec<_>>(),
            ["older-good"]
        );
        Ok(())
    }

    fn swap_payload(root: &Path) -> pending_fs::BoardRestoreSwapPayload {
        pending_fs::BoardRestoreSwapPayload {
            staged: root
                .join(".uploads.board-restore-stage.0123456789abcdef0123456789abcdef/b")
                .display()
                .to_string(),
            live: root.join("uploads/b").display().to_string(),
            previous: root
                .join("uploads/.b.restore-old.0123456789abcdef0123456789abcdef")
                .display()
                .to_string(),
        }
    }

    #[test]
    fn interrupted_swap_replays_and_never_deletes_the_only_previous_copy(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let root = directory.path().canonicalize()?;
        let payload = swap_payload(&root);
        std::fs::create_dir_all(&payload.previous)?;
        std::fs::write(Path::new(&payload.previous).join("old"), b"recover me")?;
        ensure!(
            pending_fs::finalize_board_restore_payload(&payload, &root.join("uploads")).is_err()
        );
        ensure!(Path::new(&payload.previous).join("old").exists());
        std::fs::create_dir_all(&payload.staged)?;
        std::fs::write(Path::new(&payload.staged).join("new"), b"restored")?;
        pending_fs::finalize_board_restore_payload(&payload, &root.join("uploads"))?;
        pending_fs::finalize_board_restore_payload(&payload, &root.join("uploads"))?;
        ensure_eq!(
            std::fs::read(Path::new(&payload.live).join("new"))?,
            b"restored"
        );
        ensure!(!Path::new(&payload.previous).exists());
        Ok(())
    }

    #[test]
    fn unpublished_staging_is_cleaned_but_journaled_staging_survives(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("stage");
        std::fs::create_dir(&path)?;
        let mut cleanup = recovery_files::StagingDirectories::default();
        cleanup.track(&path);
        cleanup.retain(true);
        drop(cleanup);
        ensure!(path.exists());
        let mut abandoned_cleanup = recovery_files::StagingDirectories::default();
        abandoned_cleanup.track(&path);
        drop(abandoned_cleanup);
        ensure!(!path.exists());
        Ok(())
    }
}
