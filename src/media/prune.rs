use anyhow::{Context, Result};
use rusqlite::params;
use std::path::{Component, Path, PathBuf};

const PRUNED_REASON: &str = "original file removed by active media size pruning";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PruneReport {
    pub total_before_bytes: u64,
    pub total_after_bytes: u64,
    pub removed_files: u64,
    pub removed_bytes: u64,
    pub skipped_files: u64,
}

#[derive(Debug, Clone)]
struct Candidate {
    post_id: i64,
    created_at: i64,
    paths: Vec<CandidatePath>,
    size: u64,
}

#[derive(Debug, Clone)]
struct CandidatePath {
    path: String,
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
        match delete_candidate_files(upload_root, &candidate.paths) {
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

enum CandidatePathLoad {
    Loaded(CandidatePath),
    MissingSize,
    Unsafe,
}

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
            path: path.to_string(),
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
                    path: path.to_string(),
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

fn delete_candidate_files(upload_root: &Path, paths: &[CandidatePath]) -> Result<()> {
    for candidate_path in paths {
        let relative_path = Path::new(&candidate_path.path);
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

    struct MediaPostFixture<'a> {
        board_id: i64,
        thread_id: i64,
        post_id: i64,
        created_at: i64,
        file_path: &'a str,
        thumb_path: &'a str,
        file_size: i64,
    }

    fn insert_post_with_media(conn: &rusqlite::Connection, fixture: &MediaPostFixture<'_>) {
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
        )
        .expect("insert post with media");
    }

    fn test_db_with_board() -> (crate::db::DbPool, i64, i64) {
        let pool = crate::db::init_test_pool().expect("test pool");
        let conn = pool.get().expect("conn");
        let board_id =
            crate::db::create_board(&conn, "b", "Random", "", false).expect("create board");
        let thread_id: i64 = conn
            .query_row(
                "INSERT INTO threads (board_id, subject) VALUES (?1, 'thread') RETURNING id",
                [board_id],
                |row| row.get(0),
            )
            .expect("create thread");
        drop(conn);
        (pool, board_id, thread_id)
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
    fn configured_prune_disabled_removes_nothing() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join("b/thumbs")).expect("create dirs");
        std::fs::write(dir.path().join("b/old.webp"), [0_u8; 8]).expect("write media");
        std::fs::write(dir.path().join("b/thumbs/old.webp"), [1_u8; 2]).expect("write thumb");
        let (pool, board_id, thread_id) = test_db_with_board();
        let conn = pool.get().expect("conn");
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
        );
        crate::db::set_media_prune_settings(&conn, false, 1).expect("settings");

        let report =
            run_configured_prune(&conn, dir.path().to_str().expect("utf8")).expect("prune");

        assert_eq!(report.removed_files, 0);
        assert!(dir.path().join("b/old.webp").exists());
    }

    #[test]
    fn pruning_removes_oldest_originals_and_keeps_thumbnails() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join("b/thumbs")).expect("create dirs");
        std::fs::write(dir.path().join("b/old.webp"), [0_u8; 8]).expect("write old");
        std::fs::write(dir.path().join("b/new.webp"), [0_u8; 8]).expect("write new");
        std::fs::write(dir.path().join("b/thumbs/old.webp"), [1_u8; 2]).expect("write old thumb");
        std::fs::write(dir.path().join("b/thumbs/new.webp"), [1_u8; 2]).expect("write new thumb");
        let (pool, board_id, thread_id) = test_db_with_board();
        let conn = pool.get().expect("conn");
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
        );
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
        );

        let report = prune_to_limit(&conn, dir.path().to_str().expect("utf8"), 8).expect("prune");

        assert_eq!(report.removed_files, 1);
        assert!(!dir.path().join("b/old.webp").exists());
        assert!(dir.path().join("b/new.webp").exists());
        assert!(dir.path().join("b/thumbs/old.webp").exists());
        assert_eq!(
            conn.query_row(
                "SELECT media_processing_state FROM posts WHERE id = 101",
                [],
                |row| row.get::<_, String>(0),
            )
            .expect("state"),
            crate::db::MEDIA_ORIGINAL_PRUNED
        );
    }

    #[test]
    fn pruning_ignores_archived_thread_media() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join("b/thumbs")).expect("create dirs");
        std::fs::write(dir.path().join("b/archived.webp"), [0_u8; 8]).expect("write archived");
        std::fs::write(dir.path().join("b/active.webp"), [0_u8; 8]).expect("write active");
        std::fs::write(dir.path().join("b/thumbs/archived.webp"), [1_u8; 2])
            .expect("write archived thumb");
        std::fs::write(dir.path().join("b/thumbs/active.webp"), [1_u8; 2])
            .expect("write active thumb");
        let (pool, board_id, active_thread_id) = test_db_with_board();
        let conn = pool.get().expect("conn");
        let archived_thread_id: i64 = conn
            .query_row(
                "INSERT INTO threads (board_id, subject, archived)
                 VALUES (?1, 'archived', 1)
                 RETURNING id",
                [board_id],
                |row| row.get(0),
            )
            .expect("create archived thread");
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
        );
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
        );

        let report = prune_to_limit(&conn, dir.path().to_str().expect("utf8"), 8).expect("prune");

        assert_eq!(report.total_before_bytes, 8);
        assert_eq!(report.removed_files, 0);
        assert!(dir.path().join("b/archived.webp").exists());
        assert!(dir.path().join("b/active.webp").exists());
    }

    #[test]
    fn pruning_removes_secondary_audio_original_with_combo_post() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join("b/thumbs")).expect("create dirs");
        std::fs::write(dir.path().join("b/image.webp"), [0_u8; 4]).expect("write image");
        std::fs::write(dir.path().join("b/track.flac"), [0_u8; 12]).expect("write audio");
        std::fs::write(dir.path().join("b/thumbs/image.webp"), [1_u8; 2]).expect("write thumb");
        let (pool, board_id, thread_id) = test_db_with_board();
        let conn = pool.get().expect("conn");
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
        );
        conn.execute(
            "UPDATE posts
             SET audio_file_path = 'b/track.flac',
                 audio_file_name = 'track.flac',
                 audio_file_size = 12,
                 audio_mime_type = 'audio/flac'
             WHERE id = 101",
            [],
        )
        .expect("attach combo audio");

        let report = prune_to_limit(&conn, dir.path().to_str().expect("utf8"), 0).expect("prune");

        assert_eq!(report.total_before_bytes, 16);
        assert_eq!(report.removed_files, 2);
        assert!(!dir.path().join("b/image.webp").exists());
        assert!(!dir.path().join("b/track.flac").exists());
        assert!(dir.path().join("b/thumbs/image.webp").exists());
    }

    #[cfg(unix)]
    #[test]
    fn pruning_refuses_symlink_media_paths() {
        use std::os::unix::fs as unix_fs;

        let dir = tempfile::tempdir().expect("tempdir");
        let outside = tempfile::tempdir().expect("outside");
        std::fs::create_dir_all(dir.path().join("b/thumbs")).expect("create dirs");
        std::fs::write(outside.path().join("outside.webp"), [9_u8; 8]).expect("outside media");
        unix_fs::symlink(
            outside.path().join("outside.webp"),
            dir.path().join("b/link.webp"),
        )
        .expect("symlink");
        std::fs::write(dir.path().join("b/thumbs/link.webp"), [1_u8; 2]).expect("thumb");
        let (pool, board_id, thread_id) = test_db_with_board();
        let conn = pool.get().expect("conn");
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
        );

        let report = prune_to_limit(&conn, dir.path().to_str().expect("utf8"), 0).expect("prune");

        assert_eq!(report.removed_files, 0);
        assert_eq!(report.skipped_files, 0);
        assert!(outside.path().join("outside.webp").exists());
    }

    #[cfg(unix)]
    #[test]
    fn pruning_refuses_symlink_parent_components() {
        use std::os::unix::fs as unix_fs;

        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join("b/real")).expect("create real dir");
        std::fs::create_dir_all(dir.path().join("b/thumbs")).expect("create thumbs");
        std::fs::write(dir.path().join("b/real/file.webp"), [9_u8; 8]).expect("media");
        unix_fs::symlink(dir.path().join("b/real"), dir.path().join("b/alias"))
            .expect("symlink parent");
        std::fs::write(dir.path().join("b/thumbs/file.webp"), [1_u8; 2]).expect("thumb");
        let (pool, board_id, thread_id) = test_db_with_board();
        let conn = pool.get().expect("conn");
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
        );

        let report = prune_to_limit(&conn, dir.path().to_str().expect("utf8"), 0).expect("prune");

        assert_eq!(report.removed_files, 0);
        assert!(dir.path().join("b/real/file.webp").exists());
    }
}
