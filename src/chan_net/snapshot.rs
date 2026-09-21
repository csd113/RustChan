//! Federation snapshot builders.
//!
//! Builds a full ZIP of all boards and active posts and unpacks snapshots with
//! a strict filename whitelist.

// Re-export so that all call-sites using `super::snapshot::SnapshotPost` etc.
// continue to compile without any changes.
pub use crate::models::{SnapshotBoard, SnapshotMetadata, SnapshotPost};

// build_snapshot
use anyhow::Result;
use rusqlite::Connection;
use std::io::{Cursor, Write as _};
use uuid::Uuid;
use zip::{write::SimpleFileOptions, ZipWriter};

/// Maximum decompressed bytes accepted across a federation snapshot.
const SNAPSHOT_DECOMPRESSED_MAX_BYTES: u64 = 64 * 1024 * 1024;
/// Maximum decompressed board metadata in a federation snapshot.
const SNAPSHOT_BOARDS_MAX_BYTES: u64 = 8 * 1024 * 1024;
/// Maximum decompressed post data in a federation snapshot.
const SNAPSHOT_POSTS_MAX_BYTES: u64 = 64 * 1024 * 1024;
/// Maximum decompressed transaction metadata in a federation snapshot.
const SNAPSHOT_METADATA_MAX_BYTES: u64 = 64 * 1024;
/// Maximum number of boards accepted from one federation snapshot.
const SNAPSHOT_BOARDS_MAX_COUNT: usize = 4_096;
/// Maximum number of posts accepted from one federation snapshot.
const SNAPSHOT_POSTS_MAX_COUNT: usize = 100_000;

/// Build a full in-memory snapshot ZIP of all exportable public boards and
/// their active (non-archived) posts.
///
/// Returns ZIP bytes and the transaction UUID for this snapshot.
/// Used by the federation layer (`/chan/export`, `/chan/refresh`).
///
/// # Errors
///
/// Returns an error when database reads, serialization, or ZIP construction fail.
pub fn build_snapshot(conn: &Connection) -> Result<(Vec<u8>, Uuid)> {
    // Boards
    // Column is `name` (display name), not `title` — verified against db/mod.rs.
    let mut stmt = conn.prepare(
        "SELECT short_name, name
         FROM boards
         WHERE access_mode IN ('public', 'post_password')
         ORDER BY nsfw ASC, display_order ASC, id ASC",
    )?;
    let boards: Vec<SnapshotBoard> = stmt
        .query_map([], |row| {
            Ok(SnapshotBoard {
                id: row.get(0)?,
                title: row.get(1)?, // SQL `name` → Rust field `title`
            })
        })?
        .collect::<rusqlite::Result<_>>()?;

    // Posts (text columns only — NO media columns, NO archived threads)
    let mut stmt = conn.prepare(
        "SELECT p.id, b.short_name, p.name, p.body, p.created_at
         FROM   posts   p
         JOIN   threads t ON p.thread_id = t.id
         JOIN   boards  b ON t.board_id  = b.id
         WHERE  t.archived = 0
           AND  b.access_mode IN ('public', 'post_password')
         ORDER  BY p.id",
    )?;
    let posts: Vec<SnapshotPost> = stmt
        .query_map([], |row| {
            Ok(SnapshotPost {
                post_id: row.get::<_, i64>(0)?.cast_unsigned(),
                board: row.get(1)?,
                author: row
                    .get::<_, Option<String>>(2)?
                    .unwrap_or_else(|| "anon".to_owned()),
                content: row.get::<_, Option<String>>(3)?.unwrap_or_default(),
                timestamp: row.get::<_, i64>(4)?.cast_unsigned(),
            })
        })?
        .collect::<rusqlite::Result<_>>()?;

    // Metadata
    let tx_id = Uuid::new_v4();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let metadata = SnapshotMetadata {
        generated_at: now,
        rustchan_version: env!("CARGO_PKG_VERSION").to_owned(),
        post_count: u64::try_from(posts.len())?,
        tx_id,
        signature: None,
        since: None,
        is_delta: false,
        includes_archive: false,
    };

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

// unpack_snapshot
/// Unpack and parse a federation snapshot ZIP.
///
/// Rejects any ZIP that contains files other than the three known names,
/// guarding against path traversal and unexpected content.
///
/// # Errors
///
/// Returns an error for malformed ZIP data, unexpected entries, missing
/// required entries, read failures, or invalid JSON.
pub fn unpack_snapshot(
    bytes: &[u8],
) -> Result<(Vec<SnapshotBoard>, Vec<SnapshotPost>, SnapshotMetadata)> {
    let cursor = Cursor::new(bytes);
    let mut zip = zip::ZipArchive::new(cursor)?;

    if zip.len() != 3 {
        anyhow::bail!(
            "Snapshot ZIP must contain exactly three entries; found {}",
            zip.len()
        );
    }

    // Path traversal and duplicate-entry guard — whitelist exactly once.
    let mut saw_boards = false;
    let mut saw_posts = false;
    let mut saw_metadata = false;
    for i in 0..zip.len() {
        let name = zip.by_index(i)?.name().to_owned();
        let seen = match name.as_str() {
            "boards.json" => &mut saw_boards,
            "posts.json" => &mut saw_posts,
            "metadata.json" => &mut saw_metadata,
            _ => anyhow::bail!("Unexpected file in snapshot ZIP: {name}"),
        };
        if *seen {
            anyhow::bail!("Duplicate file in snapshot ZIP: {name}");
        }
        *seen = true;
    }

    let mut decompressed_bytes = 0_u64;
    let boards: Vec<SnapshotBoard> = {
        let mut f = zip.by_name("boards.json")?;
        let declared_size = f.size();
        let remaining = SNAPSHOT_DECOMPRESSED_MAX_BYTES.saturating_sub(decompressed_bytes);
        let buf = read_limited_snapshot_component(
            &mut f,
            declared_size,
            SNAPSHOT_BOARDS_MAX_BYTES.min(remaining),
            "boards.json",
        )?;
        decompressed_bytes = decompressed_bytes
            .checked_add(u64::try_from(buf.len())?)
            .ok_or_else(|| anyhow::anyhow!("Snapshot decompressed size overflowed"))?;
        parse_bounded_snapshot_array(&buf, SNAPSHOT_BOARDS_MAX_COUNT, "boards.json")?
    };

    let posts: Vec<SnapshotPost> = {
        let mut f = zip.by_name("posts.json")?;
        let declared_size = f.size();
        let remaining = SNAPSHOT_DECOMPRESSED_MAX_BYTES.saturating_sub(decompressed_bytes);
        let buf = read_limited_snapshot_component(
            &mut f,
            declared_size,
            SNAPSHOT_POSTS_MAX_BYTES.min(remaining),
            "posts.json",
        )?;
        decompressed_bytes = decompressed_bytes
            .checked_add(u64::try_from(buf.len())?)
            .ok_or_else(|| anyhow::anyhow!("Snapshot decompressed size overflowed"))?;
        parse_bounded_snapshot_array(&buf, SNAPSHOT_POSTS_MAX_COUNT, "posts.json")?
    };

    let metadata: SnapshotMetadata = {
        let mut f = zip.by_name("metadata.json")?;
        let declared_size = f.size();
        let remaining = SNAPSHOT_DECOMPRESSED_MAX_BYTES.saturating_sub(decompressed_bytes);
        let buf = read_limited_snapshot_component(
            &mut f,
            declared_size,
            SNAPSHOT_METADATA_MAX_BYTES.min(remaining),
            "metadata.json",
        )?;
        serde_json::from_slice(&buf)?
    };

    Ok((boards, posts, metadata))
}

/// Deserializes a JSON array while stopping at the first object beyond `max_items`.
///
/// The decompressed byte limits bound input allocation. This independent object
/// limit bounds parsed-vector growth and the subsequent validation and database
/// fan-out even when an attacker supplies many tiny JSON objects.
fn parse_bounded_snapshot_array<T>(bytes: &[u8], max_items: usize, name: &str) -> Result<Vec<T>>
where
    T: serde::de::DeserializeOwned,
{
    struct BoundedArrayVisitor<'a, T> {
        /// Maximum number of values accepted from the sequence.
        max_items: usize,
        /// Snapshot component name included in validation errors.
        name: &'a str,
        /// Associates the visitor with its deserialized item type.
        item: std::marker::PhantomData<fn() -> T>,
    }

    impl<'de, T> serde::de::Visitor<'de> for BoundedArrayVisitor<'_, T>
    where
        T: serde::Deserialize<'de>,
    {
        type Value = Vec<T>;

        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(
                formatter,
                "a JSON array containing at most {} objects",
                self.max_items
            )
        }

        fn visit_seq<A>(self, mut sequence: A) -> std::result::Result<Self::Value, A::Error>
        where
            A: serde::de::SeqAccess<'de>,
        {
            use serde::de::Error as _;

            let capacity = sequence.size_hint().unwrap_or(0).min(self.max_items);
            let mut items = Vec::with_capacity(capacity);
            for _ in 0..self.max_items {
                let Some(item) = sequence.next_element()? else {
                    return Ok(items);
                };
                items.push(item);
            }

            if sequence.next_element::<serde::de::IgnoredAny>()?.is_some() {
                return Err(A::Error::custom(format_args!(
                    "Snapshot component {} exceeds the {}-object limit",
                    self.name, self.max_items
                )));
            }
            Ok(items)
        }
    }

    let visitor = BoundedArrayVisitor {
        max_items,
        name,
        item: std::marker::PhantomData,
    };
    let mut deserializer = serde_json::Deserializer::from_slice(bytes);
    let items = serde::de::Deserializer::deserialize_seq(&mut deserializer, visitor)?;
    deserializer.end()?;
    Ok(items)
}

/// Reads one decompressed snapshot component without trusting ZIP size metadata.
fn read_limited_snapshot_component(
    reader: &mut impl std::io::Read,
    declared_size: u64,
    max_bytes: u64,
    name: &str,
) -> Result<Vec<u8>> {
    use std::io::Read as _;

    if declared_size > max_bytes {
        anyhow::bail!(
            "Snapshot component {name} declares {declared_size} decompressed bytes; limit is {max_bytes}"
        );
    }

    let read_limit = max_bytes
        .checked_add(1)
        .ok_or_else(|| anyhow::anyhow!("Snapshot component limit overflowed"))?;
    let initial_capacity = usize::try_from(declared_size.min(max_bytes))?;
    let mut bytes = Vec::with_capacity(initial_capacity);
    reader.take(read_limit).read_to_end(&mut bytes)?;
    if u64::try_from(bytes.len())? > max_bytes {
        anyhow::bail!("Snapshot component {name} exceeds {max_bytes} decompressed bytes");
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::{parse_bounded_snapshot_array, read_limited_snapshot_component, SnapshotPost};
    use anyhow::Result;
    use std::io::Read as _;

    #[test]
    fn snapshot_component_reader_rejects_expansion_beyond_limit() -> Result<()> {
        let mut expanded = std::io::repeat(0).take(65);
        let Err(error) = read_limited_snapshot_component(&mut expanded, 64, 64, "posts.json")
        else {
            anyhow::bail!("decompressed data beyond the component limit was accepted");
        };
        anyhow::ensure!(
            error.to_string().contains("exceeds 64 decompressed bytes"),
            "unexpected snapshot limit error: {error}"
        );
        Ok(())
    }

    #[test]
    fn bounded_snapshot_parser_accepts_exact_object_limit() -> Result<()> {
        let posts = br#"[
            {"post_id":1,"board":"b","author":"anon","content":"one","timestamp":1},
            {"post_id":2,"board":"b","author":"anon","content":"two","timestamp":2}
        ]"#;

        let parsed = parse_bounded_snapshot_array::<SnapshotPost>(posts, 2, "posts.json")?;

        anyhow::ensure!(parsed.len() == 2, "exact object limit was not accepted");
        Ok(())
    }

    #[test]
    fn bounded_snapshot_parser_rejects_first_excess_object() -> Result<()> {
        let posts = br#"[
            {"post_id":1,"board":"b","author":"anon","content":"one","timestamp":1},
            {"post_id":2,"board":"b","author":"anon","content":"two","timestamp":2},
            {"post_id":3,"board":"b","author":"anon","content":"three","timestamp":3},
            {"this_trailing_object_would_not_deserialize_as_a_post":true}
        ]"#;

        let Err(error) = parse_bounded_snapshot_array::<SnapshotPost>(posts, 2, "posts.json")
        else {
            anyhow::bail!("snapshot objects beyond the configured limit were accepted");
        };

        anyhow::ensure!(
            error
                .to_string()
                .contains("posts.json exceeds the 2-object limit"),
            "unexpected snapshot object-count error: {error}"
        );
        Ok(())
    }
}
