//! Read-only operator snapshots from durable jobs and existing runtime trackers.

use crate::middleware::{AppState, DbMaintenanceJobStatus};
use std::sync::atomic::Ordering;

/// Maximum rows retained for each durable job state.
const JOBS_PER_STATE: u32 = 40;

/// Common lifecycle terminology used by the suite's task views.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TaskState {
    /// Waiting for a worker claim.
    Queued,
    /// Claimed or currently executing.
    Running,
    /// Successfully resolved by the worker.
    Completed,
    /// Exhausted retries or maintenance failed.
    Failed,
    /// Unrecognized persisted state, never interpreted as success.
    Unknown,
}

impl TaskState {
    /// Translate the existing database vocabulary without changing it.
    #[must_use]
    pub fn from_db(value: &str) -> Self {
        match value {
            "pending" => Self::Queued,
            "running" => Self::Running,
            "done" => Self::Completed,
            "failed" => Self::Failed,
            _ => Self::Unknown,
        }
    }

    /// Explicit status label that remains meaningful without color.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Queued => "QUEUED",
            Self::Running => "RUNNING",
            Self::Completed => "COMPLETED",
            Self::Failed => "FAILED",
            Self::Unknown => "UNKNOWN",
        }
    }
}

/// Safe, bounded presentation record; raw worker payloads are never retained.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskRow {
    /// Stable identity, including a namespace for runtime-only work.
    pub id: String,
    /// Human-readable operation type.
    pub kind: String,
    /// Nonsecret item identity or phase.
    pub item: String,
    /// Current authoritative lifecycle state.
    pub state: TaskState,
    /// Number of worker claims; absent for runtime maintenance.
    pub attempts: Option<i64>,
    /// Creation time, where the tracker provides it.
    pub created_at: Option<i64>,
    /// Latest state transition time; running uses this as claim time.
    pub changed_at: Option<i64>,
    /// Measured progress or a precise limitation.
    pub detail: String,
}

impl TaskRow {
    /// Build a durable task record from selected nonsecret SQL fields.
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        let kind: String = row.get(1)?;
        let state = TaskState::from_db(&row.get::<_, String>(2)?);
        let post_id: Option<i64> = row.get(6)?;
        let board_id: Option<i64> = row.get(7)?;
        let file: Option<String> = row.get(8)?;
        let item = post_id.map_or_else(
            || {
                board_id.map_or_else(
                    || "Item unavailable".to_owned(),
                    |id| format!("Board #{id}"),
                )
            },
            |id| {
                format!(
                    "Post #{id}{}",
                    file.map_or_else(String::new, |path| format!(" · {}", display_text(&path)))
                )
            },
        );
        Ok(Self {
            id: format!("job:{}", row.get::<_, i64>(0)?),
            kind: match kind.as_str() {
                "video_transcode" => "Video transcode",
                "audio_waveform" => "Audio waveform",
                "thread_prune" => "Thread retention",
                "spam_check" => "Spam check",
                _ => "Unknown job type",
            }
            .to_owned(),
            item,
            state,
            attempts: Some(row.get(3)?),
            created_at: Some(row.get(4)?),
            changed_at: Some(row.get(5)?),
            detail: if row.get::<_, bool>(9)? {
                "Worker error recorded. Inspect Logs or the web admin health page for diagnostics."
                    .to_owned()
            } else {
                "No percentage or ETA is published by this worker. Resolution can include stale-target cleanup.".to_owned()
            },
        })
    }
}

/// Nonsecret data sampled independently of the console interaction state.
#[derive(Clone, Debug, Default)]
pub struct OperatorSnapshot {
    /// Read failure; unknown counts must never appear as healthy zeros.
    pub error: Option<String>,
    /// Authoritative queue summary, including acknowledged-failure semantics.
    pub jobs: Option<crate::db::BackgroundJobSummary>,
    /// Bounded active and recent terminal records.
    pub tasks: Vec<TaskRow>,
    /// Whether additional records were omitted by the cap.
    pub tasks_truncated: bool,
    /// Administrator identities without password hashes.
    pub admins: Vec<(i64, String, i64)>,
    /// Total administrator count, independent of bounded recent identities.
    pub admin_count: i64,
    /// Process-wide managed-media observations.
    pub reconcile: crate::media::reconcile::ReconcileMetrics,
    /// Number of open moderation reports.
    pub reports: i64,
    /// Number of open ban appeals.
    pub appeals: i64,
    /// Number of currently active bans.
    pub bans: i64,
    /// Number of pending media records.
    pub media_pending: i64,
    /// Number of failed media records.
    pub media_failed: i64,
    /// Recent moderation events, with sensitive freeform details omitted.
    pub moderation: Vec<String>,
    /// Queue capacity rejections since process startup.
    pub dropped: u64,
    /// `FFmpeg` capabilities detected at startup; None before first sample.
    pub ffmpeg: Option<(bool, bool, bool)>,
    /// Maintenance currently holding the gate.
    pub maintenance: Option<String>,
    /// Effective runtime automatic-backup interval and retention count.
    pub backup_schedule: Option<(u64, u64)>,
}

impl OperatorSnapshot {
    /// Read one consistent, bounded operator snapshot.
    ///
    /// # Errors
    /// Returns database acquisition, query or row-validation errors.
    pub fn read(pool: &crate::db::DbPool) -> anyhow::Result<Self> {
        let connection = pool.get()?;
        let transaction = connection.unchecked_transaction()?;
        let jobs = crate::db::background_job_summary(&transaction)?;
        let mut tasks = Vec::new();
        // The existing status/priority index bounds each state's scan.
        let mut tasks_truncated = false;
        let fields = "SELECT id, job_type, status, attempts, created_at, updated_at,
             CASE WHEN json_valid(payload) THEN json_extract(payload, '$.d.post_id') END,
             CASE WHEN json_valid(payload) THEN json_extract(payload, '$.d.board_id') END,
             CASE WHEN json_valid(payload) THEN substr(json_extract(payload, '$.d.file_path'), 1, 240) END,
             last_error IS NOT NULL AND last_error != ''
             FROM background_jobs WHERE status = ?1";
        for status in ["running", "pending", "failed", "done"] {
            let ordering = if matches!(status, "running" | "pending") {
                " ORDER BY priority DESC, created_at ASC LIMIT ?2"
            } else {
                " ORDER BY updated_at DESC, id DESC LIMIT ?2"
            };
            let mut statement = transaction.prepare_cached(&format!("{fields}{ordering}"))?;
            let mut rows = statement
                .query_map(
                    rusqlite::params![status, JOBS_PER_STATE + 1],
                    TaskRow::from_row,
                )?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            let cap = usize::try_from(JOBS_PER_STATE).unwrap_or(40);
            if rows.len() > cap {
                tasks_truncated = true;
                rows.truncate(cap);
            }
            tasks.extend(rows);
        }
        let mut statement = transaction.prepare_cached("SELECT id, substr(username, 1, 64), created_at FROM admin_users ORDER BY id DESC LIMIT 25")?;
        let admins = statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(statement);
        let (reports, appeals, bans, media_pending, media_failed, admin_count) = transaction
            .query_row(
                "SELECT (SELECT COUNT(*) FROM reports WHERE status = 'open'),
             (SELECT COUNT(*) FROM ban_appeals WHERE status = 'open'),
             (SELECT COUNT(*) FROM bans WHERE expires_at IS NULL OR expires_at > unixepoch()),
             (SELECT COUNT(*) FROM posts WHERE media_processing_state IN ('pending', 'running')),
             (SELECT COUNT(*) FROM posts WHERE media_processing_state = 'failed'),
             (SELECT COUNT(*) FROM admin_users)",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                    ))
                },
            )?;
        let mut statement = transaction.prepare_cached(
            "SELECT created_at, action, target_type, target_id FROM mod_log ORDER BY id DESC LIMIT 8"
        )?;
        let moderation = statement
            .query_map([], |row| {
                Ok(format!(
                    "{} · {} · {} #{}",
                    timestamp(row.get(0)?),
                    display_text(&row.get::<_, String>(1)?),
                    display_text(&row.get::<_, String>(2)?),
                    row.get::<_, Option<i64>>(3)?.unwrap_or(0)
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(statement);
        transaction.commit()?;
        Ok(Self {
            jobs: Some(jobs),
            tasks,
            tasks_truncated,
            admins,
            admin_count,
            reports,
            appeals,
            bans,
            media_pending,
            media_failed,
            moderation,
            ..Self::default()
        })
    }

    /// Attach existing runtime trackers without running diagnostics or mutating them.
    pub fn attach_runtime(&mut self, app: &AppState) {
        self.reconcile = crate::media::reconcile::metrics_snapshot();
        self.dropped = app.job_queue.dropped_count();
        self.ffmpeg = Some((
            app.ffmpeg_available,
            app.ffmpeg_webp_available,
            app.ffmpeg_vp9_available,
        ));
        self.maintenance = app
            .maintenance_gate
            .active_label()
            .map(|label| display_text(&label));
        let settings = app.auto_full_backup_settings.snapshot();
        self.backup_schedule = Some((settings.interval_hours, settings.copies_to_keep));
        if let Some(task) = maintenance_task(app.db_maintenance_jobs.snapshot()) {
            self.tasks.insert(0, task);
        }
        if let Some(label) = &self.maintenance {
            use crate::middleware::backup_phase;
            let progress = &app.backup_progress;
            let phase = progress.phase.load(Ordering::Acquire);
            let detail = match phase {
                backup_phase::SNAPSHOT_DB => "Database snapshot".to_owned(),
                backup_phase::COUNT_FILES => "Counting files".to_owned(),
                backup_phase::COMPRESS => format!(
                    "{} / {} files · {} / {} bytes",
                    progress.files_done.load(Ordering::Relaxed),
                    progress.files_total.load(Ordering::Relaxed),
                    progress.bytes_done.load(Ordering::Relaxed),
                    progress.bytes_total.load(Ordering::Relaxed)
                ),
                _ => "No progress counters available for this operation".to_owned(),
            };
            self.tasks.insert(
                0,
                TaskRow {
                    id: "runtime:maintenance-gate".to_owned(),
                    kind: "Maintenance".to_owned(),
                    item: label.clone(),
                    state: TaskState::Running,
                    attempts: None,
                    created_at: None,
                    changed_at: None,
                    detail: if matches!(
                        label.as_str(),
                        "Full backup creation" | "Board backup creation" | "Scheduled full backup"
                    ) {
                        detail
                    } else {
                        "Serialized maintenance operation; no progress counters published."
                            .to_owned()
                    },
                },
            );
        }
    }
}

/// Convert database-maintenance states without exposing raw errors or reports.
fn maintenance_task(status: DbMaintenanceJobStatus) -> Option<TaskRow> {
    let (id, state, changed_at, item) = match status {
        DbMaintenanceJobStatus::Idle => return None,
        DbMaintenanceJobStatus::Running {
            job_id,
            started_at,
            phase,
        } => (
            job_id,
            TaskState::Running,
            Some(started_at),
            format!("Phase: {phase:?}"),
        ),
        DbMaintenanceJobStatus::Finished { job_id, report } => (
            job_id,
            TaskState::Completed,
            None,
            format!("{} maintenance steps recorded", report.repair_steps.len()),
        ),
        DbMaintenanceJobStatus::Failed {
            job_id,
            finished_at,
            ..
        } => (
            job_id,
            TaskState::Failed,
            Some(finished_at),
            "Database maintenance failed; inspect web admin diagnostics".to_owned(),
        ),
    };
    Some(TaskRow {
        id: format!("database:{id}"),
        kind: "Database maintenance".to_owned(),
        item,
        state,
        attempts: None,
        created_at: None,
        changed_at,
        detail: "Health/repair report is available in the authenticated web admin interface."
            .to_owned(),
    })
}

/// Remove terminal controls and cap untrusted identifiers at a useful length.
#[must_use]
pub fn display_text(value: &str) -> String {
    value
        .chars()
        .filter(|character| !character.is_control())
        .take(240)
        .collect()
}

/// Format a Unix timestamp as UTC without assuming a host timezone.
#[must_use]
pub fn timestamp(seconds: i64) -> String {
    chrono::DateTime::from_timestamp(seconds, 0).map_or_else(
        || "Unknown time".to_owned(),
        |time| time.format("%m-%d %H:%M:%S UTC").to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_vocabulary_is_explicit_and_unknown_fails_closed() {
        for (raw, expected) in [
            ("pending", "QUEUED"),
            ("running", "RUNNING"),
            ("done", "COMPLETED"),
            ("failed", "FAILED"),
            ("cancelled", "UNKNOWN"),
        ] {
            assert_eq!(
                TaskState::from_db(raw).label(),
                expected,
                "database state must map explicitly"
            );
        }
        assert_eq!(
            display_text("x\u{1b}\n\r"),
            "x",
            "terminal controls must not escape the model"
        );
    }

    #[test]
    fn durable_tasks_follow_real_transitions_without_exposing_payloads() -> anyhow::Result<()> {
        let pool = crate::db::init_test_pool()?;
        let connection = pool.get()?;
        let payload =
            r#"{"t":"SpamCheck","d":{"post_id":42,"ip_hash":"PRIVATE_HASH","body_len":10}}"#;
        let id = crate::db::enqueue_job(&connection, "spam_check", payload)?;
        for state in ["pending", "running", "done", "failed"] {
            connection.execute(
                "UPDATE background_jobs SET status = ?1 WHERE id = ?2",
                rusqlite::params![state, id],
            )?;
            let snapshot = OperatorSnapshot::read(&pool)?;
            anyhow::ensure!(
                snapshot
                    .tasks
                    .first()
                    .is_some_and(|task| task.state == TaskState::from_db(state)),
                "task did not follow durable state"
            );
            anyhow::ensure!(
                !format!("{snapshot:?}").contains("PRIVATE_HASH"),
                "private payload leaked"
            );
        }
        Ok(())
    }
    #[test]
    fn task_snapshots_are_capped_and_recent_terminal_rows_win() -> anyhow::Result<()> {
        let pool = crate::db::init_test_pool()?;
        let connection = pool.get()?;
        for id in 1..=100_i64 {
            connection.execute("INSERT INTO background_jobs (job_type, payload, status, created_at, updated_at) VALUES ('spam_check', '{}', 'done', ?1, ?1)", [id])?;
        }
        let snapshot = OperatorSnapshot::read(&pool)?;
        anyhow::ensure!(
            snapshot.tasks.len() == 40 && snapshot.tasks_truncated,
            "terminal history must be bounded with visible omission state"
        );
        anyhow::ensure!(
            snapshot
                .tasks
                .first()
                .is_some_and(|task| task.id == "job:100"),
            "newest terminal job must appear first"
        );
        connection.execute("DROP TABLE mod_log", [])?;
        anyhow::ensure!(
            OperatorSnapshot::read(&pool).is_err(),
            "query failure must not become a healthy empty snapshot"
        );
        Ok(())
    }

    #[test]
    fn runtime_maintenance_and_backup_use_existing_trackers() -> anyhow::Result<()> {
        let app = crate::test_support::app_state();
        let mut snapshot = OperatorSnapshot::default();
        let job_id = app.db_maintenance_jobs.mark_running();
        let guard = app.maintenance_gate.try_begin("Full backup creation")?;
        app.backup_progress
            .reset(crate::middleware::backup_phase::COMPRESS);
        app.backup_progress.files_done.store(3, Ordering::Relaxed);
        app.backup_progress.files_total.store(7, Ordering::Relaxed);
        snapshot.attach_runtime(&app);
        anyhow::ensure!(
            snapshot
                .tasks
                .iter()
                .any(|task| task.id == format!("database:{job_id}")
                    && task.state == TaskState::Running),
            "maintenance must use the actual running tracker"
        );
        anyhow::ensure!(
            snapshot
                .tasks
                .iter()
                .any(|task| task.detail.contains("3 / 7 files")),
            "backup must use actual file counters"
        );
        drop(guard);
        anyhow::ensure!(
            app.db_maintenance_jobs
                .mark_failed(job_id, "fixture failure".to_owned()),
            "matching job should transition"
        );
        let mut snapshot = OperatorSnapshot::default();
        snapshot.attach_runtime(&app);
        anyhow::ensure!(
            snapshot
                .tasks
                .iter()
                .any(|task| task.state == TaskState::Failed),
            "maintenance failure must remain visible"
        );
        Ok(())
    }
}
