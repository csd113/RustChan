use anyhow::{Context as _, Result};
use rusqlite::params;
use std::path::{Component, Path, PathBuf};

/// Persisted explanation attached to posts whose originals were pruned.
const PRUNED_REASON: &str = "original file removed by active media size pruning";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
/// Aggregate result of one active-media pruning pass.
pub struct PruneReport {
    /// Eligible media bytes present before pruning.
    pub total_before_bytes: u64,
    /// Eligible media bytes remaining after pruning.
    pub total_after_bytes: u64,
    /// Number of original files successfully removed.
    pub removed_files: u64,
    /// Total bytes successfully removed.
    pub removed_bytes: u64,
    /// Number of candidates skipped after a filesystem failure.
    pub skipped_files: u64,
}

#[derive(Debug, Clone)]
/// Active post and its original files considered by one pruning pass.
struct Candidate {
    /// Post that owns the eligible originals.
    post_id: i64,
    /// Timestamp used for oldest-first ordering.
    created_at: i64,
    /// Validated original-media paths belonging to the post.
    paths: Vec<CandidatePath>,
    /// Total bytes across all eligible original paths.
    size: u64,
}

#[derive(Debug, Clone)]
/// One validated upload-root-relative original-media path.
struct CandidatePath {
    /// Safe relative path stored in the database.
    path: String,
    /// Current on-disk size in bytes.
    size: u64,
}

/// Run the admin-configured active post-media pruning policy.
///
/// Only full-size post originals are eligible. Thumbnail paths remain in the DB
/// and on disk so archived/pruned posts can still show a useful preview.
///
/// # Errors
/// Returns an error only for database-level failures. Unsafe, missing, or
/// undeletable individual files are skipped and logged.
pub fn run_configured_prune(conn: &rusqlite::Connection, upload_dir: &str) -> Result<PruneReport> {
    if !crate::db::get_media_auto_prune_enabled(conn) {
        return Ok(PruneReport::default());
    }
    let max_bytes = crate::db::get_media_max_active_content_size_bytes(conn);
    if max_bytes == 0 {
        return Ok(PruneReport::default());
    }
    prune_to_limit(conn, upload_dir, max_bytes)
}

/// Prune oldest eligible post originals until active media is within `max_bytes`.
///
/// # Errors
/// Returns an error if the database query/update fails.
pub fn prune_to_limit(
    conn: &rusqlite::Connection,
    upload_dir: &str,
    max_bytes: u64,
) -> Result<PruneReport> {
    let upload_root = Path::new(upload_dir);
    let mut candidates = load_candidates(conn, upload_root)?;
    candidates.sort_by_key(|candidate| (candidate.created_at, candidate.post_id));

    let mut report = PruneReport {
        total_before_bytes: candidates
            .iter()
            .fold(0_u64, |sum, candidate| sum.saturating_add(candidate.size)),
        ..PruneReport::default()
    };
    let mut remaining = report.total_before_bytes;
    if remaining <= max_bytes {
        report.total_after_bytes = remaining;
        return Ok(report);
    }

    for candidate in candidates {
        if remaining <= max_bytes {
            break;
        }
        let deletion_result = delete_candidate_files(upload_root, &candidate.paths);
        match deletion_result {
            Ok(()) => {
                crate::db::set_post_media_processing_state(
                    conn,
                    candidate.post_id,
                    Some(crate::db::MEDIA_ORIGINAL_PRUNED),
                    Some(PRUNED_REASON),
                )?;
                remaining = remaining.saturating_sub(candidate.size);
                report.removed_files = report
                    .removed_files
                    .saturating_add(u64::try_from(candidate.paths.len()).unwrap_or(u64::MAX));
                report.removed_bytes = report.removed_bytes.saturating_add(candidate.size);
            }
            Err(error) => {
                report.skipped_files = report.skipped_files.saturating_add(1);
                tracing::warn!(
                    target: "media_prune",
                    post_id = candidate.post_id,
                    error = %error,
                    "skipping media prune candidate"
                );
            }
        }
    }

    report.total_after_bytes = remaining;
    if report.removed_files > 0 {
        tracing::info!(
            target: "media_prune",
            removed_files = report.removed_files,
            freed_bytes = report.removed_bytes,
            remaining_bytes = report.total_after_bytes,
            max_bytes,
            "active post media pruning complete"
        );
    }
    Ok(report)
}

/// Load and validate all active original-media candidates.
fn load_candidates(conn: &rusqlite::Connection, upload_root: &Path) -> Result<Vec<Candidate>> {
    let mut stmt = conn.prepare_cached(
        "SELECT p.id, p.created_at, p.file_path, p.file_size, b.short_name,
                p.audio_file_path, p.audio_file_size
         FROM posts p
         JOIN threads t ON t.id = p.thread_id
         JOIN boards b ON b.id = p.board_id
         WHERE p.file_path IS NOT NULL
           AND t.archived = 0
           AND COALESCE(p.media_processing_state, '') NOT IN (?1, ?2)",
    )?;
    let rows = stmt
        .query_map(
            params![
                crate::db::MEDIA_ORIGINAL_PRUNED,
                crate::db::MEDIA_PROCESSING_PENDING
            ],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<i64>>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, Option<i64>>(6)?,
                ))
            },
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    let mut candidates = Vec::new();
    for (post_id, created_at, path, db_size, board_short, audio_path, audio_size) in rows {
        let mut paths = Vec::new();
        match candidate_path(upload_root, post_id, &path, db_size, &board_short) {
            CandidatePathLoad::Loaded(candidate_path) => paths.push(candidate_path),
            CandidatePathLoad::MissingSize => {}
            CandidatePathLoad::Unsafe => continue,
        }
        if let Some(audio_path) = audio_path {
            match candidate_path(upload_root, post_id, &audio_path, audio_size, &board_short) {
                CandidatePathLoad::Loaded(candidate_path) => paths.push(candidate_path),
                CandidatePathLoad::MissingSize => {}
                CandidatePathLoad::Unsafe => continue,
            }
        }
        if paths.is_empty() {
            continue;
        }
        let size = paths.iter().fold(0_u64, |sum, candidate_path| {
            sum.saturating_add(candidate_path.size)
        });
        candidates.push(Candidate {
            post_id,
            created_at,
            paths,
            size,
        });
    }
    Ok(candidates)
}

/// Result of validating one optional original-media path.
enum CandidatePathLoad {
    /// Path resolved to a safe file with a known size.
    Loaded(CandidatePath),
    /// File was absent and its stored size was not usable.
    MissingSize,
    /// Path failed structural or filesystem safety validation.
    Unsafe,
}

/// Validate one database media-path field into a pruning candidate.
fn candidate_path(
    upload_root: &Path,
    post_id: i64,
    path: &str,
    db_size: Option<i64>,
    board_short: &str,
) -> CandidatePathLoad {
    let Some(relative_path) = validate_post_original_path(path, board_short) else {
        tracing::warn!(
            target: "media_prune",
            post_id,
            path = %path,
            "skipping unsafe or non-original media path"
        );
        return CandidatePathLoad::Unsafe;
    };
    match safe_file_size(upload_root, &relative_path) {
        Ok(Some(size)) => CandidatePathLoad::Loaded(CandidatePath {
            path: path.to_owned(),
            size,
        }),
        Ok(None) => db_size.and_then(|size| u64::try_from(size).ok()).map_or(
            CandidatePathLoad::MissingSize,
            |size| {
                tracing::warn!(
                    target: "media_prune",
                    post_id,
                    path = %path,
                    "media file missing while DB still references it"
                );
                CandidatePathLoad::Loaded(CandidatePath {
                    path: path.to_owned(),
                    size,
                })
            },
        ),
        Err(error) => {
            tracing::warn!(
                target: "media_prune",
                post_id,
                path = %path,
                error = %error,
                "skipping media path that failed safety inspection"
            );
            CandidatePathLoad::Unsafe
        }
    }
}

/// Validate a post original as a board-owned, non-thumbnail relative path.
fn validate_post_original_path(path: &str, board_short: &str) -> Option<PathBuf> {
    let rel = Path::new(path);
    if path.trim().is_empty() || rel.is_absolute() || path.contains('\\') {
        return None;
    }
    let mut components = rel.components();
    let first = components.next()?;
    if !matches!(first, Component::Normal(part) if part.to_str() == Some(board_short)) {
        return None;
    }
    if components.clone().any(|component| {
        matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_) | Component::CurDir
        )
    }) {
        return None;
    }
    let thumbs_prefix = format!("{board_short}/thumbs/");
    if path == format!("{board_short}/thumbs") || path.starts_with(&thumbs_prefix) {
        return None;
    }
    Some(rel.to_path_buf())
}

/// Return the size of a safe regular file below `upload_root`.
fn safe_file_size(upload_root: &Path, relative_path: &Path) -> Result<Option<u64>> {
    let canonical_root = upload_root
        .canonicalize()
        .with_context(|| format!("Canonicalize upload root {}", upload_root.display()))?;
    reject_symlink_components(&canonical_root, relative_path)?;
    let path = canonical_root.join(relative_path);
    let metadata = match std::fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).with_context(|| format!("Inspect {}", path.display())),
    };
    if metadata.file_type().is_symlink() {
        anyhow::bail!("media path is a symlink");
    }
    if !metadata.file_type().is_file() {
        anyhow::bail!("media path is not a regular file");
    }
    let canonical_path = path
        .canonicalize()
        .with_context(|| format!("Canonicalize media path {}", path.display()))?;
    if !canonical_path.starts_with(&canonical_root) {
        anyhow::bail!("media path escapes upload root");
    }
    Ok(Some(metadata.len()))
}

/// Reject any symlink or unsafe component between the root and media file.
fn reject_symlink_components(canonical_root: &Path, relative_path: &Path) -> Result<()> {
    let mut current = canonical_root.to_path_buf();
    for component in relative_path.components() {
        match component {
            Component::Normal(part) => current.push(part),
            Component::CurDir
            | Component::ParentDir
            | Component::RootDir
            | Component::Prefix(_) => {
                anyhow::bail!("media path contains unsafe components");
            }
        }
        let Ok(metadata) = std::fs::symlink_metadata(&current) else {
            return Ok(());
        };
        if metadata.file_type().is_symlink() {
            anyhow::bail!("media path contains a symlink component");
        }
    }
    Ok(())
}

/// Delete every still-safe original belonging to one pruning candidate.
fn delete_candidate_files(upload_root: &Path, paths: &[CandidatePath]) -> Result<()> {
    for candidate_path in paths {
        let relative_path = Path::new(&candidate_path.path);
        // `safe_file_size` is the safety gate for this delete: it rejects
        // traversal, symlink components, non-files, and root escapes.
        if safe_file_size(upload_root, relative_path)?.is_none() {
            continue;
        }
        let path = upload_root.join(relative_path);
        std::fs::remove_file(&path)
            .with_context(|| format!("Remove media file {}", path.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Result;

    struct MediaPostFixture<'a> {
        board_id: i64,
        thread_id: i64,
        post_id: i64,
        created_at: i64,
        file_path: &'a str,
        thumb_path: &'a str,
        file_size: i64,
    }

    fn insert_post_with_media(
        conn: &rusqlite::Connection,
        fixture: &MediaPostFixture<'_>,
    ) -> Result<()> {
        conn.execute(
            "INSERT INTO posts (
                id, thread_id, board_id, name, body, body_html, file_path,
                file_name, file_size, thumb_path, mime_type, deletion_token,
                is_op, media_type, created_at
             )
             VALUES (?1, ?2, ?3, 'anon', 'body', 'body', ?4, ?5, ?6, ?7,
                     'image/webp', ?8, 0, 'image', ?9)",
            rusqlite::params![
                fixture.post_id,
                fixture.thread_id,
                fixture.board_id,
                fixture.file_path,
                fixture.file_path.rsplit('/').next().unwrap_or("file.webp"),
                fixture.file_size,
                fixture.thumb_path,
                format!("token-{}", fixture.post_id),
                fixture.created_at,
            ],
        )?;
        Ok(())
    }

    fn test_db_with_board() -> Result<(crate::db::DbPool, i64, i64)> {
        let pool = crate::db::init_test_pool()?;
        let conn = pool.get()?;
        let board_id = crate::db::create_board(&conn, "b", "Random", "", false)?;
        let thread_id: i64 = conn.query_row(
            "INSERT INTO threads (board_id, subject) VALUES (?1, 'thread') RETURNING id",
            [board_id],
            |row| row.get(0),
        )?;
        drop(conn);
        Ok((pool, board_id, thread_id))
    }

    #[test]
    fn validate_post_original_path_rejects_thumbs_and_escapes() {
        assert!(validate_post_original_path("b/file.webp", "b").is_some());
        assert!(validate_post_original_path("b/thumbs/file.webp", "b").is_none());
        assert!(validate_post_original_path("../b/file.webp", "b").is_none());
        assert!(validate_post_original_path("/b/file.webp", "b").is_none());
        assert!(validate_post_original_path("tech/file.webp", "b").is_none());
    }

    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "test assertions intentionally panic on failure"
    )]
    fn configured_prune_disabled_removes_nothing() -> Result<()> {
        let dir = tempfile::tempdir()?;
        std::fs::create_dir_all(dir.path().join("b/thumbs"))?;
        std::fs::write(dir.path().join("b/old.webp"), [0_u8; 8])?;
        std::fs::write(dir.path().join("b/thumbs/old.webp"), [1_u8; 2])?;
        let (pool, board_id, thread_id) = test_db_with_board()?;
        let conn = pool.get()?;
        insert_post_with_media(
            &conn,
            &MediaPostFixture {
                board_id,
                thread_id,
                post_id: 101,
                created_at: 10,
                file_path: "b/old.webp",
                thumb_path: "b/thumbs/old.webp",
                file_size: 8,
            },
        )?;
        crate::db::set_media_prune_settings(&conn, false, 1)?;

        let upload_dir = dir
            .path()
            .to_str()
            .context("temporary path must be UTF-8")?;
        let report = run_configured_prune(&conn, upload_dir)?;

        assert_eq!(report.removed_files, 0, "disabled pruning removes nothing");
        assert!(
            dir.path().join("b/old.webp").exists(),
            "disabled pruning must preserve originals"
        );
        Ok(())
    }

    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "test assertions intentionally panic on failure"
    )]
    fn pruning_removes_oldest_originals_and_keeps_thumbnails() -> Result<()> {
        let dir = tempfile::tempdir()?;
        std::fs::create_dir_all(dir.path().join("b/thumbs"))?;
        std::fs::write(dir.path().join("b/old.webp"), [0_u8; 8])?;
        std::fs::write(dir.path().join("b/new.webp"), [0_u8; 8])?;
        std::fs::write(dir.path().join("b/thumbs/old.webp"), [1_u8; 2])?;
        std::fs::write(dir.path().join("b/thumbs/new.webp"), [1_u8; 2])?;
        let (pool, board_id, thread_id) = test_db_with_board()?;
        let conn = pool.get()?;
        insert_post_with_media(
            &conn,
            &MediaPostFixture {
                board_id,
                thread_id,
                post_id: 101,
                created_at: 10,
                file_path: "b/old.webp",
                thumb_path: "b/thumbs/old.webp",
                file_size: 8,
            },
        )?;
        insert_post_with_media(
            &conn,
            &MediaPostFixture {
                board_id,
                thread_id,
                post_id: 102,
                created_at: 20,
                file_path: "b/new.webp",
                thumb_path: "b/thumbs/new.webp",
                file_size: 8,
            },
        )?;

        let upload_dir = dir
            .path()
            .to_str()
            .context("temporary path must be UTF-8")?;
        let report = prune_to_limit(&conn, upload_dir, 8)?;

        assert_eq!(report.removed_files, 1, "one original must be pruned");
        assert!(
            !dir.path().join("b/old.webp").exists(),
            "oldest original must be removed"
        );
        assert!(
            dir.path().join("b/new.webp").exists(),
            "newest original must remain"
        );
        assert!(
            dir.path().join("b/thumbs/old.webp").exists(),
            "thumbnail must remain after original pruning"
        );
        assert_eq!(
            conn.query_row(
                "SELECT media_processing_state FROM posts WHERE id = 101",
                [],
                |row| row.get::<_, String>(0),
            )?,
            crate::db::MEDIA_ORIGINAL_PRUNED,
            "database state must record original pruning"
        );
        Ok(())
    }

    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "test assertions intentionally panic on failure"
    )]
    fn pruning_ignores_archived_thread_media() -> Result<()> {
        let dir = tempfile::tempdir()?;
        std::fs::create_dir_all(dir.path().join("b/thumbs"))?;
        std::fs::write(dir.path().join("b/archived.webp"), [0_u8; 8])?;
        std::fs::write(dir.path().join("b/active.webp"), [0_u8; 8])?;
        std::fs::write(dir.path().join("b/thumbs/archived.webp"), [1_u8; 2])?;
        std::fs::write(dir.path().join("b/thumbs/active.webp"), [1_u8; 2])?;
        let (pool, board_id, active_thread_id) = test_db_with_board()?;
        let conn = pool.get()?;
        let archived_thread_id: i64 = conn.query_row(
            "INSERT INTO threads (board_id, subject, archived)
                 VALUES (?1, 'archived', 1)
                 RETURNING id",
            [board_id],
            |row| row.get(0),
        )?;
        insert_post_with_media(
            &conn,
            &MediaPostFixture {
                board_id,
                thread_id: archived_thread_id,
                post_id: 101,
                created_at: 10,
                file_path: "b/archived.webp",
                thumb_path: "b/thumbs/archived.webp",
                file_size: 8,
            },
        )?;
        insert_post_with_media(
            &conn,
            &MediaPostFixture {
                board_id,
                thread_id: active_thread_id,
                post_id: 102,
                created_at: 20,
                file_path: "b/active.webp",
                thumb_path: "b/thumbs/active.webp",
                file_size: 8,
            },
        )?;

        let upload_dir = dir
            .path()
            .to_str()
            .context("temporary path must be UTF-8")?;
        let report = prune_to_limit(&conn, upload_dir, 8)?;

        assert_eq!(
            report.total_before_bytes, 8,
            "only active media is eligible"
        );
        assert_eq!(
            report.removed_files, 0,
            "active media already fits the limit"
        );
        assert!(
            dir.path().join("b/archived.webp").exists(),
            "archived original must remain"
        );
        assert!(
            dir.path().join("b/active.webp").exists(),
            "eligible original must remain when within limit"
        );
        Ok(())
    }

    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "test assertions intentionally panic on failure"
    )]
    fn pruning_removes_secondary_audio_original_with_combo_post() -> Result<()> {
        let dir = tempfile::tempdir()?;
        std::fs::create_dir_all(dir.path().join("b/thumbs"))?;
        std::fs::write(dir.path().join("b/image.webp"), [0_u8; 4])?;
        std::fs::write(dir.path().join("b/track.flac"), [0_u8; 12])?;
        std::fs::write(dir.path().join("b/thumbs/image.webp"), [1_u8; 2])?;
        let (pool, board_id, thread_id) = test_db_with_board()?;
        let conn = pool.get()?;
        insert_post_with_media(
            &conn,
            &MediaPostFixture {
                board_id,
                thread_id,
                post_id: 101,
                created_at: 10,
                file_path: "b/image.webp",
                thumb_path: "b/thumbs/image.webp",
                file_size: 4,
            },
        )?;
        conn.execute(
            "UPDATE posts
             SET audio_file_path = 'b/track.flac',
                 audio_file_name = 'track.flac',
                 audio_file_size = 12,
                 audio_mime_type = 'audio/flac'
             WHERE id = 101",
            [],
        )?;

        let upload_dir = dir
            .path()
            .to_str()
            .context("temporary path must be UTF-8")?;
        let report = prune_to_limit(&conn, upload_dir, 0)?;

        assert_eq!(
            report.total_before_bytes, 16,
            "both originals count toward the limit"
        );
        assert_eq!(report.removed_files, 2, "both originals must be removed");
        assert!(
            !dir.path().join("b/image.webp").exists(),
            "primary original must be removed"
        );
        assert!(
            !dir.path().join("b/track.flac").exists(),
            "secondary original must be removed"
        );
        assert!(
            dir.path().join("b/thumbs/image.webp").exists(),
            "thumbnail must remain"
        );
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "test assertions intentionally panic on failure"
    )]
    fn pruning_refuses_symlink_media_paths() -> Result<()> {
        use std::os::unix::fs as unix_fs;

        let dir = tempfile::tempdir()?;
        let outside = tempfile::tempdir()?;
        std::fs::create_dir_all(dir.path().join("b/thumbs"))?;
        std::fs::write(outside.path().join("outside.webp"), [9_u8; 8])?;
        unix_fs::symlink(
            outside.path().join("outside.webp"),
            dir.path().join("b/link.webp"),
        )?;
        std::fs::write(dir.path().join("b/thumbs/link.webp"), [1_u8; 2])?;
        let (pool, board_id, thread_id) = test_db_with_board()?;
        let conn = pool.get()?;
        insert_post_with_media(
            &conn,
            &MediaPostFixture {
                board_id,
                thread_id,
                post_id: 101,
                created_at: 10,
                file_path: "b/link.webp",
                thumb_path: "b/thumbs/link.webp",
                file_size: 8,
            },
        )?;

        let upload_dir = dir
            .path()
            .to_str()
            .context("temporary path must be UTF-8")?;
        let report = prune_to_limit(&conn, upload_dir, 0)?;

        assert_eq!(
            report.removed_files, 0,
            "symlink targets must not be removed"
        );
        assert_eq!(
            report.skipped_files, 0,
            "unsafe paths are ineligible rather than failed"
        );
        assert!(
            outside.path().join("outside.webp").exists(),
            "file outside the upload root must remain"
        );
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "test assertions intentionally panic on failure"
    )]
    fn pruning_refuses_symlink_parent_components() -> Result<()> {
        use std::os::unix::fs as unix_fs;

        let dir = tempfile::tempdir()?;
        std::fs::create_dir_all(dir.path().join("b/real"))?;
        std::fs::create_dir_all(dir.path().join("b/thumbs"))?;
        std::fs::write(dir.path().join("b/real/file.webp"), [9_u8; 8])?;
        unix_fs::symlink(dir.path().join("b/real"), dir.path().join("b/alias"))?;
        std::fs::write(dir.path().join("b/thumbs/file.webp"), [1_u8; 2])?;
        let (pool, board_id, thread_id) = test_db_with_board()?;
        let conn = pool.get()?;
        insert_post_with_media(
            &conn,
            &MediaPostFixture {
                board_id,
                thread_id,
                post_id: 101,
                created_at: 10,
                file_path: "b/alias/file.webp",
                thumb_path: "b/thumbs/file.webp",
                file_size: 8,
            },
        )?;

        let upload_dir = dir
            .path()
            .to_str()
            .context("temporary path must be UTF-8")?;
        let report = prune_to_limit(&conn, upload_dir, 0)?;

        assert_eq!(
            report.removed_files, 0,
            "symlinked parent paths must be ineligible"
        );
        assert!(
            dir.path().join("b/real/file.webp").exists(),
            "file reached through a symlinked parent must remain"
        );
        Ok(())
    }
}
