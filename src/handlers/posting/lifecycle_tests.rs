//! Shared posting-service regressions including staged media and mutable policy.

use super::tests::{reply_command, single_connection_state, temp_upload, thread_command};
use super::*;
use anyhow::{ensure, Context as _};

#[test]
fn committed_reply_replays_after_lock_or_archive_and_deleted_retry_does_not_repost(
) -> anyhow::Result<()> {
    for archived in [false, true] {
        let state = crate::test_support::app_state();
        let uploads = tempfile::tempdir()?;
        let path = uploads.path().to_str().context("upload path")?;
        db::create_board(&*state.db.get()?, "test", "Test", "", false).map(|_board| ())?;
        let op = submit_post(
            &state.db,
            &state.job_queue,
            thread_command("test", "op", "op", path),
        )?;
        let reply = submit_post(
            &state.db,
            &state.job_queue,
            reply_command("test", op.thread_id, "reply", "reply", path),
        )?;
        {
            let conn = state.db.get()?;
            if archived {
                db::set_thread_archived(&conn, op.thread_id, true)?;
            } else {
                db::set_thread_locked(&conn, op.thread_id, true)?;
            }
        }
        let retry = submit_post(
            &state.db,
            &state.job_queue,
            reply_command(
                "test",
                op.thread_id,
                "reply",
                "different content ignored",
                path,
            ),
        )?;
        ensure!(retry.post_id == reply.post_id && retry.redirect_url == reply.redirect_url);
        db::delete_post(&*state.db.get()?, reply.post_id).map(|_cleanup| ())?;
        ensure!(matches!(
            submit_post(
                &state.db,
                &state.job_queue,
                reply_command("test", op.thread_id, "reply", "retry", path)
            ),
            Err(AppError::NotFound(_))
        ));
        db::delete_thread(&*state.db.get()?, op.thread_id).map(|_cleanup| ())?;
        ensure!(matches!(
            submit_post(
                &state.db,
                &state.job_queue,
                thread_command("test", "op", "retry", path)
            ),
            Err(AppError::NotFound(_))
        ));
        let conn = state.db.get()?;
        ensure!(db::get_all_boards(&conn)?
            .iter()
            .all(
                |board| db::count_threads_for_board(&conn, board.id).is_ok_and(|count| count == 0)
            ));
        ensure!(db::get_post(&conn, op.post_id)?.is_none());
    }
    Ok(())
}

#[test]
fn reply_deleted_during_preparation_returns_not_found_and_cleans_staged_media() -> anyhow::Result<()>
{
    let state = single_connection_state()?;
    let uploads = tempfile::tempdir()?;
    let path = uploads.path().to_str().context("upload path")?;
    db::create_board(&*state.db.get()?, "test", "Test", "", false).map(|_board| ())?;
    let op = submit_post(
        &state.db,
        &state.job_queue,
        thread_command("test", "op", "op", path),
    )?;
    let mut command = reply_command("test", op.thread_id, "deleted", "saved draft", path);
    command.file_data = Some(temp_upload("image.png", &tests::one_pixel_png()?)?);
    let result = submit_post_with_preparation(&state.db, &state.job_queue, command, || {
        db::delete_thread(&*state.db.get()?, op.thread_id).map(|_cleanup| ())?;
        Ok(())
    });
    ensure!(matches!(result, Err(AppError::NotFound(_))));
    let conn = state.db.get()?;
    ensure!(conn.is_autocommit());
    ensure!(db::get_post_submission(&conn, "deleted", "actor", 1)?.is_none());
    ensure!(tests::pending_upload_stage_count(uploads.path())? == 0);
    Ok(())
}

#[test]
fn failure_to_schedule_required_media_rolls_back_post_and_staged_files() -> anyhow::Result<()> {
    let state = single_connection_state()?;
    let uploads = tempfile::tempdir()?;
    let path = uploads.path().to_str().context("upload path")?;
    {
        let conn = state.db.get()?;
        db::create_board(&conn, "test", "Test", "", false).map(|_board| ())?;
        conn.execute("UPDATE boards SET allow_audio=1", [])
            .map(|_rows| ())?;
        conn.execute_batch("CREATE TRIGGER reject_media BEFORE INSERT ON background_jobs WHEN NEW.job_type='audio_waveform' BEGIN SELECT RAISE(ABORT, 'injected media failure'); END;")?;
    }
    let mut command = thread_command("test", "media", "", path);
    command.file_data = Some(temp_upload(
        "tone.flac",
        include_bytes!("../../../tests/fixtures/media/tone.flac"),
    )?);
    ensure!(submit_post(&state.db, &state.job_queue, command).is_err());
    let conn = state.db.get()?;
    let rows: i64 = conn.query_row("SELECT (SELECT COUNT(*) FROM posts)+(SELECT COUNT(*) FROM threads)+(SELECT COUNT(*) FROM post_submissions)+(SELECT COUNT(*) FROM pending_fs_ops)", [], |row| row.get(0))?;
    ensure!(rows == 0 && conn.is_autocommit());
    ensure!(tests::pending_upload_stage_count(uploads.path())? == 0);
    Ok(())
}

#[test]
fn auxiliary_spam_job_failure_cannot_turn_a_committed_post_into_failure() -> anyhow::Result<()> {
    let state = single_connection_state()?;
    let uploads = tempfile::tempdir()?;
    let path = uploads.path().to_str().context("upload path")?;
    {
        let conn = state.db.get()?;
        db::create_board(&conn, "test", "Test", "", false).map(|_board| ())?;
        conn.execute_batch("CREATE TRIGGER reject_spam BEFORE INSERT ON background_jobs WHEN NEW.job_type='spam_check' BEGIN SELECT RAISE(ABORT, 'injected spam failure'); END;")?;
    }
    let accepted = submit_post(
        &state.db,
        &state.job_queue,
        thread_command("test", "committed", "accepted", path),
    )?;
    let conn = state.db.get()?;
    ensure!(
        db::get_post(&conn, accepted.post_id)?
            .context("accepted post")?
            .created_at
            == accepted.created_at
    );
    ensure!(conn.is_autocommit());
    Ok(())
}

#[test]
fn lost_admin_session_is_rechecked_before_cooldown_bypass() -> anyhow::Result<()> {
    let state = single_connection_state()?;
    let uploads = tempfile::tempdir()?;
    let path = uploads.path().to_str().context("upload path")?;
    db::create_board(&*state.db.get()?, "test", "Test", "", false).map(|_board| ())?;
    let op = submit_post(
        &state.db,
        &state.job_queue,
        thread_command("test", "op", "op", path),
    )?;
    {
        let conn = state.db.get()?;
        conn.execute("UPDATE boards SET post_cooldown_secs=60", [])
            .map(|_rows| ())?;
        conn.execute(
            "INSERT INTO admin_users(username,password_hash) VALUES('admin','test')",
            [],
        )
        .map(|_rows| ())?;
        conn.execute("INSERT INTO admin_sessions(id,admin_id,expires_at) VALUES('session',last_insert_rowid(),unixepoch()+3600)", []).map(|_rows| ())?;
    }
    let mut command = reply_command("test", op.thread_id, "lost-session", "reply", path);
    command.admin_session_id = Some("session".into());
    let result = submit_post_with_preparation(&state.db, &state.job_queue, command, || {
        state
            .db
            .get()?
            .execute("DELETE FROM admin_sessions", [])
            .map(|_rows| ())?;
        Ok(())
    });
    ensure!(matches!(result, Err(AppError::BadRequest(_))));
    ensure!(db::get_posts_for_thread(&*state.db.get()?, op.thread_id)?.len() == 1);
    Ok(())
}
