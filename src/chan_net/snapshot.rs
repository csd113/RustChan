// chan_net/snapshot.rs — Federation snapshot builders.
//
// Step 2.1 — Structs: SnapshotBoard, SnapshotPost, SnapshotMetadata are now
//   defined in src/models.rs and re-exported here so all existing call-sites
//   (snapshot::SnapshotPost, etc.) continue to compile without change.
//   Moving the types to models.rs resolves the layering inversion: db/chan_net.rs
//   previously imported from `crate::chan_net::snapshot`, which is only reachable
//   in the binary crate (chan_net is declared in main.rs, not lib.rs). models.rs
//   is re-exported by lib.rs and is accessible from anywhere in the crate.
//
// Step 2.2 — build_snapshot: full ZIP of all boards + active (non-archived) posts
// Step 2.3 — unpack_snapshot: strict-whitelist ZIP parser
//
// Column fix (Phase 8): boards table display-name column is `name`, not `title`.

// Re-export so that all call-sites using `super::snapshot::SnapshotPost` etc.
// continue to compile without any changes.
pub use crate::models::{SnapshotBoard, SnapshotMetadata, SnapshotPost};

// ── build_snapshot ────────────────────────────────────────────────────────────

use anyhow::Result;
use rusqlite::Connection;
use std::io::{Cursor, Write};
use uuid::Uuid;
use zip::{write::SimpleFileOptions, ZipWriter};

/// Build a full in-memory snapshot ZIP of all boards and all active
/// (non-archived) posts.
///
/// Returns ZIP bytes and the transaction UUID for this snapshot.
/// Used by the federation layer (`/chan/export`, `/chan/refresh`).
pub fn build_snapshot(conn: &Connection) -> Result<(Vec<u8>, Uuid)> {
    // ── Boards ────────────────────────────────────────────────────────────
    // Column is `name` (display name), not `title` — verified against db/mod.rs.
    let mut stmt = conn.prepare("SELECT short_name, name FROM boards ORDER BY id")?;
    let boards: Vec<SnapshotBoard> = stmt
        .query_map([], |row| {
            Ok(SnapshotBoard {
                id: row.get(0)?,
                title: row.get(1)?, // SQL `name` → Rust field `title`
            })
        })?
        .collect::<rusqlite::Result<_>>()?;

    // ── Posts (text columns only — NO media columns, NO archived threads) ─
    let mut stmt = conn.prepare(
        "SELECT p.id, b.short_name, p.name, p.body, p.created_at
         FROM   posts   p
         JOIN   threads t ON p.thread_id = t.id
         JOIN   boards  b ON t.board_id  = b.id
         WHERE  t.archived = 0
         ORDER  BY p.id",
    )?;
    let posts: Vec<SnapshotPost> = stmt
        .query_map([], |row| {
            Ok(SnapshotPost {
                post_id: row.get::<_, i64>(0)?.cast_unsigned(),
                board: row.get(1)?,
                author: row
                    .get::<_, Option<String>>(2)?
                    .unwrap_or_else(|| "anon".to_string()),
                content: row.get::<_, Option<String>>(3)?.unwrap_or_default(),
                timestamp: row.get::<_, i64>(4)?.cast_unsigned(),
            })
        })?
        .collect::<rusqlite::Result<_>>()?;

    // ── Metadata ──────────────────────────────────────────────────────────
    let tx_id = Uuid::new_v4();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let metadata = SnapshotMetadata {
        generated_at: now,
        rustchan_version: env!("CARGO_PKG_VERSION").to_string(),
        post_count: posts.len() as u64,
        tx_id,
        signature: None,
        since: None,
        is_delta: false,
        includes_archive: false,
    };

    // ── Build ZIP ─────────────────────────────────────────────────────────
    let buf = Cursor::new(Vec::new());
    let mut zip = ZipWriter::new(buf);
    let opts = SimpleFileOptions::default();

    zip.start_file("boards.json", opts)?;
    zip.write_all(&serde_json::to_vec(&boards)?)?;

    zip.start_file("posts.json", opts)?;
    zip.write_all(&serde_json::to_vec(&posts)?)?;

    zip.start_file("metadata.json", opts)?;
    zip.write_all(&serde_json::to_vec(&metadata)?)?;

    let zip_bytes = zip.finish()?.into_inner();
    Ok((zip_bytes, tx_id))
}

// ── unpack_snapshot ───────────────────────────────────────────────────────────

/// Maximum decompressed size per JSON entry in a snapshot ZIP.
/// Prevents ZIP bombs from expanding unboundedly into RAM.
/// DefaultBodyLimit caps the *compressed* payload; this caps the *decompressed* output.
const MAX_SNAPSHOT_ENTRY_BYTES: u64 = 8 * 1024 * 1024; // 8 MiB per entry

/// Read a named ZIP entry with a hard decompressed-size cap.
/// Returns an error if the entry expands beyond MAX_SNAPSHOT_ENTRY_BYTES.
fn read_zip_entry_bounded(
    archive: &mut zip::ZipArchive<std::io::Cursor<&[u8]>>,
    name: &str,
) -> anyhow::Result<Vec<u8>> {
    use std::io::Read as _;
    let f = archive.by_name(name)?;
    let mut buf = Vec::new();
    // Read at most MAX_SNAPSHOT_ENTRY_BYTES + 1 bytes.
    // If we fill the buffer beyond the limit, the entry is oversized.
    f.take(MAX_SNAPSHOT_ENTRY_BYTES + 1).read_to_end(&mut buf)?;
    if buf.len() as u64 > MAX_SNAPSHOT_ENTRY_BYTES {
        anyhow::bail!(
            "Snapshot entry '{}' exceeds {} MiB decompressed limit — possible ZIP bomb.",
            name,
            MAX_SNAPSHOT_ENTRY_BYTES / 1_048_576
        );
    }
    Ok(buf)
}

/// Unpack and parse a federation snapshot ZIP.
///
/// Rejects any ZIP that contains files other than the three known names,
/// guarding against path traversal and unexpected content.
/// FIX[C-6]: Each entry is now read through read_zip_entry_bounded to prevent
/// ZIP bombs from exhausting RAM via unbounded decompression.
pub fn unpack_snapshot(
    bytes: &[u8],
) -> anyhow::Result<(Vec<SnapshotBoard>, Vec<SnapshotPost>, SnapshotMetadata)> {
    let cursor = Cursor::new(bytes);
    let mut zip = zip::ZipArchive::new(cursor)?;

    // Path traversal guard — whitelist only.
    for i in 0..zip.len() {
        let name = zip.by_index(i)?.name().to_string();
        if !matches!(
            name.as_str(),
            "boards.json" | "posts.json" | "metadata.json"
        ) {
            anyhow::bail!("Unexpected file in snapshot ZIP: {name}");
        }
    }

    let buf = read_zip_entry_bounded(&mut zip, "boards.json")?;
    let boards: Vec<SnapshotBoard> = serde_json::from_slice(&buf)?;

    let buf = read_zip_entry_bounded(&mut zip, "posts.json")?;
    let posts: Vec<SnapshotPost> = serde_json::from_slice(&buf)?;

    let buf = read_zip_entry_bounded(&mut zip, "metadata.json")?;
    let metadata: SnapshotMetadata = serde_json::from_slice(&buf)?;

    Ok((boards, posts, metadata))
}
