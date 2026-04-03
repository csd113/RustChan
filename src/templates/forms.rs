// templates/forms.rs
//
// HTML form fragments injected into board and thread pages.
// These are not full pages — they produce <div>…</div> snippets that
// board.rs and thread.rs embed inside their own layouts.

use crate::config::CONFIG;
use crate::models::Board;
use crate::utils::sanitize::escape_html;

struct UploadFormPolicy {
    uploads_enabled: bool,
}

const AUDIO_ACCEPT: &str =
    "audio/mpeg,audio/ogg,audio/flac,audio/wav,audio/mp4,audio/aac,audio/webm,.mp3,.ogg,.flac,.wav,.m4a,.aac";
const IMAGE_ACCEPT: &str = "image/jpeg,image/png,image/gif,image/webp";

fn build_upload_form_policy(board: &Board) -> UploadFormPolicy {
    let allow_any_files = CONFIG.enable_any_file_uploads_feature && board.allow_any_files;

    let uploads_enabled =
        board.allow_images || board.allow_audio || board.allow_video || allow_any_files;

    UploadFormPolicy { uploads_enabled }
}

fn render_single_upload_row(board: &Board, audio_image_hint: &str) -> String {
    let image_mb = CONFIG.max_image_size / 1024 / 1024;
    let video_mb = CONFIG.max_video_size / 1024 / 1024;
    let audio_mb = CONFIG.max_audio_size / 1024 / 1024;
    let allow_any_files = CONFIG.enable_any_file_uploads_feature && board.allow_any_files;
    let audio_image_dual_mode =
        board.allow_audio && board.allow_images && !board.allow_video && !allow_any_files;

    let mut accept_parts: Vec<&str> = Vec::new();
    let mut hint_parts: Vec<String> = Vec::new();

    if board.allow_images {
        accept_parts.push(IMAGE_ACCEPT);
        hint_parts.push(format!("jpg/png/gif/webp · max {image_mb} MiB"));
    }
    if board.allow_video {
        accept_parts.push("video/mp4,video/webm");
        hint_parts.push(format!("mp4/webm · max {video_mb} MiB"));
    }
    if board.allow_audio {
        accept_parts.push(AUDIO_ACCEPT);
        hint_parts.push(format!("mp3/ogg/flac/wav/m4a · max {audio_mb} MiB"));
    }

    let file_accept = if allow_any_files {
        String::new()
    } else {
        accept_parts.join(",")
    };
    let file_hint = if allow_any_files {
        if hint_parts.is_empty() {
            format!("other files download safely as attachments · max {video_mb} MiB")
        } else {
            format!(
                "{} &nbsp;|&nbsp; other files download safely as attachments",
                hint_parts.join(" &nbsp;|&nbsp; ")
            )
        }
    } else {
        hint_parts.join(" &nbsp;|&nbsp; ")
    };

    let optional_image_row = if audio_image_dual_mode {
        format!(
            r#"<details class="upload-secondary-toggle">
              <summary aria-label="Show optional image upload">▾ Optional Image</summary>
              <div class="upload-secondary-panel">
                <input type="file" name="image_file" data-onchange-check-size="1" accept="{IMAGE_ACCEPT}">
                <span style="font-size:0.72rem;color:var(--text-dim)">{audio_image_hint} · jpg/png/gif/webp · max {image_mb} MiB</span>
              </div>
            </details>"#
        )
    } else {
        String::new()
    };

    let primary_name = if audio_image_dual_mode {
        "audio_file"
    } else {
        "file"
    };
    let primary_label = if primary_name == "audio_file" {
        "audio"
    } else {
        "upload"
    };
    let primary_accept = if audio_image_dual_mode {
        AUDIO_ACCEPT.to_string()
    } else {
        file_accept
    };
    let primary_hint = if audio_image_dual_mode {
        format!("mp3/ogg/flac/wav/m4a · max {audio_mb} MiB")
    } else {
        file_hint
    };

    format!(
        r#"    <tr><td>{primary_label}</td>
        <td><input type="file" name="{primary_name}" data-onchange-check-size="1" accept="{primary_accept}">
            <span style="font-size:0.72rem;color:var(--text-dim)">{primary_hint}</span>
            {optional_image_row}</td></tr>"#,
    )
}

/// New-thread submission form. Embedded on board index and catalog pages.
#[allow(clippy::too_many_lines)]
pub(super) fn new_thread_form(board_short: &str, csrf_token: &str, board: &Board) -> String {
    let upload_policy = build_upload_form_policy(board);
    let upload_row = if upload_policy.uploads_enabled {
        render_single_upload_row(board, "optional cover image for the audio post")
    } else {
        String::new()
    };

    let uploads_disabled_row = if upload_policy.uploads_enabled {
        String::new()
    } else {
        r#"    <tr><td>uploads</td>
        <td><span style="font-size:0.8rem;color:var(--text-dim)">uploads are disabled on this board</span></td></tr>"#
            .to_string()
    };

    // PoW CAPTCHA block — only rendered when the board has it enabled.
    // PoW config is passed via data-pow-board / data-pow-difficulty
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

    // captcha JS block removed — logic lives in /static/main.js.

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
    {uploads_disabled_row}
    {upload_row}
    {edit_token_row}
    {captcha_row}
        <td colspan="2">
        <details class="poll-creator">
          <summary>[ 📊 Add a Poll to this thread ]</summary>
          <div class="poll-creator-inner">
            <div class="poll-creator-row">
              <!-- maxlength matches server limit of 500 chars (was 256) -->
              <label>Question<input type="text" name="poll_question" placeholder="What do you think?" maxlength="500"></label>
            </div>
            <div id="poll-options-list">
              <!-- maxlength matches server limit of 200 chars (was 128) -->
              <div class="poll-option-row"><input type="text" name="poll_option" placeholder="Option 1" maxlength="200"><button type="button" class="poll-remove-btn" data-action="remove-poll-option" style="display:none">✕</button></div>
              <div class="poll-option-row"><input type="text" name="poll_option" placeholder="Option 2" maxlength="200"><button type="button" class="poll-remove-btn" data-action="remove-poll-option" style="display:none">✕</button></div>
            </div>
            <button type="button" class="poll-add-btn" data-action="add-poll-option">+ Add Option</button>
            <div class="poll-creator-row poll-duration-row">
              <label>Duration
                <input type="number" name="poll_duration_value" value="24" min="1" max="720" class="poll-duration-input">
                <!-- Added Days option — server now accepts "days" unit -->
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
        // poll scripts moved to /static/main.js
        board = escape_html(board_short),
        csrf = escape_html(csrf_token),
        uploads_disabled_row = uploads_disabled_row,
        upload_row = upload_row,
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
    let upload_policy = build_upload_form_policy(board);
    let upload_row = if upload_policy.uploads_enabled {
        render_single_upload_row(board, "optional cover image for the audio reply")
    } else {
        String::new()
    };

    let uploads_disabled_row = if upload_policy.uploads_enabled {
        String::new()
    } else {
        r#"    <tr><td>uploads</td>
        <td><span style="font-size:0.8rem;color:var(--text-dim)">uploads are disabled on this board</span></td></tr>"#
            .to_string()
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
    {uploads_disabled_row}
    {upload_row}
    <tr><td>options</td>
        <td><label class="sage-label"><input type="checkbox" name="sage" value="1"> sage <span class="sage-hint">(don&apos;t bump thread)</span></label></td></tr>
    {edit_token_row}
    {captcha_row}
  </table>
</form>
</div>"#,
        board = escape_html(board_short),
        tid = thread_id,
        csrf = escape_html(csrf_token),
        uploads_disabled_row = uploads_disabled_row,
        upload_row = upload_row,
        edit_token_row = edit_token_row,
        captcha_row = captcha_row,
    )
}

#[cfg(test)]
mod tests {
    use super::{build_upload_form_policy, new_thread_form, reply_form};

    fn uploads_disabled_board() -> crate::models::Board {
        crate::models::Board {
            id: 1,
            short_name: "test".to_string(),
            name: "Test".to_string(),
            description: String::new(),
            nsfw: false,
            max_threads: 100,
            max_archived_threads: 150,
            bump_limit: 500,
            allow_images: false,
            allow_video: false,
            allow_audio: false,
            allow_any_files: false,
            allow_tripcodes: true,
            edit_window_secs: 0,
            allow_editing: false,
            allow_archive: true,
            allow_video_embeds: false,
            allow_captcha: false,
            show_poster_ids: false,
            post_cooldown_secs: 0,
            created_at: 0,
        }
    }

    fn audio_image_board() -> crate::models::Board {
        crate::models::Board {
            allow_images: true,
            allow_audio: true,
            ..uploads_disabled_board()
        }
    }

    #[test]
    fn upload_policy_marks_disabled_board_as_non_uploadable() {
        let policy = build_upload_form_policy(&uploads_disabled_board());
        assert!(!policy.uploads_enabled);
    }

    #[test]
    fn new_thread_form_hides_file_input_when_uploads_disabled() {
        let html = new_thread_form("test", "csrf", &uploads_disabled_board());
        assert!(!html.contains("type=\"file\" name=\"file\""));
        assert!(!html.contains("name=\"image_file\""));
        assert!(!html.contains("name=\"audio_file\""));
        assert!(html.contains("uploads are disabled on this board"));
    }

    #[test]
    fn reply_form_hides_file_input_when_uploads_disabled() {
        let html = reply_form("test", 42, "csrf", &uploads_disabled_board());
        assert!(!html.contains("type=\"file\" name=\"file\""));
        assert!(!html.contains("name=\"image_file\""));
        assert!(!html.contains("name=\"audio_file\""));
        assert!(html.contains("uploads are disabled on this board"));
    }

    #[test]
    fn audio_image_form_is_audio_first_and_cover_image_second() {
        let html = new_thread_form("test", "csrf", &audio_image_board());
        let audio_pos = html.find("name=\"audio_file\"").expect("audio row");
        let image_pos = html.find("name=\"image_file\"").expect("image row");
        assert!(audio_pos < image_pos);
        assert!(html.contains("<td>audio</td>"));
        assert!(html.contains("Optional Image"));
        assert!(html.contains("optional cover image for the audio post"));
        assert!(html.contains("accept=\"audio/mpeg,audio/ogg,audio/flac,audio/wav,audio/mp4,audio/aac,audio/webm,.mp3,.ogg,.flac,.wav,.m4a,.aac\""));
        assert!(html.contains("mp3/ogg/flac/wav/m4a · max"));
        assert!(!html.contains("jpg/png/gif/webp · max 8 MiB &nbsp;|&nbsp; mp3/ogg/flac/wav/m4a"));
        assert!(!html.contains("video/mp4,video/webm"));
        assert!(!html.contains("name=\"file\""));
    }

    #[test]
    fn mixed_media_form_uses_single_upload_input() {
        let html = new_thread_form(
            "test",
            "csrf",
            &crate::models::Board {
                allow_images: true,
                allow_video: true,
                allow_audio: true,
                ..uploads_disabled_board()
            },
        );
        assert!(html.contains("<td>upload</td>"));
        assert!(html.contains("name=\"file\""));
        assert!(!html.contains("name=\"audio_file\""));
        assert!(!html.contains("name=\"image_file\""));
    }
}
