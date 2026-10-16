use anyhow::{Context as _, Result};

#[derive(Debug, Clone)]
/// Durable filesystem operation read from the database queue.
pub struct PendingFsOpRow {
    /// Stable operation identifier.
    pub id: String,
    /// Operation-kind discriminator.
    pub kind: String,
    /// Serialized operation payload.
    pub payload_json: String,
}

/// Trusted schema used for the durable filesystem-operation queue.
const CREATE_PENDING_FS_OPS_SQL: &str = r"
    CREATE TABLE IF NOT EXISTS pending_fs_ops (
        id           TEXT PRIMARY KEY,
        kind         TEXT NOT NULL,
        payload_json TEXT NOT NULL,
        created_at   INTEGER NOT NULL DEFAULT (unixepoch())
    );
    CREATE INDEX IF NOT EXISTS idx_pending_fs_ops_created
        ON pending_fs_ops(created_at ASC);
";

/// Ensure the `pending_fs_ops` table exists on the target database.
///
/// # Errors
/// Returns an error if the table or index cannot be created.
pub fn ensure_pending_fs_ops_table(conn: &rusqlite::Connection) -> Result<()> {
    conn.execute_batch(CREATE_PENDING_FS_OPS_SQL)
        .context("Create pending_fs_ops table failed")?;
    super::schema::ensure_pending_fs_op_invariants(conn)
}

/// Insert or replace a durable pending filesystem operation.
///
/// # Errors
/// Returns an error if the pending operation table cannot be prepared or the
/// insert fails.
pub fn insert_pending_fs_op(
    conn: &rusqlite::Connection,
    op: &crate::pending_fs::PendingFsOpInsert,
) -> Result<()> {
    ensure_pending_fs_ops_table(conn)?;
    conn.execute(
        "INSERT INTO pending_fs_ops (id, kind, payload_json) VALUES (?1, ?2, ?3)",
        rusqlite::params![op.id, op.kind, op.payload_json],
    )
    .context("Insert pending_fs_op failed")
    .map(|_affected_rows| ())?;
    Ok(())
}

/// Delete a completed pending filesystem operation.
///
/// # Errors
/// Returns an error if the delete statement fails.
pub fn delete_pending_fs_op(conn: &rusqlite::Connection, id: &str) -> Result<()> {
    conn.execute(
        "DELETE FROM pending_fs_ops WHERE id = ?1",
        rusqlite::params![id],
    )
    .with_context(|| format!("Delete pending_fs_op {id} failed"))
    .map(|_affected_rows| ())?;
    Ok(())
}

/// Quote an `SQLite` identifier by doubling embedded quotation marks.
fn quote_sqlite_identifier(identifier: &str) -> String {
    format!("\"{}\"", identifier.replace('"', "\"\""))
}

/// Replace restored `pending_fs_ops` objects with the trusted operational table.
///
/// # Errors
/// Returns an error if hostile or unexpected schema objects cannot be removed,
/// or if the trusted table cannot be recreated.
pub fn rebuild_pending_fs_ops_for_restore(conn: &rusqlite::Connection) -> Result<()> {
    let mut trigger_stmt = conn
        .prepare(
            "SELECT name FROM sqlite_schema
             WHERE type = 'trigger' AND tbl_name = 'pending_fs_ops'",
        )
        .context("Prepare pending_fs_ops trigger lookup failed")?;
    let triggers = trigger_stmt
        .query_map([], |row| row.get::<_, String>(0))
        .context("Query pending_fs_ops triggers failed")?
        .collect::<rusqlite::Result<Vec<_>>>()
        .context("Read pending_fs_ops triggers failed")?;
    drop(trigger_stmt);

    for trigger in triggers {
        conn.execute_batch(&format!(
            "DROP TRIGGER IF EXISTS {};",
            quote_sqlite_identifier(&trigger)
        ))
        .with_context(|| format!("Drop restored pending_fs_ops trigger {trigger} failed"))?;
    }

    let mut object_stmt = conn
        .prepare("SELECT type FROM sqlite_schema WHERE name = 'pending_fs_ops'")
        .context("Prepare pending_fs_ops object lookup failed")?;
    let object_types = object_stmt
        .query_map([], |row| row.get::<_, String>(0))
        .context("Query pending_fs_ops objects failed")?
        .collect::<rusqlite::Result<Vec<_>>>()
        .context("Read pending_fs_ops objects failed")?;
    drop(object_stmt);

    for object_type in object_types {
        match object_type.as_str() {
            "table" => conn
                .execute_batch("DROP TABLE IF EXISTS pending_fs_ops;")
                .context("Drop restored pending_fs_ops table failed")?,
            "view" => conn
                .execute_batch("DROP VIEW IF EXISTS pending_fs_ops;")
                .context("Drop restored pending_fs_ops view failed")?,
            "index" => conn
                .execute_batch("DROP INDEX IF EXISTS pending_fs_ops;")
                .context("Drop restored pending_fs_ops index failed")?,
            other => anyhow::bail!("Unexpected pending_fs_ops schema object type {other}"),
        }
    }

    ensure_pending_fs_ops_table(conn)?;
    Ok(())
}

/// Verify that the expected pending operation exists after restore hardening.
///
/// # Errors
/// Returns an error if the expected pending operation is missing or duplicated.
pub fn verify_pending_fs_op_present(conn: &rusqlite::Connection, expected_id: &str) -> Result<()> {
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM pending_fs_ops WHERE id = ?1",
            rusqlite::params![expected_id],
            |row| row.get(0),
        )
        .with_context(|| format!("Count pending_fs_op {expected_id} failed"))?;
    if count == 1 {
        Ok(())
    } else {
        anyhow::bail!(
            "Unexpected pending_fs_ops state after restore normalization: expected pending op {expected_id} exactly once, found {count}"
        )
    }
}

/// Load all pending filesystem operations in creation order.
///
/// # Errors
/// Returns an error if the query cannot be prepared, executed, or decoded.
pub fn list_pending_fs_ops(conn: &rusqlite::Connection) -> Result<Vec<PendingFsOpRow>> {
    ensure_pending_fs_ops_table(conn)?;
    let mut stmt = conn
        .prepare("SELECT id, kind, payload_json FROM pending_fs_ops ORDER BY created_at ASC")
        .context("Prepare list_pending_fs_ops failed")?;
    let rows = stmt
        .query_map([], |row| {
            Ok(PendingFsOpRow {
                id: row.get(0)?,
                kind: row.get(1)?,
                payload_json: row.get(2)?,
            })
        })
        .context("Query pending_fs_ops failed")?
        .collect::<rusqlite::Result<Vec<_>>>()
        .context("Read pending_fs_ops failed")?;
    Ok(rows)
}
