// templates/forms.rs
//
// HTML form fragments injected into board and thread pages.
// These are not full pages — they produce <div>…</div> snippets that
// board.rs and thread.rs embed inside their own layouts.

use crate::config::CONFIG;
use crate::models::Board;
use crate::utils::sanitize::escape_html;

/// New-thread submission form. Embedded on board index and catalog pages.
#[allow(clippy::too_many_lines)]
pub(super) fn new_thread_form(board_short: &str, csrf_token: &str, board: &Board) -> String {
    let image_mb = CONFIG.max_image_size / 1024 / 1024;
    let video_mb = CONFIG.max_video_size / 1024 / 1024;
    let audio_mb = CONFIG.max_audio_size / 1024 / 1024;

    // PoW CAPTCHA block — only rendered when the board has it enabled.
    // FIX[NEW-H1]: PoW config is passed via data-pow-board / data-pow-difficulty
    // attributes so main.js can start the solver without any inline <script>.
    let captcha_row = if board.allow_captcha {
        let difficulty: u32 = crate::utils::crypto::POW_DIFFICULTY;
        format!(
            r#"    <tr id="captcha-row-{b}"><td>captcha</td>
        <td>
          <span id="captcha-status-{b}" style="font-size:0.8rem;color:var(--text-dim)">solving proof-of-work… (this takes a moment)</span>
          <input type="hidden" name="pow_nonce" id="pow-nonce-{b}" value=""
                 data-pow-board="{b}" data-pow-difficulty="{diff}">
        </td></tr>"#,
            b = escape_html(board_short),
            diff = difficulty,
        )
    } else {
        String::new()
    };

    // FIX[NEW-H1]: captcha JS block removed — logic lives in /static/main.js.

    // Secondary audio input — shown only when the board allows both images and audio.
    let audio_combo_row = if board.allow_images && board.allow_audio {
        format!(
            r#"    <tr><td>audio<br><span style="font-size:0.65rem;color:var(--text-dim)">(+ image)</span></td>
        <td><input type="file" name="audio_file" accept="audio/mpeg,audio/ogg,audio/flac,audio/wav,audio/mp4,audio/aac,audio/webm,.mp3,.ogg,.flac,.wav,.m4a,.aac">
            <span style="font-size:0.72rem;color:var(--text-dim)">optional audio alongside image · max {audio_mb} MiB</span></td></tr>"#,
        )
    } else {
        String::new()
    };

    let edit_token_row = if board.allow_editing {
        r#"    <tr><td>edit token</td>
        <td><input type="text" name="deletion_token" placeholder="optional — lets you edit post" maxlength="64"><span style="font-size:0.72rem;color:var(--text-dim)"> keep it secret</span></td></tr>"#
    } else {
        ""
    };

    format!(
        r#"<div class="post-form-container">
<div class="post-form-title">[ new thread ]</div>
<form class="post-form" method="POST" action="/{board}" enctype="multipart/form-data">
  <input type="hidden" name="_csrf" value="{csrf}">
  <table>
    <tr><td>name</td>
        <td><input type="text" name="name" placeholder="Anonymous" maxlength="64"></td></tr>
    <tr><td>subject</td>
        <td><input type="text" name="subject" maxlength="128">
            <button type="submit">post thread</button></td></tr>
    <tr><td>body</td>
        <td><textarea name="body" rows="5" maxlength="4096"></textarea>
            <div class="markup-hint">
              <span title="Greentext">&#62;green</span>
              <span title="Bold">**bold**</span>
              <span title="Italic">__italic__</span>
              <span title="Spoiler">[spoiler]text[/spoiler]</span>
              <span title="Reply">&gt;&gt;123</span>
              <span title="Cross-thread">&gt;&gt;&gt;/b/123</span>
              <span title="Emoji">:fire:</span>
            </div>
        </td></tr>
    <tr><td>file</td>
        <td><input type="file" name="file" data-onchange-check-size="1" accept="image/jpeg,image/png,image/gif,image/webp,video/mp4,video/webm,audio/mpeg,audio/ogg,audio/flac,audio/wav,audio/mp4,audio/aac,audio/webm,.mp3,.ogg,.flac,.wav,.m4a,.aac">
            <span style="font-size:0.72rem;color:var(--text-dim)">jpg/png/gif/webp · max {image_mb} MiB &nbsp;|&nbsp; mp4/webm · max {video_mb} MiB &nbsp;|&nbsp; mp3/ogg/flac/wav/m4a · max {audio_mb} MiB</span></td></tr>
    {audio_combo_row}
    {edit_token_row}
    {captcha_row}
        <td colspan="2">
        <details class="poll-creator">
          <summary>[ 📊 Add a Poll to this thread ]</summary>
          <div class="poll-creator-inner">
            <div class="poll-creator-row">
              <!-- FIX[F-T1]: maxlength matches server limit of 500 chars (was 256) -->
              <label>Question<input type="text" name="poll_question" placeholder="What do you think?" maxlength="500"></label>
            </div>
            <div id="poll-options-list">
              <!-- FIX[F-T1]: maxlength matches server limit of 200 chars (was 128) -->
              <div class="poll-option-row"><input type="text" name="poll_option" placeholder="Option 1" maxlength="200"><button type="button" class="poll-remove-btn" data-action="remove-poll-option" style="display:none">✕</button></div>
              <div class="poll-option-row"><input type="text" name="poll_option" placeholder="Option 2" maxlength="200"><button type="button" class="poll-remove-btn" data-action="remove-poll-option" style="display:none">✕</button></div>
            </div>
            <button type="button" class="poll-add-btn" data-action="add-poll-option">+ Add Option</button>
            <div class="poll-creator-row poll-duration-row">
              <label>Duration
                <input type="number" name="poll_duration_value" value="24" min="1" max="720" class="poll-duration-input">
                <!-- FIX[F-T2]: Added Days option — server now accepts "days" unit -->
                <select name="poll_duration_unit" class="poll-duration-unit">
                  <option value="hours">Hours</option>
                  <option value="minutes">Minutes</option>
                  <option value="days">Days</option>
                </select>
              </label>
            </div>
          </div>
        </details>
        </td></tr>
  </table>
</form>
</div>
"#,
        // FIX[NEW-H1]: poll scripts moved to /static/main.js
        board = escape_html(board_short),
        csrf = escape_html(csrf_token),
        image_mb = image_mb,
        video_mb = video_mb,
        audio_mb = audio_mb,
        audio_combo_row = audio_combo_row,
        edit_token_row = edit_token_row,
        captcha_row = captcha_row,
    )
}

/// Reply form injected into thread pages.
pub(super) fn reply_form(
    board_short: &str,
    thread_id: i64,
    csrf_token: &str,
    board: &Board,
) -> String {
    let image_mb = CONFIG.max_image_size / 1024 / 1024;
    let video_mb = CONFIG.max_video_size / 1024 / 1024;
    let audio_mb = CONFIG.max_audio_size / 1024 / 1024;

    // Build the accept attribute and hint based on which media types are enabled.
    let mut accept_parts: Vec<&str> = Vec::new();
    let mut hint_parts: Vec<String> = Vec::new();
    if board.allow_images {
        accept_parts.push("image/jpeg,image/png,image/gif,image/webp");
        hint_parts.push(format!("jpg/png/gif/webp · max {image_mb} MiB"));
    }
    if board.allow_video {
        accept_parts.push("video/mp4,video/webm");
        hint_parts.push(format!("mp4/webm · max {video_mb} MiB"));
    }
    if board.allow_audio {
        accept_parts.push("audio/mpeg,audio/ogg,audio/flac,audio/wav,audio/mp4,audio/aac,audio/webm,.mp3,.ogg,.flac,.wav,.m4a,.aac");
        hint_parts.push(format!("mp3/ogg/flac/wav/m4a · max {audio_mb} MiB"));
    }
    let file_accept = accept_parts.join(",");
    let file_hint = hint_parts.join(" &nbsp;|&nbsp; ");

    // Secondary audio-alongside-image row.
    let audio_combo_row = if board.allow_images && board.allow_audio {
        format!(
            r#"    <tr><td>audio<br><span style="font-size:0.65rem;color:var(--text-dim)">(+ image)</span></td>
        <td><input type="file" name="audio_file" accept="audio/mpeg,audio/ogg,audio/flac,audio/wav,audio/mp4,audio/aac,audio/webm,.mp3,.ogg,.flac,.wav,.m4a,.aac">
            <span style="font-size:0.72rem;color:var(--text-dim)">optional audio alongside image · max {audio_mb} MiB</span></td></tr>"#,
        )
    } else {
        String::new()
    };

    let edit_token_row = if board.allow_editing {
        r#"    <tr><td>edit token</td>
        <td><input type="text" name="deletion_token" placeholder="optional — lets you edit post" maxlength="64"><span style="font-size:0.72rem;color:var(--text-dim)"> keep it secret</span></td></tr>"#
    } else {
        ""
    };

    // PoW CAPTCHA block — only rendered when the board has it enabled.
    let captcha_row = if board.allow_captcha {
        let difficulty: u32 = crate::utils::crypto::POW_DIFFICULTY;
        format!(
            r#"    <tr id="captcha-row-{b}-reply"><td>captcha</td>
        <td>
          <span id="captcha-status-{b}-reply" style="font-size:0.8rem;color:var(--text-dim)">solving proof-of-work… (this takes a moment)</span>
          <input type="hidden" name="pow_nonce" id="pow-nonce-{b}-reply" value=""
                 data-pow-board="{b}" data-pow-difficulty="{diff}">
        </td></tr>"#,
            b = escape_html(board_short),
            diff = difficulty,
        )
    } else {
        String::new()
    };

    format!(
        r#"<div class="post-form-container reply-form-container">
<div class="post-form-title">[ reply to thread ]</div>
<form class="post-form" method="POST" action="/{board}/thread/{tid}" enctype="multipart/form-data">
  <input type="hidden" name="_csrf" value="{csrf}">
  <table>
    <tr><td>name</td>
        <td><input type="text" name="name" placeholder="Anonymous" maxlength="64"></td></tr>
    <tr><td>body</td>
        <td><textarea id="reply-body" name="body" rows="4" maxlength="4096"></textarea>
            <button type="submit">post reply</button></td></tr>
    <tr><td>file</td>
        <td><input type="file" name="file" data-onchange-check-size="1" accept="{file_accept}">
            <span style="font-size:0.72rem;color:var(--text-dim)">{file_hint}</span></td></tr>
{audio_combo_row}    <tr><td>options</td>
        <td><label class="sage-label"><input type="checkbox" name="sage" value="1"> sage <span class="sage-hint">(don&apos;t bump thread)</span></label></td></tr>
    {edit_token_row}
    {captcha_row}
  </table>
</form>
</div>"#,
        board = escape_html(board_short),
        tid = thread_id,
        csrf = escape_html(csrf_token),
        file_accept = file_accept,
        file_hint = file_hint,
        audio_combo_row = audio_combo_row,
        edit_token_row = edit_token_row,
        captcha_row = captcha_row,
    )
}
