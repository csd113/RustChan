//! Regressions for real upload rejections and their HTTP adaptation.

use super::classify_upload_error;
use crate::{error::AppError, handlers::board, utils::files};
use anyhow::{Context as _, Result};
use axum::{
    body::Body,
    extract::{FromRequest as _, Multipart, Request},
    http::StatusCode,
    response::IntoResponse as _,
};
use std::io::Write as _;

macro_rules! ensure {
    ($condition:expr_2021, $message:literal) => {
        if !$condition {
            anyhow::bail!($message);
        }
    };
}

fn rejected<T>(result: Result<T>) -> Result<anyhow::Error> {
    result
        .err()
        .context("expected the actual upload producer to reject the fixture")
}

fn options(root: &str, limit: usize) -> files::SaveUploadOptions<'_> {
    files::SaveUploadOptions {
        original_filename: "fixture",
        boards_dir: root,
        board_short: "test",
        thumb_size: 16,
        max_image_size: limit,
        max_video_size: limit,
        max_audio_size: limit,
        max_pdf_size: limit,
        ffmpeg_available: false,
        allow_any_files: false,
    }
}

fn wav_fixture() -> Vec<u8> {
    [
        b"RIFF".as_slice(),
        &38_u32.to_le_bytes(),
        b"WAVEfmt ",
        &16_u32.to_le_bytes(),
        &1_u16.to_le_bytes(),
        &1_u16.to_le_bytes(),
        &8_000_u32.to_le_bytes(),
        &16_000_u32.to_le_bytes(),
        &2_u16.to_le_bytes(),
        &16_u16.to_le_bytes(),
        b"data",
        &2_u32.to_le_bytes(),
        &[0, 0],
    ]
    .concat()
}

async fn parsed_audio(limit: usize) -> crate::error::Result<crate::handlers::PostFormData> {
    let token = crate::utils::crypto::make_csrf_form_token(
        "fixture-token",
        &crate::config::CONFIG.cookie_secret,
    );
    let mut body = format!("--test-boundary\r\nContent-Disposition: form-data; name=\"_csrf\"\r\n\r\n{token}\r\n--test-boundary\r\nContent-Disposition: form-data; name=\"audio_file\"; filename=\"image.png\"\r\nContent-Type: image/png\r\n\r\n").into_bytes();
    body.extend_from_slice(b"\x89PNG\r\n\x1a\n");
    body.extend_from_slice(b"\r\n--test-boundary--\r\n");
    let request = Request::builder()
        .header(
            "content-type",
            "multipart/form-data; boundary=test-boundary",
        )
        .body(Body::from(body))
        .map_err(|error| AppError::Internal(error.into()))?;
    let multipart = Multipart::from_request(request, &())
        .await
        .map_err(|error| AppError::BadRequest(error.to_string()))?;
    let gate = crate::middleware::MediaUploadGate::new();
    let mut draft = crate::templates::forms::PostFormState::default();
    crate::handlers::parse_post_multipart(
        multipart,
        Some("fixture-token"),
        100,
        100,
        limit,
        100,
        crate::handlers::PostMultipartContext {
            media_upload_gate: &gate,
            draft: &mut draft,
        },
    )
    .await
}

#[tokio::test]
async fn secondary_audio_type_and_size_keep_processed_http_semantics() -> Result<()> {
    let native_error = parsed_audio(7)
        .await
        .err()
        .context("multipart must reject cap-plus-one audio")?;
    let native = crate::handlers::native_post_error_response(native_error, None);
    ensure!(
        native.status() == StatusCode::PAYLOAD_TOO_LARGE,
        "native multipart rejection stays 413"
    );
    let xhr_error = parsed_audio(7)
        .await
        .err()
        .context("multipart must reject cap-plus-one audio")?;
    let xhr = board::xhr_post_error_response(xhr_error)?;
    ensure!(
        xhr.status() == StatusCode::OK,
        "handled multipart XHR keeps successful transport"
    );
    ensure!(
        xhr.headers()
            .get("x-rustchan-error-status")
            .is_some_and(|value| value == "413"),
        "multipart XHR carries semantic 413"
    );
    let mut parsed = parsed_audio(8).await?;
    let (audio, filename) = parsed
        .audio_file
        .take()
        .context("at-limit audio slot is accepted by intake")?;
    drop(parsed);
    let mut source = tempfile::NamedTempFile::new()?;
    let wav = wav_fixture();
    source.write_all(&wav)?;
    let root = tempfile::tempdir()?;
    let root = root.path().to_str().context("UTF-8 test directory")?;
    let size_error = rejected(files::save_audio_with_image_thumb_from_path(
        source.path(),
        &wav,
        wav.len(),
        "audio.wav",
        root,
        "test",
        wav.len().saturating_sub(1),
    ))?;
    let size_error = classify_upload_error(size_error);
    let (size_status, size_message) = board::handled_post_error_status(size_error)?;
    ensure!(
        size_status == StatusCode::PAYLOAD_TOO_LARGE,
        "audio storage size rejection must remain 413"
    );
    ensure!(
        size_message.starts_with("Audio file too large"),
        "retain the producer's actionable size message"
    );
    let size_response = board::xhr_error_response(size_status, &size_message)?;
    ensure!(
        size_response.status() == size_status,
        "processed XHR failures retain their HTTP status"
    );
    ensure!(
        size_response
            .headers()
            .get("x-rustchan-error-status")
            .is_some_and(|value| value == "413"),
        "processed XHR carries semantic 413"
    );

    let type_error = rejected(files::save_audio_with_image_thumb_from_path(
        audio.temp_file.path(),
        &audio.sniff_bytes,
        audio.size_bytes,
        &filename,
        root,
        "test",
        8,
    ))?;
    let type_error = classify_upload_error(type_error);
    let (type_status, type_message) = board::handled_post_error_status(type_error)?;
    ensure!(
        type_status == StatusCode::UNSUPPORTED_MEDIA_TYPE,
        "wrong-type secondary audio must be 415"
    );
    let type_response = board::xhr_error_response(type_status, &type_message)?;
    ensure!(
        type_response.status() == type_status,
        "processed wrong-type XHR retains 415 transport"
    );
    Ok(())
}

#[test]
fn primary_pdf_limit_boundary_survives_private_context() -> Result<()> {
    let bytes = b"%PDF-1.7\n%%EOF\n";
    let mut source = tempfile::NamedTempFile::new()?;
    source.write_all(bytes)?;
    let root = tempfile::tempdir()?;
    let root = root.path().to_str().context("UTF-8 test directory")?;
    files::validate_upload_from_path(
        source.path(),
        bytes,
        bytes.len(),
        &options(root, bytes.len()),
    )?;
    let error = rejected(
        files::validate_upload_from_path(
            source.path(),
            bytes,
            bytes.len(),
            &options(root, bytes.len().saturating_sub(1)),
        )
        .context("private diagnostic /operator/private/path"),
    )?;
    let error = classify_upload_error(error);
    let (status, message) = board::handled_post_error_status(error)?;
    ensure!(
        status == StatusCode::PAYLOAD_TOO_LARGE,
        "typed size rejection must survive context"
    );
    ensure!(
        !message.contains("private"),
        "public rejection must not expose diagnostic context"
    );
    Ok(())
}

#[test]
fn malformed_upload_and_filesystem_errors_keep_safe_responses() -> Result<()> {
    let bytes = b"\x89PNG\r\n\x1a\n";
    let mut source = tempfile::NamedTempFile::new()?;
    source.write_all(bytes)?;
    let root = tempfile::tempdir()?;
    let root_str = root.path().to_str().context("UTF-8 test directory")?;
    let malformed_error = rejected(files::validate_upload_from_path(
        source.path(),
        bytes,
        bytes.len(),
        &options(root_str, 100),
    ))?;
    let malformed_error = classify_upload_error(malformed_error);
    let (malformed_status, malformed_message) = board::handled_post_error_status(malformed_error)?;
    ensure!(
        malformed_status == StatusCode::UNPROCESSABLE_ENTITY,
        "malformed processed upload remains 422"
    );
    ensure!(
        malformed_message == "The image header is malformed. Check that the image is valid.",
        "malformed header retains safe advice"
    );
    let ebml = b"\x1a\x45\xdf\xa3invalid";
    let ebml_error = rejected(files::classify_upload_mime(source.path(), ebml, false))?;
    let (ebml_status, _) = board::handled_post_error_status(classify_upload_error(ebml_error))?;
    ensure!(
        ebml_status == StatusCode::UNSUPPORTED_MEDIA_TYPE,
        "invalid EBML remains a disallowed type"
    );
    let missing = root.path().join("private-missing.png");
    let filesystem_error = rejected(files::validate_upload_from_path(
        &missing,
        bytes,
        bytes.len(),
        &options(root_str, 100),
    ))?;
    let filesystem_error = classify_upload_error(filesystem_error);
    ensure!(
        matches!(&filesystem_error, AppError::Internal(_)),
        "filesystem failures stay internal"
    );
    if let AppError::Internal(cause) = &filesystem_error {
        ensure!(
            cause
                .chain()
                .any(<dyn std::error::Error + 'static>::is::<std::io::Error>),
            "filesystem source chains survive adaptation"
        );
    }
    let filesystem_response = filesystem_error.into_response();
    ensure!(
        matches!(filesystem_response.extensions().get::<crate::error::ErrorPage>(), Some(crate::error::ErrorPage::Message(filesystem_message)) if !filesystem_message.contains("private-missing")),
        "internal response does not reveal local paths"
    );
    ensure!(
        filesystem_response.status() == StatusCode::INTERNAL_SERVER_ERROR,
        "filesystem failures remain 500"
    );
    Ok(())
}

#[test]
fn publish_preflight_checks_destination_and_removes_its_probe() -> Result<()> {
    let root = tempfile::tempdir()?;
    let root_text = root.path().to_str().context("UTF-8 upload fixture")?;
    let stage = root.path().join(".pending/upload-fixture");
    std::fs::create_dir_all(&stage)?;
    std::fs::write(stage.join("source.png"), b"staged source")?;
    // Staging is writable even when the actual board destination cannot be used.
    std::fs::write(root.path().join("test"), b"destination is a file")?;
    ensure!(
        super::preflight_publish_directories(root_text, &["test/photo.png".to_owned()]).is_err(),
        "a writable staging area must not hide an unusable final destination"
    );
    std::fs::remove_file(root.path().join("test"))?;
    super::preflight_publish_directories(root_text, &["test/photo.png".to_owned()])?;
    ensure!(
        std::fs::read_dir(root.path().join("test"))?
            .next()
            .is_none(),
        "preflight must not publish artifacts or retain probe files"
    );
    Ok(())
}

#[test]
fn committed_upload_failure_is_not_reported_as_success() -> Result<()> {
    let root = tempfile::tempdir()?;
    let root_text = root.path().to_str().context("UTF-8 upload fixture")?;
    let stage = crate::pending_fs::create_stage_root(root_text, "upload")?;
    let pool = r2d2::Pool::builder()
        .max_size(1)
        .build(r2d2_sqlite::SqliteConnectionManager::memory())?;
    let uploads = super::ProcessedUploads {
        primary: None,
        audio: None,
        pending_finalize: Some(super::PendingUploadFinalize {
            op_id: "committed-upload-fixture".to_owned(),
            payload: crate::pending_fs::UploadFinalizePayload {
                stage_dir: stage.display().to_string(),
                relative_paths: vec!["test/missing.png".to_owned()],
                optional_paths: Vec::new(),
                artifact_sha256: std::collections::BTreeMap::default(),
                primary_hash: None,
                primary_file_path: None,
                primary_thumb_path: None,
                primary_mime_type: None,
            },
        }),
    };
    let error = super::finalize_pending_uploads(&pool, root_text, &uploads, "/test/thread/1#p1")
        .err()
        .context("missing committed artifact must produce a warning, not success")?;
    let AppError::Conflict(message) = error else {
        anyhow::bail!("committed upload failure must keep its actionable public response");
    };
    ensure!(
        message.contains("saved at /test/thread/1#p1")
            && message.contains("Do not create another post"),
        "the author must know the post already exists before retrying"
    );
    ensure!(
        !message.contains(root_text) && stage.exists(),
        "hide internal paths and retain staged recovery evidence"
    );
    Ok(())
}
