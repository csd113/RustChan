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
    let mut candidates = load_candidates(conn, upload_root)?;
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
        let intent = match persist_prune_intent(conn, &candidate) {
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

        match finalize_original_prune_payload(conn, upload_dir, &intent.id, &intent.payload) {
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

    let mut posts = Vec::new();
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
                unique_paths
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
        validate_post_original_path(&path.path, &path.board_short)
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

        if safe_file_size(Path::new(upload_dir), Path::new(&path.path))?.is_some() {
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
                conn.execute(
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

    fn post_state(conn: &rusqlite::Connection, post_id: i64) -> Result<String> {
        Ok(conn.query_row(
            "SELECT COALESCE(media_processing_state, '') FROM posts WHERE id = ?1",
            [post_id],
            |row| row.get(0),
        )?)
    }

    fn pending_prune_intent(
        conn: &rusqlite::Connection,
        upload_root: &Path,
    ) -> Result<PersistedPruneIntent> {
        let candidates = load_candidates(conn, upload_root)?;
        let candidate = candidates
            .first()
            .context("test fixture should produce a prune candidate")?;
        persist_prune_intent(conn, candidate)
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

    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "test assertions intentionally panic on failure"
    )]
    fn shared_original_is_counted_once_and_waits_for_every_reference() -> Result<()> {
        let dir = tempfile::tempdir()?;
        std::fs::create_dir_all(dir.path().join("b/thumbs"))?;
        std::fs::write(dir.path().join("b/shared.webp"), [0_u8; 8])?;
        std::fs::write(dir.path().join("b/thumbs/shared.webp"), [1_u8; 2])?;
        let (pool, board_id, active_thread_id) = test_db_with_board()?;
        let conn = pool.get()?;
        let archived_thread_id: i64 = conn.query_row(
            "INSERT INTO threads (board_id, subject, archived)
             VALUES (?1, 'archived', 1) RETURNING id",
            [board_id],
            |row| row.get(0),
        )?;
        for (post_id, thread_id, created_at) in
            [(101, active_thread_id, 10), (102, archived_thread_id, 20)]
        {
            insert_post_with_media(
                &conn,
                &MediaPostFixture {
                    board_id,
                    thread_id,
                    post_id,
                    created_at,
                    file_path: "b/shared.webp",
                    thumb_path: "b/thumbs/shared.webp",
                    file_size: 8,
                },
            )?;
        }
        crate::db::record_file_hash(
            &conn,
            "shared-hash",
            "b/shared.webp",
            "b/thumbs/shared.webp",
            "image/webp",
        )?;
        let upload_dir = dir.path().to_str().context("UTF-8 upload path")?;

        let blocked = prune_to_limit(&conn, upload_dir, 0)?;
        assert_eq!(blocked.total_before_bytes, 8, "physical bytes count once");
        assert_eq!(blocked.removed_files, 0);
        assert!(dir.path().join("b/shared.webp").exists());
        assert_eq!(post_state(&conn, 101)?, "");
        assert_eq!(post_state(&conn, 102)?, "");

        conn.execute(
            "UPDATE threads SET archived = 0 WHERE id = ?1",
            [archived_thread_id],
        )?;
        let completed = prune_to_limit(&conn, upload_dir, 0)?;
        assert_eq!(completed.total_before_bytes, 8, "shared bytes stay unique");
        assert_eq!(completed.removed_files, 1, "one physical file is removed");
        assert!(!dir.path().join("b/shared.webp").exists());
        assert!(dir.path().join("b/thumbs/shared.webp").exists());
        assert_eq!(post_state(&conn, 101)?, crate::db::MEDIA_ORIGINAL_PRUNED);
        assert_eq!(post_state(&conn, 102)?, crate::db::MEDIA_ORIGINAL_PRUNED);
        assert_eq!(
            conn.query_row(
                "SELECT COUNT(*) FROM file_hashes WHERE file_path = 'b/shared.webp'",
                [],
                |row| row.get::<_, i64>(0),
            )?,
            0,
            "final physical deletion clears stale hash metadata"
        );
        Ok(())
    }

    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "test assertions intentionally panic on failure"
    )]
    fn intent_insert_failure_rolls_back_post_state_and_filesystem() -> Result<()> {
        let dir = tempfile::tempdir()?;
        std::fs::create_dir_all(dir.path().join("b/thumbs"))?;
        std::fs::write(dir.path().join("b/file.webp"), [0_u8; 8])?;
        let (pool, board_id, thread_id) = test_db_with_board()?;
        let conn = pool.get()?;
        insert_post_with_media(
            &conn,
            &MediaPostFixture {
                board_id,
                thread_id,
                post_id: 101,
                created_at: 10,
                file_path: "b/file.webp",
                thumb_path: "b/thumbs/file.webp",
                file_size: 8,
            },
        )?;
        conn.execute_batch(
            "CREATE TRIGGER fail_prune_intent
             BEFORE INSERT ON pending_fs_ops
             WHEN NEW.kind = 'original_prune'
             BEGIN SELECT RAISE(ABORT, 'injected intent failure'); END;",
        )?;

        let report = prune_to_limit(&conn, dir.path().to_str().context("UTF-8 path")?, 0)?;
        assert_eq!(report.removed_files, 0);
        assert_eq!(post_state(&conn, 101)?, "");
        assert!(dir.path().join("b/file.webp").exists());
        assert!(crate::db::list_pending_fs_ops(&conn)?.is_empty());
        Ok(())
    }

    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "test assertions intentionally panic on failure"
    )]
    fn recovery_resumes_before_and_during_multi_file_deletion_idempotently() -> Result<()> {
        for removed_before_replay in 0..=2 {
            let dir = tempfile::tempdir()?;
            std::fs::create_dir_all(dir.path().join("b/thumbs"))?;
            std::fs::write(dir.path().join("b/image.webp"), [0_u8; 4])?;
            std::fs::write(dir.path().join("b/audio.flac"), [0_u8; 6])?;
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
                 SET audio_file_path = 'b/audio.flac', audio_file_size = 6
                 WHERE id = 101",
                [],
            )?;
            let intent = pending_prune_intent(&conn, dir.path())?;
            assert_eq!(
                post_state(&conn, 101)?,
                crate::db::MEDIA_ORIGINAL_PRUNE_PENDING
            );
            for path in intent.payload.paths.iter().take(removed_before_replay) {
                crate::utils::files::delete_file_checked(
                    dir.path().to_str().context("UTF-8 path")?,
                    &path.path,
                )?;
            }
            drop(conn);

            let upload_dir = dir.path().to_str().context("UTF-8 path")?;
            crate::pending_fs::reconcile_pending_fs_ops(&pool, upload_dir)?;
            let before_second = std::fs::read_dir(dir.path().join("b"))?
                .filter_map(std::result::Result::ok)
                .count();
            crate::pending_fs::reconcile_pending_fs_ops(&pool, upload_dir)?;
            let after_second = std::fs::read_dir(dir.path().join("b"))?
                .filter_map(std::result::Result::ok)
                .count();
            let conn = pool.get()?;
            assert_eq!(post_state(&conn, 101)?, crate::db::MEDIA_ORIGINAL_PRUNED);
            assert!(crate::db::list_pending_fs_ops(&conn)?.is_empty());
            assert!(!dir.path().join("b/image.webp").exists());
            assert!(!dir.path().join("b/audio.flac").exists());
            assert_eq!(
                before_second, after_second,
                "second replay changes no files"
            );
        }
        Ok(())
    }

    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "test assertions intentionally panic on failure"
    )]
    fn final_database_failure_leaves_intent_replayable_after_files_are_absent() -> Result<()> {
        let dir = tempfile::tempdir()?;
        std::fs::create_dir_all(dir.path().join("b/thumbs"))?;
        std::fs::write(dir.path().join("b/file.webp"), [0_u8; 8])?;
        let (pool, board_id, thread_id) = test_db_with_board()?;
        let conn = pool.get()?;
        insert_post_with_media(
            &conn,
            &MediaPostFixture {
                board_id,
                thread_id,
                post_id: 101,
                created_at: 10,
                file_path: "b/file.webp",
                thumb_path: "b/thumbs/file.webp",
                file_size: 8,
            },
        )?;
        let intent = pending_prune_intent(&conn, dir.path())?;
        conn.execute_batch(
            "CREATE TRIGGER fail_prune_finalize
             BEFORE UPDATE OF media_processing_state ON posts
             WHEN NEW.media_processing_state = 'pruned'
             BEGIN SELECT RAISE(ABORT, 'injected finalization failure'); END;",
        )?;
        assert!(finalize_original_prune_payload(
            &conn,
            dir.path().to_str().context("UTF-8 path")?,
            &intent.id,
            &intent.payload,
        )
        .is_err());
        assert!(!dir.path().join("b/file.webp").exists());
        assert_eq!(
            post_state(&conn, 101)?,
            crate::db::MEDIA_ORIGINAL_PRUNE_PENDING
        );
        assert_eq!(crate::db::list_pending_fs_ops(&conn)?.len(), 1);
        conn.execute_batch("DROP TRIGGER fail_prune_finalize")?;
        drop(conn);

        crate::pending_fs::reconcile_pending_fs_ops(
            &pool,
            dir.path().to_str().context("UTF-8 path")?,
        )?;
        let conn = pool.get()?;
        assert_eq!(post_state(&conn, 101)?, crate::db::MEDIA_ORIGINAL_PRUNED);
        assert!(crate::db::list_pending_fs_ops(&conn)?.is_empty());
        Ok(())
    }

    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "test assertions intentionally panic on failure"
    )]
    fn deleting_target_post_while_intent_is_pending_does_not_reattach_state() -> Result<()> {
        let dir = tempfile::tempdir()?;
        std::fs::create_dir_all(dir.path().join("b/thumbs"))?;
        std::fs::write(dir.path().join("b/file.webp"), [0_u8; 8])?;
        let (pool, board_id, thread_id) = test_db_with_board()?;
        let conn = pool.get()?;
        insert_post_with_media(
            &conn,
            &MediaPostFixture {
                board_id,
                thread_id,
                post_id: 101,
                created_at: 10,
                file_path: "b/file.webp",
                thumb_path: "b/thumbs/file.webp",
                file_size: 8,
            },
        )?;
        pending_prune_intent(&conn, dir.path())?;
        conn.execute("DELETE FROM posts WHERE id = 101", [])?;
        drop(conn);

        crate::pending_fs::reconcile_pending_fs_ops(
            &pool,
            dir.path().to_str().context("UTF-8 path")?,
        )?;
        let conn = pool.get()?;
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM posts WHERE id = 101", [], |row| {
                row.get::<_, i64>(0)
            })?,
            0
        );
        assert!(!dir.path().join("b/file.webp").exists());
        assert!(crate::db::list_pending_fs_ops(&conn)?.is_empty());
        Ok(())
    }

    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "test assertions intentionally panic on failure"
    )]
    fn replay_refuses_a_new_surviving_reference_before_physical_deletion() -> Result<()> {
        let dir = tempfile::tempdir()?;
        std::fs::create_dir_all(dir.path().join("b/thumbs"))?;
        std::fs::write(dir.path().join("b/file.webp"), [0_u8; 8])?;
        let (pool, board_id, thread_id) = test_db_with_board()?;
        let conn = pool.get()?;
        insert_post_with_media(
            &conn,
            &MediaPostFixture {
                board_id,
                thread_id,
                post_id: 101,
                created_at: 10,
                file_path: "b/file.webp",
                thumb_path: "b/thumbs/file.webp",
                file_size: 8,
            },
        )?;
        let intent = pending_prune_intent(&conn, dir.path())?;
        insert_post_with_media(
            &conn,
            &MediaPostFixture {
                board_id,
                thread_id,
                post_id: 102,
                created_at: 20,
                file_path: "b/file.webp",
                thumb_path: "b/thumbs/file.webp",
                file_size: 8,
            },
        )?;

        let upload_dir = dir.path().to_str().context("UTF-8 path")?;
        assert!(
            finalize_original_prune_payload(&conn, upload_dir, &intent.id, &intent.payload)
                .is_err()
        );
        assert!(
            dir.path().join("b/file.webp").exists(),
            "new normal reference must fail deletion closed"
        );
        assert_eq!(post_state(&conn, 102)?, "");
        conn.execute("DELETE FROM posts WHERE id = 102", [])?;
        drop(conn);

        crate::pending_fs::reconcile_pending_fs_ops(&pool, upload_dir)?;
        let conn = pool.get()?;
        assert_eq!(post_state(&conn, 101)?, crate::db::MEDIA_ORIGINAL_PRUNED);
        assert!(!dir.path().join("b/file.webp").exists());
        assert!(crate::db::list_pending_fs_ops(&conn)?.is_empty());
        Ok(())
    }

    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "test assertions intentionally panic on failure"
    )]
    fn malformed_intent_does_not_block_later_valid_prune_recovery() -> Result<()> {
        let dir = tempfile::tempdir()?;
        std::fs::create_dir_all(dir.path().join("b/thumbs"))?;
        std::fs::write(dir.path().join("b/file.webp"), [0_u8; 8])?;
        let (pool, board_id, thread_id) = test_db_with_board()?;
        let conn = pool.get()?;
        insert_post_with_media(
            &conn,
            &MediaPostFixture {
                board_id,
                thread_id,
                post_id: 101,
                created_at: 10,
                file_path: "b/file.webp",
                thumb_path: "b/thumbs/file.webp",
                file_size: 8,
            },
        )?;
        pending_prune_intent(&conn, dir.path())?;
        conn.execute(
            "INSERT INTO pending_fs_ops (id, kind, payload_json, created_at)
             VALUES ('malformed-prune', 'original_prune', '{', unixepoch() - 10)",
            [],
        )?;
        drop(conn);

        let result = crate::pending_fs::reconcile_pending_fs_ops(
            &pool,
            dir.path().to_str().context("UTF-8 path")?,
        );
        assert!(result.is_err(), "malformed entry remains fail-closed");
        let conn = pool.get()?;
        assert_eq!(post_state(&conn, 101)?, crate::db::MEDIA_ORIGINAL_PRUNED);
        assert!(!dir.path().join("b/file.webp").exists());
        let pending = crate::db::list_pending_fs_ops(&conn)?;
        assert_eq!(pending.len(), 1);
        assert_eq!(
            pending.first().map(|op| op.id.as_str()),
            Some("malformed-prune")
        );
        Ok(())
    }

    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "test assertions intentionally panic on failure"
    )]
    fn concurrent_prune_attempts_converge_without_double_deletion() -> Result<()> {
        let dir = tempfile::tempdir()?;
        std::fs::create_dir_all(dir.path().join("b/thumbs"))?;
        std::fs::write(dir.path().join("b/file.webp"), [0_u8; 8])?;
        let (pool, board_id, thread_id) = test_db_with_board()?;
        let conn = pool.get()?;
        insert_post_with_media(
            &conn,
            &MediaPostFixture {
                board_id,
                thread_id,
                post_id: 101,
                created_at: 10,
                file_path: "b/file.webp",
                thumb_path: "b/thumbs/file.webp",
                file_size: 8,
            },
        )?;
        drop(conn);

        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let upload_dir = dir.path().to_str().context("UTF-8 path")?.to_owned();
        let mut handles = Vec::new();
        for _ in 0..2 {
            let thread_pool = pool.clone();
            let thread_barrier = std::sync::Arc::clone(&barrier);
            let thread_upload_dir = upload_dir.clone();
            handles.push(std::thread::spawn(move || -> Result<PruneReport> {
                let conn = thread_pool.get()?;
                thread_barrier.wait();
                prune_to_limit(&conn, &thread_upload_dir, 0)
            }));
        }
        let reports = handles
            .into_iter()
            .map(|handle| {
                handle
                    .join()
                    .map_err(|_| anyhow::anyhow!("prune thread panicked"))?
            })
            .collect::<Result<Vec<_>>>()?;

        assert_eq!(
            reports
                .iter()
                .map(|report| report.removed_files)
                .sum::<u64>(),
            1,
            "the physical file is removed exactly once"
        );
        let conn = pool.get()?;
        assert_eq!(post_state(&conn, 101)?, crate::db::MEDIA_ORIGINAL_PRUNED);
        assert!(crate::db::list_pending_fs_ops(&conn)?.is_empty());
        assert!(!dir.path().join("b/file.webp").exists());
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
