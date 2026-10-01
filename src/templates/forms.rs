//! HTML form fragments injected into board and thread pages.
//!
//! These are not full pages: they produce `<div>` snippets that board and
//! thread templates embed inside their layouts.

use crate::config::CONFIG;
use crate::models::Board;
use crate::utils::sanitize::escape_html;
use std::fmt::Write as _;

/// User-entered fields preserved when a post form must be rendered again.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PostFormState {
    /// Poster name field.
    pub name: String,
    /// Thread subject field.
    pub subject: String,
    /// Post body field.
    pub body: String,
    /// Whether the reply should avoid bumping its thread.
    pub sage: bool,
    /// Poll draft entered for a new thread.
    pub poll: Option<PollFormState>,
    /// Whether a rejected request contained attachments that must be reselected.
    pub had_attachments: bool,
    /// Whether parsing stopped before every submitted control could be captured.
    pub incomplete: bool,
}

/// Poll controls preserved independently of normalized server-side validation.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PollFormState {
    /// Poll question field.
    pub question: String,
    /// Nonempty poll option fields, in submitted order.
    pub options: Vec<String>,
    /// Duration number as entered by the user.
    pub duration_value: String,
    /// Duration unit selected by the user.
    pub duration_unit: String,
}

/// Renders posting-only recovery advice and a copyable draft when available.
#[must_use]
pub fn post_error_recovery_body(message: &str, prefill: Option<&PostFormState>) -> String {
    let draft = prefill.map_or_else(String::new, |state| {
        let poll = state.poll.as_ref().map_or_else(String::new, |poll| {
            let mut options = String::new();
            for (index, option) in poll.options.iter().enumerate() {
                // Writing formatted text to a String cannot fail.
                let _ = write!(options, "<p>Option {}: {}</p>", index + 1, escape_html(option));
            }
            format!("<p>Poll: {}</p>{options}<p>Duration: {} {}</p>",
                escape_html(&poll.question), escape_html(&poll.duration_value), escape_html(&poll.duration_unit))
        });
        let partial = if state.incomplete { "<p>Only controls received before the upload stopped are saved below. Check your name, subject, body, sage and poll options before retrying.</p>" } else { "" };
        format!(r#"{partial}<div class="post-form-container"><div class="post-form-title">[ saved draft — copy before leaving ]</div>
<div class="post-form"><p>Name: {name}</p><p>Subject: {subject}</p>
<label for="post-recovery-body">Draft body</label><textarea id="post-recovery-body" rows="6" readonly>{body}</textarea>
<p>Sage: {sage}</p>{poll}</div></div>"#,
            name = escape_html(&state.name), subject = escape_html(&state.subject), body = escape_html(&state.body),
            sage = if state.sage { "yes" } else { "no" })
    });
    format!(
        r#"<div class="page-box error-page"><h1>Post could not be completed</h1><p>{message}</p>
<p>Copy the saved draft below before leaving. Your browser’s Back button may restore the original form, but some browsers clear it. Open a fresh posting form and paste your saved text if needed. Choose attachments again; browsers cannot restore file selections on a new page.</p>
<p>If the result is uncertain, check the thread or board for your post before submitting again.</p>{draft}<p><a href="/">return home</a></p></div>"#,
        message = escape_html(message)
    )
}

/// Upload capabilities needed to choose the form's media controls.
struct UploadFormPolicy {
    /// Whether the board accepts at least one upload type.
    uploads_enabled: bool,
}

/// Creates the opaque token used to reject duplicate form submissions.
fn new_submission_token() -> String {
    crate::utils::crypto::random_hex(16)
}

/// Renders the initially hidden upload progress row.
const fn upload_progress_row() -> &'static str {
    r#"    <tr class="upload-progress-row" hidden>
        <td>upload</td>
        <td>
          <div class="compress-progress upload-progress-wrap" style="display:block;margin:0">
            <div class="compress-progress-track" aria-hidden="true"><div class="compress-progress-bar upload-progress-bar" style="width:0%"></div></div>
            <div class="compress-progress-text upload-progress-text" role="status" aria-live="polite" aria-atomic="true">Preparing upload…</div>
          </div>
        </td></tr>"#
}

/// MIME types and extensions accepted by the audio input.
const AUDIO_ACCEPT: &str =
    "audio/mpeg,audio/mp3,audio/ogg,application/ogg,audio/oga,audio/opus,audio/flac,audio/x-flac,audio/wav,audio/wave,audio/x-wav,audio/vnd.wave,audio/mp4,audio/m4a,audio/x-m4a,audio/aac,audio/x-aac,audio/webm,.mp3,.ogg,.oga,.opus,.flac,.wav,.m4a,.aac,.webm";
/// MIME types and extensions accepted by the video input.
const VIDEO_ACCEPT: &str = "video/mp4,video/webm,video/x-matroska,video/matroska,.mp4,.webm,.mkv";
/// MIME types and extensions accepted by the image input.
const IMAGE_ACCEPT: &str =
    "image/jpeg,image/png,image/gif,image/webp,image/heic,image/heif,.heic,.heif";
/// Maximum number of characters in a poll option.
const POLL_OPTION_MAX_LENGTH: usize = 200;
/// Maximum number of options in a poll.
const POLL_OPTION_MAX_COUNT: usize = 20;

/// Derives upload-control visibility from the board configuration.
fn build_upload_form_policy(board: &Board) -> UploadFormPolicy {
    let allow_any_files = CONFIG.enable_any_file_uploads_feature && board.allow_any_files;

    let uploads_enabled = board.allow_images
        || board.allow_audio
        || board.allow_video
        || board.allow_pdf
        || allow_any_files;

    UploadFormPolicy { uploads_enabled }
}

/// Wraps explanatory copy in the standard form-help element.
fn form_hint(text: &str) -> String {
    format!(r#"<span class="form-field-help">{text}</span>"#)
}

/// Renders the row shown when all upload types are disabled.
const fn render_uploads_disabled_row() -> &'static str {
    r#"    <tr><td>uploads</td>
        <td><span class="post-form-mobile-label">Uploads</span><span class="form-field-help">uploads are disabled on this board</span></td></tr>"#
}

/// Renders a CAPTCHA challenge row with board-specific element identifiers.
fn render_captcha_row(board_short: &str, reply_suffix: &str, refresh_href: &str) -> String {
    let captcha_id = crate::captcha::new_captcha_id();
    let board = escape_html(board_short);
    let image_src = format!("/captcha/{captcha_id}?board={board}");
    let answer_id = format!("captcha-answer-{board}{reply_suffix}");
    let (refresh_path, fragment) = refresh_href
        .split_once('#')
        .map_or((refresh_href, None), |(path, fragment)| {
            (path, Some(fragment))
        });
    let query_separator = if refresh_path.contains('?') { '&' } else { '?' };
    let refresh_href = format!(
        "{refresh_path}{query_separator}captcha_refresh={captcha_id}{}",
        fragment.map_or_else(String::new, |value| format!("#{value}"))
    );
    format!(
        r#"    <tr id="captcha-row-{board}{suffix}"><td><label for="{answer_id}">captcha</label></td>
        <td>
          <label class="post-form-mobile-label" for="{answer_id}">Captcha</label>
          <div class="captcha-challenge">
            <img class="captcha-image" src="{image_src}" alt="CAPTCHA challenge image" width="220" height="120">
            <a class="form-field-help captcha-refresh-link" href="{refresh_href}">new challenge</a>
          </div>
          <input type="hidden" name="captcha_id" value="{captcha_id}">
          <input type="text" id="{answer_id}" name="captcha_answer" autocomplete="off" autocapitalize="characters" spellcheck="false" maxlength="16" required>
          <span class="form-field-help">Enter the text shown in the image. If it expires or fails, request a new challenge.</span>
          <noscript><span class="form-field-help">A new challenge reloads this page. Copy your unsent text and poll choices first; choose attachments again after reloading.</span></noscript>
        </td></tr>"#,
        board = board,
        suffix = reply_suffix,
        answer_id = escape_html(&answer_id),
        image_src = escape_html(&image_src),
        captcha_id = escape_html(&captcha_id),
        refresh_href = escape_html(&refresh_href),
    )
}

/// Renders one numbered poll-option input row.
fn render_poll_option_row(option_number: usize, value: &str) -> String {
    format!(
        r#"<div class="poll-option-row"><input type="text" class="poll-option-input" name="poll_option" value="{value}" aria-label="poll option {option_number}" placeholder="Option {option_number}" maxlength="{POLL_OPTION_MAX_LENGTH}"><button type="button" class="poll-remove-btn" data-action="remove-poll-option" aria-label="Remove poll option" hidden>✕</button></div>"#,
        value = escape_html(value),
    )
}

/// Image, video, audio, PDF, and generic upload limits in mebibytes.
type UploadSizeLimitsMb = (usize, usize, usize, usize, usize);

/// Converts a board's byte limits to the mebibyte values displayed by forms.
fn upload_size_limits_mb(board: &Board) -> UploadSizeLimitsMb {
    (
        board.max_image_size_bytes() / 1024 / 1024,
        board.max_video_size_bytes() / 1024 / 1024,
        board.max_audio_size_bytes() / 1024 / 1024,
        board.max_pdf_size_bytes() / 1024 / 1024,
        board.max_generic_upload_size_bytes() / 1024 / 1024,
    )
}

/// Builds the `accept` value and explanatory copy for a combined file input.
fn single_upload_accept_and_hint(
    board: &Board,
    allow_any_files: bool,
    limits: UploadSizeLimitsMb,
) -> (String, String) {
    let (image_mb, video_mb, audio_mb, pdf_mb, generic_upload_mb) = limits;
    let mut accept_parts: Vec<&str> = Vec::new();
    let mut hint_parts: Vec<String> = Vec::new();

    if board.allow_images {
        accept_parts.push(IMAGE_ACCEPT);
        hint_parts.push(format!("jpg/png/gif/webp/heic · max {image_mb} MiB"));
    }
    if board.allow_video {
        accept_parts.push(VIDEO_ACCEPT);
        hint_parts.push(format!("mp4/webm/mkv · max {video_mb} MiB"));
    }
    if board.allow_audio {
        accept_parts.push(AUDIO_ACCEPT);
        hint_parts.push(format!(
            "mp3/ogg/oga/opus/flac/wav/m4a/aac/webm · max {audio_mb} MiB"
        ));
    }
    if board.allow_pdf {
        accept_parts.push("application/pdf,.pdf");
        hint_parts.push(format!("pdf · max {pdf_mb} MiB"));
    }
    match (board.allow_images, board.allow_video) {
        (true, true) => hint_parts.push("oversized images/videos can auto-compress".to_owned()),
        (true, false) => hint_parts.push("oversized images can auto-compress".to_owned()),
        (false, true) => hint_parts.push("oversized videos can auto-compress".to_owned()),
        (false, false) => {}
    }

    let file_accept = if allow_any_files {
        String::new()
    } else {
        accept_parts.join(",")
    };
    let file_hint = if allow_any_files && hint_parts.is_empty() {
        format!("other files download safely as attachments · max {generic_upload_mb} MiB")
    } else if allow_any_files {
        format!(
            "{} &nbsp;|&nbsp; other files download safely as attachments",
            hint_parts.join(" &nbsp;|&nbsp; ")
        )
    } else {
        hint_parts.join(" &nbsp;|&nbsp; ")
    };
    (file_accept, file_hint)
}

/// Renders the upload controls used when one primary file input is sufficient.
fn render_single_upload_row(board: &Board, audio_image_hint: &str) -> String {
    let limits = upload_size_limits_mb(board);
    let (image_mb, _, audio_mb, _, _) = limits;
    let allow_any_files = CONFIG.enable_any_file_uploads_feature && board.allow_any_files;
    let audio_image_dual_mode = board.allow_audio
        && board.allow_images
        && !board.allow_video
        && !board.allow_pdf
        && !allow_any_files;
    let (file_accept, file_hint) = single_upload_accept_and_hint(board, allow_any_files, limits);

    let optional_image_row = if audio_image_dual_mode {
        format!(
            r#"<details class="upload-secondary-toggle">
              <summary aria-label="Show optional image upload">▾ Optional Image</summary>
              <div class="upload-secondary-panel">
                <label class="upload-secondary-label" for="post-form-image-file">optional image</label>
                <input type="file" id="post-form-image-file" name="image_file" data-onchange-check-size="1" accept="{IMAGE_ACCEPT}">
                <span class="form-field-help">{audio_image_hint} · jpg/png/gif/webp/heic · max {image_mb} MiB · oversized images can auto-compress</span>
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
    let primary_id = if primary_name == "audio_file" {
        "post-form-audio-file"
    } else {
        "post-form-file"
    };
    let primary_accept = if audio_image_dual_mode {
        AUDIO_ACCEPT.to_owned()
    } else {
        file_accept
    };
    let primary_hint = if audio_image_dual_mode {
        format!("mp3/ogg/oga/opus/flac/wav/m4a/aac/webm · max {audio_mb} MiB")
    } else {
        file_hint
    };

    let mobile_label = if primary_name == "audio_file" {
        "Audio"
    } else {
        "Upload"
    };

    format!(
        r#"    <tr><td><label for="{primary_id}">{primary_label}</label></td>
        <td><label class="post-form-mobile-label" for="{primary_id}">{mobile_label}</label><input type="file" id="{primary_id}" name="{primary_name}" data-onchange-check-size="1" accept="{primary_accept}">
            {primary_hint_html}
            {optional_image_row}</td></tr>"#,
        primary_id = primary_id,
        mobile_label = mobile_label,
        primary_hint_html = form_hint(&primary_hint),
    )
}

/// Renders the shared poll creator, including native additional-option controls.
fn render_poll_creator(poll: Option<&PollFormState>) -> String {
    let visible_option_count = poll.map_or(2, |state| {
        state.options.len().clamp(2, POLL_OPTION_MAX_COUNT)
    });
    let poll_option_rows: String = (1..=visible_option_count)
        .map(|number| {
            render_poll_option_row(
                number,
                poll.and_then(|state| state.options.get(number - 1))
                    .map_or("", String::as_str),
            )
        })
        .collect();
    let extra_poll_option_rows: String = (visible_option_count + 1..=POLL_OPTION_MAX_COUNT)
        .map(|number| render_poll_option_row(number, ""))
        .collect();
    let poll_question = poll.map_or("", |state| state.question.as_str());
    let poll_open =
        if poll.is_some_and(|state| !state.question.is_empty() || !state.options.is_empty()) {
            " open"
        } else {
            ""
        };
    let duration_value = poll.map_or("24", |state| state.duration_value.as_str());
    let duration_unit = poll.map_or("hours", |state| state.duration_unit.as_str());
    format!(
        r#"    <tr class="poll-row">
        <td colspan="2">
        <span class="post-form-mobile-label">Poll</span>
        <details class="poll-creator"{poll_open}>
          <summary>[ 📊 Add a Poll to this thread ]</summary>
          <div class="poll-creator-inner">
            <div class="poll-creator-row">
              <label>Question<input type="text" name="poll_question" value="{poll_question}" placeholder="What do you think?" maxlength="500"></label>
            </div>
            <div id="poll-options-list" data-poll-option-maxlength="{poll_option_max_length}" data-poll-option-maxcount="{poll_option_max_count}">
              {poll_option_rows}
              <noscript><details><summary>More poll options</summary>{extra_poll_option_rows}</details></noscript>
            </div>
            <button type="button" class="poll-add-btn" data-action="add-poll-option">+ Add Option</button>
            <div class="poll-creator-row poll-duration-row">
              <label>Duration
                <input type="number" name="poll_duration_value" value="{duration_value}" min="1" max="720" class="poll-duration-input">
                <select name="poll_duration_unit" class="poll-duration-unit">
                  <option value="hours"{hours_selected}>Hours</option>
                  <option value="minutes"{minutes_selected}>Minutes</option>
                  <option value="days"{days_selected}>Days</option>
                </select>
              </label>
            </div>
          </div>
        </details>
        </td></tr>"#,
        poll_option_max_length = POLL_OPTION_MAX_LENGTH,
        poll_option_max_count = POLL_OPTION_MAX_COUNT,
        poll_option_rows = poll_option_rows,
        poll_question = escape_html(poll_question),
        duration_value = escape_html(duration_value),
        hours_selected = if duration_unit == "hours" {
            " selected"
        } else {
            ""
        },
        minutes_selected = if duration_unit == "minutes" {
            " selected"
        } else {
            ""
        },
        days_selected = if duration_unit == "days" {
            " selected"
        } else {
            ""
        },
    )
}

/// New-thread submission form. Embedded on board index and catalog pages.
pub(super) fn new_thread_form(
    board_short: &str,
    csrf_token: &str,
    board: &Board,
    prefill: Option<&PostFormState>,
    refresh_href: &str,
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
        render_uploads_disabled_row().to_owned()
    };

    let captcha_row = if board.allow_captcha {
        render_captcha_row(board_short, "", refresh_href)
    } else {
        String::new()
    };

    let poll_creator = render_poll_creator(prefill.and_then(|state| state.poll.as_ref()));
    let attachment_recovery = attachment_recovery_row(prefill);
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
    <tr><td><label for="thread-name">name</label></td>
        <td><label class="post-form-mobile-label" for="thread-name">Name</label><input type="text" id="thread-name" name="name" value="{name_value}" placeholder="Anonymous" maxlength="64"></td></tr>
    <tr><td><label for="thread-subject">subject</label></td>
        <td><label class="post-form-mobile-label" for="thread-subject">Subject</label><input type="text" id="thread-subject" name="subject" value="{subject_value}" maxlength="128">
            <button type="submit">post thread</button></td></tr>
    <tr><td><label for="thread-body">body</label></td>
        <td><label class="post-form-mobile-label" for="thread-body">Body</label><textarea id="thread-body" name="body" rows="5" maxlength="4096">{body_value}</textarea>
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
    {attachment_recovery}
    {upload_progress_row}
    {captcha_row}
    {poll_creator}
  </table>
</form>
</div>
"#,
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
    )
}

/// Explains the browser file-input restriction after a rejected native submission.
fn attachment_recovery_row(prefill: Option<&PostFormState>) -> &'static str {
    if prefill.is_some_and(|state| state.had_attachments) {
        r#"<tr><td>attachments</td><td><span class="form-field-help">Choose your attachments again. Your browser cannot restore file selections after this page reload.</span></td></tr>"#
    } else {
        ""
    }
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
        render_uploads_disabled_row().to_owned()
    };

    let captcha_row = if board.allow_captcha {
        render_captcha_row(
            board_short,
            "-reply",
            &format!("/{board_short}/thread/{thread_id}#post-form-wrap"),
        )
    } else {
        String::new()
    };
    let attachment_recovery = attachment_recovery_row(prefill);
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
    <tr><td><label for="reply-name">name</label></td>
        <td><label class="post-form-mobile-label" for="reply-name">Name</label><input type="text" id="reply-name" name="name" value="{name_value}" placeholder="Anonymous" maxlength="64"></td></tr>
    <tr><td><label for="reply-body">body</label></td>
        <td><label class="post-form-mobile-label" for="reply-body">Body</label><textarea id="reply-body" name="body" rows="4" maxlength="4096">{body_value}</textarea>
            <button type="submit">post reply</button></td></tr>
    {uploads_disabled_row}
    {upload_row}
    {attachment_recovery}
    {upload_progress_row}
    <tr><td>options</td>
        <td><span class="post-form-mobile-label">Options</span><label class="sage-label"><input type="checkbox" name="sage" value="1"{sage_checked}> sage <span class="sage-hint">(don&apos;t bump thread)</span></label></td></tr>
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
        build_upload_form_policy, new_thread_form, render_captcha_row, render_poll_option_row,
        reply_form, PollFormState, PostFormState, AUDIO_ACCEPT, POLL_OPTION_MAX_COUNT,
        POLL_OPTION_MAX_LENGTH,
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
        let html = new_thread_form("test", "csrf", &uploads_disabled_board(), None, "/test");
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
        let html = new_thread_form("test", "csrf", &audio_image_board(), None, "/test");
        let audio_pos = html.find("name=\"audio_file\"");
        let image_pos = html.find("name=\"image_file\"");
        assert!(audio_pos.is_some(), "audio row should be present");
        assert!(image_pos.is_some(), "image row should be present");
        assert!(audio_pos < image_pos);
        assert!(html.contains(r#"<td><label for="post-form-audio-file">audio</label></td>"#));
        assert!(html.contains("Optional Image"));
        assert!(html.contains("optional cover image for the audio post"));
        assert!(html.contains("image/heic"));
        assert!(html.contains(".heic"));
        assert!(html.contains(&format!("accept=\"{AUDIO_ACCEPT}\"")));
        assert!(html.contains("mp3/ogg/oga/opus/flac/wav/m4a/aac/webm · max"));
        assert!(
            !html.contains(
                "jpg/png/gif/webp/heic · max 8 MiB &nbsp;|&nbsp; mp3/ogg/oga/opus/flac/wav/m4a/aac/webm"
            )
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
            "/test",
        );
        assert!(html.contains("<td>upload</td>"));
        assert!(html.contains("name=\"file\""));
        assert!(!html.contains("name=\"audio_file\""));
        assert!(!html.contains("name=\"image_file\""));
    }

    #[test]
    fn post_forms_include_submission_token() {
        let board = uploads_disabled_board();
        let thread_html = new_thread_form("test", "csrf", &board, None, "/test");
        let reply_html = reply_form("test", 42, "csrf", &board, None);

        assert!(thread_html.contains("name=\"submission_token\""));
        assert!(reply_html.contains("name=\"submission_token\""));
    }

    #[test]
    fn poll_option_rows_share_the_same_max_length() {
        let initial_row = render_poll_option_row(1, "");
        assert!(initial_row.contains(&format!(r#"maxlength="{POLL_OPTION_MAX_LENGTH}""#)));

        let html = new_thread_form("test", "csrf", &uploads_disabled_board(), None, "/test");
        assert!(html.contains(&format!(
            r#"data-poll-option-maxlength="{POLL_OPTION_MAX_LENGTH}""#
        )));
        assert!(html.contains(&format!(
            r#"data-poll-option-maxcount="{POLL_OPTION_MAX_COUNT}""#
        )));
        assert_eq!(
            html.matches(r#"class="poll-option-input""#).count(),
            POLL_OPTION_MAX_COUNT
        );
    }

    #[test]
    fn poll_creator_is_wrapped_in_a_valid_table_row() {
        let html = new_thread_form("test", "csrf", &uploads_disabled_board(), None, "/test");

        assert!(html.contains(
            r#"<tr class="poll-row">
        <td colspan="2">
        <span class="post-form-mobile-label">Poll</span>"#,
        ));
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
            ..PostFormState::default()
        };
        let thread_html = new_thread_form("test", "csrf", &board, Some(&state), "/test");
        let reply_html = reply_form("test", 42, "csrf", &board, Some(&state));

        assert!(thread_html.contains(r#"<label for="thread-name">name</label>"#));
        assert!(thread_html.contains(r#"id="thread-name" name="name" value="anon""#));
        assert!(thread_html.contains(r#"<label for="thread-subject">subject</label>"#));
        assert!(thread_html.contains(r#"id="thread-subject" name="subject" value="subject""#));
        assert!(thread_html.contains(">draft body</textarea>"));
        assert!(!thread_html.contains(r#"name="deletion_token""#));

        assert!(reply_html.contains(r#"<label for="reply-name">name</label>"#));
        assert!(reply_html.contains(r#"id="reply-name" name="name" value="anon""#));
        assert!(reply_html.contains(">draft body</textarea>"));
        assert!(!reply_html.contains(r#"name="deletion_token""#));
        assert!(reply_html.contains(r#"name="sage" value="1" checked"#));
    }

    #[test]
    fn rejected_thread_retains_escaped_poll_controls_and_extra_options() {
        let state = PostFormState {
            poll: Some(PollFormState {
                question: "<question> & 日本語".into(),
                options: vec!["First".into(), "<second>".into(), "Third".into()],
                duration_value: "3".into(),
                duration_unit: "days".into(),
            }),
            ..PostFormState::default()
        };
        let html = new_thread_form(
            "test",
            "csrf",
            &uploads_disabled_board(),
            Some(&state),
            "/test",
        );

        assert!(html.contains(r#"class="poll-creator" open"#));
        assert!(html.contains(r#"name="poll_question" value="&lt;question&gt; &amp; 日本語""#));
        assert!(html.contains(r#"name="poll_option" value="&lt;second&gt;""#));
        assert!(html.contains(r#"name="poll_duration_value" value="3""#));
        assert!(html.contains(r#"<option value="days" selected>Days</option>"#));
        let third_option = html.find(r#"name="poll_option" value="Third""#);
        assert!(third_option.is_some());
        assert!(third_option < html.find("<noscript><details>"));
        assert_eq!(
            html.matches(r#"class="poll-option-input""#).count(),
            POLL_OPTION_MAX_COUNT
        );
    }

    #[test]
    fn unavailable_posting_page_preserves_an_escaped_copyable_draft() {
        let state = PostFormState {
            name: "<name>".into(),
            body: format!("</textarea><{}>lost draft</{}>", "script", "script"),
            sage: true,
            ..PostFormState::default()
        };
        let html = super::post_error_recovery_body("Thread <missing>", Some(&state));
        assert!(html.contains("Thread &lt;missing&gt;"));
        assert!(html.contains("Name: &lt;name&gt;"));
        assert!(html.contains("Sage: yes"));
        assert!(html.contains(
            "readonly>&lt;/textarea&gt;&lt;script&gt;lost draft&lt;/script&gt;</textarea>"
        ));
        assert!(!html.contains("<script>"));
        assert!(!html.contains("<form"));
    }

    #[test]
    fn rejected_attachment_hint_only_appears_when_files_were_submitted() {
        let board = audio_image_board();
        let state = PostFormState {
            had_attachments: true,
            ..PostFormState::default()
        };
        let hint = "Choose your attachments again. Your browser cannot restore file selections";

        assert!(new_thread_form("test", "csrf", &board, Some(&state), "/test").contains(hint));
        assert!(reply_form("test", 42, "csrf", &board, Some(&state)).contains(hint));
        assert!(!new_thread_form("test", "csrf", &board, None, "/test").contains(hint));
        assert!(!reply_form("test", 42, "csrf", &board, None).contains(hint));
    }

    #[test]
    fn captcha_row_uses_server_side_image_challenge() {
        let html = new_thread_form(
            "test",
            "csrf",
            &crate::models::Board {
                allow_captcha: true,
                ..uploads_disabled_board()
            },
            None,
            "/test",
        );

        assert!(html.contains("name=\"captcha_id\""));
        assert!(html.contains("name=\"captcha_answer\""));
        assert!(html.contains("/captcha/"));
        assert!(html.contains("?captcha_refresh="));
        assert!(html.contains("new challenge"));
        assert!(!html.contains("pow_nonce"));
    }

    #[test]
    fn captcha_refresh_query_precedes_reply_form_fragment() {
        let html = render_captcha_row("test", "-reply", "/test/thread/7#post-form-wrap");

        assert!(html.contains("/test/thread/7?captcha_refresh="));
        assert!(html.contains("#post-form-wrap\">new challenge</a>"));
    }
    #[test]
    fn posting_error_marks_partial_capture_without_promising_history_recovery() {
        let state = PostFormState {
            body: "received draft".into(),
            incomplete: true,
            ..Default::default()
        };
        let html = super::post_error_recovery_body("Upload stopped.", Some(&state));
        assert!(html.contains("Only controls received before the upload stopped"));
        assert!(html.contains("some browsers clear it"));
        assert!(html.contains("received draft"));
    }
}
