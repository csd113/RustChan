//! Posting state-machine regressions against the full schema and file-backed WAL pool.

use super::*;
use crate::db::{self, DbPool, NewPost};
use crate::error::AppError;
use anyhow::ensure;
use std::sync::Barrier;

/// Minimal valid post shared by lifecycle tests.
fn post(board_id: i64, thread_id: i64) -> NewPost {
    NewPost {
        thread_id,
        board_id,
        name: "Anonymous".into(),
        tripcode: None,
        subject: None,
        body: "lifecycle reply".into(),
        body_html: "lifecycle reply".into(),
        ip_hash: Some("actor".into()),
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
        deletion_token: "secret".into(),
        is_op: false,
    }
}

/// Install a board and its OP in an isolated pool using production pragmas.
fn fixture() -> Result<(DbPool, i64, i64)> {
    let pool = db::init_test_pool()?;
    let conn = pool.get()?;
    let board = db::create_board(&conn, "life", "Lifecycle", "", false)?;
    let (thread, _, _) = create_thread_with_optional_poll(
        &conn,
        board,
        None,
        &post(board, 0),
        "op-token",
        None,
        None,
    )?;
    drop(conn);
    Ok((pool, board, thread))
}

/// Check relational, OP, counter, ID, and cached-view invariants after mutations.
fn invariants(conn: &rusqlite::Connection) -> Result<()> {
    ensure!(
        conn.is_autocommit(),
        "a failed operation leaked its transaction"
    );
    let broken: i64 = conn.query_row(
        "SELECT (SELECT COUNT(*) FROM pragma_foreign_key_check)
            + (SELECT COUNT(*) FROM posts p LEFT JOIN threads t ON t.id=p.thread_id
               WHERE t.id IS NULL OR p.board_id != t.board_id)
            + (SELECT COUNT(*) FROM threads t WHERE t.reply_count !=
                (SELECT COUNT(*) FROM posts p WHERE p.thread_id=t.id AND p.is_op=0)
                OR 1 != (SELECT COUNT(*) FROM posts p WHERE p.thread_id=t.id AND p.is_op=1))",
        [],
        |row| row.get(0),
    )?;
    ensure!(broken == 0, "lifecycle invariants failed: {broken}");
    for board in db::get_all_boards(conn)? {
        for thread in get_threads_for_board(conn, board.id, 1_000, 0)? {
            let actual: i64 = conn.query_row(
                "SELECT COUNT(*) FROM posts WHERE thread_id=?1 AND file_path IS NOT NULL",
                [thread.id],
                |row| row.get(0),
            )?;
            ensure!(thread.image_count == actual, "listing media count diverged");
        }
    }
    Ok(())
}

/// Join scoped workers without allowing a panic to hide a failed operation.
fn join<T>(handle: std::thread::ScopedJoinHandle<'_, T>) -> Result<T> {
    handle.join().map_err(|payload| {
        anyhow::anyhow!(
            "lifecycle worker panicked: {}",
            crate::media::process::panic_message(payload.as_ref())
        )
    })
}

#[test]
fn wal_reply_bursts_preserve_every_id_counter_and_sage_bump() -> Result<()> {
    for size in [2_usize, 5, 10, 25, 64] {
        let (pool, board, thread) = fixture()?;
        pool.get()?
            .execute("UPDATE threads SET bumped_at=1 WHERE id=?1", [thread])
            .map(|_rows| ())?;
        let barrier = Barrier::new(size);
        let ids = std::thread::scope(|scope| -> Result<Vec<i64>> {
            let mut handles = Vec::new();
            for index in 0..size {
                let pool = &pool;
                let barrier = &barrier;
                handles.push(scope.spawn(move || -> Result<i64> {
                    let _wait = barrier.wait();
                    let conn = pool.get()?;
                    let mut reply = post(board, thread);
                    if index.is_multiple_of(2) {
                        reply.file_path = Some(format!("life/burst-{index}.png"));
                    }
                    create_reply_with_thread_update(
                        &conn,
                        &reply,
                        &format!("reply-{index}"),
                        false,
                        None,
                    )
                }));
            }
            handles.into_iter().map(|handle| join(handle)?).collect()
        })?;
        ensure!(ids.iter().collect::<std::collections::HashSet<_>>().len() == size);
        let conn = pool.get()?;
        let target = get_thread(&conn, thread)?.context("thread")?;
        ensure!(target.reply_count == i64::try_from(size)? && target.bumped_at == 1);
        ensure!(db::get_posts_for_thread(&conn, thread)?.len() == size.saturating_add(1));
        ensure!(
            get_threads_for_board(&conn, board, 10, 0)?
                .first()
                .context("listed thread")?
                .image_count
                == i64::try_from(size.div_ceil(2))?
        );
        invariants(&conn)?;
    }
    Ok(())
}

#[test]
fn concurrent_creators_leave_durable_coalesced_retention_work() -> Result<()> {
    let (pool, board, _) = fixture()?;
    pool.get()?
        .execute("UPDATE boards SET max_threads=1 WHERE id=?1", [board])
        .map(|_rows| ())?;
    let barrier = Barrier::new(25);
    std::thread::scope(|scope| -> Result<()> {
        let mut handles = Vec::new();
        for index in 0_i32..25_i32 {
            let pool = &pool;
            let barrier = &barrier;
            handles.push(scope.spawn(move || -> Result<()> {
                let _wait = barrier.wait();
                let conn = pool.get()?;
                let outcome = create_thread_submission(
                    &conn,
                    board,
                    None,
                    &post(board, 0),
                    &format!("create-{index}"),
                    None,
                    PostFilesystemCommit::new(None, &[], true),
                )?;
                ensure!(matches!(outcome, PostCreationOutcome::Created(_, _)));
                Ok(())
            }));
        }
        for handle in handles {
            join(handle)??;
        }
        Ok(())
    })?;
    let conn = pool.get()?;
    let jobs: i64 = conn.query_row(
        "SELECT COUNT(*) FROM background_jobs WHERE job_type='thread_prune' AND status='pending'",
        [],
        |row| row.get(0),
    )?;
    ensure!(jobs == 1, "overflow must leave one durable evaluation");
    let newest: i64 = conn.query_row(
        "SELECT MAX(id) FROM threads WHERE board_id=?1",
        [board],
        |row| row.get(0),
    )?;
    let _cleanup = prune_old_threads(&conn, board, 1)?;
    ensure!(count_threads_for_board(&conn, board)? == 1 && get_thread(&conn, newest)?.is_some());
    invariants(&conn)?;
    Ok(())
}

#[test]
fn deleted_submissions_cannot_recreate_threads_or_replies() -> Result<()> {
    let (pool, board, thread) = fixture()?;
    let conn = pool.get()?;
    let reply =
        create_reply_with_thread_update(&conn, &post(board, thread), "reply-token", true, None)?;
    let _cleanup = db::delete_post(&conn, reply)?;
    let receipt = db::get_post_submission(&conn, "reply-token", "actor", board)?
        .context("deleted receipt")?;
    ensure!(receipt.post_id == reply && !receipt.is_thread);
    ensure!(
        create_reply_with_thread_update(&conn, &post(board, thread), "reply-token", true, None)?
            == reply
    );
    ensure!(db::get_post(&conn, reply)?.is_none());
    delete_thread(&conn, thread).map(|_thread_cleanup| ())?;
    let original_target = create_thread_with_optional_poll(
        &conn,
        board,
        None,
        &post(board, 0),
        "op-token",
        None,
        None,
    )?;
    ensure!(original_target.0 == thread && get_thread(&conn, thread)?.is_none());
    ensure!(count_threads_for_board(&conn, board)? == 0);
    invariants(&conn)?;
    Ok(())
}

#[test]
fn receipts_are_bound_to_operation_and_reply_target() -> Result<()> {
    let (pool, board, thread) = fixture()?;
    let conn = pool.get()?;
    let (other, _, _) =
        create_thread_with_optional_poll(&conn, board, None, &post(board, 0), "other", None, None)?;
    let reply =
        create_reply_with_thread_update(&conn, &post(board, thread), "reply-token", true, None)?;
    for (target, token) in [(other, "reply-token"), (thread, "op-token")] {
        let error = create_reply_with_thread_update(&conn, &post(board, target), token, true, None)
            .err()
            .context("must reject wrong form")?;
        ensure!(matches!(
            error.downcast_ref::<AppError>(),
            Some(AppError::Conflict(_))
        ));
    }
    let error = create_thread_with_optional_poll(
        &conn,
        board,
        None,
        &post(board, 0),
        "reply-token",
        None,
        None,
    )
    .err()
    .context("must reject wrong operation")?;
    ensure!(matches!(
        error.downcast_ref::<AppError>(),
        Some(AppError::Conflict(_))
    ));
    let mut forged_op = post(board, thread);
    forged_op.is_op = true;
    ensure!(create_reply_with_thread_update(&conn, &forged_op, "forged", true, None).is_err());
    ensure!(db::get_post(&conn, reply)?.is_some());
    invariants(&conn)?;
    Ok(())
}

/// Competing transitions supported by the actual thread state model.
#[derive(Clone, Copy)]
enum Transition {
    Lock,
    Delete,
    Archive,
    Prune,
}

#[test]
fn concurrent_reply_and_moderation_prune_always_leave_valid_state() -> Result<()> {
    for transition in [
        Transition::Lock,
        Transition::Delete,
        Transition::Archive,
        Transition::Prune,
    ] {
        for _round in 0_i32..8_i32 {
            let (pool, board, thread) = fixture()?;
            {
                let conn = pool.get()?;
                conn.execute("UPDATE threads SET bumped_at=1 WHERE id=?1", [thread])
                    .map(|_rows| ())?;
                create_thread_with_optional_poll(
                    &conn,
                    board,
                    None,
                    &post(board, 0),
                    "new",
                    None,
                    None,
                )
                .map(|_ids| ())?;
            }
            let barrier = Barrier::new(2);
            let result = std::thread::scope(|scope| -> Result<Result<i64>> {
                let reply = scope.spawn(|| -> Result<i64> {
                    let _wait = barrier.wait();
                    let conn = pool.get()?;
                    create_reply_with_thread_update(
                        &conn,
                        &post(board, thread),
                        "race",
                        false,
                        None,
                    )
                });
                let moderation = scope.spawn(|| -> Result<()> {
                    let _wait = barrier.wait();
                    let conn = pool.get()?;
                    match transition {
                        Transition::Lock => set_thread_locked(&conn, thread, true),
                        Transition::Delete => delete_thread(&conn, thread)
                            .map(|_cleanup| ())
                            .map_err(anyhow::Error::new),
                        Transition::Archive => set_thread_archived(&conn, thread, true),
                        Transition::Prune => prune_old_threads(&conn, board, 1).map(|_cleanup| ()),
                    }
                });
                join(moderation)??;
                join(reply)
            })?;
            if let Err(error) = result {
                ensure!(
                    error.downcast_ref::<ThreadClosed>().is_some()
                        || matches!(
                            error.downcast_ref::<AppError>(),
                            Some(AppError::NotFound(_))
                        ),
                    "unexpected race failure: {error}"
                );
            }
            let conn = pool.get()?;
            invariants(&conn)?;
            match transition {
                Transition::Lock | Transition::Archive => {
                    ensure!(get_thread(&conn, thread)?.context("closed thread")?.locked);
                    ensure!(create_reply_with_thread_update(
                        &conn,
                        &post(board, thread),
                        "new-race",
                        true,
                        None
                    )
                    .is_err());
                }
                Transition::Delete | Transition::Prune => {
                    ensure!(get_thread(&conn, thread)?.is_none());
                }
            }
        }
    }
    Ok(())
}

#[test]
fn media_job_failure_rolls_back_post_and_claim_waits_for_publication() -> Result<()> {
    let (pool, board, thread) = fixture()?;
    let conn = pool.get()?;
    let mut media_post = post(board, thread);
    media_post.file_path = Some("life/audio.flac".into());
    media_post.media_type = Some("audio".into());
    conn.execute_batch("CREATE TRIGGER reject_media BEFORE INSERT ON background_jobs WHEN NEW.job_type='audio_waveform' BEGIN SELECT RAISE(ABORT, 'injected'); END;")?;
    let pending = crate::pending_fs::PendingFsOpInsert {
        id: "stage".into(),
        kind: crate::pending_fs::UPLOAD_FINALIZE_KIND,
        payload_json: r#"{"relative_paths":["life/audio.flac"]}"#.into(),
    };
    let create = |token: &str| {
        create_reply_submission(
            &conn,
            &media_post,
            token,
            true,
            PostFilesystemCommit::new_with_media_validation(
                Some(&pending),
                &[],
                false,
                Some("life"),
                |_: &rusqlite::Connection| Ok(()),
            ),
        )
    };
    ensure!(create("failed-media").is_err());
    invariants(&conn)?;
    ensure!(db::get_post_submission(&conn, "failed-media", "actor", board)?.is_none());
    conn.execute_batch("DROP TRIGGER reject_media")?;
    let outcome = create("media")?;
    let PostCreationOutcome::Created(post_id, created_at) = outcome else {
        anyhow::bail!("expected new media post");
    };
    let stored = db::get_post(&conn, post_id)?.context("media post")?;
    ensure!(
        stored.created_at == created_at
            && stored.media_processing_state.as_deref() == Some(db::MEDIA_PROCESSING_PENDING)
    );
    ensure!(
        db::claim_next_job(&conn)?.is_none(),
        "worker must wait for durable upload publication"
    );
    conn.execute("DELETE FROM pending_fs_ops WHERE id='stage'", [])
        .map(|_rows| ())?;
    let (_job_id, payload) = db::claim_next_job(&conn)?.context("published media job")?;
    ensure!(
        matches!(serde_json::from_str::<crate::workers::Job>(&payload)?, crate::workers::Job::AudioWaveform { post_id: target, file_path: _, board_short: _ } if target == post_id)
    );
    invariants(&conn)?;
    Ok(())
}

#[test]
fn failed_post_commit_rolls_back_every_required_row_and_releases_the_writer() -> Result<()> {
    let (pool, board, thread) = fixture()?;
    let conn = pool.get()?;
    conn.execute_batch("CREATE TABLE commit_failure(parent INTEGER REFERENCES threads(id) DEFERRABLE INITIALLY DEFERRED);
        CREATE TRIGGER reject_commit AFTER INSERT ON posts BEGIN INSERT INTO commit_failure VALUES(-1); END;")?;
    for new_thread in [false, true] {
        let result = if new_thread {
            create_thread_with_optional_poll(
                &conn,
                board,
                None,
                &post(board, 0),
                "failed",
                None,
                None,
            )
            .map(|_ids| ())
        } else {
            create_reply_with_thread_update(&conn, &post(board, thread), "failed", true, None)
                .map(|_id| ())
        };
        ensure!(result.is_err());
        invariants(&conn)?;
        ensure!(
            get_thread(&conn, thread)?
                .context("original thread")?
                .reply_count
                == 0
        );
        ensure!(db::get_post_submission(&conn, "failed", "actor", board)?.is_none());
        ensure!(count_threads_for_board(&conn, board)? == 1);
        ensure!(db::get_posts_for_thread(&conn, thread)?.len() == 1);
    }
    conn.execute_batch("DROP TRIGGER reject_commit; DROP TABLE commit_failure;")?;
    create_reply_with_thread_update(&conn, &post(board, thread), "failed", true, None)
        .map(|_id| ())?;
    invariants(&conn)?;
    Ok(())
}

#[test]
fn posting_busy_error_is_retryable_and_does_not_leave_a_partial_post() -> Result<()> {
    let (pool, board, thread) = fixture()?;
    let writer = pool.get()?;
    writer.execute_batch("BEGIN IMMEDIATE")?;
    let client = pool.get()?;
    let error = create_reply_with_thread_update(&client, &post(board, thread), "busy", true, None)
        .err()
        .context("posting must respect the other writer")?;
    ensure!(matches!(AppError::from(error), AppError::DbBusy));
    invariants(&client)?;
    writer.execute_batch("ROLLBACK")?;
    create_reply_with_thread_update(&client, &post(board, thread), "busy", true, None)
        .map(|_id| ())?;
    ensure!(get_thread(&client, thread)?.context("thread")?.reply_count == 1);
    invariants(&client)?;
    Ok(())
}

#[test]
fn self_actions_use_current_permission_inside_the_write_transaction() -> Result<()> {
    let (pool, board, thread) = fixture()?;
    let conn = pool.get()?;
    conn.execute(
        "UPDATE boards SET allow_editing=1,allow_self_delete=1 WHERE id=?1",
        [board],
    )
    .map(|_rows| ())?;
    let old_policy = db::get_board_by_short(&conn, "life")?.context("board")?;
    ensure!(old_policy.allow_editing && old_policy.allow_self_delete);
    let reply = create_reply_with_thread_update(&conn, &post(board, thread), "own", true, None)?;
    conn.execute(
        "UPDATE boards SET allow_editing=0,allow_self_delete=0 WHERE id=?1",
        [board],
    )
    .map(|_rows| ())?;
    let current_policy = |validation_conn: &rusqlite::Connection| -> crate::error::Result<()> {
        if validation_conn.is_autocommit() {
            return Err(AppError::Internal(anyhow::anyhow!(
                "permission check ran outside the write transaction"
            )));
        }
        let current = db::get_board_by_short(validation_conn, "life")?.context("board")?;
        if !current.allow_editing || !current.allow_self_delete {
            return Err(AppError::Forbidden("permission revoked".into()));
        }
        Ok(())
    };
    let edited = db::posts::edit_post_with_validation(
        &conn,
        reply,
        "secret",
        "changed",
        "changed",
        60,
        current_policy,
    );
    ensure!(matches!(
        edited
            .err()
            .and_then(|error| error.downcast::<AppError>().ok()),
        Some(AppError::Forbidden(_))
    ));
    let deleted =
        db::posts::self_delete_post_with_validation(&conn, reply, "secret", 60, current_policy);
    ensure!(matches!(deleted, Err(AppError::Forbidden(_))));
    ensure!(db::get_post(&conn, reply)?.context("preserved reply")?.body == "lifecycle reply");
    invariants(&conn)?;
    Ok(())
}

#[test]
fn sequence_of_mutations_preserves_invariants_and_deleted_search_state() -> Result<()> {
    let (pool, board, thread) = fixture()?;
    let conn = pool.get()?;
    let mut random = 47_u64;
    for step in 0_i32..160_i32 {
        random = random
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1);
        match random % 6 {
            0 => set_thread_locked(&conn, thread, true)?,
            1 => set_thread_locked(&conn, thread, false)?,
            2 => {
                let mut reply = post(board, thread);
                reply.file_path = Some(format!("life/{step}.png"));
                let result = create_reply_with_thread_update(
                    &conn,
                    &reply,
                    &format!("sequence-{step}"),
                    true,
                    None,
                );
                if let Err(error) = result {
                    ensure!(error.downcast_ref::<ThreadClosed>() == Some(&ThreadClosed::Locked));
                }
            }
            3 => {
                if let Some(reply) = db::get_posts_for_thread(&conn, thread)?
                    .iter()
                    .find(|reply| !reply.is_op)
                {
                    db::delete_post(&conn, reply.id).map(|_cleanup| ())?;
                }
            }
            4 => set_thread_sticky(&conn, thread, random.is_multiple_of(2))?,
            _ => {
                let result = create_reply_with_thread_update(
                    &conn,
                    &post(board, thread),
                    &format!("sequence-{step}"),
                    false,
                    None,
                );
                if let Err(error) = result {
                    ensure!(error.downcast_ref::<ThreadClosed>() == Some(&ThreadClosed::Locked));
                }
            }
        }
        invariants(&conn)?;
    }
    delete_thread(&conn, thread).map(|_cleanup| ())?;
    ensure!(db::search_posts(&conn, board, "lifecycle", 100, 0)?.is_empty());
    invariants(&conn)?;
    Ok(())
}
