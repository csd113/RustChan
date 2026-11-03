//! Current-policy thread retention and bounded durable-intent recovery.

use super::{log_thread_prune_schedule, Job};
use crate::config::CONFIG;
use crate::db::DbPool;
use anyhow::{Context as _, Result};
use rusqlite::OptionalExtension as _;
use tracing::{error, warn};

/// Applies one board's live and archived thread-retention limits.
pub(super) async fn prune_threads(board_id: i64, pool: DbPool) -> Result<()> {
    crate::middleware::data_access::spawn_blocking(move || {
        let conn = pool.get()?;
        // Policy and both retention transitions share one write lock. A concurrent
        // board-settings edit cannot change the policy between read and deletion.
        let tx =
            rusqlite::Transaction::new_unchecked(&conn, rusqlite::TransactionBehavior::Immediate)?;
        let Some(policy) = load_board_retention_policy(&tx, board_id)? else {
            warn!(target: "workers", board_id,
                "thread-prune intent targeted a missing board; resolving safely");
            return Ok(());
        };
        let do_archive = policy.allow_archive || CONFIG.archive_before_prune;
        let (count, deleted) = if do_archive {
            (
                crate::db::threads::archive_old_threads_in_tx(&tx, board_id, policy.max_threads)?,
                None,
            )
        } else {
            (
                0,
                Some(crate::db::threads::prune_old_threads_in_tx(
                    &tx,
                    board_id,
                    policy.max_threads,
                    false,
                )?),
            )
        };
        let archived = crate::db::threads::prune_old_threads_in_tx(
            &tx,
            board_id,
            policy.max_archived_threads,
            true,
        )?;
        tx.commit()?;
        if count > 0 {
            tracing::info!(target: "workers", count, board = %policy.board_short,
                board_id, "ThreadArchive: threads archived");
        }
        if let Some(deleted) = deleted {
            finalize_thread_prune_cleanup(
                &conn,
                &policy,
                &deleted,
                "thread prune cleanup did not fully complete",
            );
            if !deleted.paths.is_empty() {
                tracing::info!(target: "workers", board = %policy.board_short, board_id,
                    files_removed = deleted.paths.len(), "ThreadPrune: threads deleted");
            }
        }
        finalize_thread_prune_cleanup(
            &conn,
            &policy,
            &archived,
            "archived prune cleanup did not fully complete",
        );
        if !archived.paths.is_empty() {
            tracing::info!(target: "workers", archived_cap = policy.max_archived_threads,
                board = %policy.board_short, board_id, files_removed = archived.paths.len(),
                "ThreadArchivePrune: archived threads deleted");
        }

        let (live_eligible, archived_count) = board_retention_counts(&conn, board_id)?;
        if live_eligible > policy.max_threads || archived_count > policy.max_archived_threads {
            warn!(
                target: "workers",
                board = %policy.board_short,
                board_id,
                live_eligible,
                live_limit = policy.max_threads,
                archived_count,
                archived_limit = policy.max_archived_threads,
                "board remains over its current retention limit after prune attempt"
            );
        }
        Ok(())
    })
    .await?
}

/// Current validated policy loaded at execution time, never from job payloads.
struct BoardRetentionPolicy {
    /// Database identifier used for mutation and structured diagnostics.
    board_id: i64,
    /// Current board route name used only for structured diagnostics.
    board_short: String,
    /// Current maximum retained non-sticky live threads.
    max_threads: i64,
    /// Current maximum retained archived threads.
    max_archived_threads: i64,
    /// Current board preference for archiving live overflow.
    allow_archive: bool,
}

/// Load and validate all mutable values that can decide content retention.
fn load_board_retention_policy(
    conn: &rusqlite::Connection,
    board_id: i64,
) -> Result<Option<BoardRetentionPolicy>> {
    let raw = conn
        .query_row(
            "SELECT short_name, max_threads, max_archived_threads, allow_archive
             FROM boards WHERE id = ?1",
            rusqlite::params![board_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                ))
            },
        )
        .optional()
        .context("Load current board retention policy failed")?;
    let Some((board_short, max_threads, max_archived_threads, allow_archive)) = raw else {
        return Ok(None);
    };
    anyhow::ensure!(
        (1..=1_000).contains(&max_threads),
        "invalid current live-thread limit for board {board_id}: {max_threads}; preserving content"
    );
    anyhow::ensure!(
        (1..=10_000).contains(&max_archived_threads),
        "invalid current archived-thread limit for board {board_id}: {max_archived_threads}; preserving content"
    );
    anyhow::ensure!(
        matches!(allow_archive, 0 | 1),
        "invalid current archive policy for board {board_id}: {allow_archive}; preserving content"
    );
    Ok(Some(BoardRetentionPolicy {
        board_id,
        board_short,
        max_threads,
        max_archived_threads,
        allow_archive: allow_archive == 1,
    }))
}

/// Count only live threads eligible for the existing sticky-preserving prune,
/// plus every archived thread subject to the archive cap.
fn board_retention_counts(conn: &rusqlite::Connection, board_id: i64) -> Result<(i64, i64)> {
    conn.query_row(
        "SELECT
             SUM(CASE WHEN archived = 0 AND sticky = 0 THEN 1 ELSE 0 END),
             SUM(CASE WHEN archived = 1 THEN 1 ELSE 0 END)
         FROM threads WHERE board_id = ?1",
        rusqlite::params![board_id],
        |row| {
            Ok((
                row.get::<_, Option<i64>>(0)?.unwrap_or(0),
                row.get::<_, Option<i64>>(1)?.unwrap_or(0),
            ))
        },
    )
    .context("Count board retention state failed")
}

/// Replay durable filesystem cleanup after the deletion transaction commits.
fn finalize_thread_prune_cleanup(
    conn: &rusqlite::Connection,
    policy: &BoardRetentionPolicy,
    deleted: &crate::db::DeletePathsResult,
    message: &'static str,
) {
    if let Err(error) = crate::pending_fs::finalize_delete_files_payload(
        conn,
        &CONFIG.upload_dir,
        deleted.pending_fs_op_id.as_deref(),
        &deleted.paths,
    ) {
        warn!(
            target: "workers",
            board = %policy.board_short,
            board_id = policy.board_id,
            error = %error,
            "{message}"
        );
    }
}

/// Maximum boards inspected by one startup or periodic retention pass.
#[expect(
    clippy::redundant_pub_crate,
    reason = "the private owner preserves crate-visible workers re-exports without widening the API"
)]
pub(crate) const THREAD_PRUNE_RECONCILE_BATCH: usize = 128;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
/// Summary of one bounded board-retention reconciliation page.
#[expect(
    clippy::redundant_pub_crate,
    reason = "the private owner preserves crate-visible workers re-exports without widening the API"
)]
pub(crate) struct ThreadPruneReconciliation {
    /// Boards whose current retention state was inspected.
    pub scanned: usize,
    /// Over-limit boards for which durable work was newly inserted.
    pub inserted: usize,
    /// Over-limit boards already represented by pending work.
    pub coalesced: usize,
    /// Boards that could not be evaluated or scheduled.
    pub failures: usize,
    /// Cursor for the next page, or zero after reaching the end.
    pub next_board_id: i64,
}

/// Inspect a bounded page of boards and durably schedule each current policy
/// violation. Errors are isolated per board so one bad row does not block the
/// remainder of the page.
///
/// # Errors
/// Returns an error only when the database connection or page query itself
/// cannot be completed.
#[expect(
    clippy::redundant_pub_crate,
    reason = "the private owner preserves crate-visible workers re-exports without widening the API"
)]
pub(crate) fn reconcile_thread_prune_intents(
    pool: &DbPool,
    after_board_id: i64,
    limit: usize,
) -> Result<ThreadPruneReconciliation> {
    let conn = pool
        .get()
        .context("Get DB connection for thread-prune reconciliation failed")?;
    let bounded_limit = limit.clamp(1, THREAD_PRUNE_RECONCILE_BATCH);
    let mut stmt = conn
        .prepare(
            "SELECT b.id, b.short_name, b.max_threads, b.max_archived_threads,
                    b.allow_archive,
                    (SELECT COUNT(*) FROM threads AS live
                     WHERE live.board_id = b.id AND live.archived = 0 AND live.sticky = 0),
                    (SELECT COUNT(*) FROM threads AS archived
                     WHERE archived.board_id = b.id AND archived.archived = 1)
             FROM boards AS b
             WHERE b.id > ?1
             ORDER BY b.id ASC
             LIMIT ?2",
        )
        .context("Prepare thread-prune reconciliation page failed")?;
    let rows = stmt
        .query_map(
            rusqlite::params![
                after_board_id.max(0),
                i64::try_from(bounded_limit).unwrap_or(i64::MAX)
            ],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, i64>(6)?,
                ))
            },
        )
        .context("Query thread-prune reconciliation page failed")?
        .collect::<rusqlite::Result<Vec<_>>>()
        .context("Read thread-prune reconciliation page failed")?;
    drop(stmt);

    let mut report = ThreadPruneReconciliation {
        scanned: rows.len(),
        next_board_id: rows.last().map_or(0, |row| row.0),
        ..ThreadPruneReconciliation::default()
    };
    if rows.len() < bounded_limit {
        report.next_board_id = 0;
    }

    for (
        board_id,
        board_short,
        max_threads,
        max_archived_threads,
        allow_archive,
        live_count,
        archived_count,
    ) in rows
    {
        let policy_valid = (1..=1_000).contains(&max_threads)
            && (1..=10_000).contains(&max_archived_threads)
            && matches!(allow_archive, 0 | 1);
        if !policy_valid {
            report.failures = report.failures.saturating_add(1);
            warn!(
                target: "workers",
                board = %board_short,
                board_id,
                max_threads,
                max_archived_threads,
                allow_archive,
                "thread-prune reconciliation found invalid policy; preserving content"
            );
            continue;
        }
        if live_count <= max_threads && archived_count <= max_archived_threads {
            continue;
        }

        let schedule_result = crate::db::persist_thread_prune_intent(&conn, board_id);
        match schedule_result {
            Ok(schedule) => {
                log_thread_prune_schedule(board_id, schedule);
                if schedule.inserted {
                    report.inserted = report.inserted.saturating_add(1);
                } else {
                    report.coalesced = report.coalesced.saturating_add(1);
                }
            }
            Err(error) => {
                report.failures = report.failures.saturating_add(1);
                error!(
                    target: "workers",
                    board = %board_short,
                    board_id,
                    error = %error,
                    "failed to persist or coalesce reconciled board prune intent"
                );
            }
        }
    }
    Ok(report)
}

/// Normalize a bounded set of active legacy prune payloads before workers
/// start. Policy fields in those payloads are ignored by `Job` deserialization.
/// Missing-board jobs are resolved, and duplicate pending jobs are coalesced.
///
/// # Errors
/// Returns an error if the legacy job page cannot be queried.
#[expect(
    clippy::redundant_pub_crate,
    reason = "the private owner preserves crate-visible workers re-exports without widening the API"
)]
pub(crate) fn normalize_legacy_thread_prune_jobs(pool: &DbPool, limit: usize) -> Result<()> {
    let conn = pool
        .get()
        .context("Get DB connection for legacy thread-prune recovery failed")?;
    let mut stmt = conn
        .prepare(
            "SELECT id, payload
             FROM background_jobs
             WHERE job_type = 'thread_prune' AND status = 'pending'
             ORDER BY id ASC LIMIT ?1",
        )
        .context("Prepare legacy thread-prune recovery failed")?;
    let jobs = stmt
        .query_map(
            rusqlite::params![i64::try_from(limit.max(1)).unwrap_or(i64::MAX)],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
        )
        .context("Query legacy thread-prune jobs failed")?
        .collect::<rusqlite::Result<Vec<_>>>()
        .context("Read legacy thread-prune jobs failed")?;
    drop(stmt);

    let mut board_ids = std::collections::BTreeSet::new();
    for (job_id, payload) in jobs {
        match serde_json::from_str::<Job>(&payload) {
            Ok(Job::ThreadPrune { board_id }) => {
                let _previous_entry = board_ids.insert(board_id);
            }
            Ok(_) => {
                error!(target: "workers", job_id, "thread_prune row contained another job type");
            }
            Err(error) => {
                error!(target: "workers", job_id, error = %error, "invalid legacy thread-prune payload remains retryable");
            }
        }
    }

    for board_id in board_ids {
        let exists = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM boards WHERE id = ?1)",
                rusqlite::params![board_id],
                |row| row.get::<_, bool>(0),
            )
            .context("Check legacy thread-prune board failed")?;
        if exists {
            let schedule_result = crate::db::persist_thread_prune_intent(&conn, board_id);
            match schedule_result {
                Ok(schedule) => log_thread_prune_schedule(board_id, schedule),
                Err(error) => error!(
                    target: "workers",
                    board_id,
                    error = %error,
                    "failed to normalize legacy thread-prune jobs"
                ),
            }
        } else {
            let resolved = conn
                .execute(
                    "UPDATE background_jobs
                     SET status = 'done',
                         last_error = 'resolved missing-board legacy thread-prune job',
                         updated_at = unixepoch()
                     WHERE job_type = 'thread_prune' AND status = 'pending'
                       AND CAST(json_extract(
                             CASE WHEN json_valid(payload) THEN payload ELSE NULL END,
                             '$.d.board_id'
                           ) AS INTEGER) = ?1",
                    rusqlite::params![board_id],
                )
                .context("Resolve missing-board legacy thread-prune jobs failed")?;
            warn!(
                target: "workers",
                board_id,
                jobs_resolved = resolved,
                "resolved legacy thread-prune work for missing board"
            );
        }
    }
    Ok(())
}
