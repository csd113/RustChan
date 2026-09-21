// Request handlers.

/// Original tracing target, retained for existing `RUST_LOG` filters.
const LOG_TARGET: &str = concat!(env!("CARGO_CRATE_NAME"), "::handlers");

pub(crate) mod admin;
pub(crate) mod banner;
pub(crate) mod board;
pub(crate) mod captcha;
pub(crate) mod favicon;
pub(crate) mod posting;
pub(crate) mod render;
pub(crate) mod setup;
pub(crate) mod thread;

// Shared multipart form parsing
// Both create_thread and post_reply parse the same multipart fields.
// This helper consolidates that duplicated logic into one place.

use crate::error::{AppError, Result};
use crate::middleware::validate_csrf;
use crate::workers::JobQueue;
use axum::{
    body::Body,
    extract::{Multipart, Request},
    middleware::Next,
    response::Response,
};
use futures::StreamExt as _;
use std::collections::HashSet;
use std::time::Duration;
use tokio::io::AsyncWriteExt as _;

/// MIME sniff bytes used by this handler.
const MIME_SNIFF_BYTES: usize = 512;
/// Text multipart field max bytes used by this handler.
const TEXT_MULTIPART_FIELD_MAX_BYTES: usize = 64 * 1024;
/// Unknown multipart field max bytes used by this handler.
const UNKNOWN_MULTIPART_FIELD_MAX_BYTES: usize = 64 * 1024;
// Public post uploads intentionally bypass Axum's global body cap so per-board
// limits above old defaults still work. This aggregate budget bounds the whole
// multipart stream after board settings are loaded.
/// Public multipart aggregate max bytes used by this handler.
const PUBLIC_MULTIPART_AGGREGATE_MAX_BYTES: usize = 512 * 1024 * 1024;
// Leave bounded room for boundaries and field headers without reducing the
// documented 512 MiB aggregate field-data allowance. The route-level limiter
// applies before Multer can buffer a malformed preamble or field-header block.
/// Maximum complete request body accepted by public multipart posting routes.
pub(crate) const PUBLIC_MULTIPART_REQUEST_MAX_BYTES: usize =
    PUBLIC_MULTIPART_AGGREGATE_MAX_BYTES + 4 * 1024 * 1024;
/// Maximum preamble or per-field header envelope retained by the multipart parser.
pub(crate) const PUBLIC_MULTIPART_ENVELOPE_MAX_BYTES: usize = 64 * 1024;
/// Maximum accepted multipart boundary length.
const PUBLIC_MULTIPART_BOUNDARY_MAX_BYTES: usize = 200;
/// Marker propagated through Axum's body error for envelope-limit classification.
const PUBLIC_MULTIPART_ENVELOPE_LIMIT_MARKER: &str = "rustchan multipart envelope limit";
// Caps field spam and duplicate-slot churn before bodies are streamed.
/// Public multipart max fields used by this handler.
const PUBLIC_MULTIPART_MAX_FIELDS: usize = 64;
// Conservative whole-request upload timeout. True min-rate enforcement would be
// more precise, but this avoids indefinite request-slot pinning without breaking
// normal large LAN uploads.
/// Public upload timeout used by this handler.
pub(crate) const PUBLIC_UPLOAD_TIMEOUT: Duration = Duration::from_mins(10);

/// Rejects malformed multipart preambles and field-header blocks before Multer
/// can retain a request-sized buffer while searching for their delimiters.
///
/// The wrapped body remains fully streaming: only parser state and the boundary
/// matcher are retained here, and field data is passed through unchanged.
pub(crate) async fn enforce_public_multipart_envelope(
    mut request: Request,
    next: Next,
) -> Result<Response> {
    let content_type = request
        .headers()
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| AppError::BadRequest("Multipart Content-Type is required.".into()))?;
    let boundary = parse_public_multipart_boundary(content_type).ok_or_else(|| {
        AppError::BadRequest("Multipart Content-Type has an invalid boundary.".into())
    })?;

    let body = std::mem::replace(request.body_mut(), Body::empty());
    let mut scanner = MultipartEnvelopeScanner::new(&boundary);
    let inspected = body.into_data_stream().map(move |result| match result {
        Ok(bytes) => scanner
            .inspect(&bytes)
            .map(|()| bytes)
            .map_err(std::io::Error::other),
        Err(error) => Err(std::io::Error::other(error)),
    });
    *request.body_mut() = Body::from_stream(inspected);
    Ok(next.run(request).await)
}

fn parse_public_multipart_boundary(content_type: &str) -> Option<Vec<u8>> {
    let segments = split_mime_parameters(content_type)?;
    if !segments
        .first()?
        .trim()
        .eq_ignore_ascii_case("multipart/form-data")
    {
        return None;
    }

    let mut boundary = None;
    for segment in segments.iter().skip(1) {
        let (name, raw_value) = segment.split_once('=')?;
        if !name.trim().eq_ignore_ascii_case("boundary") {
            continue;
        }
        if boundary.is_some() {
            return None;
        }
        boundary = Some(parse_mime_parameter_value(raw_value.trim())?);
    }

    let boundary = boundary?;
    let bytes = boundary.into_bytes();
    if bytes.is_empty()
        || bytes.len() > PUBLIC_MULTIPART_BOUNDARY_MAX_BYTES
        || bytes.last() == Some(&b' ')
        || !bytes.iter().all(|byte| matches!(*byte, 0x20..=0x7e))
    {
        return None;
    }
    Some(bytes)
}

/// Splits MIME parameters without treating semicolons inside quotes as separators.
fn split_mime_parameters(value: &str) -> Option<Vec<&str>> {
    let mut segments = Vec::new();
    let mut start = 0;
    let mut quoted = false;
    let mut escaped = false;
    for (index, byte) in value.bytes().enumerate() {
        if escaped {
            escaped = false;
            continue;
        }
        if quoted && byte == b'\\' {
            escaped = true;
        } else if byte == b'"' {
            quoted = !quoted;
        } else if byte == b';' && !quoted {
            segments.push(value.get(start..index)?);
            start = index.saturating_add(1);
        }
    }
    if quoted || escaped {
        return None;
    }
    segments.push(value.get(start..)?);
    Some(segments)
}

/// Decodes a token or quoted MIME parameter value.
fn parse_mime_parameter_value(raw: &str) -> Option<String> {
    if !raw.starts_with('"') {
        return (!raw.is_empty() && !raw.contains('"')).then(|| raw.to_owned());
    }

    let mut decoded = String::new();
    let mut escaped = false;
    let mut closing_index = None;
    for (offset, character) in raw.get(1..)?.char_indices() {
        if escaped {
            decoded.push(character);
            escaped = false;
        } else if character == '\\' {
            escaped = true;
        } else if character == '"' {
            closing_index = Some(offset.saturating_add(2));
            break;
        } else {
            decoded.push(character);
        }
    }
    let closing_index = closing_index?;
    if escaped || !raw.get(closing_index..)?.trim().is_empty() {
        return None;
    }
    Some(decoded)
}

/// Streaming state used to locate multipart field envelopes without retaining bodies.
struct MultipartEnvelopeScanner {
    /// Boundary marker that separates field data from the next envelope.
    field_boundary: Vec<u8>,
    /// Current multipart parsing stage.
    stage: MultipartEnvelopeStage,
}

impl MultipartEnvelopeScanner {
    fn new(boundary: &[u8]) -> Self {
        let mut first_boundary = Vec::with_capacity(boundary.len().saturating_add(2));
        first_boundary.extend_from_slice(b"--");
        first_boundary.extend_from_slice(boundary);

        let mut field_boundary = Vec::with_capacity(boundary.len().saturating_add(4));
        field_boundary.extend_from_slice(b"\r\n--");
        field_boundary.extend_from_slice(boundary);

        Self {
            field_boundary,
            stage: MultipartEnvelopeStage::FirstBoundary {
                matcher: BytePatternMatcher::new(first_boundary),
                bytes_seen: 0,
            },
        }
    }

    /// Inspects a streamed body chunk and retains no field data.
    fn inspect(&mut self, bytes: &[u8]) -> std::result::Result<(), String> {
        for byte in bytes {
            self.inspect_byte(*byte)?;
        }
        Ok(())
    }

    /// Advances the envelope state machine by one byte.
    fn inspect_byte(&mut self, byte: u8) -> std::result::Result<(), String> {
        let mut transition = None;
        match &mut self.stage {
            MultipartEnvelopeStage::FirstBoundary {
                matcher,
                bytes_seen,
            } => {
                *bytes_seen = bytes_seen.saturating_add(1);
                if *bytes_seen
                    > PUBLIC_MULTIPART_ENVELOPE_MAX_BYTES.saturating_add(matcher.pattern_len())
                {
                    return Err(format!(
                        "{PUBLIC_MULTIPART_ENVELOPE_LIMIT_MARKER}: preamble exceeds {PUBLIC_MULTIPART_ENVELOPE_MAX_BYTES} bytes"
                    ));
                }
                if matcher.feed(byte) {
                    transition = Some(MultipartEnvelopeStage::BoundarySuffix(
                        MultipartBoundarySuffix::Start,
                    ));
                }
            }
            MultipartEnvelopeStage::BoundarySuffix(suffix) => {
                transition = suffix.feed(byte)?;
            }
            MultipartEnvelopeStage::FieldHeaders {
                matcher,
                bytes_seen,
            } => {
                *bytes_seen = bytes_seen.saturating_add(1);
                if *bytes_seen > PUBLIC_MULTIPART_ENVELOPE_MAX_BYTES {
                    return Err(format!(
                        "{PUBLIC_MULTIPART_ENVELOPE_LIMIT_MARKER}: field headers exceed {PUBLIC_MULTIPART_ENVELOPE_MAX_BYTES} bytes"
                    ));
                }
                if matcher.feed(byte) {
                    transition = Some(MultipartEnvelopeStage::FieldData(BytePatternMatcher::new(
                        self.field_boundary.clone(),
                    )));
                }
            }
            MultipartEnvelopeStage::FieldData(matcher) => {
                if matcher.feed(byte) {
                    transition = Some(MultipartEnvelopeStage::BoundarySuffix(
                        MultipartBoundarySuffix::Start,
                    ));
                }
            }
            MultipartEnvelopeStage::Epilogue => {}
        }

        if let Some(next_stage) = transition {
            self.stage = next_stage;
        }
        Ok(())
    }
}

/// States in the multipart envelope scanner.
enum MultipartEnvelopeStage {
    /// Search for the first boundary while bounding any preamble.
    FirstBoundary {
        /// Incremental boundary matcher.
        matcher: BytePatternMatcher,
        /// Bytes observed before finding the boundary.
        bytes_seen: usize,
    },
    /// Validate the bytes following a boundary marker.
    BoundarySuffix(MultipartBoundarySuffix),
    /// Search for the CRLF pair terminating a field-header block.
    FieldHeaders {
        /// Incremental header terminator matcher.
        matcher: BytePatternMatcher,
        /// Bytes retained by Multer while seeking the terminator.
        bytes_seen: usize,
    },
    /// Stream field bytes while searching for the next boundary.
    FieldData(BytePatternMatcher),
    /// Closing boundary observed; remaining epilogue is irrelevant to field parsing.
    Epilogue,
}

/// State for bytes immediately following a multipart boundary.
enum MultipartBoundarySuffix {
    /// No suffix byte has been consumed.
    Start,
    /// One dash of a closing `--` suffix has been consumed.
    FinalDash,
    /// Optional transport padding is being consumed.
    Padding(usize),
    /// A carriage return was consumed and must be followed by a line feed.
    CarriageReturn,
}

impl MultipartBoundarySuffix {
    /// Consumes one suffix byte and returns a stage transition when complete.
    fn feed(&mut self, byte: u8) -> std::result::Result<Option<MultipartEnvelopeStage>, String> {
        match self {
            Self::Start => match byte {
                b'-' => *self = Self::FinalDash,
                b' ' | b'\t' => *self = Self::Padding(1),
                b'\r' => *self = Self::CarriageReturn,
                _ => return Err("malformed multipart boundary suffix".to_owned()),
            },
            Self::FinalDash => {
                if byte != b'-' {
                    return Err("malformed multipart closing boundary".to_owned());
                }
                return Ok(Some(MultipartEnvelopeStage::Epilogue));
            }
            Self::Padding(bytes_seen) => match byte {
                b' ' | b'\t' => {
                    *bytes_seen = bytes_seen.saturating_add(1);
                    if *bytes_seen > PUBLIC_MULTIPART_ENVELOPE_MAX_BYTES {
                        return Err(format!(
                            "{PUBLIC_MULTIPART_ENVELOPE_LIMIT_MARKER}: boundary padding exceeds {PUBLIC_MULTIPART_ENVELOPE_MAX_BYTES} bytes"
                        ));
                    }
                }
                b'\r' => *self = Self::CarriageReturn,
                _ => return Err("malformed multipart boundary padding".to_owned()),
            },
            Self::CarriageReturn => {
                if byte != b'\n' {
                    return Err("malformed multipart boundary line ending".to_owned());
                }
                return Ok(Some(MultipartEnvelopeStage::FieldHeaders {
                    matcher: BytePatternMatcher::new(b"\r\n\r\n".to_vec()),
                    bytes_seen: 0,
                }));
            }
        }
        Ok(None)
    }
}

/// Incremental Knuth-Morris-Pratt matcher for delimiters split across body frames.
struct BytePatternMatcher {
    /// Delimiter bytes.
    pattern: Vec<u8>,
    /// Failure-function prefix lengths.
    prefix: Vec<usize>,
    /// Bytes currently matched.
    matched: usize,
}

impl BytePatternMatcher {
    fn new(pattern: Vec<u8>) -> Self {
        debug_assert!(
            !pattern.is_empty(),
            "multipart delimiter patterns must not be empty"
        );
        let mut prefix = vec![0; pattern.len()];
        let mut matched = 0;
        for index in 1..pattern.len() {
            while matched > 0 && pattern.get(matched) != pattern.get(index) {
                matched = prefix
                    .get(matched.saturating_sub(1))
                    .copied()
                    .unwrap_or_default();
            }
            if pattern.get(matched) == pattern.get(index) {
                matched = matched.saturating_add(1);
            }
            if let Some(value) = prefix.get_mut(index) {
                *value = matched;
            }
        }
        Self {
            pattern,
            prefix,
            matched: 0,
        }
    }

    /// Returns the pattern length.
    const fn pattern_len(&self) -> usize {
        self.pattern.len()
    }

    /// Feeds one byte and returns true when the complete pattern is observed.
    fn feed(&mut self, byte: u8) -> bool {
        while self.matched > 0 && self.pattern.get(self.matched) != Some(&byte) {
            self.matched = self
                .prefix
                .get(self.matched.saturating_sub(1))
                .copied()
                .unwrap_or_default();
        }
        if self.pattern.get(self.matched) == Some(&byte) {
            self.matched = self.matched.saturating_add(1);
        }
        if self.matched == self.pattern.len() {
            self.matched = self
                .prefix
                .get(self.matched.saturating_sub(1))
                .copied()
                .unwrap_or_default();
            return true;
        }
        false
    }
}

fn multipart_read_error(
    context: &'static str,
    error: &axum::extract::multipart::MultipartError,
) -> AppError {
    let message = error.to_string();
    let lower = message.to_ascii_lowercase();
    if error_chain_contains(error, PUBLIC_MULTIPART_ENVELOPE_LIMIT_MARKER) {
        tracing::warn!(target: LOG_TARGET, context, error = %message, "multipart envelope exceeded parser limit");
        return AppError::UploadTooLarge("Multipart field envelope is too large.".into());
    }
    if lower.contains("body write aborted")
        || lower.contains("error reading a body")
        || lower.contains("connection")
        || lower.contains("early eof")
        || lower.contains("unexpected eof")
    {
        tracing::warn!(target: LOG_TARGET, context, error = %message, "client disconnected during multipart upload");
    } else {
        tracing::warn!(target: LOG_TARGET, context, error = %message, "multipart parsing failed");
    }
    AppError::BadRequest(message)
}

fn error_chain_contains(error: &(dyn std::error::Error + 'static), needle: &str) -> bool {
    let mut current = Some(error);
    while let Some(source) = current {
        if source.to_string().to_ascii_lowercase().contains(needle) {
            return true;
        }
        current = source.source();
    }
    false
}

#[derive(Default)]
struct PublicMultipartBudget {
    fields_seen: usize,
    bytes_seen: usize,
}

impl PublicMultipartBudget {
    fn note_field(&mut self) -> Result<()> {
        self.fields_seen = self.fields_seen.saturating_add(1);
        if self.fields_seen > PUBLIC_MULTIPART_MAX_FIELDS {
            return Err(AppError::BadRequest(
                "Multipart form contains too many fields.".into(),
            ));
        }
        Ok(())
    }

    fn note_chunk(&mut self, len: usize) -> Result<()> {
        self.bytes_seen = self.bytes_seen.saturating_add(len);
        if self.bytes_seen > PUBLIC_MULTIPART_AGGREGATE_MAX_BYTES {
            return Err(AppError::UploadTooLarge(
                "Multipart upload is too large.".into(),
            ));
        }
        Ok(())
    }
}

async fn read_text_field(
    mut field: axum::extract::multipart::Field<'_>,
    budget: &mut PublicMultipartBudget,
) -> Result<String> {
    let mut bytes = Vec::new();
    loop {
        let next_chunk = field
            .chunk()
            .await
            .map_err(|e| multipart_read_error("text field", &e))?;
        let Some(chunk) = next_chunk else {
            break;
        };
        budget.note_chunk(chunk.len())?;
        if bytes.len().saturating_add(chunk.len()) > TEXT_MULTIPART_FIELD_MAX_BYTES {
            tracing::warn!(target: LOG_TARGET,
                limit_bytes = TEXT_MULTIPART_FIELD_MAX_BYTES,
                "multipart text field exceeded parser limit"
            );
            return Err(AppError::UploadTooLarge(
                "Multipart text field is too large.".into(),
            ));
        }
        bytes.extend_from_slice(&chunk);
    }
    String::from_utf8(bytes)
        .map_err(|_error| AppError::BadRequest("Multipart text field is not valid UTF-8.".into()))
}

pub(crate) async fn discard_unknown_multipart_field(
    mut field: axum::extract::multipart::Field<'_>,
) -> Result<()> {
    let mut total = 0usize;
    loop {
        let next_chunk = field
            .chunk()
            .await
            .map_err(|e| multipart_read_error("unknown field", &e))?;
        let Some(chunk) = next_chunk else {
            break;
        };
        total = total.saturating_add(chunk.len());
        if total > UNKNOWN_MULTIPART_FIELD_MAX_BYTES {
            return Err(AppError::UploadTooLarge(
                "Unexpected multipart field is too large.".into(),
            ));
        }
    }
    Ok(())
}

async fn discard_unknown_public_multipart_field(
    mut field: axum::extract::multipart::Field<'_>,
    budget: &mut PublicMultipartBudget,
) -> Result<()> {
    let mut total = 0usize;
    loop {
        let next_chunk = field
            .chunk()
            .await
            .map_err(|e| multipart_read_error("unknown field", &e))?;
        let Some(chunk) = next_chunk else {
            break;
        };
        budget.note_chunk(chunk.len())?;
        total = total.saturating_add(chunk.len());
        if total > UNKNOWN_MULTIPART_FIELD_MAX_BYTES {
            return Err(AppError::UploadTooLarge(
                "Unexpected multipart field is too large.".into(),
            ));
        }
    }
    Ok(())
}

// Streaming multipart size limit
//
// Upload fields stream directly to disk and abort with HTTP 413 as soon as the
// running total exceeds the configured board limit.
//
// Text fields (CSRF token, post body, …) use the same chunked parser with a
// small fixed cap, so disabling Axum's route-level body limit for upload routes
// does not leave text fields unbounded.

async fn stream_field_to_temp_file(
    mut field: axum::extract::multipart::Field<'_>,
    max_bytes: usize,
    field_name: &'static str,
    budget: &mut PublicMultipartBudget,
    media_upload_gate: &crate::middleware::MediaUploadGate,
    media_upload_guard: &mut Option<crate::middleware::MediaUploadGuard>,
) -> Result<TempUpload> {
    let temp_file = tempfile::Builder::new()
        .prefix("rustchan-upload-")
        .tempfile()
        .map_err(|e| AppError::Internal(anyhow::anyhow!("Create temp upload file: {e}")))?;
    let std_file = temp_file
        .reopen()
        .map_err(|e| AppError::Internal(anyhow::anyhow!("Reopen temp upload file: {e}")))?;
    let mut file = tokio::fs::File::from_std(std_file);
    let mut sniff_bytes = Vec::with_capacity(MIME_SNIFF_BYTES);
    let mut size_bytes = 0usize;

    loop {
        let next_chunk = field
            .chunk()
            .await
            .map_err(|e| multipart_read_error(field_name, &e))?;
        let Some(chunk) = next_chunk else {
            break;
        };
        if !chunk.is_empty() && media_upload_guard.is_none() {
            // Acquire before the first upload byte is written. The owned guard
            // remains attached to the parsed form through blocking media work,
            // so concurrent requests cannot stage large files ahead of the
            // resource-intensive processing gate.
            *media_upload_guard = Some(media_upload_gate.try_begin()?);
        }
        budget.note_chunk(chunk.len())?;
        if size_bytes.saturating_add(chunk.len()) > max_bytes {
            tracing::warn!(target: LOG_TARGET,
                field = field_name,
                streamed_bytes = size_bytes,
                next_chunk_bytes = chunk.len(),
                limit_bytes = max_bytes,
                "multipart upload field exceeded board limit"
            );
            return Err(AppError::UploadTooLarge(format!(
                "File too large. Maximum upload size is {}.",
                format_upload_limit(max_bytes)
            )));
        }
        if sniff_bytes.len() < MIME_SNIFF_BYTES {
            let remaining = MIME_SNIFF_BYTES.saturating_sub(sniff_bytes.len());
            let take = remaining.min(chunk.len());
            if let Some(prefix) = chunk.get(..take) {
                sniff_bytes.extend_from_slice(prefix);
            }
        }
        file.write_all(&chunk)
            .await
            .map_err(|e| AppError::Internal(anyhow::anyhow!("Write temp upload file: {e}")))?;
        size_bytes = size_bytes.saturating_add(chunk.len());
    }
    file.flush()
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!("Flush temp upload file: {e}")))?;

    tracing::info!(target: LOG_TARGET,
        field = field_name,
        size_bytes,
        limit_bytes = max_bytes,
        "multipart upload field staged successfully"
    );

    Ok(TempUpload {
        temp_file,
        sniff_bytes,
        size_bytes,
    })
}

fn format_upload_limit(max_bytes: usize) -> String {
    crate::utils::files::format_file_size(i64::try_from(max_bytes).unwrap_or(i64::MAX))
}

async fn read_upload_field(
    field: axum::extract::multipart::Field<'_>,
    max_bytes: usize,
    default_name: &str,
    field_name: &'static str,
    budget: &mut PublicMultipartBudget,
    media_upload_gate: &crate::middleware::MediaUploadGate,
    media_upload_guard: &mut Option<crate::middleware::MediaUploadGuard>,
) -> Result<Option<(TempUpload, String)>> {
    let submitted_filename = field.file_name().map(str::to_owned);
    let fname = submitted_filename
        .as_deref()
        .filter(|name| !name.is_empty())
        .unwrap_or(default_name)
        .to_owned();
    let upload = stream_field_to_temp_file(
        field,
        max_bytes,
        field_name,
        budget,
        media_upload_gate,
        media_upload_guard,
    )
    .await?;
    if upload.size_bytes == 0 {
        if submitted_filename
            .as_deref()
            .is_some_and(|name| !name.is_empty())
        {
            return Err(AppError::BadRequest("Uploaded file is empty.".into()));
        }
        return Ok(None);
    }
    Ok(Some((upload, fname)))
}

pub(crate) struct TempUpload {
    pub temp_file: tempfile::NamedTempFile,
    pub sniff_bytes: Vec<u8>,
    pub size_bytes: usize,
}

/// Parsed fields from a post/thread creation multipart form.
pub(crate) struct PostFormData {
    /// Permit acquired before the first non-empty media byte is staged.
    pub media_upload_guard: Option<crate::middleware::MediaUploadGuard>,
    pub csrf_verified: bool,
    pub submission_token: String,
    pub name: String,
    pub subject: String,
    pub body: String,
    pub deletion_token: String,
    /// Legacy/general upload slot (used for video or arbitrary files).
    pub file: Option<(TempUpload, String)>,
    /// Primary audio slot shown first in the posting UI.
    pub audio_file: Option<(TempUpload, String)>,
    /// Optional cover-image slot shown second in the posting UI.
    pub image_file: Option<(TempUpload, String)>,
    // Poll fields are used only when creating a new thread.
    pub poll_question: String,
    pub poll_options: Vec<String>,
    /// Duration in seconds (parsed from value + unit)
    pub poll_duration_secs: Option<i64>,
    /// Sage — when true the reply must not bump the thread.
    pub sage: bool,
    /// Server-side CAPTCHA challenge id submitted by posting forms when enabled.
    pub captcha_id: String,
    /// Human-entered CAPTCHA answer submitted by posting forms when enabled.
    pub captcha_answer: String,
}

/// Drain all fields from a multipart form into [`PostFormData`].
/// `csrf_cookie` is the value from the browser cookie for CSRF verification.
#[expect(
    clippy::cognitive_complexity,
    reason = "the multipart parser keeps per-field size and duplicate checks at one input boundary"
)]
#[expect(
    clippy::too_many_lines,
    reason = "all multipart field limits and duplicate checks must remain at one input boundary"
)]
pub(crate) async fn parse_post_multipart(
    mut multipart: Multipart,
    csrf_cookie: Option<&str>,
    max_image_size: usize,
    max_video_size: usize,
    max_audio_size: usize,
    max_pdf_size: usize,
    media_upload_gate: &crate::middleware::MediaUploadGate,
) -> Result<PostFormData> {
    tracing::info!(target: LOG_TARGET,
        max_image_bytes = max_image_size,
        max_video_bytes = max_video_size,
        max_audio_bytes = max_audio_size,
        max_pdf_bytes = max_pdf_size,
        "accepted multipart upload limits for post request"
    );

    let mut csrf_verified = false;
    let mut submission_token = String::new();
    let mut name = String::new();
    let mut subject = String::new();
    let mut body = String::new();
    let mut deletion_token = String::new();
    let mut file: Option<(TempUpload, String)> = None;
    let mut audio_file: Option<(TempUpload, String)> = None;
    let mut image_file: Option<(TempUpload, String)> = None;
    let mut poll_question = String::new();
    let mut poll_options: Vec<String> = Vec::new();
    let mut poll_duration_value: Option<i64> = None;
    let mut poll_duration_unit = String::from("hours");
    let mut sage = false;
    let mut captcha_id = String::new();
    let mut captcha_answer = String::new();
    let mut budget = PublicMultipartBudget::default();
    let mut seen_upload_slots = HashSet::new();
    let mut media_upload_guard = None;

    loop {
        let next_field = multipart
            .next_field()
            .await
            .map_err(|e| multipart_read_error("multipart", &e))?;
        let Some(field) = next_field else {
            break;
        };
        budget.note_field()?;
        match field.name() {
            Some("_csrf") => {
                let v = read_text_field(field, &mut budget).await?;
                if validate_csrf(csrf_cookie, &v) {
                    csrf_verified = true;
                }
            }
            Some("submission_token") => {
                submission_token = read_text_field(field, &mut budget).await?;
            }
            Some("name") => name = read_text_field(field, &mut budget).await?,
            Some("subject") => subject = read_text_field(field, &mut budget).await?,
            Some("body") => body = read_text_field(field, &mut budget).await?,
            Some("deletion_token") => deletion_token = read_text_field(field, &mut budget).await?,
            Some("sage") => {
                let v = read_text_field(field, &mut budget).await?;
                sage = v == "1" || v.eq_ignore_ascii_case("on") || v.eq_ignore_ascii_case("true");
            }
            Some("captcha_id") => captcha_id = read_text_field(field, &mut budget).await?,
            Some("captcha_answer") => {
                captcha_answer = read_text_field(field, &mut budget).await?;
            }
            Some("poll_question") => {
                let v = read_text_field(field, &mut budget).await?;
                if v.chars().count() > 500 {
                    return Err(AppError::BadRequest(
                        "Poll question must be 500 characters or fewer.".into(),
                    ));
                }
                poll_question = v;
            }
            Some("poll_option") => {
                let v = read_text_field(field, &mut budget).await?;
                let trimmed = v.trim().to_owned();
                if !trimmed.is_empty() {
                    if poll_options.len() >= 20 {
                        return Err(AppError::BadRequest(
                            "Polls are limited to 20 options.".into(),
                        ));
                    }
                    if trimmed.chars().count() > 200 {
                        return Err(AppError::BadRequest(
                            "Each poll option must be 200 characters or fewer.".into(),
                        ));
                    }
                    poll_options.push(trimmed);
                }
            }
            Some("poll_duration_value") => {
                let v = read_text_field(field, &mut budget).await?;
                poll_duration_value = v.trim().parse::<i64>().ok();
            }
            Some("poll_duration_unit") => {
                poll_duration_unit = read_text_field(field, &mut budget).await?;
            }
            Some("file") => {
                if !seen_upload_slots.insert("file") {
                    return Err(AppError::BadRequest(
                        "Duplicate upload field 'file'.".into(),
                    ));
                }
                file = read_upload_field(
                    field,
                    max_image_size
                        .max(max_video_size)
                        .max(max_audio_size)
                        .max(max_pdf_size),
                    "upload",
                    "file",
                    &mut budget,
                    media_upload_gate,
                    &mut media_upload_guard,
                )
                .await?;
            }
            Some("audio_file") => {
                if !seen_upload_slots.insert("audio_file") {
                    return Err(AppError::BadRequest(
                        "Duplicate upload field 'audio_file'.".into(),
                    ));
                }
                audio_file = read_upload_field(
                    field,
                    max_audio_size,
                    "audio",
                    "audio_file",
                    &mut budget,
                    media_upload_gate,
                    &mut media_upload_guard,
                )
                .await?;
            }
            Some("image_file") => {
                if !seen_upload_slots.insert("image_file") {
                    return Err(AppError::BadRequest(
                        "Duplicate upload field 'image_file'.".into(),
                    ));
                }
                image_file = read_upload_field(
                    field,
                    max_image_size,
                    "image",
                    "image_file",
                    &mut budget,
                    media_upload_gate,
                    &mut media_upload_guard,
                )
                .await?;
            }
            _ => {
                discard_unknown_public_multipart_field(field, &mut budget).await?;
            }
        }
    }

    // Convert duration value + unit → seconds (saturating to prevent overflow).
    // The unit is validated against an explicit allow-list (case-insensitive) so
    // that a tampered form field does not silently multiply by an arbitrary factor.
    let poll_duration_secs = if poll_question.trim().is_empty() {
        None
    } else {
        match poll_duration_value {
            None => None,
            Some(v) => {
                let unit = poll_duration_unit.trim().to_ascii_lowercase();
                let secs = match unit.as_str() {
                    "minutes" => v.saturating_mul(60),
                    "hours" => v.saturating_mul(3600),
                    "days" => v.saturating_mul(86_400),
                    other => {
                        return Err(AppError::BadRequest(format!("Invalid poll duration unit '{other}'. Use 'minutes', 'hours', or 'days'.")));
                    }
                };
                Some(secs)
            }
        }
    };

    Ok(PostFormData {
        media_upload_guard,
        csrf_verified,
        submission_token,
        name,
        subject,
        body,
        deletion_token,
        file,
        audio_file,
        image_file,
        poll_question,
        poll_options,
        poll_duration_secs,
        sage,
        captcha_id,
        captcha_answer,
    })
}

// Upload error classifier (#6)
/// Convert an anyhow error from `save_upload` into the most appropriate
/// `AppError` variant, giving clients accurate HTTP status codes:
///   • "File too large"          → 413 `UploadTooLarge`
///   • "Insufficient disk space" → 413 `UploadTooLarge`
///   • "File type not allowed"   → 415 `InvalidMediaType`
///   • "Not an audio file"       → 415 `InvalidMediaType`
///   • anything else             → 400 `BadRequest`
pub(crate) fn classify_upload_error(e: &anyhow::Error) -> AppError {
    let msg = e.to_string();
    // Compare lower-cased so minor wording changes in save_upload don't silently
    // fall through to a generic 400 instead of the correct 413 / 415.
    let lower = msg.to_ascii_lowercase();
    if lower.starts_with("file too large") || lower.starts_with("insufficient disk space") {
        AppError::UploadTooLarge(msg)
    } else if lower.starts_with("file type not allowed") || lower.starts_with("not an audio file") {
        AppError::InvalidMediaType(msg)
    } else {
        AppError::BadRequest(msg)
    }
}

// Shared media upload processing (R2-2)
// create_thread (board.rs) and post_reply (thread.rs) had identical blocks for:
//   1. Magic-byte mime detection + per-board toggle enforcement
//   2. SHA-256 deduplication lookup
//   3. save_upload / save_audio_with_image_thumb
//   4. record_file_hash
//   5. Image+audio combo validation
//   6. Background job enqueueing
//
// Both handlers now call these shared functions instead of duplicating the code.

use crate::models::Board;

/// Process the primary file upload for a new post: detect mime type, enforce
/// per-board media toggles, SHA-256 dedup, save to disk and record hash.
///
/// Returns `Ok(None)` when `file_data` is `None` (no file attached).
/// Must be called from inside a `spawn_blocking` closure.
#[expect(
    clippy::too_many_arguments,
    reason = "the parameters mirror the validated upload, board policy, and destination records"
)]
#[expect(
    clippy::too_many_lines,
    reason = "media validation, persistence, thumbnailing, and cleanup form one guarded operation"
)]
pub(crate) fn process_primary_upload(
    file_data: Option<(TempUpload, String)>,
    board: &Board,
    conn: &rusqlite::Connection,
    upload_dir: &str,
    save_root: &str,
    thumb_size: u32,
    max_image_size: usize,
    max_video_size: usize,
    max_audio_size: usize,
    max_pdf_size: usize,
    ffmpeg_available: bool,
    ffprobe_available: bool,
    ffmpeg_webp_available: bool,
) -> Result<(Option<crate::utils::files::UploadedFile>, Option<String>)> {
    let Some((upload, fname)) = file_data else {
        return Ok((None, None));
    };
    let allow_any_files =
        crate::config::CONFIG.enable_any_file_uploads_feature && board.allow_any_files;
    let detected_mime = crate::utils::files::classify_upload_mime(
        upload.temp_file.path(),
        &upload.sniff_bytes,
        ffprobe_available,
        allow_any_files,
    )
    .map_err(|error| classify_upload_error(&error))?;
    let detected_media = crate::models::MediaType::from_mime(&detected_mime);
    let ambiguous_webm = detected_mime == crate::utils::files::AMBIGUOUS_WEBM_MIME;

    match detected_media {
        crate::models::MediaType::Image if !board.allow_images => {
            return Err(AppError::BadRequest(
                "Image uploads are disabled on this board.".into(),
            ))
        }
        crate::models::MediaType::Video if !board.allow_video => {
            return Err(AppError::BadRequest(
                "Video uploads are disabled on this board.".into(),
            ))
        }
        crate::models::MediaType::Audio if !board.allow_audio => {
            return Err(AppError::BadRequest(
                "Audio uploads are disabled on this board.".into(),
            ))
        }
        crate::models::MediaType::Pdf if !board.allow_pdf => {
            return Err(AppError::BadRequest(
                "PDF uploads are disabled on this board.".into(),
            ))
        }
        crate::models::MediaType::Other
            if ambiguous_webm && (!board.allow_video || !board.allow_audio) =>
        {
            return Err(AppError::BadRequest(
                "WebM stream type could not be established; both video and audio uploads must be enabled to accept it as a neutral download."
                    .into(),
            ));
        }
        crate::models::MediaType::Other if !ambiguous_webm && !allow_any_files => {
            return Err(AppError::BadRequest(
                "This board only accepts image, video, audio, or PDF uploads.".into(),
            ))
        }
        crate::models::MediaType::Image
        | crate::models::MediaType::Video
        | crate::models::MediaType::Audio
        | crate::models::MediaType::Pdf
        | crate::models::MediaType::Other => {}
    }

    let upload_options = crate::utils::files::SaveUploadOptions {
        original_filename: &fname,
        boards_dir: save_root,
        board_short: &board.short_name,
        thumb_size,
        max_image_size,
        max_video_size,
        max_audio_size,
        max_pdf_size,
        ffmpeg_available,
        ffprobe_available,
        ffmpeg_webp_available,
        allow_any_files,
    };
    let validated = crate::utils::files::storage::validate_upload_for_storage(
        upload.temp_file.path(),
        &upload.sniff_bytes,
        upload.size_bytes,
        &upload_options,
    )
    .map_err(|error| classify_upload_error(&error))?;

    // SHA-256 deduplication — serve the cached entry without re-saving.
    //
    // Validate that both the cached file and thumbnail still exist
    // on disk before returning the dedup hit.  When a thread or board is
    // deleted its files are removed from disk, but the file_hashes table is
    // not pruned.  Without this check, re-uploading the same image after its
    // original thread/board was deleted would return stale paths pointing at
    // deleted files, so the post would display no image and no thumbnail.
    //
    // If either path is missing we fall through to re-process the upload.
    // record_file_hash uses INSERT OR REPLACE, so the cache entry is
    // automatically refreshed to point at the newly saved files.
    let hash = sha256_file_hex(upload.temp_file.path())?;
    if let Some(cached) = crate::db::find_file_by_hash(conn, &hash)? {
        let same_board_cache = cached_paths_belong_to_board(&cached, &board.short_name);
        let file_ok = std::path::Path::new(upload_dir)
            .join(&cached.file_path)
            .exists();
        let thumb_ok = cached.thumb_path.is_empty()
            || std::path::Path::new(upload_dir)
                .join(&cached.thumb_path)
                .exists();

        if same_board_cache && file_ok && thumb_ok {
            let cached_media = crate::models::MediaType::from_mime(&cached.mime_type);
            let cached_size =
                std::fs::metadata(std::path::Path::new(upload_dir).join(&cached.file_path))
                    .ok()
                    .and_then(|metadata| i64::try_from(metadata.len()).ok())
                    .unwrap_or_else(|| i64::try_from(upload.size_bytes).unwrap_or(0));
            return Ok((
                Some(crate::utils::files::UploadedFile {
                    file_path: cached.file_path,
                    thumb_path: cached.thumb_path,
                    original_name: crate::utils::sanitize::sanitize_filename(&fname),
                    mime_type: cached.mime_type,
                    file_size: cached_size,
                    media_type: cached_media,
                    processing_pending: false,
                    dedup_reused: true,
                }),
                None,
            ));
        }

        // One or both paths are gone — the entry is stale.  Log and fall
        // through so the file is re-saved and the cache is updated below.
        // Cross-board hits are also re-saved under the current board; otherwise
        // a protected board could point at public media from another board.
        tracing::debug!(target: LOG_TARGET,
            "dedup cache miss: same_board_cache={same_board_cache} file_ok={file_ok} thumb_ok={thumb_ok}, \
             re-processing upload for hash {hash}"
        );
    }

    let f =
        crate::utils::files::storage::save_validated_upload_from_path(validated, &upload_options)
            .map_err(|e| classify_upload_error(&e))?;
    Ok((Some(f), Some(hash)))
}

fn cached_paths_belong_to_board(cached: &crate::db::CachedFile, board_short: &str) -> bool {
    upload_path_belongs_to_board(&cached.file_path, board_short)
        && (cached.thumb_path.is_empty()
            || upload_path_belongs_to_board(&cached.thumb_path, board_short))
}

fn upload_path_belongs_to_board(path: &str, board_short: &str) -> bool {
    path.split('/').next() == Some(board_short)
}

fn temp_upload_mime(
    upload: &TempUpload,
    ffprobe_available: bool,
    allow_any_files: bool,
) -> Result<String> {
    crate::utils::files::classify_upload_mime(
        upload.temp_file.path(),
        &upload.sniff_bytes,
        ffprobe_available,
        allow_any_files,
    )
    .map_err(|error| classify_upload_error(&error))
}

/// Process the secondary audio file for an image+audio combo upload.
/// `primary_upload` must already be the processed primary image.
///
/// Returns `Ok(None)` when `audio_file_data` is `None`.
/// Must be called from inside a `spawn_blocking` closure.
pub(crate) fn process_audio_combo(
    audio_file_data: Option<(TempUpload, String)>,
    primary_upload: Option<&crate::utils::files::UploadedFile>,
    board: &Board,
    upload_dir: &str,
    max_audio_size: usize,
    ffprobe_available: bool,
) -> Result<Option<crate::utils::files::UploadedFile>> {
    let Some((audio_upload, aud_fname)) = audio_file_data else {
        return Ok(None);
    };

    if !board.allow_audio {
        return Err(AppError::BadRequest(
            "Audio uploads are disabled on this board.".into(),
        ));
    }

    // Audio combo requires the primary file to be an image.
    let primary_is_image =
        primary_upload.is_some_and(|u| matches!(u.media_type, crate::models::MediaType::Image));
    if !primary_is_image {
        return Err(AppError::BadRequest(
            "Audio can only be combined with an image upload.".into(),
        ));
    }

    let mut aud_file = crate::utils::files::save_audio_with_image_thumb_from_path(
        audio_upload.temp_file.path(),
        &audio_upload.sniff_bytes,
        audio_upload.size_bytes,
        &aud_fname,
        upload_dir,
        &board.short_name,
        max_audio_size,
        ffprobe_available,
    )
    .map_err(|e| classify_upload_error(&e))?;

    // Use the image thumbnail as the audio's visual.
    if let Some(img) = primary_upload {
        aud_file.thumb_path.clone_from(&img.thumb_path);
    }
    Ok(Some(aud_file))
}

#[expect(
    clippy::too_many_arguments,
    reason = "the parameters represent the three optional media inputs and their shared post context"
)]
pub(crate) fn process_audio_first_uploads(
    audio_file_data: Option<(TempUpload, String)>,
    image_file_data: Option<(TempUpload, String)>,
    fallback_file_data: Option<(TempUpload, String)>,
    board: &Board,
    conn: &rusqlite::Connection,
    upload_dir: &str,
    save_root_str: &str,
    thumb_size: u32,
    max_image_size: usize,
    max_video_size: usize,
    max_audio_size: usize,
    max_pdf_size: usize,
    ffmpeg_available: bool,
    ffprobe_available: bool,
    ffmpeg_webp_available: bool,
) -> Result<(
    Option<crate::utils::files::UploadedFile>,
    Option<crate::utils::files::UploadedFile>,
    Option<String>,
)> {
    let allow_any_files =
        crate::config::CONFIG.enable_any_file_uploads_feature && board.allow_any_files;
    let has_audio_or_image_upload = audio_file_data.is_some() || image_file_data.is_some();
    let save_primary = |file_data| {
        process_primary_upload(
            file_data,
            board,
            conn,
            upload_dir,
            save_root_str,
            thumb_size,
            max_image_size,
            max_video_size,
            max_audio_size,
            max_pdf_size,
            ffmpeg_available,
            ffprobe_available,
            ffmpeg_webp_available,
        )
    };

    if has_audio_or_image_upload && fallback_file_data.is_some() {
        return Err(AppError::BadRequest(
            "Use either the audio/image upload flow or the other-file slot, not both in the same post."
                .into(),
        ));
    }

    if let Some((image_upload, image_name)) = image_file_data {
        let (primary, primary_hash) = save_primary(Some((image_upload, image_name)))?;

        let audio = process_audio_combo(
            audio_file_data,
            primary.as_ref(),
            board,
            save_root_str,
            max_audio_size,
            ffprobe_available,
        )?;

        return Ok((primary, audio, primary_hash));
    }

    if let Some((audio_upload, audio_name)) = audio_file_data {
        let audio_mime = temp_upload_mime(&audio_upload, ffprobe_available, allow_any_files)?;
        if crate::models::MediaType::from_mime(&audio_mime) != crate::models::MediaType::Audio {
            return Err(AppError::BadRequest(
                "The audio slot only accepts audio files.".into(),
            ));
        }

        let (primary, primary_hash) = save_primary(Some((audio_upload, audio_name)))?;

        return Ok((primary, None, primary_hash));
    }

    let (primary, primary_hash) = save_primary(fallback_file_data)?;

    Ok((primary, None, primary_hash))
}

fn sha256_file_hex(path: &std::path::Path) -> Result<String> {
    use sha2::Digest as _;
    let mut file = std::fs::File::open(path)
        .map_err(|e| AppError::Internal(anyhow::anyhow!("Open temp upload for hash: {e}")))?;
    let mut hasher = sha2::Sha256::new();
    let mut buf = [0u8; 8192];
    loop {
        let read = std::io::Read::read(&mut file, &mut buf)
            .map_err(|e| AppError::Internal(anyhow::anyhow!("Hash temp upload: {e}")))?;
        if read == 0 {
            break;
        }
        if let Some(bytes) = buf.get(..read) {
            hasher.update(bytes);
        }
    }
    Ok(hex::encode(hasher.finalize()))
}

/// Enqueue background media-processing and spam-check jobs for a newly created
/// post.  Shared by `create_thread` and `post_reply`.
pub(crate) fn enqueue_post_jobs(
    job_queue: &JobQueue,
    conn: &rusqlite::Connection,
    post_id: i64,
    ip_hash: &str,
    body_len: usize,
    uploaded: Option<&crate::utils::files::UploadedFile>,
    board_short: &str,
) -> Result<()> {
    // 1. Media post-processing (video transcode / audio waveform)
    if let Some(up) = uploaded {
        if up.processing_pending {
            let job = match up.media_type {
                crate::models::MediaType::Video => Some(crate::workers::Job::VideoTranscode {
                    post_id,
                    file_path: up.file_path.clone(),
                    board_short: board_short.to_owned(),
                }),
                crate::models::MediaType::Audio => Some(crate::workers::Job::AudioWaveform {
                    post_id,
                    file_path: up.file_path.clone(),
                    board_short: board_short.to_owned(),
                }),
                crate::models::MediaType::Image
                | crate::models::MediaType::Pdf
                | crate::models::MediaType::Other => None,
            };
            if let Some(j) = job {
                match job_queue.enqueue_media(conn, &j) {
                    Ok(crate::workers::EnqueueOutcome::Enqueued(job_id)) => tracing::debug!(
                        target: "workers",
                        post_id,
                        job_id,
                        job_type = j.type_str(),
                        board = board_short,
                        "atomically scheduled post media processing"
                    ),
                    Ok(crate::workers::EnqueueOutcome::DroppedAtCapacity) => {
                        tracing::warn!(
                            target: "workers",
                            post_id,
                            job_type = j.type_str(),
                            board = board_short,
                            "media job rejected at capacity with terminal post state"
                        );
                    }
                    Err(error) => return Err(error.into()),
                }
            }
        }
    }

    // 2. Spam analysis
    drop(job_queue.enqueue(&crate::workers::Job::SpamCheck {
        post_id,
        ip_hash: ip_hash.to_owned(),
        body_len,
    }));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        parse_post_multipart, process_audio_first_uploads, MultipartEnvelopeScanner, TempUpload,
        PUBLIC_MULTIPART_ENVELOPE_LIMIT_MARKER, PUBLIC_MULTIPART_ENVELOPE_MAX_BYTES,
    };
    use anyhow::{bail, ensure, Context as _};
    use axum::{
        body::Body,
        extract::FromRequest as _,
        http::{header, Request, StatusCode},
        routing::post,
        Router,
    };
    use sha2::Digest as _;
    use tower::ServiceExt as _;

    const MIB: i64 = 1024 * 1024;

    fn sample_board() -> crate::models::Board {
        crate::models::Board {
            allow_any_files: true,
            ..crate::test_fixtures::sample_board()
        }
    }

    fn temp_upload(name: &str, bytes: &[u8]) -> anyhow::Result<(TempUpload, String)> {
        let temp_file = tempfile::Builder::new()
            .prefix("rustchan-test-upload-")
            .tempfile()
            .context("create temporary upload")?;
        std::fs::write(temp_file.path(), bytes).context("write temporary upload")?;
        Ok((
            TempUpload {
                temp_file,
                sniff_bytes: bytes.to_vec(),
                size_bytes: bytes.len(),
            },
            name.to_owned(),
        ))
    }

    fn valid_pdf() -> &'static [u8] {
        b"%PDF-1.4
1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj
2 0 obj << /Type /Pages /Kids [3 0 R] /Count 1 >> endobj
3 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Resources << >> /Contents 4 0 R >> endobj
4 0 obj << /Length 0 >> stream

endstream endobj
trailer << /Root 1 0 R >>
%%EOF
"
    }

    async fn multipart_from_bytes(
        boundary: &str,
        body: Vec<u8>,
    ) -> anyhow::Result<axum::extract::Multipart> {
        let request = Request::builder()
            .method("POST")
            .uri("/parse")
            .header(
                header::CONTENT_TYPE,
                format!("multipart/form-data; boundary={boundary}"),
            )
            .body(Body::from(body))
            .context("build multipart extraction request")?;
        axum::extract::Multipart::from_request(request, &())
            .await
            .map_err(|rejection| anyhow::anyhow!(rejection.to_string()))
    }

    #[test]
    fn public_multipart_boundary_parser_accepts_quoted_parameter() -> anyhow::Result<()> {
        let parsed = super::parse_public_multipart_boundary(
            "multipart/form-data; charset=utf-8; boundary=\"rust;chan-boundary\"",
        )
        .context("quoted boundary was rejected")?;
        ensure!(parsed == b"rust;chan-boundary");
        ensure!(
            super::parse_public_multipart_boundary(
                "multipart/form-data; boundary=first; boundary=second"
            )
            .is_none(),
            "duplicate boundary parameter was accepted"
        );
        Ok(())
    }

    #[test]
    fn public_multipart_envelope_scanner_accepts_split_valid_form() -> anyhow::Result<()> {
        let body = b"preamble--boundary\r\nContent-Disposition: form-data; name=\"body\"\r\n\r\nhello\r\n--boundary--\r\n";
        let mut scanner = MultipartEnvelopeScanner::new(b"boundary");
        for byte in body {
            scanner
                .inspect(std::slice::from_ref(byte))
                .map_err(anyhow::Error::msg)?;
        }
        Ok(())
    }

    #[test]
    fn public_multipart_envelope_scanner_rejects_unterminated_large_headers() -> anyhow::Result<()>
    {
        let mut scanner = MultipartEnvelopeScanner::new(b"boundary");
        let mut body = b"--boundary\r\n".to_vec();
        body.extend(std::iter::repeat_n(
            b'a',
            PUBLIC_MULTIPART_ENVELOPE_MAX_BYTES.saturating_add(1),
        ));
        let Err(error) = scanner.inspect(&body) else {
            anyhow::bail!("oversized unterminated field headers were accepted");
        };
        anyhow::ensure!(error.contains(PUBLIC_MULTIPART_ENVELOPE_LIMIT_MARKER));
        Ok(())
    }

    #[test]
    fn public_multipart_envelope_scanner_rejects_oversized_preamble_before_boundary(
    ) -> anyhow::Result<()> {
        let mut scanner = MultipartEnvelopeScanner::new(b"boundary");
        let mut body = vec![b'a'; PUBLIC_MULTIPART_ENVELOPE_MAX_BYTES.saturating_add(1)];
        body.extend_from_slice(b"--boundary\r\n\r\n");
        let Err(error) = scanner.inspect(&body) else {
            anyhow::bail!("oversized multipart preamble was accepted");
        };
        anyhow::ensure!(error.contains(PUBLIC_MULTIPART_ENVELOPE_LIMIT_MARKER));
        Ok(())
    }

    fn create_file_hash_table(conn: &rusqlite::Connection) -> anyhow::Result<()> {
        conn.execute(
            "CREATE TABLE file_hashes (
                sha256 TEXT PRIMARY KEY,
                file_path TEXT NOT NULL,
                thumb_path TEXT NOT NULL DEFAULT '',
                mime_type TEXT NOT NULL DEFAULT ''
            )",
            [],
        )
        .context("create file_hashes table")?;
        Ok(())
    }

    async fn parse_scaled_audio_limit(
        multipart: axum::extract::Multipart,
    ) -> crate::error::Result<&'static str> {
        let gate = crate::middleware::MediaUploadGate::new();
        let form = parse_post_multipart(
            multipart,
            Some("csrf123"),
            1_024,
            1_024,
            5_000,
            1_024,
            &gate,
        )
        .await?;
        let (upload, _) = form.audio_file.as_ref().ok_or_else(|| {
            crate::error::AppError::Internal(anyhow::anyhow!("audio upload was not parsed"))
        })?;
        if upload.size_bytes != 4_500 {
            return Err(crate::error::AppError::Internal(anyhow::anyhow!(
                "parsed audio size was {}, expected 4500",
                upload.size_bytes
            )));
        }
        drop(form);
        Ok("ok")
    }

    async fn parse_scaled_audio_oversize(
        multipart: axum::extract::Multipart,
    ) -> crate::error::Result<&'static str> {
        let gate = crate::middleware::MediaUploadGate::new();
        parse_post_multipart(
            multipart,
            Some("csrf123"),
            1_024,
            1_024,
            5_000,
            1_024,
            &gate,
        )
        .await?;
        Ok("ok")
    }

    #[test]
    fn board_specific_audio_limit_500_mib_is_not_clamped_to_default() {
        let board = crate::models::Board {
            allow_audio: true,
            max_audio_size: 500 * MIB,
            ..crate::test_fixtures::sample_board()
        };

        let limit = board.max_audio_size_bytes();
        let upload_size = 450usize * 1024 * 1024;

        assert_eq!(limit, 500usize * 1024 * 1024);
        assert!(limit > 150usize * 1024 * 1024);
        assert!(limit > upload_size + 1024 * 1024);
    }

    #[tokio::test]
    async fn multipart_parser_accepts_audio_within_board_specific_limit() -> anyhow::Result<()> {
        let router = Router::new().route("/parse", post(parse_scaled_audio_limit));
        let audio = vec![b'a'; 4_500];
        let (boundary, body) = crate::test_support::multipart_body(
            &[("_csrf", "csrf123"), ("body", "audio post")],
            Some(("audio_file", "track.mp3", &audio, "audio/mpeg")),
        );

        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/parse")
                    .header(
                        header::CONTENT_TYPE,
                        format!("multipart/form-data; boundary={boundary}"),
                    )
                    .body(Body::from(body))
                    .context("build audio multipart request")?,
            )
            .await
            .context("receive audio multipart response")?;

        ensure!(response.status() == StatusCode::OK);
        Ok(())
    }

    #[tokio::test]
    async fn multipart_parser_rejects_oversized_audio_cleanly() -> anyhow::Result<()> {
        let router = Router::new().route("/parse", post(parse_scaled_audio_oversize));
        let audio = vec![b'a'; 5_001];
        let (boundary, body) = crate::test_support::multipart_body(
            &[("_csrf", "csrf123"), ("body", "audio post")],
            Some(("audio_file", "track.mp3", &audio, "audio/mpeg")),
        );

        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/parse")
                    .header(
                        header::CONTENT_TYPE,
                        format!("multipart/form-data; boundary={boundary}"),
                    )
                    .body(Body::from(body))
                    .context("build oversized audio multipart request")?,
            )
            .await
            .context("receive oversized audio multipart response")?;

        ensure!(response.status() == StatusCode::PAYLOAD_TOO_LARGE);
        Ok(())
    }

    async fn parse_default_limits(
        multipart: axum::extract::Multipart,
    ) -> crate::error::Result<&'static str> {
        let gate = crate::middleware::MediaUploadGate::new();
        parse_post_multipart(
            multipart,
            Some("csrf123"),
            1_024,
            1_024,
            1_024,
            1_024,
            &gate,
        )
        .await?;
        Ok("ok")
    }

    async fn parse_pdf_limit(
        multipart: axum::extract::Multipart,
    ) -> crate::error::Result<&'static str> {
        let gate = crate::middleware::MediaUploadGate::new();
        let form = parse_post_multipart(
            multipart,
            Some("csrf123"),
            1_024,
            1_024,
            1_024,
            2_048,
            &gate,
        )
        .await?;
        let (upload, _) = form.file.as_ref().ok_or_else(|| {
            crate::error::AppError::Internal(anyhow::anyhow!("PDF upload was not parsed"))
        })?;
        if upload.size_bytes != 2_048 {
            return Err(crate::error::AppError::Internal(anyhow::anyhow!(
                "parsed PDF size was {}, expected 2048",
                upload.size_bytes
            )));
        }
        drop(form);
        Ok("ok")
    }

    fn multipart_body_with_files(
        fields: &[(&str, &str)],
        files: &[(&str, &str, &[u8], &str)],
    ) -> (String, Vec<u8>) {
        let boundary = "rustchan-test-boundary".to_owned();
        let mut body = Vec::new();

        for (name, value) in fields {
            body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
            body.extend_from_slice(
                format!("Content-Disposition: form-data; name=\"{name}\"\r\n\r\n").as_bytes(),
            );
            body.extend_from_slice(value.as_bytes());
            body.extend_from_slice(b"\r\n");
        }

        for (field_name, filename, contents, content_type) in files {
            body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
            body.extend_from_slice(
                format!(
                    "Content-Disposition: form-data; name=\"{field_name}\"; filename=\"{filename}\"\r\n"
                )
                .as_bytes(),
            );
            body.extend_from_slice(format!("Content-Type: {content_type}\r\n\r\n").as_bytes());
            body.extend_from_slice(contents);
            body.extend_from_slice(b"\r\n");
        }

        body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
        (boundary, body)
    }

    #[tokio::test]
    async fn multipart_parser_holds_media_gate_from_first_upload_byte() -> anyhow::Result<()> {
        let gate = crate::middleware::MediaUploadGate::new();
        let (boundary, body) = multipart_body_with_files(
            &[("_csrf", "csrf123"), ("body", "guarded")],
            &[("file", "sample.bin", b"payload", "application/octet-stream")],
        );
        let multipart = multipart_from_bytes(&boundary, body).await?;
        let form = parse_post_multipart(
            multipart,
            Some("csrf123"),
            1_024,
            1_024,
            1_024,
            1_024,
            &gate,
        )
        .await?;

        ensure!(
            matches!(gate.try_begin(), Err(crate::error::AppError::DbBusy)),
            "parsed upload released the media gate before processing"
        );
        drop(form);
        ensure!(
            gate.try_begin().is_ok(),
            "dropping the parsed upload did not reopen the media gate"
        );
        Ok(())
    }

    #[tokio::test]
    async fn multipart_parser_does_not_gate_empty_file_control() -> anyhow::Result<()> {
        let gate = crate::middleware::MediaUploadGate::new();
        let (boundary, body) = multipart_body_with_files(
            &[("_csrf", "csrf123"), ("body", "text only")],
            &[("file", "", b"", "application/octet-stream")],
        );
        let multipart = multipart_from_bytes(&boundary, body).await?;
        let form = parse_post_multipart(
            multipart,
            Some("csrf123"),
            1_024,
            1_024,
            1_024,
            1_024,
            &gate,
        )
        .await?;

        ensure!(form.media_upload_guard.is_none());
        ensure!(gate.try_begin().is_ok());
        Ok(())
    }

    #[tokio::test]
    async fn multipart_parser_rejects_duplicate_upload_slot_before_second_body(
    ) -> anyhow::Result<()> {
        let router = Router::new().route("/parse", post(parse_default_limits));
        let first = b"one";
        let second = vec![b'a'; 10_000];
        let (boundary, body) = multipart_body_with_files(
            &[("_csrf", "csrf123"), ("body", "duplicate")],
            &[
                ("file", "one.bin", first, "application/octet-stream"),
                ("file", "two.bin", &second, "application/octet-stream"),
            ],
        );

        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/parse")
                    .header(
                        header::CONTENT_TYPE,
                        format!("multipart/form-data; boundary={boundary}"),
                    )
                    .body(Body::from(body))
                    .context("build duplicate upload multipart request")?,
            )
            .await
            .context("receive duplicate upload multipart response")?;

        ensure!(response.status() == StatusCode::BAD_REQUEST);
        Ok(())
    }

    #[tokio::test]
    async fn multipart_parser_rejects_named_zero_byte_upload() -> anyhow::Result<()> {
        let router = Router::new().route("/parse", post(parse_default_limits));
        let (boundary, body) = multipart_body_with_files(
            &[("_csrf", "csrf123"), ("body", "zero byte")],
            &[("file", "empty.png", b"", "image/png")],
        );

        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/parse")
                    .header(
                        header::CONTENT_TYPE,
                        format!("multipart/form-data; boundary={boundary}"),
                    )
                    .body(Body::from(body))
                    .context("build zero-byte multipart request")?,
            )
            .await
            .context("receive zero-byte multipart response")?;

        ensure!(response.status() == StatusCode::BAD_REQUEST);
        Ok(())
    }

    #[tokio::test]
    async fn multipart_parser_counts_file_payload_not_multipart_overhead_for_exact_limit(
    ) -> anyhow::Result<()> {
        let router = Router::new().route("/parse", post(parse_pdf_limit));
        let pdf = vec![b'p'; 2_048];
        let (boundary, body) = multipart_body_with_files(
            &[("_csrf", "csrf123"), ("body", "pdf")],
            &[("file", "exact.pdf", &pdf, "application/pdf")],
        );

        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/parse")
                    .header(
                        header::CONTENT_TYPE,
                        format!("multipart/form-data; boundary={boundary}"),
                    )
                    .body(Body::from(body))
                    .context("build exact-limit PDF multipart request")?,
            )
            .await
            .context("receive exact-limit PDF multipart response")?;

        ensure!(response.status() == StatusCode::OK);

        let over_pdf = vec![b'p'; 2_049];
        let (boundary, body) = multipart_body_with_files(
            &[("_csrf", "csrf123"), ("body", "pdf")],
            &[("file", "over.pdf", &over_pdf, "application/pdf")],
        );
        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/parse")
                    .header(
                        header::CONTENT_TYPE,
                        format!("multipart/form-data; boundary={boundary}"),
                    )
                    .body(Body::from(body))
                    .context("build over-limit PDF multipart request")?,
            )
            .await
            .context("receive over-limit PDF multipart response")?;

        ensure!(response.status() == StatusCode::PAYLOAD_TOO_LARGE);
        Ok(())
    }

    #[tokio::test]
    async fn multipart_parser_ignores_empty_unselected_file_control() -> anyhow::Result<()> {
        let router = Router::new().route("/parse", post(parse_default_limits));
        let (boundary, body) = multipart_body_with_files(
            &[("_csrf", "csrf123"), ("body", "text only")],
            &[("file", "", b"", "application/octet-stream")],
        );

        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/parse")
                    .header(
                        header::CONTENT_TYPE,
                        format!("multipart/form-data; boundary={boundary}"),
                    )
                    .body(Body::from(body))
                    .context("build empty file-control multipart request")?,
            )
            .await
            .context("receive empty file-control multipart response")?;

        ensure!(response.status() == StatusCode::OK);
        Ok(())
    }

    #[test]
    fn public_multipart_budget_enforces_aggregate_bytes_and_field_count() -> anyhow::Result<()> {
        let mut budget = super::PublicMultipartBudget::default();
        for _ in 0..super::PUBLIC_MULTIPART_MAX_FIELDS {
            budget.note_field()?;
        }
        ensure!(budget.note_field().is_err());

        let mut budget = super::PublicMultipartBudget::default();
        budget.note_chunk(super::PUBLIC_MULTIPART_AGGREGATE_MAX_BYTES)?;
        ensure!(budget.note_chunk(1).is_err());
        Ok(())
    }

    #[test]
    fn audio_first_flow_rejects_mixing_other_slot_with_audio_or_image_slots() -> anyhow::Result<()>
    {
        let conn = rusqlite::Connection::open_in_memory().context("open in-memory SQLite")?;
        let board = sample_board();
        let audio = temp_upload("track.flac", b"fLaC\x00\x00\x00\x22test")?;
        let other = temp_upload("clip.webm", b"\x1a\x45\xdf\xa3webm")?;
        let boards_dir = tempfile::tempdir().context("create boards directory")?;
        let uploads_dir = tempfile::tempdir().context("create uploads directory")?;

        let result = process_audio_first_uploads(
            Some(audio),
            None,
            Some(other),
            &board,
            &conn,
            boards_dir
                .path()
                .to_str()
                .context("boards path is not UTF-8")?,
            uploads_dir
                .path()
                .to_str()
                .context("uploads path is not UTF-8")?,
            150,
            1024 * 1024,
            1024 * 1024,
            1024 * 1024,
            1024 * 1024,
            false,
            false,
            false,
        );

        let Err(error) = result else {
            bail!("mixed upload modes should be rejected");
        };
        ensure!(
            error
                .to_string()
                .contains("Use either the audio/image upload flow or the other-file slot"),
            "unexpected mixed-upload error: {error}"
        );
        Ok(())
    }

    #[test]
    fn primary_upload_rejects_malformed_image_even_when_hash_is_cached() -> anyhow::Result<()> {
        let conn = rusqlite::Connection::open_in_memory().context("open in-memory SQLite")?;
        create_file_hash_table(&conn)?;

        let board = crate::test_fixtures::sample_board();
        let uploads_dir = tempfile::tempdir().context("create uploads directory")?;
        let save_root = tempfile::tempdir().context("create save root")?;
        let malformed = b"\x89PNG\r\n\x1a\nthis is not a complete png";
        let upload = temp_upload("broken.png", malformed)?;

        let mut hasher = sha2::Sha256::new();
        hasher.update(malformed);
        let hash = hex::encode(hasher.finalize());

        let board_dir = uploads_dir.path().join(&board.short_name);
        let thumbs_dir = board_dir.join("thumbs");
        std::fs::create_dir_all(&thumbs_dir).context("create thumbnail directory")?;
        std::fs::write(board_dir.join("cached.png"), malformed).context("write cached image")?;
        std::fs::write(thumbs_dir.join("cached.webp"), b"fake thumb")
            .context("write cached thumbnail")?;
        crate::db::record_file_hash(
            &conn,
            &hash,
            &format!("{}/cached.png", board.short_name),
            &format!("{}/thumbs/cached.webp", board.short_name),
            "image/png",
        )
        .context("record cached file hash")?;

        let result = super::process_primary_upload(
            Some(upload),
            &board,
            &conn,
            uploads_dir
                .path()
                .to_str()
                .context("uploads path is not UTF-8")?,
            save_root
                .path()
                .to_str()
                .context("save path is not UTF-8")?,
            64,
            1024 * 1024,
            1024 * 1024,
            1024 * 1024,
            1024 * 1024,
            false,
            false,
            false,
        );

        let Err(error) = result else {
            bail!("malformed image should be rejected before dedup reuse");
        };
        ensure!(
            error.to_string().contains("image header is malformed"),
            "unexpected malformed-image error: {error}"
        );
        Ok(())
    }

    #[test]
    fn primary_upload_does_not_reuse_dedup_cache_from_another_board() -> anyhow::Result<()> {
        let conn = rusqlite::Connection::open_in_memory().context("open in-memory SQLite")?;
        create_file_hash_table(&conn)?;

        let board = crate::models::Board {
            short_name: "secret".to_owned(),
            allow_pdf: true,
            ..crate::test_fixtures::sample_board()
        };
        let uploads_dir = tempfile::tempdir().context("create uploads directory")?;
        let save_root = tempfile::tempdir().context("create save root")?;
        let pdf = valid_pdf();
        let mut hasher = sha2::Sha256::new();
        hasher.update(pdf);
        let hash = hex::encode(hasher.finalize());

        let public_thumb_dir = uploads_dir.path().join("img/thumbs");
        std::fs::create_dir_all(&public_thumb_dir).context("create public thumbnail directory")?;
        std::fs::write(uploads_dir.path().join("img/cached.pdf"), pdf)
            .context("write public PDF")?;
        std::fs::write(public_thumb_dir.join("cached.svg"), b"<svg></svg>")
            .context("write public thumbnail")?;
        crate::db::record_file_hash(
            &conn,
            &hash,
            "img/cached.pdf",
            "img/thumbs/cached.svg",
            "application/pdf",
        )
        .context("record cross-board hash")?;

        let _override = crate::media::thumbnail::override_pdf_renderer_mode(
            crate::media::thumbnail::TestPdfRendererMode::Unavailable,
        );
        let (uploaded, primary_hash) = super::process_primary_upload(
            Some(temp_upload("doc.pdf", pdf)?),
            &board,
            &conn,
            uploads_dir
                .path()
                .to_str()
                .context("uploads path is not UTF-8")?,
            save_root
                .path()
                .to_str()
                .context("save path is not UTF-8")?,
            64,
            1024 * 1024,
            1024 * 1024,
            1024 * 1024,
            1024 * 1024,
            false,
            false,
            false,
        )
        .context("accept PDF upload")?;
        let uploaded = uploaded.context("PDF upload result was empty")?;

        ensure!(uploaded.file_path.starts_with("secret/"));
        ensure!(!uploaded.dedup_reused);
        ensure!(primary_hash.as_deref() == Some(hash.as_str()));
        ensure!(save_root.path().join(&uploaded.file_path).exists());
        Ok(())
    }

    #[test]
    fn primary_upload_rejects_pdf_when_board_disables_pdf() -> anyhow::Result<()> {
        let conn = rusqlite::Connection::open_in_memory().context("open in-memory SQLite")?;
        create_file_hash_table(&conn)?;

        let board = crate::models::Board {
            allow_pdf: false,
            ..crate::test_fixtures::sample_board()
        };
        let uploads_dir = tempfile::tempdir().context("create uploads directory")?;
        let save_root = tempfile::tempdir().context("create save root")?;
        let result = super::process_primary_upload(
            Some(temp_upload("doc.pdf", valid_pdf())?),
            &board,
            &conn,
            uploads_dir
                .path()
                .to_str()
                .context("uploads path is not UTF-8")?,
            save_root
                .path()
                .to_str()
                .context("save path is not UTF-8")?,
            64,
            1024 * 1024,
            1024 * 1024,
            1024 * 1024,
            1024 * 1024,
            false,
            false,
            false,
        );

        let Err(error) = result else {
            bail!("PDF upload should be rejected when disabled");
        };
        ensure!(error.to_string().contains("PDF uploads are disabled"));
        ensure!(!save_root.path().join(&board.short_name).exists());
        Ok(())
    }

    #[test]
    fn primary_upload_rejects_pdf_over_pdf_size_limit() -> anyhow::Result<()> {
        let conn = rusqlite::Connection::open_in_memory().context("open in-memory SQLite")?;
        create_file_hash_table(&conn)?;

        let board = crate::models::Board {
            allow_pdf: true,
            max_pdf_size: 8,
            max_video_size: 1024 * 1024,
            max_audio_size: 1024 * 1024,
            ..crate::test_fixtures::sample_board()
        };
        let uploads_dir = tempfile::tempdir().context("create uploads directory")?;
        let save_root = tempfile::tempdir().context("create save root")?;
        let result = super::process_primary_upload(
            Some(temp_upload("doc.pdf", valid_pdf())?),
            &board,
            &conn,
            uploads_dir
                .path()
                .to_str()
                .context("uploads path is not UTF-8")?,
            save_root
                .path()
                .to_str()
                .context("save path is not UTF-8")?,
            64,
            1024 * 1024,
            1024 * 1024,
            1024 * 1024,
            board.max_pdf_size_bytes(),
            false,
            false,
            false,
        );

        let Err(error) = result else {
            bail!("oversized PDF should be rejected");
        };
        ensure!(error.to_string().contains("Maximum PDF upload size is 8 B"));
        Ok(())
    }

    #[test]
    fn primary_upload_rejects_renamed_non_pdf() -> anyhow::Result<()> {
        let conn = rusqlite::Connection::open_in_memory().context("open in-memory SQLite")?;
        create_file_hash_table(&conn)?;

        let board = crate::models::Board {
            allow_pdf: true,
            ..crate::test_fixtures::sample_board()
        };
        let uploads_dir = tempfile::tempdir().context("create uploads directory")?;
        let save_root = tempfile::tempdir().context("create save root")?;
        let result = super::process_primary_upload(
            Some(temp_upload("not-really.pdf", b"plain text")?),
            &board,
            &conn,
            uploads_dir
                .path()
                .to_str()
                .context("uploads path is not UTF-8")?,
            save_root
                .path()
                .to_str()
                .context("save path is not UTF-8")?,
            64,
            1024 * 1024,
            1024 * 1024,
            1024 * 1024,
            1024 * 1024,
            false,
            false,
            false,
        );

        let Err(error) = result else {
            bail!("renamed non-PDF should be rejected");
        };
        ensure!(error.to_string().contains("File type not allowed"));
        ensure!(!save_root.path().join(&board.short_name).exists());
        Ok(())
    }

    #[test]
    fn primary_upload_accepts_pdf_when_board_enables_pdf() -> anyhow::Result<()> {
        let conn = rusqlite::Connection::open_in_memory().context("open in-memory SQLite")?;
        create_file_hash_table(&conn)?;

        let board = crate::models::Board {
            allow_pdf: true,
            ..crate::test_fixtures::sample_board()
        };
        let uploads_dir = tempfile::tempdir().context("create uploads directory")?;
        let save_root = tempfile::tempdir().context("create save root")?;
        let _override = crate::media::thumbnail::override_pdf_renderer_mode(
            crate::media::thumbnail::TestPdfRendererMode::Unavailable,
        );
        let (uploaded, _) = super::process_primary_upload(
            Some(temp_upload("doc.pdf", valid_pdf())?),
            &board,
            &conn,
            uploads_dir
                .path()
                .to_str()
                .context("uploads path is not UTF-8")?,
            save_root
                .path()
                .to_str()
                .context("save path is not UTF-8")?,
            64,
            1024 * 1024,
            1024 * 1024,
            1024 * 1024,
            1024 * 1024,
            false,
            false,
            false,
        )
        .context("accept PDF upload")?;
        let uploaded = uploaded.context("PDF upload result was empty")?;

        ensure!(uploaded.mime_type == "application/pdf");
        ensure!(uploaded.media_type == crate::models::MediaType::Pdf);
        ensure!(save_root.path().join(uploaded.file_path).exists());
        ensure!(std::path::Path::new(&uploaded.thumb_path)
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("svg")));
        ensure!(save_root.path().join(&uploaded.thumb_path).exists());
        Ok(())
    }
}
