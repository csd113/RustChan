//! Streaming multipart envelopes, bounded form intake, and temporary uploads.

use super::LOG_TARGET;
use crate::error::{AppError, Result};
use crate::middleware::validate_csrf;
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
#[expect(
    clippy::redundant_pub_crate,
    reason = "intake items retain their existing crate visibility through the handlers facade"
)]
pub(crate) const PUBLIC_MULTIPART_REQUEST_MAX_BYTES: usize =
    PUBLIC_MULTIPART_AGGREGATE_MAX_BYTES + 4 * 1024 * 1024;
/// Maximum preamble or per-field header envelope retained by the multipart parser.
#[expect(
    clippy::redundant_pub_crate,
    reason = "intake items retain their existing crate visibility through the handlers facade"
)]
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
#[expect(
    clippy::redundant_pub_crate,
    reason = "intake items retain their existing crate visibility through the handlers facade"
)]
pub(crate) const PUBLIC_UPLOAD_TIMEOUT: Duration = Duration::from_mins(10);

/// Rejects malformed multipart preambles and field-header blocks before Multer
/// can retain a request-sized buffer while searching for their delimiters.
///
/// The wrapped body remains fully streaming: only parser state and the boundary
/// matcher are retained here, and field data is passed through unchanged.
#[expect(
    clippy::redundant_pub_crate,
    reason = "intake items retain their existing crate visibility through the handlers facade"
)]
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
    /// Count shared by normal parsing and rejected-upload recovery.
    fields_seen: usize,
    /// Aggregate field-data bytes observed across the request.
    bytes_seen: usize,
    /// Additional allowance activated only after a board upload limit rejects a file.
    rejected_upload: Option<RejectedUploadRecovery>,
}

/// One shared allowance for rejected field data and trailing draft controls.
struct RejectedUploadRecovery {
    /// Rejected excess and trailing controls consumed within the shared allowance.
    bytes_seen: usize,
    /// Single deadline for the rejected field and every trailing control.
    deadline: tokio::time::Instant,
    /// Whether the rejected field ended before the allowance was exhausted.
    field_complete: bool,
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
        if let Some(recovery) = &mut self.rejected_upload {
            recovery.bytes_seen = recovery.bytes_seen.saturating_add(len);
            if recovery.bytes_seen > REJECTED_UPLOAD_DISCARD_MAX_BYTES
                || tokio::time::Instant::now() >= recovery.deadline
            {
                return Err(AppError::UploadTooLarge(
                    "Rejected upload recovery limit reached.".into(),
                ));
            }
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

#[expect(
    clippy::redundant_pub_crate,
    reason = "intake items retain their existing crate visibility through the handlers facade"
)]
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
// Upload fields stream directly to disk and stop staging at the configured
// board limit. A small, time-bounded discard can then deliver the original 413
// without retaining or processing excess bytes.
//
// Text fields (CSRF token, post body, …) use the same chunked parser with a
// small fixed cap, so disabling Axum's route-level body limit for upload routes
// does not leave text fields unbounded.

/// Maximum additional upload bytes discarded after a board-size rejection.
const REJECTED_UPLOAD_DISCARD_MAX_BYTES: usize = 8 * 1024 * 1024;
/// Maximum time spent discarding a rejected upload before closing its request.
const REJECTED_UPLOAD_DISCARD_TIMEOUT: Duration = Duration::from_secs(1);

/// Discards a small finite rejected field so browsers can receive its original error.
/// Excess bytes never reach disk, and every read retains the aggregate upload budget.
async fn discard_rejected_upload_field(
    field: &mut axum::extract::multipart::Field<'_>,
    budget: &mut PublicMultipartBudget,
    initial_excess_bytes: usize,
) -> bool {
    let Some(deadline) = tokio::time::Instant::now().checked_add(REJECTED_UPLOAD_DISCARD_TIMEOUT)
    else {
        return false;
    };
    budget.rejected_upload = Some(RejectedUploadRecovery {
        bytes_seen: initial_excess_bytes,
        deadline,
        field_complete: false,
    });
    if initial_excess_bytes > REJECTED_UPLOAD_DISCARD_MAX_BYTES {
        return false;
    }
    let discard = async {
        loop {
            // Check ready streams too; the outer timer also bounds stalled reads.
            if tokio::time::Instant::now() >= deadline {
                return false;
            }
            let Ok(chunk) = field.chunk().await else {
                return false;
            };
            let Some(chunk) = chunk else {
                return true;
            };
            if budget.note_chunk(chunk.len()).is_err() {
                return false;
            }
        }
    };
    let complete = matches!(tokio::time::timeout_at(deadline, discard).await, Ok(true));
    if let Some(recovery) = &mut budget.rejected_upload {
        recovery.field_complete = complete;
    }
    complete
}

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
            // Keep the media permit until this bounded discard completes; reject
            // regardless of the outcome and never stage or inspect excess bytes.
            let excess = size_bytes
                .saturating_add(chunk.len())
                .saturating_sub(max_bytes);
            let _discard_completed =
                discard_rejected_upload_field(&mut field, budget, excess).await;
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
        .filter(|filename| !filename.is_empty())
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
            .is_some_and(|filename| !filename.is_empty())
        {
            return Err(AppError::BadRequest("Uploaded file is empty.".into()));
        }
        return Ok(None);
    }
    Ok(Some((upload, fname)))
}

#[expect(
    clippy::redundant_pub_crate,
    reason = "intake items retain their existing crate visibility through the handlers facade"
)]
pub(crate) struct TempUpload {
    pub temp_file: tempfile::NamedTempFile,
    pub sniff_bytes: Vec<u8>,
    pub size_bytes: usize,
}

/// Parsed fields from a post/thread creation multipart form.
#[expect(
    clippy::redundant_pub_crate,
    reason = "intake items retain their existing crate visibility through the handlers facade"
)]
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
    /// Duration in seconds (parsed from value + unit).
    pub poll_duration_secs: Option<i64>,
    /// Sage — when true the reply must not bump the thread.
    pub sage: bool,
    /// Server-side CAPTCHA challenge id submitted by posting forms when enabled.
    pub captcha_id: String,
    /// Human-entered CAPTCHA answer submitted by posting forms when enabled.
    pub captcha_answer: String,
}

/// Request resources and a bounded draft retained even when parsing fails.
#[expect(
    clippy::redundant_pub_crate,
    reason = "intake items retain their existing crate visibility through the handlers facade"
)]
pub(crate) struct PostMultipartContext<'a> {
    /// Existing concurrency gate held while upload bytes are staged or discarded.
    pub media_upload_gate: &'a crate::middleware::MediaUploadGate,
    /// Escaped at rendering; records only bounded recoverable user controls.
    pub draft: &'a mut crate::templates::forms::PostFormState,
}

/// Records only known bounded user controls; never tokens, CAPTCHA answers or uploads.
fn capture_post_draft(
    name: &str,
    value: &str,
    draft: &mut crate::templates::forms::PostFormState,
) -> Result<()> {
    match name {
        "name" => value.clone_into(&mut draft.name),
        "subject" => value.clone_into(&mut draft.subject),
        "body" => value.clone_into(&mut draft.body),
        "sage" => {
            draft.sage = value == "1"
                || value.eq_ignore_ascii_case("on")
                || value.eq_ignore_ascii_case("true");
        }
        "poll_question" if value.chars().count() <= 500 => {
            value.clone_into(&mut draft.poll.get_or_insert_default().question);
        }
        "poll_option" => {
            let value = value.trim();
            if !value.is_empty() {
                let poll = draft.poll.get_or_insert_default();
                if poll.options.len() >= 20 || value.chars().count() > 200 {
                    return Err(AppError::BadRequest(
                        "Poll recovery control exceeds its limit.".into(),
                    ));
                }
                poll.options.push(value.to_owned());
            }
        }
        "poll_duration_value" => {
            value.clone_into(&mut draft.poll.get_or_insert_default().duration_value);
        }
        "poll_duration_unit" => {
            value.clone_into(&mut draft.poll.get_or_insert_default().duration_unit);
        }
        "poll_question" => {
            return Err(AppError::BadRequest(
                "Poll recovery question exceeds its limit.".into(),
            ));
        }
        _ => {}
    }
    Ok(())
}

/// Reads trailing controls only after a finite rejected field; shares its cap and deadline.
async fn recover_rejected_post_tail(
    multipart: &mut Multipart,
    budget: &mut PublicMultipartBudget,
    draft: &mut crate::templates::forms::PostFormState,
) {
    let Some(recovery) = budget
        .rejected_upload
        .as_ref()
        .filter(|state| state.field_complete)
    else {
        return;
    };
    let deadline = recovery.deadline;
    let recover = async {
        loop {
            if tokio::time::Instant::now() >= deadline {
                return Err(AppError::BadRequest("Draft recovery timed out.".into()));
            }
            let next_field = multipart
                .next_field()
                .await
                .map_err(|error| multipart_read_error("draft recovery", &error))?;
            let Some(mut field) = next_field else {
                return Ok(());
            };
            budget.note_field()?;
            let name = field.name().unwrap_or_default().to_owned();
            match name.as_str() {
                "name"
                | "subject"
                | "body"
                | "sage"
                | "poll_question"
                | "poll_option"
                | "poll_duration_value"
                | "poll_duration_unit" => {
                    let value = read_text_field(field, budget).await?;
                    capture_post_draft(&name, &value, draft)?;
                }
                "file" | "audio_file" | "image_file" => {
                    draft.had_attachments |= field
                        .file_name()
                        .is_some_and(|filename| !filename.is_empty());
                    loop {
                        let next_chunk = field.chunk().await.map_err(|error| {
                            multipart_read_error("rejected upload tail", &error)
                        })?;
                        let Some(chunk) = next_chunk else {
                            break;
                        };
                        budget.note_chunk(chunk.len())?;
                    }
                }
                _ => discard_unknown_public_multipart_field(field, budget).await?,
            }
        }
    };
    if matches!(tokio::time::timeout_at(deadline, recover).await, Ok(Ok(()))) {
        draft.incomplete = false;
    }
}

/// Retains the original upload error after bounded best-effort draft recovery.
async fn recover_upload_parse_error<T>(
    result: Result<T>,
    multipart: &mut Multipart,
    budget: &mut PublicMultipartBudget,
    draft: &mut crate::templates::forms::PostFormState,
) -> Result<T> {
    if result.is_err() {
        recover_rejected_post_tail(multipart, budget, draft).await;
    }
    result
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
#[expect(
    clippy::redundant_pub_crate,
    reason = "intake items retain their existing crate visibility through the handlers facade"
)]
pub(crate) async fn parse_post_multipart(
    mut multipart: Multipart,
    csrf_cookie: Option<&str>,
    max_image_size: usize,
    max_video_size: usize,
    max_audio_size: usize,
    max_pdf_size: usize,
    context: PostMultipartContext<'_>,
) -> Result<PostFormData> {
    let PostMultipartContext {
        media_upload_gate,
        draft,
    } = context;
    draft.incomplete = true;
    draft.poll = Some(crate::templates::forms::PollFormState {
        duration_unit: "hours".into(),
        ..Default::default()
    });
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
    let mut poll_duration_value = String::new();
    let mut poll_duration_unit = String::from("hours");
    let mut sage = false;
    let mut captcha_id = String::new();
    let mut captcha_answer = String::new();
    let mut budget = PublicMultipartBudget::default();
    let mut seen_upload_slots = HashSet::new();
    let mut seen_scalar_fields = HashSet::new();
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
        if let Some(field_name) = field.name().filter(|control_name| {
            matches!(
                *control_name,
                "_csrf"
                    | "submission_token"
                    | "name"
                    | "subject"
                    | "body"
                    | "deletion_token"
                    | "sage"
                    | "captcha_id"
                    | "captcha_answer"
                    | "poll_question"
                    | "poll_duration_value"
                    | "poll_duration_unit"
            )
        }) {
            if !seen_scalar_fields.insert(field_name.to_owned()) {
                return Err(AppError::BadRequest(format!(
                    "Duplicate posting field '{field_name}'."
                )));
            }
        }
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
            Some("name") => {
                name = read_text_field(field, &mut budget).await?;
                capture_post_draft("name", &name, draft)?;
            }
            Some("subject") => {
                subject = read_text_field(field, &mut budget).await?;
                capture_post_draft("subject", &subject, draft)?;
            }
            Some("body") => {
                body = read_text_field(field, &mut budget).await?;
                capture_post_draft("body", &body, draft)?;
            }
            Some("deletion_token") => deletion_token = read_text_field(field, &mut budget).await?,
            Some("sage") => {
                let v = read_text_field(field, &mut budget).await?;
                sage = v == "1" || v.eq_ignore_ascii_case("on") || v.eq_ignore_ascii_case("true");
                capture_post_draft("sage", &v, draft)?;
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
                capture_post_draft("poll_question", &v, draft)?;
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
                    capture_post_draft("poll_option", &trimmed, draft)?;
                    poll_options.push(trimmed);
                }
            }
            Some("poll_duration_value") => {
                poll_duration_value = read_text_field(field, &mut budget).await?;
                capture_post_draft("poll_duration_value", &poll_duration_value, draft)?;
            }
            Some("poll_duration_unit") => {
                poll_duration_unit = read_text_field(field, &mut budget).await?;
                capture_post_draft("poll_duration_unit", &poll_duration_unit, draft)?;
            }
            Some("file") => {
                if !seen_upload_slots.insert("file") {
                    return Err(AppError::BadRequest(
                        "Duplicate upload field 'file'.".into(),
                    ));
                }
                draft.had_attachments |= field
                    .file_name()
                    .is_some_and(|filename| !filename.is_empty());
                let upload_result = read_upload_field(
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
                .await;
                draft.had_attachments |= upload_result.as_ref().is_ok_and(Option::is_some);
                file =
                    recover_upload_parse_error(upload_result, &mut multipart, &mut budget, draft)
                        .await?;
            }
            Some("audio_file") => {
                if !seen_upload_slots.insert("audio_file") {
                    return Err(AppError::BadRequest(
                        "Duplicate upload field 'audio_file'.".into(),
                    ));
                }
                draft.had_attachments |= field
                    .file_name()
                    .is_some_and(|filename| !filename.is_empty());
                let upload_result = read_upload_field(
                    field,
                    max_audio_size,
                    "audio",
                    "audio_file",
                    &mut budget,
                    media_upload_gate,
                    &mut media_upload_guard,
                )
                .await;
                draft.had_attachments |= upload_result.as_ref().is_ok_and(Option::is_some);
                audio_file =
                    recover_upload_parse_error(upload_result, &mut multipart, &mut budget, draft)
                        .await?;
            }
            Some("image_file") => {
                if !seen_upload_slots.insert("image_file") {
                    return Err(AppError::BadRequest(
                        "Duplicate upload field 'image_file'.".into(),
                    ));
                }
                draft.had_attachments |= field
                    .file_name()
                    .is_some_and(|filename| !filename.is_empty());
                let upload_result = read_upload_field(
                    field,
                    max_image_size,
                    "image",
                    "image_file",
                    &mut budget,
                    media_upload_gate,
                    &mut media_upload_guard,
                )
                .await;
                draft.had_attachments |= upload_result.as_ref().is_ok_and(Option::is_some);
                image_file =
                    recover_upload_parse_error(upload_result, &mut multipart, &mut budget, draft)
                        .await?;
            }
            _ => {
                discard_unknown_public_multipart_field(field, &mut budget).await?;
            }
        }
    }

    draft.incomplete = false;

    // Convert duration value + unit → seconds (saturating to prevent overflow).
    // The unit is validated against an explicit allow-list (case-insensitive) so
    // that a tampered form field does not silently multiply by an arbitrary factor.
    let poll_duration_secs = if poll_question.trim().is_empty() {
        None
    } else {
        match poll_duration_value.trim().parse::<i64>().ok() {
            None => None,
            Some(v) => {
                let unit = poll_duration_unit.trim().to_ascii_lowercase();
                let secs = match unit.as_str() {
                    "minutes" => v.saturating_mul(60),
                    "hours" => v.saturating_mul(3600),
                    "days" => v.saturating_mul(86_400),
                    other => {
                        return Err(AppError::BadRequest(format!(
                            "Invalid poll duration unit '{other}'. Use 'minutes', 'hours', or 'days'."
                        )));
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
