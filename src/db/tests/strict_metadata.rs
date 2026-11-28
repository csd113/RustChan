//! Upgrade must preserve board data, relationships, constraints and opt-in state.

use anyhow::{ensure, Result};

/// Rebuilding an old board must neither enable the policy nor damage its data.
#[test]
fn strict_policy_upgrade_is_opt_in_and_preserves_data() -> Result<()> {
    let conn = rusqlite::Connection::open_in_memory()?;
    conn.execute_batch("PRAGMA foreign_keys = ON")?;
    super::install_or_migrate_schema(&conn)?;
    // This is the previous baseline: every other schema object is unchanged.
    conn.execute_batch(
        "ALTER TABLE boards DROP COLUMN strict_metadata_uploads;
         INSERT INTO boards (id, short_name, name, description, allow_audio)
             VALUES (7, 'test', 'Preserved', 'unchanged', 1);
         INSERT INTO boards (id, short_name, name) VALUES (99, 'deleted', 'Deleted');
         DELETE FROM boards WHERE id = 99;
         INSERT INTO threads (id, board_id) VALUES (3, 7);
         INSERT INTO posts (thread_id, board_id, body, body_html, ip_hash, is_op, deletion_token)
             VALUES (3, 7, 'retained', 'retained', 'fixture', 1, 'owned-fixture');",
    )?;
    super::install_or_migrate_schema(&conn)?;
    let board = crate::db::get_board_by_short(&conn, "test")?
        .ok_or_else(|| anyhow::anyhow!("board missing"))?;
    ensure!(
        !board.strict_metadata_uploads && board.allow_audio && board.description == "unchanged",
        "upgrade changed board policy or data"
    );
    let retained: i64 = conn.query_row(
        "SELECT COUNT(*) FROM posts WHERE board_id=7 AND thread_id=3 AND body='retained'",
        [],
        |row| row.get(0),
    )?;
    ensure!(retained == 1, "upgrade lost post relationships");
    conn.execute_batch("INSERT INTO boards (short_name, name) VALUES ('next', 'Next')")?;
    ensure!(
        conn.last_insert_rowid() > 99,
        "board high-water mark was lost"
    );
    conn.execute_batch("UPDATE boards SET strict_metadata_uploads = 1 WHERE id=7")?;
    super::install_or_migrate_schema(&conn)?;
    let strict: bool = conn.query_row(
        "SELECT strict_metadata_uploads FROM boards WHERE id=7",
        [],
        |row| row.get(0),
    )?;
    ensure!(strict, "subsequent startup disabled the policy");
    for invalid in ["2", "-1", "'unsafe'", "NULL"] {
        ensure!(
            conn.execute_batch(&format!(
                "UPDATE boards SET strict_metadata_uploads={invalid} WHERE id=7"
            ))
            .is_err(),
            "invalid policy value accepted"
        );
    }
    let foreign_keys: bool = conn.query_row("PRAGMA foreign_keys", [], |row| row.get(0))?;
    ensure!(foreign_keys, "upgrade disabled foreign key enforcement");
    super::verify_database_schema(&conn)
}

/// Unknown drift is not permission to rebuild a database or silently opt it in.
#[test]
fn strict_policy_upgrade_refuses_unknown_schema_without_mutation() -> Result<()> {
    let conn = rusqlite::Connection::open_in_memory()?;
    super::install_or_migrate_schema(&conn)?;
    conn.execute_batch("ALTER TABLE boards DROP COLUMN strict_metadata_uploads; ALTER TABLE boards ADD COLUMN unknown_setting TEXT")?;
    ensure!(
        super::install_or_migrate_schema(&conn).is_err(),
        "unrecognized schema was migrated"
    );
    ensure!(
        !super::table_columns(&conn, "boards")?.contains_key("strict_metadata_uploads"),
        "failed migration partially changed schema"
    );
    Ok(())
}
