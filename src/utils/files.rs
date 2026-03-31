// src/utils/files.rs

mod disk_space;
mod jpeg;
mod mime;
mod storage;

pub use mime::{detect_mime_type, fallback_download_mime_type};
pub use storage::{
    delete_file, format_file_size, mime_to_ext_pub, save_audio_with_image_thumb_from_path,
    save_upload_from_path, UploadedFile,
};

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]
    use super::*;
    #[test]
    fn format_bytes_exact() {
        assert_eq!(format_file_size(0), "0 B");
        assert_eq!(format_file_size(1), "1 B");
        assert_eq!(format_file_size(1023), "1023 B");
    }

    #[test]
    fn format_kib_boundary() {
        assert_eq!(format_file_size(1024), "1.0 KiB");
        assert_eq!(format_file_size(1536), "1.5 KiB");
        assert_eq!(format_file_size(1024 * 1024 - 1), "1024.0 KiB");
    }

    #[test]
    fn format_mib() {
        assert_eq!(format_file_size(1024 * 1024), "1.0 MiB");
        assert_eq!(format_file_size(1024 * 1024 * 2), "2.0 MiB");
    }

    #[test]
    fn mime_to_ext_known_types() {
        assert_eq!(mime_to_ext_pub("image/jpeg"), "jpg");
        assert_eq!(mime_to_ext_pub("image/png"), "png");
        assert_eq!(mime_to_ext_pub("image/gif"), "gif");
        assert_eq!(mime_to_ext_pub("image/webp"), "webp");
        assert_eq!(mime_to_ext_pub("video/mp4"), "mp4");
        assert_eq!(mime_to_ext_pub("video/webm"), "webm");
        assert_eq!(mime_to_ext_pub("audio/webm"), "webm");
        assert_eq!(mime_to_ext_pub("audio/mpeg"), "mp3");
        assert_eq!(mime_to_ext_pub("audio/ogg"), "ogg");
        assert_eq!(mime_to_ext_pub("audio/flac"), "flac");
        assert_eq!(mime_to_ext_pub("audio/wav"), "wav");
        assert_eq!(mime_to_ext_pub("audio/mp4"), "m4a");
        assert_eq!(mime_to_ext_pub("audio/aac"), "aac");
    }

    #[test]
    fn detect_empty_is_error() {
        assert!(detect_mime_type(b"").is_err());
    }

    #[test]
    fn detect_jpeg() {
        assert_eq!(
            detect_mime_type(b"\xff\xd8\xff\xe0rest").expect("jpeg"),
            "image/jpeg"
        );
    }

    #[test]
    fn detect_png() {
        assert_eq!(
            detect_mime_type(b"\x89PNG\r\n\x1a\nrest").expect("png"),
            "image/png"
        );
    }

    #[test]
    fn detect_gif() {
        assert_eq!(detect_mime_type(b"GIF89arest").expect("gif"), "image/gif");
    }

    #[test]
    fn detect_webp() {
        assert_eq!(
            detect_mime_type(b"RIFF\x00\x00\x00\x00WEBPrest").expect("webp"),
            "image/webp"
        );
    }

    #[test]
    fn detect_wav() {
        assert_eq!(
            detect_mime_type(b"RIFF\x00\x00\x00\x00WAVErest").expect("wav"),
            "audio/wav"
        );
    }

    #[test]
    fn detect_mp3_raw_sync() {
        assert_eq!(
            detect_mime_type(b"\xff\xfbrest").expect("mp3"),
            "audio/mpeg"
        );
    }

    #[test]
    fn detect_aac() {
        assert_eq!(detect_mime_type(b"\xff\xf1rest").expect("aac"), "audio/aac");
    }

    #[test]
    fn detect_webm_doctype() {
        let data: &[u8] = b"\x1a\x45\xdf\xa3\x00\x00\x00\x00\x00\x00\x42\x82\x84webm\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00";
        assert_eq!(detect_mime_type(data).expect("webm"), "video/webm");
    }

    #[test]
    fn exif_orientation_6_rotates_90cw_swaps_dims() {
        let img = image::DynamicImage::new_rgba8(4, 6);
        let out = crate::media::exif::apply_exif_orientation(img, 6);
        assert_eq!(out.width(), 6);
        assert_eq!(out.height(), 4);
    }
}
