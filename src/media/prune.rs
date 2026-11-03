use anyhow::{Context as _, Result};
use rusqlite::{params, OptionalExtension as _};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Component, Path, PathBuf};

/// Persisted explanation attached to posts whose originals were pruned.
const PRUNED_REASON: &str = "original file removed by active media size pruning";

/// Durable filesystem-operation kind for original-media pruning.
pub const ORIGINAL_PRUNE_KIND: &str = "original_prune";

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
/// One active post and its validated original paths.
struct PostCandidate {
    /// Database post identifier.
    post_id: i64,
    /// Oldest-first pruning timestamp.
    created_at: i64,
    /// Validated original paths referenced by the post.
    paths: Vec<CandidatePath>,
}

#[derive(Debug, Clone)]
/// Connected posts that must transition together because they share originals.
struct Candidate {
    /// Posts connected by one or more shared original paths.
    post_ids: Vec<i64>,
    /// Oldest post timestamp in the component.
    created_at: i64,
    /// Unique physical paths in the component.
    paths: Vec<CandidatePath>,
    /// Sum of unique physical path sizes.
    size: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
/// One validated upload-root-relative original-media path.
pub struct CandidatePath {
    /// Upload-root-relative physical path.
    pub path: String,
    /// Board storage namespace that owns the path.
    pub board_short: String,
    /// Size observed while the intent was planned.
    pub size: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
/// Durable transition applied before original files are removed.
pub struct OriginalPrunePayload {
    /// Posts that must transition together before any shared path is removed.
    pub post_ids: Vec<i64>,
    /// Unique physical originals removed by this intent.
    pub paths: Vec<CandidatePath>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
/// Filesystem work completed while replaying one prune intent.
pub struct OriginalPruneFinalizeReport {
    /// Files newly removed during this replay pass.
    pub removed_files: u64,
    /// Planned bytes represented by newly removed files.
    pub removed_bytes: u64,
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
    let candidates = load_candidates(conn, upload_root)?;
    prune_candidates(candidates, upload_dir, max_bytes, || Ok(conn))
}

/// Run configured pruning with no pooled connection during candidate filesystem scans.
///
/// # Errors
/// Returns an error if settings, candidate queries or checkout fail.
pub fn run_configured_prune_with_pool(
    pool: &crate::db::DbPool,
    upload_dir: &str,
) -> Result<PruneReport> {
    let (max_bytes, rows) = {
        let conn = pool.get()?;
        if !crate::db::get_media_auto_prune_enabled(&conn) {
            return Ok(PruneReport::default());
        }
        let max_bytes = crate::db::get_media_max_active_content_size_bytes(&conn);
        if max_bytes == 0 {
            return Ok(PruneReport::default());
        }
        (max_bytes, load_candidate_rows(&conn)?)
    };
    let candidates = validate_candidate_rows(rows, Path::new(upload_dir))?;
    prune_candidates(candidates, upload_dir, max_bytes, || Ok(pool.get()?))
}

/// Process candidates, checking out only for the race-safe mutation boundary.
fn prune_candidates<C: std::ops::Deref<Target = rusqlite::Connection>>(
    mut candidates: Vec<Candidate>,
    upload_dir: &str,
    max_bytes: u64,
    mut connection: impl FnMut() -> Result<C>,
) -> Result<PruneReport> {
    candidates.sort_by_key(|candidate| {
        (
            candidate.created_at,
            candidate.post_ids.first().copied().unwrap_or(i64::MAX),
        )
    });

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
        let conn = connection()?;
        let intent = match persist_prune_intent(&conn, &candidate) {
            Ok(intent) => intent,
            Err(error) => {
                report.skipped_files = report.skipped_files.saturating_add(1);
                tracing::warn!(
                    target: "media_prune",
                    post_ids = ?candidate.post_ids,
                    error = %error,
                    "could not persist media prune intent"
                );
                continue;
            }
        };

        let finalized =
            finalize_original_prune_payload(&conn, upload_dir, &intent.id, &intent.payload);
        match finalized {
            Ok(finalized) => {
                remaining = remaining.saturating_sub(candidate.size);
                report.removed_files = report.removed_files.saturating_add(finalized.removed_files);
                report.removed_bytes = report.removed_bytes.saturating_add(finalized.removed_bytes);
            }
            Err(error) => {
                report.skipped_files = report.skipped_files.saturating_add(1);
                tracing::warn!(
                    target: "media_prune",
                    op_id = %intent.id,
                    error = %error,
                    "media prune intent remains pending for recovery"
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

/// Database-only candidate columns, collected before filesystem validation.
type CandidateRow = (
    i64,
    i64,
    String,
    Option<i64>,
    String,
    Option<String>,
    Option<i64>,
);

/// Collect and validate candidates for callers already owning a connection.
fn load_candidates(conn: &rusqlite::Connection, upload_root: &Path) -> Result<Vec<Candidate>> {
    validate_candidate_rows(load_candidate_rows(conn)?, upload_root)
}

/// Collect candidate rows without performing filesystem access.
fn load_candidate_rows(conn: &rusqlite::Connection) -> Result<Vec<CandidateRow>> {
    let mut stmt = conn.prepare_cached(
        "SELECT p.id, p.created_at, p.file_path, p.file_size, b.short_name,
                p.audio_file_path, p.audio_file_size
         FROM posts p
         JOIN threads t ON t.id = p.thread_id
         JOIN boards b ON b.id = p.board_id
         WHERE p.file_path IS NOT NULL
           AND t.archived = 0
           AND COALESCE(p.media_processing_state, '') NOT IN (?1, ?2, ?3)",
    )?;
    let rows = stmt
        .query_map(
            params![
                crate::db::MEDIA_ORIGINAL_PRUNED,
                crate::db::MEDIA_PROCESSING_PENDING,
                crate::db::MEDIA_ORIGINAL_PRUNE_PENDING
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

    Ok(rows)
}

/// Inspect untrusted paths and group references after releasing the pool borrow.
fn validate_candidate_rows(rows: Vec<CandidateRow>, upload_root: &Path) -> Result<Vec<Candidate>> {
    let canonical_root = upload_root
        .canonicalize()
        .with_context(|| format!("Canonicalize upload root {}", upload_root.display()))?;
    let mut posts = Vec::new();
    for (post_id, created_at, path, db_size, board_short, audio_path, audio_size) in rows {
        let mut paths = Vec::new();
        match candidate_path(&canonical_root, post_id, &path, db_size, &board_short) {
            CandidatePathLoad::Loaded(candidate_path) => paths.push(candidate_path),
            CandidatePathLoad::MissingSize => {}
            CandidatePathLoad::Unsafe => continue,
        }
        if let Some(audio_path) = audio_path {
            match candidate_path(
                &canonical_root,
                post_id,
                &audio_path,
                audio_size,
                &board_short,
            ) {
                CandidatePathLoad::Loaded(candidate_path) => paths.push(candidate_path),
                CandidatePathLoad::MissingSize => {}
                CandidatePathLoad::Unsafe => continue,
            }
        }
        if paths.is_empty() {
            continue;
        }
        posts.push(PostCandidate {
            post_id,
            created_at,
            paths,
        });
    }
    Ok(group_shared_candidates(&posts))
}

/// Group posts by connected shared-path components and count each file once.
fn group_shared_candidates(posts: &[PostCandidate]) -> Vec<Candidate> {
    let mut path_posts: HashMap<&str, Vec<usize>> = HashMap::new();
    for (index, post) in posts.iter().enumerate() {
        for path in &post.paths {
            path_posts.entry(&path.path).or_default().push(index);
        }
    }

    let mut visited = vec![false; posts.len()];
    let mut candidates = Vec::new();
    for start in 0..posts.len() {
        if visited.get(start).copied().unwrap_or(true) {
            continue;
        }
        let mut queue = VecDeque::from([start]);
        let mut indices = Vec::new();
        if let Some(entry) = visited.get_mut(start) {
            *entry = true;
        }
        while let Some(index) = queue.pop_front() {
            indices.push(index);
            let Some(post) = posts.get(index) else {
                continue;
            };
            for path in &post.paths {
                if let Some(neighbors) = path_posts.get(path.path.as_str()) {
                    for &neighbor in neighbors {
                        if visited.get(neighbor).is_some_and(|entry| !entry) {
                            if let Some(entry) = visited.get_mut(neighbor) {
                                *entry = true;
                            }
                            queue.push_back(neighbor);
                        }
                    }
                }
            }
        }

        let mut unique_paths = HashMap::<String, CandidatePath>::new();
        let mut post_ids = Vec::with_capacity(indices.len());
        let mut created_at = i64::MAX;
        for index in indices {
            let Some(post) = posts.get(index) else {
                continue;
            };
            post_ids.push(post.post_id);
            created_at = created_at.min(post.created_at);
            for path in &post.paths {
                let _entry = unique_paths
                    .entry(path.path.clone())
                    .or_insert_with(|| path.clone());
            }
        }
        post_ids.sort_unstable();
        let mut paths: Vec<_> = unique_paths.into_values().collect();
        paths.sort_by(|left, right| left.path.cmp(&right.path));
        let size = paths
            .iter()
            .fold(0_u64, |sum, path| sum.saturating_add(path.size));
        candidates.push(Candidate {
            post_ids,
            created_at,
            paths,
            size,
        });
    }
    candidates
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
    canonical_root: &Path,
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
    match safe_file_size(canonical_root, &relative_path) {
        Ok(Some(size)) => CandidatePathLoad::Loaded(CandidatePath {
            path: path.to_owned(),
            board_short: board_short.to_owned(),
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
                    board_short: board_short.to_owned(),
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

/// Return the size of a safe regular file below an already-canonicalized root.
fn safe_file_size(canonical_root: &Path, relative_path: &Path) -> Result<Option<u64>> {
    reject_symlink_components(canonical_root, relative_path)?;
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
    if !canonical_path.starts_with(canonical_root) {
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

/// Newly committed durable prune operation and its decoded payload.
struct PersistedPruneIntent {
    /// Pending filesystem operation identifier.
    id: String,
    /// Exact posts and physical paths protected by the intent.
    payload: OriginalPrunePayload,
}

/// Atomically mark every dependent post and persist its filesystem intent.
fn persist_prune_intent(
    conn: &rusqlite::Connection,
    candidate: &Candidate,
) -> Result<PersistedPruneIntent> {
    conn.execute_batch("BEGIN IMMEDIATE")
        .context("Begin original-prune intent transaction failed")?;
    let result = persist_prune_intent_in_tx(conn, candidate);
    match result {
        Ok(intent) => {
            if let Err(error) = conn.execute_batch("COMMIT") {
                drop(conn.execute_batch("ROLLBACK"));
                return Err(error).context("Commit original-prune intent failed");
            }
            Ok(intent)
        }
        Err(error) => {
            drop(conn.execute_batch("ROLLBACK"));
            Err(error)
        }
    }
}

/// Validate and write one original-prune transition inside an open transaction.
fn persist_prune_intent_in_tx(
    conn: &rusqlite::Connection,
    candidate: &Candidate,
) -> Result<PersistedPruneIntent> {
    let target_ids: HashSet<i64> = candidate.post_ids.iter().copied().collect();
    validate_current_target_paths(conn, &target_ids, &candidate.paths, true)?;
    for path in &candidate.paths {
        let mut stmt = conn.prepare_cached(
            "SELECT p.id, COALESCE(p.media_processing_state, ''), b.short_name
             FROM posts p
             JOIN boards b ON b.id = p.board_id
             WHERE p.file_path = ?1 OR p.audio_file_path = ?1 OR p.thumb_path = ?1",
        )?;
        let references = stmt
            .query_map([&path.path], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for (post_id, state, board_short) in references {
            if board_short != path.board_short {
                anyhow::bail!("shared original path crosses board boundaries");
            }
            if state != crate::db::MEDIA_ORIGINAL_PRUNED && !target_ids.contains(&post_id) {
                anyhow::bail!(
                    "original path {:?} remains referenced by post {post_id}",
                    path.path
                );
            }
        }
    }

    for post_id in &candidate.post_ids {
        let updated = conn.execute(
            "UPDATE posts
             SET media_processing_state = ?1, media_processing_error = ?2
             WHERE id = ?3
               AND COALESCE(media_processing_state, '') NOT IN (?4, ?5, ?6)
               AND EXISTS (
                   SELECT 1 FROM threads t
                   WHERE t.id = posts.thread_id AND t.archived = 0
               )",
            params![
                crate::db::MEDIA_ORIGINAL_PRUNE_PENDING,
                PRUNED_REASON,
                post_id,
                crate::db::MEDIA_ORIGINAL_PRUNED,
                crate::db::MEDIA_PROCESSING_PENDING,
                crate::db::MEDIA_ORIGINAL_PRUNE_PENDING,
            ],
        )?;
        if updated != 1 {
            anyhow::bail!("post {post_id} changed before original-prune intent commit");
        }
    }

    let payload = OriginalPrunePayload {
        post_ids: candidate.post_ids.clone(),
        paths: candidate.paths.clone(),
    };
    let intent = PersistedPruneIntent {
        id: uuid::Uuid::new_v4().simple().to_string(),
        payload,
    };
    let pending = crate::pending_fs::PendingFsOpInsert {
        id: intent.id.clone(),
        kind: ORIGINAL_PRUNE_KIND,
        payload_json: serde_json::to_string(&intent.payload)
            .context("Serialize original-prune payload failed")?,
    };
    crate::db::insert_pending_fs_op(conn, &pending)?;
    Ok(intent)
}

/// Replay one committed original-prune intent to an idempotent fixed point.
///
/// # Errors
/// Returns an error when the payload is unsafe, a surviving post still needs a
/// path, filesystem deletion fails, or final database updates cannot commit.
pub fn finalize_original_prune_payload(
    conn: &rusqlite::Connection,
    upload_dir: &str,
    pending_op_id: &str,
    payload: &OriginalPrunePayload,
) -> Result<OriginalPruneFinalizeReport> {
    validate_prune_payload(payload)?;
    conn.execute_batch("BEGIN IMMEDIATE")
        .context("Begin original-prune replay transaction failed")?;
    let result = finalize_original_prune_payload_in_tx(conn, upload_dir, pending_op_id, payload);
    match result {
        Ok(report) => {
            if let Err(error) = conn.execute_batch("COMMIT") {
                drop(conn.execute_batch("ROLLBACK"));
                return Err(error).context("Commit original-prune replay failed");
            }
            Ok(report)
        }
        Err(error) => {
            drop(conn.execute_batch("ROLLBACK"));
            Err(error)
        }
    }
}

/// Reject malformed, duplicate, cross-root, or thumbnail prune payload paths.
fn validate_prune_payload(payload: &OriginalPrunePayload) -> Result<()> {
    if payload.post_ids.is_empty() || payload.paths.is_empty() {
        anyhow::bail!("original-prune payload must contain posts and paths");
    }
    let unique_posts: HashSet<_> = payload.post_ids.iter().collect();
    let unique_paths: HashSet<_> = payload
        .paths
        .iter()
        .map(|path| path.path.as_str())
        .collect();
    if unique_posts.len() != payload.post_ids.len() || unique_paths.len() != payload.paths.len() {
        anyhow::bail!("original-prune payload contains duplicate posts or paths");
    }
    for path in &payload.paths {
        let _validated_path = validate_post_original_path(&path.path, &path.board_short)
            .ok_or_else(|| anyhow::anyhow!("unsafe original-prune path {:?}", path.path))?;
    }
    Ok(())
}

/// Delete safe paths and finalize posts, hashes, and intent in one DB transaction.
fn finalize_original_prune_payload_in_tx(
    conn: &rusqlite::Connection,
    upload_dir: &str,
    pending_op_id: &str,
    payload: &OriginalPrunePayload,
) -> Result<OriginalPruneFinalizeReport> {
    let target_ids: HashSet<i64> = payload.post_ids.iter().copied().collect();
    validate_current_target_paths(conn, &target_ids, &payload.paths, false)?;
    let canonical_root = Path::new(upload_dir)
        .canonicalize()
        .with_context(|| format!("Canonicalize upload root {upload_dir}"))?;
    let mut report = OriginalPruneFinalizeReport::default();
    for path in &payload.paths {
        let mut stmt = conn.prepare_cached(
            "SELECT p.id, COALESCE(p.media_processing_state, ''), b.short_name
             FROM posts p
             JOIN boards b ON b.id = p.board_id
             WHERE p.file_path = ?1 OR p.audio_file_path = ?1 OR p.thumb_path = ?1",
        )?;
        let references = stmt
            .query_map([&path.path], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for (post_id, state, board_short) in references {
            let target_is_transitioning = target_ids.contains(&post_id)
                && matches!(
                    state.as_str(),
                    crate::db::MEDIA_ORIGINAL_PRUNE_PENDING | crate::db::MEDIA_ORIGINAL_PRUNED
                );
            if board_short != path.board_short
                || (!target_is_transitioning && state != crate::db::MEDIA_ORIGINAL_PRUNED)
            {
                anyhow::bail!(
                    "original path {:?} is still required by surviving post {post_id}",
                    path.path
                );
            }
        }

        if safe_file_size(&canonical_root, Path::new(&path.path))?.is_some() {
            crate::utils::files::delete_file_checked(upload_dir, &path.path)?;
            report.removed_files = report.removed_files.saturating_add(1);
            report.removed_bytes = report.removed_bytes.saturating_add(path.size);
        }
    }

    for post_id in &payload.post_ids {
        let state = conn
            .query_row(
                "SELECT COALESCE(media_processing_state, '') FROM posts WHERE id = ?1",
                [post_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        match state.as_deref() {
            None | Some(crate::db::MEDIA_ORIGINAL_PRUNED) => {}
            Some(crate::db::MEDIA_ORIGINAL_PRUNE_PENDING) => {
                let _rows_affected = conn.execute(
                    "UPDATE posts
                     SET media_processing_state = ?1, media_processing_error = ?2
                     WHERE id = ?3 AND media_processing_state = ?4",
                    params![
                        crate::db::MEDIA_ORIGINAL_PRUNED,
                        PRUNED_REASON,
                        post_id,
                        crate::db::MEDIA_ORIGINAL_PRUNE_PENDING,
                    ],
                )?;
            }
            Some(other) => anyhow::bail!(
                "post {post_id} left its original-prune transition with state {other:?}"
            ),
        }
    }

    for path in &payload.paths {
        let still_required = conn
            .query_row(
                "SELECT 1 FROM posts
                 WHERE (file_path = ?1 OR audio_file_path = ?1 OR thumb_path = ?1)
                   AND COALESCE(media_processing_state, '') != ?2
                 LIMIT 1",
                params![path.path, crate::db::MEDIA_ORIGINAL_PRUNED],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        if !still_required {
            let _rows_affected =
                conn.execute("DELETE FROM file_hashes WHERE file_path = ?1", [&path.path])?;
        }
    }
    crate::db::delete_pending_fs_op(conn, pending_op_id)?;
    Ok(report)
}

/// Ensure a target did not acquire a new original between planning and replay.
fn validate_current_target_paths(
    conn: &rusqlite::Connection,
    target_ids: &HashSet<i64>,
    intended_paths: &[CandidatePath],
    require_active: bool,
) -> Result<()> {
    let intended: HashMap<&str, &str> = intended_paths
        .iter()
        .map(|path| (path.path.as_str(), path.board_short.as_str()))
        .collect();
    for post_id in target_ids {
        let row = conn
            .query_row(
                "SELECT p.file_path, p.audio_file_path,
                        COALESCE(p.media_processing_state, ''), b.short_name, t.archived
                 FROM posts p
                 JOIN boards b ON b.id = p.board_id
                 JOIN threads t ON t.id = p.thread_id
                 WHERE p.id = ?1",
                [post_id],
                |row| {
                    Ok((
                        row.get::<_, Option<String>>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, bool>(4)?,
                    ))
                },
            )
            .optional()?;
        let Some((file_path, audio_path, state, board_short, archived)) = row else {
            if require_active {
                anyhow::bail!("post {post_id} disappeared before original-prune intent commit");
            }
            continue;
        };
        if require_active
            && (archived
                || matches!(
                    state.as_str(),
                    crate::db::MEDIA_ORIGINAL_PRUNED
                        | crate::db::MEDIA_PROCESSING_PENDING
                        | crate::db::MEDIA_ORIGINAL_PRUNE_PENDING
                ))
        {
            anyhow::bail!("post {post_id} is no longer eligible for original pruning");
        }
        if !require_active
            && !matches!(
                state.as_str(),
                crate::db::MEDIA_ORIGINAL_PRUNE_PENDING | crate::db::MEDIA_ORIGINAL_PRUNED
            )
        {
            anyhow::bail!("post {post_id} left its durable original-prune transition");
        }
        for current_path in [file_path.as_deref(), audio_path.as_deref()]
            .into_iter()
            .flatten()
        {
            if intended.get(current_path).copied() != Some(board_short.as_str()) {
                anyhow::bail!("post {post_id} acquired unexpected original path {current_path:?}");
            }
        }
    }
    Ok(())
}
