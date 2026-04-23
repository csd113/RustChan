use anyhow::{Context, Result};

#[derive(Debug, Clone)]
pub struct PendingFsOpRow {
    pub id: String,
    pub kind: String,
    pub payload_json: String,
}

#[derive(Debug, Clone)]
struct RestoredPendingFsOpRow {
    id: String,
    kind: String,
    payload_json: String,
    created_at: i64,
}

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
        .context("Create pending_fs_ops table failed")
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
    .context("Insert pending_fs_op failed")?;
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
    .with_context(|| format!("Delete pending_fs_op {id} failed"))?;
    Ok(())
}

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

    let preserved_rows = if object_types
        .iter()
        .any(|object_type| object_type == "table")
    {
        load_pending_fs_ops_for_restore(conn)?
    } else {
        Vec::new()
    };

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
    restore_pending_fs_ops_rows(conn, &preserved_rows)
}

fn load_pending_fs_ops_for_restore(
    conn: &rusqlite::Connection,
) -> Result<Vec<RestoredPendingFsOpRow>> {
    let mut stmt = conn
        .prepare(
            "SELECT id, kind, payload_json, created_at
             FROM pending_fs_ops
             ORDER BY created_at ASC, id ASC",
        )
        .context("Prepare pending_fs_ops restore preservation failed")?;
    let rows = stmt
        .query_map([], |row| {
            Ok(RestoredPendingFsOpRow {
                id: row.get(0)?,
                kind: row.get(1)?,
                payload_json: row.get(2)?,
                created_at: row.get(3)?,
            })
        })
        .context("Query pending_fs_ops restore preservation failed")?
        .collect::<rusqlite::Result<Vec<_>>>()
        .context("Read pending_fs_ops restore preservation failed")?;
    Ok(rows)
}

fn restore_pending_fs_ops_rows(
    conn: &rusqlite::Connection,
    rows: &[RestoredPendingFsOpRow],
) -> Result<()> {
    for row in rows {
        conn.execute(
            "INSERT INTO pending_fs_ops (id, kind, payload_json, created_at)
             VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![&row.id, &row.kind, &row.payload_json, row.created_at],
        )
        .with_context(|| format!("Restore pending_fs_op {} failed", row.id))?;
    }
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

#[cfg(test)]
mod tests {
    use super::{
        insert_pending_fs_op, list_pending_fs_ops, rebuild_pending_fs_ops_for_restore,
        verify_pending_fs_op_present,
    };
    use std::collections::HashSet;

    #[test]
    fn rebuild_pending_fs_ops_for_restore_preserves_legitimate_rows_and_allows_restore_swap_insert()
    {
        let conn = rusqlite::Connection::open_in_memory().expect("in-memory db");
        conn.execute_batch(
            r#"
            CREATE TABLE pending_fs_ops (
                id           TEXT PRIMARY KEY,
                kind         TEXT NOT NULL,
                payload_json TEXT NOT NULL,
                created_at   INTEGER NOT NULL DEFAULT (unixepoch())
            );
            INSERT INTO pending_fs_ops (id, kind, payload_json, created_at)
            VALUES
                ('upload-finalize', 'upload_finalize', '{"stage_dir":"stage","relative_paths":["tech/file.webp"],"primary_hash":null,"primary_file_path":null,"primary_thumb_path":null,"primary_mime_type":null}', 10),
                ('delete-files', 'delete_files', '{"paths":["tech/file.webp"]}', 20);
            CREATE TRIGGER pending_fs_ops_reseed
            AFTER INSERT ON pending_fs_ops
            BEGIN
                INSERT OR IGNORE INTO pending_fs_ops (id, kind, payload_json)
                VALUES ('trigger-evil', 'delete_files', '{"paths":["uploads/trigger"]}');
            END;
            "#,
        )
        .expect("hostile schema");

        rebuild_pending_fs_ops_for_restore(&conn).expect("rebuild trusted table");
        let rebuilt_rows = list_pending_fs_ops(&conn).expect("list rebuilt ops");
        let [upload_finalize, delete_files] = rebuilt_rows.as_slice() else {
            panic!("expected two preserved pending ops");
        };
        assert_eq!(upload_finalize.id, "upload-finalize");
        assert_eq!(
            upload_finalize.kind,
            crate::pending_fs::UPLOAD_FINALIZE_KIND
        );
        assert_eq!(delete_files.id, "delete-files");
        assert_eq!(delete_files.kind, crate::pending_fs::DELETE_FILES_KIND);

        let restore_op = crate::pending_fs::PendingFsOpInsert {
            id: "expected-restore".into(),
            kind: crate::pending_fs::FULL_RESTORE_SWAP_KIND,
            payload_json: r#"{"staged":"stage","live":"live","previous":"old"}"#.into(),
        };
        insert_pending_fs_op(&conn, &restore_op).expect("insert restore op");

        verify_pending_fs_op_present(&conn, &restore_op.id).expect("restore op present");
        let pending_ops = list_pending_fs_ops(&conn).expect("list ops");
        assert_eq!(pending_ops.len(), 3);
        let unique_ids = pending_ops
            .iter()
            .map(|op| op.id.clone())
            .collect::<HashSet<_>>();
        assert_eq!(unique_ids.len(), pending_ops.len());
        assert!(pending_ops
            .iter()
            .any(|op| op.id == "upload-finalize"
                && op.kind == crate::pending_fs::UPLOAD_FINALIZE_KIND));
        assert!(pending_ops
            .iter()
            .any(|op| op.id == "delete-files" && op.kind == crate::pending_fs::DELETE_FILES_KIND));
        assert!(pending_ops.iter().any(
            |op| op.id == restore_op.id && op.kind == crate::pending_fs::FULL_RESTORE_SWAP_KIND
        ));
        assert!(!pending_ops.iter().any(|op| op.id == "trigger-evil"));
    }

    #[test]
    fn rebuild_pending_fs_ops_for_restore_fails_closed_on_unexpected_schema_object_type() {
        let conn = rusqlite::Connection::open_in_memory().expect("in-memory db");
        conn.execute_batch(
            r"
            CREATE TABLE other_table (value INTEGER);
            CREATE TRIGGER pending_fs_ops
            AFTER INSERT ON other_table
            BEGIN
                SELECT 1;
            END;
            ",
        )
        .expect("unexpected schema object");

        let error = rebuild_pending_fs_ops_for_restore(&conn).expect_err("unexpected type fails");
        assert!(error
            .to_string()
            .contains("Unexpected pending_fs_ops schema object type trigger"));
    }
}
