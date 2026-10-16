/// Free-space preflight checks.
mod disk_space;
/// JPEG metadata normalization.
mod jpeg;
/// Content-based MIME detection.
mod mime;
/// Validated upload persistence.
pub(crate) mod storage;

/// Inspect the content signature used to distinguish accepted video containers.
pub(crate) fn video_container_mime(header: &[u8]) -> anyhow::Result<&'static str> {
    mime::detect_mime_type(header)
}
pub use mime::fallback_download_mime_type;
pub use storage::{
    classify_upload_mime, delete_file_checked, format_file_size, mime_to_ext_pub,
    save_audio_with_image_thumb_from_path, save_upload_from_path, validate_upload_from_path,
    SaveUploadOptions, UploadedFile,
};
/// Internal marker for a `WebM` upload whose stream type remains unknown.
pub(crate) const AMBIGUOUS_WEBM_MIME: &str = "application/x-rustchan-ambiguous-webm";
