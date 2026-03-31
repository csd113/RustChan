use anyhow::Result;

#[allow(clippy::arithmetic_side_effects)]
pub fn detect_mime_type(data: &[u8]) -> Result<&'static str> {
    if data.is_empty() {
        return Err(anyhow::anyhow!("File is empty."));
    }
    let header = data.get(..data.len().min(12)).unwrap_or(data);

    if data.get(4..8) == Some(b"ftyp") {
        if let Some(brand) = data.get(8..12) {
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
        let scan = data.get(..data.len().min(64)).unwrap_or(data);
        if let Some(pos) = scan.windows(2).position(|w| w == [0x42, 0x82]) {
            let size_idx = pos + 2;
            if let Some(&sz) = scan.get(size_idx) {
                let len = usize::from(sz & 0x7F);
                let start = size_idx + 1;
                let end = start.saturating_add(len);
                if let Some(dt) = scan.get(start..end) {
                    if dt.eq_ignore_ascii_case(b"webm") {
                        return Ok("video/webm");
                    }
                    if dt.eq_ignore_ascii_case(b"matroska") {
                        return Err(anyhow::anyhow!(
                            "File type not allowed. Matroska/MKV is not accepted; use WebM."
                        ));
                    }
                }
            }
        }
        return Err(anyhow::anyhow!(
            "File type not allowed. EBML container is not a valid WebM."
        ));
    }

    if header.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return Ok("image/jpeg");
    }
    if header.starts_with(b"\x89PNG\r\n\x1A\n") {
        return Ok("image/png");
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
        "File type not allowed. Accepted: JPEG, PNG, GIF, WebP, BMP, TIFF, \
         MP4, WebM, MP3, OGG, FLAC, WAV, M4A, AAC"
    ))
}
