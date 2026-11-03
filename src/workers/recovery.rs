//! Bounded repair of durable media-job and post-processing state.

use super::{media_job_identity, transcoded_webm_name, Job};
use crate::config::CONFIG;
use crate::db::DbPool;
use anyhow::{Context as _, Result};
use rusqlite::OptionalExtension as _;
use tracing::{debug, warn};

/// Maximum media jobs and pending posts inspected by one reconciliation pass.
#[expect(
    clippy::redundant_pub_crate,
    reason = "the private owner preserves the crate-visible workers facade without widening the API"
)]
pub(crate) const MEDIA_RECONCILE_BATCH: usize = 128;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
/// Summary of one bounded media-state reconciliation page.
pub struct MediaReconciliation {
    /// Active media-job rows inspected.
    pub jobs_scanned: usize,
    /// Pending post rows inspected.
    pub posts_scanned: usize,
    /// Job rows moved to a safe terminal state.
    pub jobs_resolved: usize,
    /// Post media states repaired.
    pub posts_repaired: usize,
    /// Malformed rows isolated without blocking later work.
    pub malformed_jobs: usize,
    /// Cursor for the next active-job page, or zero after wrapping.
    pub next_job_id: i64,
    /// Cursor for the next pending-post page, or zero after wrapping.
    pub next_post_id: i64,
}

/// Derive the deterministic output named by a video job payload.
fn deterministic_video_output_path(source: &str, board: &str) -> Option<String> {
    let source = std::path::Path::new(source);
    let stem = source.file_stem()?.to_str()?;
    let extension = source.extension()?.to_str()?.to_ascii_lowercase();
    matches!(extension.as_str(), "mp4" | "mkv" | "webm")
        .then(|| format!("{board}/{}", transcoded_webm_name(stem, &extension)))
}

/// Derive the deterministic output named by an audio-waveform payload.
fn deterministic_waveform_output_path(source: &str, board: &str) -> Option<String> {
    let stem = std::path::Path::new(source).file_stem()?.to_str()?;
    (!stem.is_empty()).then(|| format!("{board}/thumbs/{stem}.png"))
}

/// Reconcile one bounded page of legacy media job/post inconsistencies.
///
/// Repairs use only the job payload's source identity and current durable post
/// state. Deterministic files are never assumed to belong to a post merely from
/// their name.
///
/// # Errors
/// Returns an error if the immediate transaction or bounded queries fail.
#[expect(
    clippy::cognitive_complexity,
    clippy::too_many_lines,
    reason = "the bounded legacy-state matrix is intentionally explicit"
)]
pub fn reconcile_media_job_states(
    pool: &DbPool,
    after_job_id: i64,
    after_post_id: i64,
    limit: usize,
) -> Result<MediaReconciliation> {
    let conn = pool
        .get()
        .context("Get DB connection for media reconciliation failed")?;
    conn.execute_batch("BEGIN IMMEDIATE")
        .context("Begin media reconciliation transaction failed")?;
    let result = (|| {
        let bounded_limit = limit.clamp(1, MEDIA_RECONCILE_BATCH);
        let sql_limit = i64::try_from(bounded_limit).unwrap_or(i64::MAX);
        let mut job_stmt = conn.prepare(
            "SELECT id, job_type, payload
             FROM background_jobs
             WHERE id > ?1 AND status IN ('pending', 'running')
               AND job_type IN ('video_transcode', 'audio_waveform')
             ORDER BY id ASC LIMIT ?2",
        )?;
        let jobs = job_stmt
            .query_map(rusqlite::params![after_job_id.max(0), sql_limit], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(job_stmt);

        let mut report = MediaReconciliation {
            jobs_scanned: jobs.len(),
            next_job_id: jobs.last().map_or(0, |job| job.0),
            ..MediaReconciliation::default()
        };
        if jobs.len() < bounded_limit {
            report.next_job_id = 0;
        }

        for (job_id, stored_type, payload) in jobs {
            let parsed = serde_json::from_str::<Job>(&payload);
            let job = match parsed {
                Ok(job) if job.type_str() == stored_type => job,
                Ok(_) | Err(_) => {
                    let _rows_affected = conn.execute(
                        "UPDATE background_jobs
                         SET status = 'failed',
                             last_error = 'malformed legacy media job payload',
                             updated_at = unixepoch()
                         WHERE id = ?1 AND status IN ('pending', 'running')",
                        rusqlite::params![job_id],
                    )?;
                    report.jobs_resolved = report.jobs_resolved.saturating_add(1);
                    report.malformed_jobs = report.malformed_jobs.saturating_add(1);
                    warn!(
                        target: "workers",
                        job_id,
                        job_type = %stored_type,
                        "isolated malformed legacy media job"
                    );
                    continue;
                }
            };
            let Some(identity) = media_job_identity(&job) else {
                continue;
            };
            let post = conn
                .query_row(
                    "SELECT file_path, media_processing_state
                     FROM posts WHERE id = ?1",
                    rusqlite::params![identity.post_id],
                    |row| Ok((row.get::<_, Option<String>>(0)?, row.get::<_, String>(1)?)),
                )
                .optional()?;
            let compatible = post.as_ref().is_some_and(|(path, state)| {
                path.as_deref() == Some(identity.expected_source.as_str())
                    && matches!(
                        state.as_str(),
                        crate::db::MEDIA_PROCESSING_PENDING | "running"
                    )
            });
            let older_duplicate: bool = conn.query_row(
                "SELECT EXISTS(
                     SELECT 1 FROM background_jobs
                     WHERE id < ?1 AND status IN ('pending', 'running')
                       AND job_type = ?2 AND json_valid(payload)
                       AND CAST(json_extract(payload, '$.d.post_id') AS INTEGER) = ?3
                       AND json_extract(payload, '$.d.file_path') = ?4
                 )",
                rusqlite::params![
                    job_id,
                    identity.job_type,
                    identity.post_id,
                    identity.expected_source
                ],
                |row| row.get(0),
            )?;
            if compatible && !older_duplicate {
                continue;
            }

            let _rows_affected = conn.execute(
                "UPDATE background_jobs
                 SET status = 'done',
                     last_error = ?2,
                     updated_at = unixepoch()
                 WHERE id = ?1 AND status IN ('pending', 'running')",
                rusqlite::params![
                    job_id,
                    if post.is_none() {
                        "resolved missing media target"
                    } else if older_duplicate {
                        "resolved duplicate media job"
                    } else {
                        "resolved stale or terminal media target"
                    }
                ],
            )?;
            report.jobs_resolved = report.jobs_resolved.saturating_add(1);
            debug!(
                target: "workers",
                post_id = identity.post_id,
                job_id,
                job_type = identity.job_type,
                "resolved stale, duplicate, or missing-target media job"
            );
        }

        let mut post_stmt = conn.prepare(
            "SELECT p.id, p.file_path, p.thumb_path
             FROM posts p
             WHERE p.id > ?1 AND p.media_processing_state = ?2
             ORDER BY p.id ASC LIMIT ?3",
        )?;
        let posts = post_stmt
            .query_map(
                rusqlite::params![
                    after_post_id.max(0),
                    crate::db::MEDIA_PROCESSING_PENDING,
                    sql_limit
                ],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<String>>(2)?,
                    ))
                },
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(post_stmt);
        report.posts_scanned = posts.len();
        report.next_post_id = posts.last().map_or(0, |post| post.0);
        if posts.len() < bounded_limit {
            report.next_post_id = 0;
        }

        for (post_id, source, current_thumb) in posts {
            let Some(source) = source else {
                let _rows_affected = conn.execute(
                    "UPDATE posts SET media_processing_state = ?1,
                         media_processing_error = 'pending media post has no source'
                     WHERE id = ?2 AND media_processing_state = ?3",
                    rusqlite::params![
                        crate::db::MEDIA_PROCESSING_FAILED,
                        post_id,
                        crate::db::MEDIA_PROCESSING_PENDING
                    ],
                )?;
                report.posts_repaired = report.posts_repaired.saturating_add(1);
                continue;
            };
            let mut active_stmt = conn.prepare(
                "SELECT job_type, payload FROM background_jobs
                 WHERE status IN ('pending', 'running')
                   AND job_type IN ('video_transcode', 'audio_waveform')
                   AND json_valid(payload)
                   AND CAST(json_extract(payload, '$.d.post_id') AS INTEGER) = ?1
                 ORDER BY id ASC",
            )?;
            let active_payloads = active_stmt
                .query_map(rusqlite::params![post_id], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            drop(active_stmt);
            let has_compatible = active_payloads.iter().any(|(stored_type, payload)| {
                serde_json::from_str::<Job>(payload).is_ok_and(|job| {
                    job.type_str() == stored_type
                        && media_job_identity(&job).is_some_and(|identity| {
                            identity.post_id == post_id && identity.expected_source == source
                        })
                })
            });
            if has_compatible {
                continue;
            }

            let mut terminal_stmt = conn.prepare(
                "SELECT job_type, payload, status, last_error FROM background_jobs
                     WHERE status IN ('done', 'failed')
                       AND job_type IN ('video_transcode', 'audio_waveform')
                       AND json_valid(payload)
                       AND CAST(json_extract(payload, '$.d.post_id') AS INTEGER) = ?1
                     ORDER BY id DESC LIMIT 16",
            )?;
            let terminal_rows = terminal_stmt
                .query_map(rusqlite::params![post_id], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, Option<String>>(3)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            drop(terminal_stmt);
            let mut terminal = None;
            for (job_type, payload, status, error) in terminal_rows {
                let Ok(job) = serde_json::from_str::<Job>(&payload) else {
                    continue;
                };
                if job.type_str() != job_type {
                    continue;
                }
                let disposition = match job {
                    Job::VideoTranscode {
                        file_path,
                        board_short,
                        post_id: _,
                    } => {
                        let expected_output_path =
                            deterministic_video_output_path(&file_path, &board_short);
                        let installed_output_matches = status == "done"
                            && expected_output_path.as_deref() == Some(source.as_str())
                            && crate::utils::fs_security::existing_regular_file_child(
                                std::path::Path::new(&CONFIG.upload_dir),
                                &source,
                            )
                            .is_ok();
                        if file_path == source || installed_output_matches {
                            Some((status.as_str(), error))
                        } else {
                            None
                        }
                    }
                    Job::AudioWaveform {
                        file_path,
                        board_short,
                        post_id: _,
                    } => {
                        let expected_thumb_path =
                            deterministic_waveform_output_path(&file_path, &board_short);
                        if file_path == source
                            && (status != "done"
                                || expected_thumb_path.is_none()
                                || current_thumb.as_deref() == expected_thumb_path.as_deref())
                        {
                            Some((status.as_str(), error))
                        } else {
                            None
                        }
                    }
                    Job::ThreadPrune { board_id: _ }
                    | Job::SpamCheck {
                        post_id: _,
                        ip_hash: _,
                        body_len: _,
                    } => None,
                };
                if let Some((terminal_status, terminal_error)) = disposition {
                    terminal = Some((terminal_status.to_owned(), terminal_error));
                    break;
                }
            }
            let (state, detail) = match terminal {
                Some((status, _error)) if status == "done" => ("", None),
                Some((_status, error)) => (
                    crate::db::MEDIA_PROCESSING_FAILED,
                    error.or_else(|| Some("media job permanently failed".to_owned())),
                ),
                None => (
                    crate::db::MEDIA_PROCESSING_FAILED,
                    Some("pending media state has no durable compatible job".to_owned()),
                ),
            };
            let _rows_affected = conn.execute(
                "UPDATE posts SET media_processing_state = ?1, media_processing_error = ?2
                 WHERE id = ?3 AND file_path = ?4 AND media_processing_state = ?5",
                rusqlite::params![
                    state,
                    detail,
                    post_id,
                    source,
                    crate::db::MEDIA_PROCESSING_PENDING
                ],
            )?;
            report.posts_repaired = report.posts_repaired.saturating_add(1);
            warn!(
                target: "workers",
                post_id,
                "repaired pending post without compatible durable media work"
            );
        }
        Ok(report)
    })();

    match result {
        Ok(report) => {
            if let Err(error) = conn.execute_batch("COMMIT") {
                drop(conn.execute_batch("ROLLBACK"));
                return Err(error).context("Commit media reconciliation failed");
            }
            Ok(report)
        }
        Err(error) => {
            drop(conn.execute_batch("ROLLBACK"));
            Err(error)
        }
    }
}
