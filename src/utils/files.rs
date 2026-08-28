// src/utils/files.rs

/// Free-space preflight checks.
mod disk_space;
/// JPEG metadata normalization.
mod jpeg;
/// Content-based MIME detection.
mod mime;
/// Validated upload persistence.
pub(crate) mod storage;

pub use mime::fallback_download_mime_type;
pub use storage::{
    classify_upload_mime, delete_file_checked, format_file_size, mime_to_ext_pub,
    save_audio_with_image_thumb_from_path, save_upload_from_path, validate_upload_from_path,
    SaveUploadOptions, UploadedFile,
};
/// Internal marker for a `WebM` upload whose stream type remains unknown.
pub(crate) const AMBIGUOUS_WEBM_MIME: &str = "application/x-rustchan-ambiguous-webm";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_bytes_exact() {
        assert_eq!(format_file_size(0), "0 B", "zero-byte formatting changed");
        assert_eq!(format_file_size(1), "1 B", "single-byte formatting changed");
        assert_eq!(
            format_file_size(1023),
            "1023 B",
            "sub-KiB formatting changed"
        );
    }

    #[test]
    fn format_kib_boundary() {
        assert_eq!(
            format_file_size(1024),
            "1.0 KiB",
            "KiB boundary formatting changed"
        );
        assert_eq!(
            format_file_size(1536),
            "1.5 KiB",
            "fractional KiB formatting changed"
        );
        assert_eq!(
            format_file_size(1024 * 1024 - 1),
            "1024.0 KiB",
            "upper KiB boundary formatting changed"
        );
    }

    #[test]
    fn format_mib() {
        assert_eq!(
            format_file_size(1024 * 1024),
            "1.0 MiB",
            "MiB boundary formatting changed"
        );
        assert_eq!(
            format_file_size(1024 * 1024 * 2),
            "2.0 MiB",
            "whole MiB formatting changed"
        );
        assert_eq!(
            format_file_size(1024 * 1024 * 1024 - 1),
            "1024.0 MiB",
            "upper MiB boundary formatting changed"
        );
    }

    #[test]
    fn format_gib() {
        assert_eq!(
            format_file_size(1024 * 1024 * 1024),
            "1.0 GiB",
            "GiB boundary formatting changed"
        );
        assert_eq!(
            format_file_size(2 * 1024 * 1024 * 1024),
            "2.0 GiB",
            "whole GiB formatting changed"
        );
    }

    #[test]
    fn mime_to_ext_known_types() {
        for (mime, extension) in [
            ("image/jpeg", "jpg"),
            ("image/png", "png"),
            ("image/gif", "gif"),
            ("image/webp", "webp"),
            ("image/heic", "heic"),
            ("image/heif", "heif"),
            ("video/mp4", "mp4"),
            ("video/webm", "webm"),
            ("video/x-matroska", "mkv"),
            ("video/matroska", "mkv"),
            ("audio/webm", "webm"),
            ("audio/mpeg", "mp3"),
            ("audio/ogg", "ogg"),
            ("application/ogg", "ogg"),
            ("audio/opus", "opus"),
            ("audio/flac", "flac"),
            ("audio/x-flac", "flac"),
            ("audio/wav", "wav"),
            ("audio/x-wav", "wav"),
            ("audio/mp4", "m4a"),
            ("audio/x-m4a", "m4a"),
            ("audio/aac", "aac"),
            ("audio/x-aac", "aac"),
        ] {
            assert_eq!(
                mime_to_ext_pub(mime),
                extension,
                "canonical extension changed for {mime}"
            );
        }
    }

    #[test]
    fn detect_empty_is_error() {
        assert!(
            mime::detect_mime_type(b"").is_err(),
            "empty input must not receive a MIME classification"
        );
    }

    #[test]
    fn detect_jpeg() -> anyhow::Result<()> {
        let detected = mime::detect_mime_type(b"\xff\xd8\xff\xe0rest")?;
        anyhow::ensure!(detected == "image/jpeg", "JPEG signature was misclassified");
        Ok(())
    }

    #[test]
    fn detect_png() -> anyhow::Result<()> {
        let detected = mime::detect_mime_type(b"\x89PNG\r\n\x1a\nrest")?;
        anyhow::ensure!(detected == "image/png", "PNG signature was misclassified");
        Ok(())
    }

    #[test]
    fn detect_gif() -> anyhow::Result<()> {
        let detected = mime::detect_mime_type(b"GIF89arest")?;
        anyhow::ensure!(detected == "image/gif", "GIF signature was misclassified");
        Ok(())
    }

    #[test]
    fn detect_webp() -> anyhow::Result<()> {
        let detected = mime::detect_mime_type(b"RIFF\x00\x00\x00\x00WEBPrest")?;
        anyhow::ensure!(detected == "image/webp", "WebP signature was misclassified");
        Ok(())
    }

    #[test]
    fn detect_heic_ftyp_brand() -> anyhow::Result<()> {
        let detected = mime::detect_mime_type(b"\x00\x00\x00\x18ftypheic\x00\x00\x00\x00")?;
        anyhow::ensure!(detected == "image/heic", "HEIC brand was misclassified");
        Ok(())
    }

    #[test]
    fn detect_heif_ftyp_brand() -> anyhow::Result<()> {
        let detected = mime::detect_mime_type(b"\x00\x00\x00\x18ftypmif1\x00\x00\x00\x00")?;
        anyhow::ensure!(detected == "image/heif", "HEIF brand was misclassified");
        Ok(())
    }

    #[test]
    fn detect_heic_compatible_ftyp_brand() -> anyhow::Result<()> {
        let detected = mime::detect_mime_type(b"\x00\x00\x00\x20ftypmif1\x00\x00\x00\x00heic")?;
        anyhow::ensure!(
            detected == "image/heic",
            "compatible HEIC brand was misclassified"
        );
        Ok(())
    }

    #[test]
    fn detect_wav() -> anyhow::Result<()> {
        let detected = mime::detect_mime_type(b"RIFF\x00\x00\x00\x00WAVErest")?;
        anyhow::ensure!(detected == "audio/wav", "WAV signature was misclassified");
        Ok(())
    }

    #[test]
    fn detect_mp3_raw_sync() -> anyhow::Result<()> {
        let detected = mime::detect_mime_type(b"\xff\xfbrest")?;
        anyhow::ensure!(detected == "audio/mpeg", "MP3 sync word was misclassified");
        Ok(())
    }

    #[test]
    fn detect_aac() -> anyhow::Result<()> {
        let detected = mime::detect_mime_type(b"\xff\xf1rest")?;
        anyhow::ensure!(detected == "audio/aac", "AAC sync word was misclassified");
        Ok(())
    }

    #[test]
    fn detect_webm_doctype() -> anyhow::Result<()> {
        let data: &[u8] = b"\x1a\x45\xdf\xa3\x00\x00\x00\x00\x00\x00\x42\x82\x84webm\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00";
        let detected = mime::detect_mime_type(data)?;
        anyhow::ensure!(detected == "video/webm", "WebM doctype was misclassified");
        Ok(())
    }

    #[test]
    fn detect_matroska_doctype() -> anyhow::Result<()> {
        let data: &[u8] = b"\x1a\x45\xdf\xa3\xa3\x42\x86\x81\x01\x42\xf7\x81\x01\x42\xf2\x81\x04\x42\xf3\x81\x08\x42\x82\x88matroska\x42\x87\x81\x04";
        let detected = mime::detect_mime_type(data)?;
        anyhow::ensure!(
            detected == "video/x-matroska",
            "Matroska doctype was misclassified"
        );
        Ok(())
    }

    #[test]
    fn detect_ogg_opus_header() -> anyhow::Result<()> {
        let data: &[u8] = b"OggS\x00\x02\x00\x00\x00\x00\x00\x00\x00\x00OpusHead\x01\x01\x38\x01";
        let detected = mime::detect_mime_type(data)?;
        anyhow::ensure!(
            detected == "audio/opus",
            "Ogg Opus header was misclassified"
        );
        Ok(())
    }

    #[test]
    fn exif_orientation_6_rotates_90cw_swaps_dims() {
        let img = image::DynamicImage::new_rgba8(4, 6);
        let out = crate::media::exif::apply_exif_orientation(img, 6);
        assert_eq!(out.width(), 6, "orientation 6 must swap image width");
        assert_eq!(out.height(), 4, "orientation 6 must swap image height");
    }
}
