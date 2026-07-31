// src/utils/files/mime.rs

use anyhow::Result;

/// Detect the MIME type of an uploaded file from its magic bytes.
///
/// # Errors
/// Returns an error when the header is empty or the file type is not one of
/// `RustChan`'s accepted upload formats.
pub(super) fn detect_mime_type(data: &[u8]) -> Result<&'static str> {
    if data.is_empty() {
        return Err(anyhow::anyhow!("File is empty."));
    }
    let header = data.get(..data.len().min(12)).unwrap_or(data);

    if data.get(4..8) == Some(b"ftyp") {
        if let Some(brand) = data.get(8..12) {
            if has_ftyp_brand(data, &[b"heic", b"heix", b"hevc", b"hevx"]) {
                return Ok("image/heic");
            }
            if has_ftyp_brand(data, &[b"mif1", b"msf1"]) {
                return Ok("image/heif");
            }
            if brand == b"M4A " || brand == b"m4a " {
                return Ok("audio/mp4");
            }
        }
        return Ok("video/mp4");
    }

    if header.starts_with(b"RIFF") {
        return match data.get(8..12) {
            Some(b"WEBP") => Ok("image/webp"),
            Some(b"WAVE") => Ok("audio/wav"),
            _ => Err(anyhow::anyhow!(
                "RIFF container with unknown subtype. Accepted: WebP, WAV"
            )),
        };
    }

    if header.starts_with(&[0x1A, 0x45, 0xDF, 0xA3]) {
        let scan = data.get(..data.len().min(512)).unwrap_or(data);
        if let Some(doc_type) = ebml_doc_type(scan) {
            if doc_type.eq_ignore_ascii_case(b"webm") {
                return Ok("video/webm");
            }
            if doc_type.eq_ignore_ascii_case(b"matroska") {
                return Ok("video/x-matroska");
            }
        }
        return Err(anyhow::anyhow!(
            "File type not allowed. EBML container is not valid WebM or Matroska/MKV media."
        ));
    }

    if header.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return Ok("image/jpeg");
    }
    if header.starts_with(b"\x89PNG\r\n\x1A\n") {
        return Ok("image/png");
    }
    if header.starts_with(b"%PDF-") {
        return Ok("application/pdf");
    }
    if header.starts_with(b"GIF87a") || header.starts_with(b"GIF89a") {
        return Ok("image/gif");
    }
    if header.starts_with(b"ID3") || matches!(header.get(..2), Some([0xFF, 0xFB | 0xF3 | 0xF2])) {
        return Ok("audio/mpeg");
    }
    if header.starts_with(&[0xFF, 0xF1]) || header.starts_with(&[0xFF, 0xF9]) {
        return Ok("audio/aac");
    }
    if header.starts_with(b"OggS") {
        if data
            .get(..data.len().min(512))
            .is_some_and(|scan| scan.windows(b"OpusHead".len()).any(|w| w == b"OpusHead"))
        {
            return Ok("audio/opus");
        }
        return Ok("audio/ogg");
    }
    if header.starts_with(b"fLaC") {
        return Ok("audio/flac");
    }
    if header.starts_with(b"BM") {
        return Ok("image/bmp");
    }
    if header.starts_with(b"II*\0") || header.starts_with(b"MM\0*") {
        return Ok("image/tiff");
    }

    let probe = data.get(..data.len().min(256)).unwrap_or(data);
    if let Ok(text) = std::str::from_utf8(probe) {
        let trimmed = text.trim_start_matches('\u{FEFF}').trim_start();
        if trimmed.starts_with("<svg")
            || trimmed.starts_with("<?xml") && trimmed.to_ascii_lowercase().contains("<svg")
        {
            return Ok("image/svg+xml");
        }
    }

    Err(anyhow::anyhow!(
        "File type not allowed. Accepted: JPEG, PNG, GIF, WebP, HEIC, HEIF, BMP, TIFF, \
         MP4, WebM, MP3, OGG, FLAC, WAV, M4A, AAC, PDF"
    ))
}

/// Return whether an ISO base-media header declares any accepted brand.
fn has_ftyp_brand(data: &[u8], accepted: &[&[u8; 4]]) -> bool {
    data.get(8..data.len().min(64)).is_some_and(|brands| {
        brands.chunks_exact(4).any(|brand| {
            accepted
                .iter()
                .any(|candidate| brand == candidate.as_slice())
        })
    })
}

/// Extract a bounded EBML document-type value from the sniff buffer.
fn ebml_doc_type(scan: &[u8]) -> Option<&[u8]> {
    let pos = scan.windows(2).position(|w| w == [0x42, 0x82])?;
    let size_idx = pos.checked_add(2)?;
    let size = *scan.get(size_idx)?;
    let len = usize::from(size & 0x7F);
    if len == 0 || len > 32 {
        return None;
    }
    let start = size_idx.checked_add(1)?;
    let end = start.checked_add(len)?;
    scan.get(start..end)
}

#[must_use]
/// Return the safe generic MIME type used for unrecognized downloads.
pub const fn fallback_download_mime_type() -> &'static str {
    "application/octet-stream"
}
