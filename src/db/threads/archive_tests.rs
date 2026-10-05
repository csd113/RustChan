//! Archive lifecycle regressions using real schema, transactions and file-backed WAL.

use super::*;
use anyhow::ensure;
use rusqlite::Connection;

fn fixture(conn: &Connection) -> Result<i64> {
    crate::db::schema::install_or_migrate_schema(conn)?;
    crate::db::create_board(conn, "archive", "Archive", "", false)
}

fn thread(conn: &Connection, board: i64, bump: i64) -> Result<i64> {
    let id = conn.query_row(
        "INSERT INTO threads(board_id, subject, bumped_at) VALUES(?1, 'café <script>', ?2) RETURNING id",
        params![board, bump], |row| row.get(0),
    )?;
    conn.execute(
        "INSERT INTO posts(thread_id, board_id, body, body_html, deletion_token, is_op)
         VALUES(?1, ?2, 'archive needle', 'archive needle', 'delete', 1)",
        params![id, board],
    )
    .map(|_rows| ())?;
    Ok(id)
}

fn ids(conn: &Connection, archived: bool) -> Result<Vec<i64>> {
    let mut statement = conn.prepare("SELECT id FROM threads WHERE archived=?1 ORDER BY id")?;
    let ids = statement
        .query_map([i64::from(archived)], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(ids)
}

#[test]
fn archive_threshold_ties_sticky_locked_and_idempotency() -> Result<()> {
    let conn = Connection::open_in_memory()?;
    let board = fixture(&conn)?;
    let oldest = thread(&conn, board, 100)?;
    let newer = thread(&conn, board, 100)?;
    let newest = thread(&conn, board, 100)?;
    let sticky = thread(&conn, board, 1)?;
    set_thread_sticky(&conn, sticky, true)?;
    set_thread_locked(&conn, oldest, true)?;
    ensure!(archive_old_threads(&conn, board, 3)? == 0);
    ensure!(archive_old_threads(&conn, board, 2)? == 1);
    ensure!(ids(&conn, true)? == [oldest]);
    ensure!(ids(&conn, false)? == [newer, newest, sticky]);
    ensure!(archive_old_threads(&conn, board, 2)? == 0);
    let state = get_thread(&conn, oldest)?.context("archived thread")?;
    ensure!(state.archived && state.locked);
    Ok(())
}

#[test]
fn destructive_limits_fail_closed_at_database_boundary() -> Result<()> {
    let conn = Connection::open_in_memory()?;
    let board = fixture(&conn)?;
    let _thread = thread(&conn, board, 100)?;
    for max in [i64::MIN, -1, 0, 10_001, i64::MAX] {
        ensure!(archive_old_threads(&conn, board, max).is_err());
        ensure!(prune_old_threads(&conn, board, max).is_err());
        ensure!(prune_old_archived_threads(&conn, board, max).is_err());
        ensure!(conn.is_autocommit());
        ensure!(ids(&conn, false)?.len() == 1);
    }
    Ok(())
}

#[test]
fn archive_pages_and_retention_share_tie_order_and_search_survives() -> Result<()> {
    let conn = Connection::open_in_memory()?;
    let board = fixture(&conn)?;
    let mut expected = Vec::new();
    for _ in 0_i32..45_i32 {
        let id = thread(&conn, board, 100)?;
        set_thread_archived(&conn, id, true)?;
        expected.push(id);
    }
    let mut paged = Vec::new();
    for offset in [0, 20, 40] {
        paged.extend(
            get_archived_threads_for_board(&conn, board, 20, offset)?
                .iter()
                .map(|t| t.id),
        );
    }
    ensure!(paged == expected);
    ensure!(crate::db::search_posts(&conn, board, "needle", 50, 0)?.len() == 45);
    ensure!(get_threads_for_board(&conn, board, 20, 0)?.is_empty());
    let _deleted = prune_old_archived_threads(&conn, board, 20)?;
    ensure!(ids(&conn, true)? == expected.get(..20).context("first archive page")?);
    ensure!(crate::db::search_posts(&conn, board, "needle", 50, 0)?.len() == 20);
    Ok(())
}

#[test]
fn live_deletion_ties_keep_the_visible_newest_threads() -> Result<()> {
    let conn = Connection::open_in_memory()?;
    let board = fixture(&conn)?;
    let old = thread(&conn, board, 100)?;
    let newer = thread(&conn, board, 100)?;
    let newest = thread(&conn, board, 100)?;
    let _deleted = prune_old_threads(&conn, board, 2)?;
    ensure!(ids(&conn, false)? == [newer, newest]);
    ensure!(get_thread(&conn, old)?.is_none());
    Ok(())
}

#[test]
fn combined_retention_failure_rolls_back_archive_transition() -> Result<()> {
    let conn = Connection::open_in_memory()?;
    let board = fixture(&conn)?;
    for _ in 0_i32..4_i32 {
        let _thread = thread(&conn, board, 100)?;
    }
    conn.execute_batch(
        "CREATE TRIGGER fail_prune BEFORE DELETE ON threads
                        BEGIN SELECT RAISE(ABORT, 'injected archive retention failure'); END;",
    )?;
    {
        let tx =
            rusqlite::Transaction::new_unchecked(&conn, rusqlite::TransactionBehavior::Immediate)?;
        ensure!(archive_old_threads_in_tx(&tx, board, 1)? == 3);
        ensure!(prune_old_threads_in_tx(&tx, board, 1, true).is_err());
    }
    ensure!(conn.is_autocommit());
    ensure!(ids(&conn, false)?.len() == 4 && ids(&conn, true)?.is_empty());
    conn.execute_batch("DROP TRIGGER fail_prune")?;
    ensure!(archive_old_threads(&conn, board, 1)? == 3);
    Ok(())
}

#[test]
fn archived_media_survives_and_shared_paths_are_never_pruned() -> Result<()> {
    let conn = Connection::open_in_memory()?;
    let board = fixture(&conn)?;
    let first = thread(&conn, board, 1)?;
    let second = thread(&conn, board, 2)?;
    let live = thread(&conn, board, 3)?;
    for id in [first, second, live] {
        conn.execute("UPDATE posts SET file_path='archive/shared.webp', thumb_path='archive/thumbs/shared.webp', audio_file_path='archive/shared.opus' WHERE thread_id=?1", [id]).map(|_rows| ())?;
    }
    ensure!(archive_old_threads(&conn, board, 1)? == 2);
    let posts = crate::db::get_posts_for_thread(&conn, first)?;
    let post = posts.first().context("archived OP")?;
    ensure!(post.file_path.as_deref() == Some("archive/shared.webp"));
    ensure!(post.audio_file_path.as_deref() == Some("archive/shared.opus"));
    let deleted = prune_old_archived_threads(&conn, board, 1)?;
    ensure!(deleted.paths.is_empty() && deleted.pending_fs_op_id.is_none());
    ensure!(get_thread(&conn, second)?.is_some());
    Ok(())
}

#[test]
fn archive_recovery_handles_more_than_sqlite_parameter_limit() -> Result<()> {
    let conn = Connection::open_in_memory()?;
    let board = fixture(&conn)?;
    conn.execute(
        "WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<33000)
                  INSERT INTO threads(board_id,bumped_at) SELECT ?1,x FROM n",
        [board],
    )
    .map(|_rows| ())?;
    ensure!(archive_old_threads(&conn, board, 1)? == 32_999);
    let _deleted = prune_old_archived_threads(&conn, board, 1)?;
    ensure!(ids(&conn, true)?.len() == 1 && ids(&conn, false)?.len() == 1);
    Ok(())
}

#[test]
fn archive_busy_failure_and_restart_are_recoverable() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("archive.sqlite3");
    let conn = Connection::open(&path)?;
    conn.execute_batch(crate::db::pool::CONNECTION_PRAGMAS)?;
    let board = fixture(&conn)?;
    let old = thread(&conn, board, 1)?;
    let _new = thread(&conn, board, 2)?;
    let other = Connection::open(&path)?;
    other.busy_timeout(std::time::Duration::from_millis(10))?;
    {
        let _tx =
            rusqlite::Transaction::new_unchecked(&conn, rusqlite::TransactionBehavior::Immediate)?;
        ensure!(archive_old_threads(&other, board, 1).is_err());
        ensure!(other.is_autocommit());
    }
    ensure!(archive_old_threads(&other, board, 1)? == 1);
    drop(other);
    drop(conn);
    let restarted = Connection::open(path)?;
    ensure!(ids(&restarted, true)? == [old]);
    ensure!(archive_old_threads(&restarted, board, 1)? == 0);
    crate::db::verify_database_schema(&restarted)?;
    Ok(())
}

#[test]
fn overlapping_creators_and_archive_passes_preserve_every_thread() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("concurrent.sqlite3");
    let conn = Connection::open(&path)?;
    conn.execute_batch(crate::db::pool::CONNECTION_PRAGMAS)?;
    let board = fixture(&conn)?;
    let barrier = std::sync::Barrier::new(4);
    std::thread::scope(|scope| -> Result<()> {
        let mut handles = Vec::new();
        for worker in 0_i32..4_i32 {
            let path = &path;
            let barrier = &barrier;
            handles.push(scope.spawn(move || -> Result<()> {
                let worker_conn = Connection::open(path)?;
                worker_conn.execute_batch(crate::db::pool::CONNECTION_PRAGMAS)?;
                worker_conn.busy_timeout(std::time::Duration::from_secs(5))?;
                let _barrier = barrier.wait();
                for _ in 0_i32..10_i32 {
                    if worker < 2_i32 {
                        let tx = rusqlite::Transaction::new_unchecked(
                            &worker_conn,
                            rusqlite::TransactionBehavior::Immediate,
                        )?;
                        let _thread = thread(&tx, board, 100)?;
                        let _schedule = crate::db::persist_thread_prune_intent_in_tx(&tx, board)?;
                        tx.commit()?;
                    } else {
                        let _count = archive_old_threads(&worker_conn, board, 3)?;
                    }
                }
                Ok(())
            }));
        }
        for handle in handles {
            handle.join().map_err(|payload| {
                anyhow::anyhow!(
                    "archive worker panicked: {}",
                    crate::media::process::panic_message(payload.as_ref())
                )
            })??;
        }
        Ok(())
    })?;
    let _count = archive_old_threads(&conn, board, 3)?;
    ensure!(ids(&conn, false)?.len() == 3 && ids(&conn, true)?.len() == 17);
    ensure!(crate::db::search_posts(&conn, board, "needle", 100, 0)?.len() == 20);
    crate::db::verify_database_schema(&conn)?;
    Ok(())
}

#[test]
#[ignore = "explicit archive-heavy performance and query-plan evidence"]
fn archive_query_profile() -> Result<()> {
    let mut evidence = Vec::new();
    let conn = Connection::open_in_memory()?;
    let board = fixture(&conn)?;
    conn.execute(
        "WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<10000)
                  INSERT INTO threads(board_id,bumped_at,archived) SELECT ?1,x/100, x>100 FROM n",
        [board],
    )
    .map(|_rows| ())?;
    conn.execute("INSERT INTO posts(thread_id,board_id,body,body_html,deletion_token,is_op) SELECT id,board_id,'needle','needle','delete',1 FROM threads", []).map(|_rows| ())?;
    conn.execute_batch(
        "WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<20)
        INSERT INTO posts(thread_id,board_id,body,body_html,deletion_token)
        SELECT t.id,t.board_id,'reply','reply','delete' FROM threads t CROSS JOIN n;",
    )?;
    let query = "SELECT id FROM threads WHERE board_id=1 AND archived=0 AND sticky=0 ORDER BY bumped_at DESC,id DESC LIMIT -1 OFFSET 50";
    for indexed in [false, true] {
        if indexed {
            conn.execute_batch("CREATE INDEX IF NOT EXISTS idx_threads_active_order ON threads(board_id,sticky DESC,bumped_at DESC,id DESC) WHERE archived=0; DROP INDEX IF EXISTS idx_threads_board_sticky_bumped;")?;
        } else {
            conn.execute_batch("DROP INDEX IF EXISTS idx_threads_active_order; CREATE INDEX IF NOT EXISTS idx_threads_board_sticky_bumped ON threads(board_id,sticky DESC,bumped_at DESC);")?;
        }
        conn.execute_batch("ANALYZE")?;
        let plan = conn
            .prepare(&format!("EXPLAIN QUERY PLAN {query}"))?
            .query_map([], |r| r.get::<_, String>(3))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let start = std::time::Instant::now();
        for _ in 0_i32..200_i32 {
            let result = conn
                .prepare(query)?
                .query_map([], |r| r.get::<_, i64>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            ensure!(result.len() == 50);
        }
        evidence.push(format!(
            "archive-heavy active retention indexed={indexed}: {:?} for 200 queries; {plan:?}",
            start.elapsed()
        ));
    }
    for offset in [0, 5000, 9880] {
        let start = std::time::Instant::now();
        for _ in 0_i32..20_i32 {
            ensure!(get_archived_threads_for_board(&conn, board, 20, offset)?.len() == 20);
        }
        evidence.push(format!(
            "archive page offset={offset}: {:?} for 20 loads",
            start.elapsed()
        ));
    }
    let thread_started = std::time::Instant::now();
    let _thread = get_thread(&conn, 9999)?;
    let _posts = crate::db::get_posts_for_thread(&conn, 9999)?;
    evidence.push(format!(
        "archived thread load: {:?}",
        thread_started.elapsed()
    ));
    let search_started = std::time::Instant::now();
    let _search = crate::db::search_posts(&conn, board, "needle", 20, 0)?;
    evidence.push(format!(
        "archive FTS search: {:?}",
        search_started.elapsed()
    ));
    let maintenance_started = std::time::Instant::now();
    let _count = archive_old_threads(&conn, board, 50)?;
    let _deleted = prune_old_archived_threads(&conn, board, 9000)?;
    evidence.push(format!(
        "archive + retention of 950 threads/19950 posts: {:?}",
        maintenance_started.elapsed()
    ));
    std::fs::write(
        std::env::var("RUSTCHAN_ARCHIVE_EVIDENCE")?,
        evidence.join("\n"),
    )?;
    Ok(())
}

fn reply_post(board_id: i64, thread_id: i64) -> crate::db::NewPost {
    crate::db::NewPost {
        thread_id,
        board_id,
        name: "anon".to_owned(),
        tripcode: None,
        subject: None,
        body: "reply".to_owned(),
        body_html: "reply".to_owned(),
        ip_hash: None,
        file_path: None,
        file_name: None,
        file_size: None,
        thumb_path: None,
        mime_type: None,
        media_type: None,
        audio_file_path: None,
        audio_file_name: None,
        audio_file_size: None,
        audio_mime_type: None,
        deletion_token: "delete".to_owned(),
        is_op: false,
    }
}

#[test]
fn sage_bump_limit_and_deleted_replies_preserve_archive_order() -> Result<()> {
    let conn = Connection::open_in_memory()?;
    let board = fixture(&conn)?;
    let old = thread(&conn, board, 100)?;
    let recent = thread(&conn, board, 200)?;
    conn.execute("UPDATE boards SET bump_limit=1 WHERE id=?1", [board])
        .map(|_rows| ())?;
    let reply = reply_post(board, old);
    let sage_id = create_reply_with_thread_update(&conn, &reply, "sage", false, None)?;
    let _over_limit = create_reply_with_thread_update(&conn, &reply, "after-limit", true, None)?;
    let state = get_thread(&conn, old)?.context("sage thread")?;
    ensure!(state.reply_count == 2 && state.bumped_at == 100);
    let _deleted = crate::db::delete_post(&conn, sage_id)?;
    let after_deletion = get_thread(&conn, old)?.context("thread after deletion")?;
    ensure!(after_deletion.reply_count == 1 && after_deletion.bumped_at == 100);
    ensure!(archive_old_threads(&conn, board, 1)? == 1);
    ensure!(ids(&conn, true)? == [old] && ids(&conn, false)? == [recent]);
    // Even when a moderator clears locked, archive state remains authoritative.
    set_thread_locked(&conn, old, false)?;
    let rejected = create_reply_with_thread_update(&conn, &reply, "archived", true, None)
        .err()
        .context("archive must reject reply")?;
    ensure!(rejected.downcast_ref::<ThreadClosed>() == Some(&ThreadClosed::Archived));
    Ok(())
}

#[test]
fn concurrent_oldest_reply_and_archive_obey_the_committed_bump() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("reply-race.sqlite3");
    let conn = Connection::open(&path)?;
    conn.execute_batch(crate::db::pool::CONNECTION_PRAGMAS)?;
    let board = fixture(&conn)?;
    for round in 0_i32..8_i32 {
        let old = thread(&conn, board, i64::from(round) + 100)?;
        let recent = thread(&conn, board, i64::from(round) + 200)?;
        let barrier = std::sync::Barrier::new(2);
        let replied = std::thread::scope(|scope| -> Result<bool> {
            let barrier_ref = &barrier;
            let path_ref = &path;
            let reply_handle = scope.spawn(move || -> Result<bool> {
                let reply_conn = Connection::open(path_ref)?;
                reply_conn.execute_batch(crate::db::pool::CONNECTION_PRAGMAS)?;
                reply_conn.busy_timeout(std::time::Duration::from_secs(5))?;
                let _barrier = barrier_ref.wait();
                let outcome = match create_reply_with_thread_update(
                    &reply_conn,
                    &reply_post(board, old),
                    "race",
                    true,
                    None,
                ) {
                    Ok(_id) => Ok(true),
                    Err(error)
                        if error.downcast_ref::<ThreadClosed>()
                            == Some(&ThreadClosed::Archived) =>
                    {
                        Ok(false)
                    }
                    Err(error) => Err(error),
                };
                outcome
            });
            let _barrier = barrier.wait();
            ensure!(archive_old_threads(&conn, board, 1)? >= 1);
            reply_handle.join().map_err(|payload| {
                anyhow::anyhow!(
                    "reply worker panicked: {}",
                    crate::media::process::panic_message(payload.as_ref())
                )
            })?
        })?;
        let old_state = get_thread(&conn, old)?.context("old thread")?;
        let recent_state = get_thread(&conn, recent)?.context("recent thread")?;
        ensure!(old_state.archived != replied && recent_state.archived == replied);
        ensure!(old_state.reply_count == i64::from(replied));
        // Remove the survivor before the next round to keep each race independent.
        let _deleted = delete_thread(&conn, if replied { old } else { recent })?;
    }
    crate::db::verify_database_schema(&conn)?;
    Ok(())
}

#[test]
fn failed_archive_commit_releases_the_lock_and_rolls_back() -> Result<()> {
    let conn = Connection::open_in_memory()?;
    let board = fixture(&conn)?;
    let _old = thread(&conn, board, 1)?;
    let _new = thread(&conn, board, 2)?;
    conn.execute_batch("CREATE TABLE commit_failure(parent INTEGER REFERENCES threads(id) DEFERRABLE INITIALLY DEFERRED);
        CREATE TRIGGER injected_archive_commit_failure AFTER UPDATE OF archived ON threads
        WHEN NEW.archived=1 BEGIN INSERT INTO commit_failure VALUES(-1); END;")?;
    ensure!(archive_old_threads(&conn, board, 1).is_err());
    ensure!(conn.is_autocommit() && ids(&conn, true)?.is_empty());
    conn.execute_batch("DROP TRIGGER injected_archive_commit_failure; DROP TABLE commit_failure;")?;
    ensure!(archive_old_threads(&conn, board, 1)? == 1);
    crate::db::verify_database_schema(&conn)?;
    Ok(())
}

#[test]
fn concurrent_moderation_and_archive_are_serialized_by_sqlite() -> Result<()> {
    for action in ["sticky", "lock", "delete"] {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("moderation-race.sqlite3");
        let conn = Connection::open(&path)?;
        conn.execute_batch(crate::db::pool::CONNECTION_PRAGMAS)?;
        let board = fixture(&conn)?;
        let old = thread(&conn, board, 100)?;
        let _recent = thread(&conn, board, 200)?;
        let barrier = std::sync::Barrier::new(2);
        let already_archived = std::thread::scope(|scope| -> Result<bool> {
            let barrier_ref = &barrier;
            let path_ref = &path;
            let moderator = scope.spawn(move || -> Result<bool> {
                let mod_conn = Connection::open(path_ref)?;
                mod_conn.execute_batch(crate::db::pool::CONNECTION_PRAGMAS)?;
                mod_conn.busy_timeout(std::time::Duration::from_secs(5))?;
                let _barrier = barrier_ref.wait();
                if action == "delete" {
                    let _deleted = delete_thread(&mod_conn, old)?;
                    return Ok(false);
                }
                let tx = rusqlite::Transaction::new_unchecked(
                    &mod_conn,
                    rusqlite::TransactionBehavior::Immediate,
                )?;
                let before = get_thread(&tx, old)?.context("moderated thread")?.archived;
                if action == "sticky" {
                    set_thread_sticky(&tx, old, true)?;
                } else {
                    set_thread_locked(&tx, old, true)?;
                }
                tx.commit()?;
                Ok(before)
            });
            let _barrier = barrier.wait();
            let _count = archive_old_threads(&conn, board, 1)?;
            moderator.join().map_err(|payload| {
                anyhow::anyhow!(
                    "moderator panicked: {}",
                    crate::media::process::panic_message(payload.as_ref())
                )
            })?
        })?;
        match action {
            "sticky" => {
                let state = get_thread(&conn, old)?.context("sticky thread")?;
                ensure!(state.sticky && state.archived == already_archived);
            }
            "lock" => {
                let state = get_thread(&conn, old)?.context("locked thread")?;
                ensure!(state.archived && state.locked);
            }
            _ => ensure!(get_thread(&conn, old)?.is_none()),
        }
        crate::db::verify_database_schema(&conn)?;
    }
    Ok(())
}

#[test]
#[ignore = "invoked only by the interrupted archive regression"]
fn interrupted_archive_child() -> Result<()> {
    let path = std::env::var("RUSTCHAN_ARCHIVE_CRASH_DB")?;
    let ready = std::env::var("RUSTCHAN_ARCHIVE_CRASH_READY")?;
    let conn = Connection::open(path)?;
    conn.execute_batch(crate::db::pool::CONNECTION_PRAGMAS)?;
    let tx = rusqlite::Transaction::new_unchecked(&conn, rusqlite::TransactionBehavior::Immediate)?;
    ensure!(archive_old_threads_in_tx(&tx, 1, 1)? == 1);
    std::fs::write(ready, b"archive updated inside uncommitted transaction")?;
    // The parent kills this isolated fixture process while the transaction is open.
    std::thread::sleep(std::time::Duration::from_secs(30));
    tx.commit()?;
    Ok(())
}

#[test]
fn process_interruption_during_archive_recovers_without_content_loss() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("interrupted.sqlite3");
    let ready = dir.path().join("ready");
    let conn = Connection::open(&path)?;
    conn.execute_batch(crate::db::pool::CONNECTION_PRAGMAS)?;
    let board = fixture(&conn)?;
    let old = thread(&conn, board, 1)?;
    let new = thread(&conn, board, 2)?;
    drop(conn);
    let mut child = std::process::Command::new(std::env::current_exe()?)
        .args([
            "--ignored",
            "--exact",
            "db::threads::archive_tests::interrupted_archive_child",
        ])
        .env("RUSTCHAN_ARCHIVE_CRASH_DB", &path)
        .env("RUSTCHAN_ARCHIVE_CRASH_READY", &ready)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !ready.exists() && std::time::Instant::now() < deadline {
        if child.try_wait()?.is_some() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    if child.try_wait()?.is_none() {
        child.kill()?;
    }
    let _status = child.wait()?;
    ensure!(
        ready.exists(),
        "archive crash fixture did not reach its uncommitted transition"
    );
    let recovered = Connection::open(path)?;
    recovered.execute_batch(crate::db::pool::CONNECTION_PRAGMAS)?;
    ensure!(ids(&recovered, false)? == [old, new] && ids(&recovered, true)?.is_empty());
    ensure!(archive_old_threads(&recovered, board, 1)? == 1);
    crate::db::verify_database_schema(&recovered)?;
    Ok(())
}
