// HTML form fragments injected into board and thread pages.
// These are not full pages — they produce <div>…</div> snippets that
// board.rs and thread.rs embed inside their own layouts.

use crate::config::CONFIG;
use crate::models::Board;
use crate::utils::sanitize::escape_html;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PostFormState {
    pub name: String,
    pub subject: String,
    pub body: String,
    pub sage: bool,
}

struct UploadFormPolicy {
    uploads_enabled: bool,
}

fn new_submission_token() -> String {
    crate::utils::crypto::random_hex(16)
}

const fn upload_progress_row() -> &'static str {
    r#"    <tr class="upload-progress-row" hidden>
        <td>upload</td>
        <td>
          <div class="compress-progress upload-progress-wrap" style="display:block;margin:0">
            <div class="compress-progress-track"><div class="compress-progress-bar upload-progress-bar" style="width:0%"></div></div>
            <div class="compress-progress-text upload-progress-text">Preparing upload…</div>
          </div>
        </td></tr>"#
}

const AUDIO_ACCEPT: &str =
    "audio/mpeg,audio/ogg,audio/flac,audio/wav,audio/mp4,audio/aac,audio/webm,.mp3,.ogg,.flac,.wav,.m4a,.aac";
const IMAGE_ACCEPT: &str =
    "image/jpeg,image/png,image/gif,image/webp,image/heic,image/heif,.heic,.heif";
const POLL_OPTION_MAX_LENGTH: usize = 200;
const POLL_OPTION_MAX_COUNT: usize = 20;

fn build_upload_form_policy(board: &Board) -> UploadFormPolicy {
    let allow_any_files = CONFIG.enable_any_file_uploads_feature && board.allow_any_files;

    let uploads_enabled =
        board.allow_images || board.allow_audio || board.allow_video || allow_any_files;

    UploadFormPolicy { uploads_enabled }
}

fn form_hint(text: &str) -> String {
    format!(r#"<span class="form-field-help">{text}</span>"#)
}

const fn render_uploads_disabled_row() -> &'static str {
    r#"    <tr><td>uploads</td>
        <td><span class="form-field-help">uploads are disabled on this board</span></td></tr>"#
}

fn render_captcha_row(board_short: &str, reply_suffix: &str) -> String {
    let difficulty: u32 = crate::utils::crypto::POW_DIFFICULTY;
    format!(
        r#"    <tr id="captcha-row-{board}{suffix}"><td>captcha</td>
        <td>
          <span id="captcha-status-{board}{suffix}" class="form-field-help">waiting for the JavaScript proof-of-work solver…</span>
          <noscript><div class="form-field-help">This board&apos;s CAPTCHA is solved in JavaScript. Enable JavaScript and wait for the checkmark before posting.</div></noscript>
          <input type="hidden" name="pow_nonce" id="pow-nonce-{board}{suffix}" value=""
                 data-pow-board="{board}" data-pow-difficulty="{difficulty}">
        </td></tr>"#,
        board = escape_html(board_short),
        suffix = reply_suffix,
        difficulty = difficulty,
    )
}

fn render_poll_option_row(option_number: usize) -> String {
    format!(
        r#"<div class="poll-option-row"><input type="text" class="poll-option-input" name="poll_option" placeholder="Option {option_number}" maxlength="{POLL_OPTION_MAX_LENGTH}"><button type="button" class="poll-remove-btn" data-action="remove-poll-option" aria-label="Remove poll option" hidden>✕</button></div>"#
    )
}

fn render_single_upload_row(board: &Board, audio_image_hint: &str) -> String {
    let image_mb = CONFIG.max_image_size / 1024 / 1024;
    let video_mb = CONFIG.max_video_size / 1024 / 1024;
    let audio_mb = CONFIG.max_audio_size / 1024 / 1024;
    let generic_upload_mb = CONFIG
        .max_image_size
        .max(CONFIG.max_video_size)
        .max(CONFIG.max_audio_size)
        / 1024
        / 1024;
    let allow_any_files = CONFIG.enable_any_file_uploads_feature && board.allow_any_files;
    let audio_image_dual_mode =
        board.allow_audio && board.allow_images && !board.allow_video && !allow_any_files;

    let mut accept_parts: Vec<&str> = Vec::new();
    let mut hint_parts: Vec<String> = Vec::new();

    if board.allow_images {
        accept_parts.push(IMAGE_ACCEPT);
        hint_parts.push(format!("jpg/png/gif/webp/heic · max {image_mb} MiB"));
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
            format!("other files download safely as attachments · max {generic_upload_mb} MiB")
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
                <span class="form-field-help">{audio_image_hint} · jpg/png/gif/webp/heic · max {image_mb} MiB</span>
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
            {primary_hint_html}
            {optional_image_row}</td></tr>"#,
        primary_hint_html = form_hint(&primary_hint),
    )
}

/// New-thread submission form. Embedded on board index and catalog pages.
#[allow(clippy::too_many_lines)]
pub(super) fn new_thread_form(
    board_short: &str,
    csrf_token: &str,
    board: &Board,
    prefill: Option<&PostFormState>,
) -> String {
    let submission_token = new_submission_token();
    let upload_policy = build_upload_form_policy(board);
    let upload_row = if upload_policy.uploads_enabled {
        render_single_upload_row(board, "optional cover image for the audio post")
    } else {
        String::new()
    };

    let uploads_disabled_row = if upload_policy.uploads_enabled {
        String::new()
    } else {
        render_uploads_disabled_row().to_string()
    };

    // PoW CAPTCHA block — only rendered when the board has it enabled.
    // PoW config is passed via data-pow-board / data-pow-difficulty
    // attributes so main.js can start the solver without any inline <script>.
    let captcha_row = if board.allow_captcha {
        render_captcha_row(board_short, "")
    } else {
        String::new()
    };

    // captcha JS block removed — logic lives in /static/main.js.

    let poll_option_rows = [render_poll_option_row(1), render_poll_option_row(2)].concat();
    let name_value = prefill.map_or("", |state| state.name.as_str());
    let subject_value = prefill.map_or("", |state| state.subject.as_str());
    let body_value = prefill.map_or("", |state| state.body.as_str());

    format!(
        r#"<div class="post-form-container">
<div class="post-form-title">[ new thread ]</div>
<form class="post-form" method="POST" action="/{board}" enctype="multipart/form-data">
  <input type="hidden" name="_csrf" value="{csrf}">
  <input type="hidden" name="submission_token" value="{submission_token}">
  <table>
    <tr><td>name</td>
        <td><input type="text" name="name" value="{name_value}" placeholder="Anonymous" maxlength="64"></td></tr>
    <tr><td>subject</td>
        <td><input type="text" name="subject" value="{subject_value}" maxlength="128">
            <button type="submit">post thread</button></td></tr>
    <tr><td>body</td>
        <td><textarea name="body" rows="5" maxlength="4096">{body_value}</textarea>
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
    {upload_progress_row}
    {captcha_row}
        <td colspan="2">
        <details class="poll-creator">
          <summary>[ 📊 Add a Poll to this thread ]</summary>
          <div class="poll-creator-inner">
            <div class="poll-creator-row">
              <label>Question<input type="text" name="poll_question" placeholder="What do you think?" maxlength="500"></label>
            </div>
            <div id="poll-options-list" data-poll-option-maxlength="{poll_option_max_length}" data-poll-option-maxcount="{poll_option_max_count}">
              {poll_option_rows}
            </div>
            <button type="button" class="poll-add-btn" data-action="add-poll-option">+ Add Option</button>
            <div class="poll-creator-row poll-duration-row">
              <label>Duration
                <input type="number" name="poll_duration_value" value="24" min="1" max="720" class="poll-duration-input">
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
        submission_token = escape_html(&submission_token),
        name_value = escape_html(name_value),
        subject_value = escape_html(subject_value),
        body_value = escape_html(body_value),
        uploads_disabled_row = uploads_disabled_row,
        upload_row = upload_row,
        upload_progress_row = upload_progress_row(),
        captcha_row = captcha_row,
        poll_option_max_length = POLL_OPTION_MAX_LENGTH,
        poll_option_max_count = POLL_OPTION_MAX_COUNT,
        poll_option_rows = poll_option_rows,
    )
}

/// Reply form injected into thread pages.
pub(super) fn reply_form(
    board_short: &str,
    thread_id: i64,
    csrf_token: &str,
    board: &Board,
    prefill: Option<&PostFormState>,
) -> String {
    let submission_token = new_submission_token();
    let upload_policy = build_upload_form_policy(board);
    let upload_row = if upload_policy.uploads_enabled {
        render_single_upload_row(board, "optional cover image for the audio reply")
    } else {
        String::new()
    };

    let uploads_disabled_row = if upload_policy.uploads_enabled {
        String::new()
    } else {
        render_uploads_disabled_row().to_string()
    };

    // PoW CAPTCHA block — only rendered when the board has it enabled.
    let captcha_row = if board.allow_captcha {
        render_captcha_row(board_short, "-reply")
    } else {
        String::new()
    };
    let name_value = prefill.map_or("", |state| state.name.as_str());
    let body_value = prefill.map_or("", |state| state.body.as_str());
    let sage_checked = if prefill.is_some_and(|state| state.sage) {
        " checked"
    } else {
        ""
    };

    format!(
        r#"<div class="post-form-container reply-form-container">
<div class="post-form-title">[ reply to thread ]</div>
<form class="post-form" method="POST" action="/{board}/thread/{tid}" enctype="multipart/form-data">
  <input type="hidden" name="_csrf" value="{csrf}">
  <input type="hidden" name="submission_token" value="{submission_token}">
  <table>
    <tr><td>name</td>
        <td><input type="text" name="name" value="{name_value}" placeholder="Anonymous" maxlength="64"></td></tr>
    <tr><td>body</td>
        <td><textarea id="reply-body" name="body" rows="4" maxlength="4096">{body_value}</textarea>
            <button type="submit">post reply</button></td></tr>
    {uploads_disabled_row}
    {upload_row}
    {upload_progress_row}
    <tr><td>options</td>
        <td><label class="sage-label"><input type="checkbox" name="sage" value="1"{sage_checked}> sage <span class="sage-hint">(don&apos;t bump thread)</span></label></td></tr>
    {captcha_row}
  </table>
</form>
</div>"#,
        board = escape_html(board_short),
        tid = thread_id,
        csrf = escape_html(csrf_token),
        submission_token = escape_html(&submission_token),
        name_value = escape_html(name_value),
        body_value = escape_html(body_value),
        sage_checked = sage_checked,
        uploads_disabled_row = uploads_disabled_row,
        upload_row = upload_row,
        upload_progress_row = upload_progress_row(),
        captcha_row = captcha_row,
    )
}

#[cfg(test)]
mod tests {
    use super::{
        build_upload_form_policy, new_thread_form, render_poll_option_row, reply_form,
        PostFormState, POLL_OPTION_MAX_COUNT, POLL_OPTION_MAX_LENGTH,
    };

    fn uploads_disabled_board() -> crate::models::Board {
        crate::models::Board {
            allow_images: false,
            allow_video: false,
            allow_audio: false,
            ..crate::test_fixtures::sample_board()
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
        let html = new_thread_form("test", "csrf", &uploads_disabled_board(), None);
        assert!(!html.contains("type=\"file\" name=\"file\""));
        assert!(!html.contains("name=\"image_file\""));
        assert!(!html.contains("name=\"audio_file\""));
        assert!(html.contains("uploads are disabled on this board"));
    }

    #[test]
    fn reply_form_hides_file_input_when_uploads_disabled() {
        let html = reply_form("test", 42, "csrf", &uploads_disabled_board(), None);
        assert!(!html.contains("type=\"file\" name=\"file\""));
        assert!(!html.contains("name=\"image_file\""));
        assert!(!html.contains("name=\"audio_file\""));
        assert!(html.contains("uploads are disabled on this board"));
    }

    #[test]
    fn audio_image_form_is_audio_first_and_cover_image_second() {
        let html = new_thread_form("test", "csrf", &audio_image_board(), None);
        let audio_pos = html.find("name=\"audio_file\"").expect("audio row");
        let image_pos = html.find("name=\"image_file\"").expect("image row");
        assert!(audio_pos < image_pos);
        assert!(html.contains("<td>audio</td>"));
        assert!(html.contains("Optional Image"));
        assert!(html.contains("optional cover image for the audio post"));
        assert!(html.contains("image/heic"));
        assert!(html.contains(".heic"));
        assert!(html.contains("accept=\"audio/mpeg,audio/ogg,audio/flac,audio/wav,audio/mp4,audio/aac,audio/webm,.mp3,.ogg,.flac,.wav,.m4a,.aac\""));
        assert!(html.contains("mp3/ogg/flac/wav/m4a · max"));
        assert!(
            !html.contains("jpg/png/gif/webp/heic · max 8 MiB &nbsp;|&nbsp; mp3/ogg/flac/wav/m4a")
        );
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
            None,
        );
        assert!(html.contains("<td>upload</td>"));
        assert!(html.contains("name=\"file\""));
        assert!(!html.contains("name=\"audio_file\""));
        assert!(!html.contains("name=\"image_file\""));
    }

    #[test]
    fn post_forms_include_submission_token() {
        let board = uploads_disabled_board();
        let thread_html = new_thread_form("test", "csrf", &board, None);
        let reply_html = reply_form("test", 42, "csrf", &board, None);

        assert!(thread_html.contains("name=\"submission_token\""));
        assert!(reply_html.contains("name=\"submission_token\""));
    }

    #[test]
    fn poll_option_rows_share_the_same_max_length() {
        let initial_row = render_poll_option_row(1);
        assert!(initial_row.contains(&format!(r#"maxlength="{POLL_OPTION_MAX_LENGTH}""#)));

        let html = new_thread_form("test", "csrf", &uploads_disabled_board(), None);
        assert!(html.contains(&format!(
            r#"data-poll-option-maxlength="{POLL_OPTION_MAX_LENGTH}""#
        )));
        assert!(html.contains(&format!(
            r#"data-poll-option-maxcount="{POLL_OPTION_MAX_COUNT}""#
        )));
        assert_eq!(html.matches(r#"class="poll-option-input""#).count(), 2);
    }

    #[test]
    fn post_forms_preserve_submitted_text_state() {
        let board = crate::models::Board {
            allow_editing: true,
            ..uploads_disabled_board()
        };
        let state = PostFormState {
            name: "anon".into(),
            subject: "subject".into(),
            body: "draft body".into(),
            sage: true,
        };
        let thread_html = new_thread_form("test", "csrf", &board, Some(&state));
        let reply_html = reply_form("test", 42, "csrf", &board, Some(&state));

        assert!(thread_html.contains(r#"name="name" value="anon""#));
        assert!(thread_html.contains(r#"name="subject" value="subject""#));
        assert!(thread_html.contains(">draft body</textarea>"));
        assert!(!thread_html.contains(r#"name="deletion_token""#));

        assert!(reply_html.contains(r#"name="name" value="anon""#));
        assert!(reply_html.contains(">draft body</textarea>"));
        assert!(!reply_html.contains(r#"name="deletion_token""#));
        assert!(reply_html.contains(r#"name="sage" value="1" checked"#));
    }

    #[test]
    fn captcha_row_includes_noscript_guidance() {
        let html = new_thread_form(
            "test",
            "csrf",
            &crate::models::Board {
                allow_captcha: true,
                ..uploads_disabled_board()
            },
            None,
        );

        assert!(html.contains("waiting for the JavaScript proof-of-work solver"));
        assert!(html.contains("Enable JavaScript and wait for the checkmark before posting"));
    }
}
