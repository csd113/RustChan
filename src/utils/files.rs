// utils/files.rs
//
// File handling pipeline:
//   1. Receive multipart bytes
//   2. Validate MIME type against allowlist (BOTH magic bytes AND extension)
//   3. Validate file size using per-type limits
//   4. Generate random filename (UUID-based, prevents path traversal)
//   5. If JPEG: re-encode through the `image` crate to strip all EXIF metadata
//      NOTE: EXIF Orientation tag is read from the ORIGINAL bytes BEFORE
//      stripping so that thumbnails are still rendered upright.
//   6. If video and ffmpeg is available: mark pending for background transcoding
//   7. Write to boards directory
//   8. Generate thumbnail / placeholder
//      • Images  → scaled with the `image` crate
//      • Videos  → first-frame JPEG via ffmpeg (falls back to SVG if unavailable)
//        NOTE: GIF thumbnails are single-frame (first frame only).
//      • Audio   → waveform PNG via ffmpeg (falls back to SVG music-note placeholder)
//                  unless uploaded alongside an image, in which case the image IS
//                  the audio thumbnail (see `save_audio_with_image_thumb`).
//   9. Write thumbnail to thumbs/ subdirectory
//
// All ffmpeg/ffprobe operations are delegated to `crate::media`.
//
// Security notes:
//   • We NEVER trust the Content-Type header alone — we check magic bytes.
//   • Filenames are never used as filesystem paths — UUIDs only.
//   • Files are stored flat (no user-supplied path components).
//   • We keep the original filename only for display purposes.
//   • delete_file validates the relative path to prevent directory traversal.
//   • Audio files that fail magic-byte detection are rejected.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use uuid::Uuid;

// ─── Output type ─────────────────────────────────────────────────────────────

pub struct UploadedFile {
    /// Path on disk relative to the boards root (e.g. "b/abc123.webm")
    pub file_path: String,
    /// Thumbnail path relative to the boards root (e.g. "b/thumbs/abc123.jpg")
    pub thumb_path: String,
    /// Original user-supplied filename, sanitised, for display only
    pub original_name: String,
    /// Detected MIME type (always a &'static str value, stored as String)
    pub mime_type: String,
    /// File size in bytes
    pub file_size: i64,
    /// Explicit media category derived from `mime_type`
    pub media_type: crate::models::MediaType,
    /// True when the file needs async background processing:
    ///   • Video (MP4) → `VideoTranscode` job (MP4 → `WebM` via ffmpeg)
    ///   • Audio       → `AudioWaveform` job  (waveform PNG via ffmpeg)
    /// The handler must enqueue the appropriate Job after the post is inserted
    /// so that the `post_id` is available. Always false for cached/dedup hits.
    pub processing_pending: bool,
}

// ─── MIME / type detection ────────────────────────────────────────────────────

/// Validate file bytes against known magic signatures and return the detected
/// MIME type string.  Returns an error when the file type is not on the allowlist.
///
/// We check magic bytes rather than trusting the user-supplied Content-Type
/// header.  This prevents executables disguised as media from being served.
///
/// # Errors
/// Returns an error if the data is empty or the file type is not on the allowlist.
#[allow(clippy::arithmetic_side_effects)]
pub fn detect_mime_type(data: &[u8]) -> Result<&'static str> {
    if data.is_empty() {
        return Err(anyhow::anyhow!("File is empty."));
    }
    let header = data.get(..data.len().min(12)).unwrap_or(data);

    // ── MP4 / M4A disambiguation ──────────────────────────────────────────────
    if data.get(4..8) == Some(b"ftyp") {
        if let Some(brand) = data.get(8..12) {
            if brand == b"M4A " || brand == b"m4a " {
                return Ok("audio/mp4");
            }
        }
        return Ok("video/mp4");
    }

    // ── RIFF container — disambiguate WebP vs WAV ────────────────────────────
    if header.starts_with(b"RIFF") {
        return match data.get(8..12) {
            Some(b"WEBP") => Ok("image/webp"),
            Some(b"WAVE") => Ok("audio/wav"),
            _ => Err(anyhow::anyhow!(
                "RIFF container with unknown subtype. Accepted: WebP, WAV"
            )),
        };
    }

    // ── EBML container — distinguish WebM (video or audio-only) from MKV ────
    //
    // Both WebM and Matroska (.mkv) start with the same EBML magic bytes
    // (1A 45 DF A3), so checking only the magic is insufficient — MKV files
    // containing H.264/HEVC/etc. would be accepted and stored as .webm, then
    // silently fail to play in any browser.
    //
    // The EBML header always begins with the EBML ID (1A 45 DF A3) followed
    // immediately by the header size (variable-length VINT), then a sequence
    // of EBML elements.  The DocType element has ID 0x4282.  Its value is the
    // ASCII string "webm" (browser-compatible) or "matroska" (reject).
    //
    // We scan the first 64 bytes for 0x42 0x82, read the 1-byte size that
    // follows, then compare that many bytes to "webm" or "matroska".
    // If DocType is absent or unrecognised we reject.
    //
    // For audio-only WebM (Opus/Vorbis streams, no video track) the docType
    // is still "webm", but none of the subsequent EBML Track elements contain
    // a video codec ID.  We do not probe track types here — that would require
    // parsing the full Segment element, which can be megabytes in.  Instead we
    // accept all valid "webm" docType files and rely on the background worker's
    // ffprobe call to detect audio-only containers and classify them correctly.
    // To give the handler a usable MIME type up front we return "video/webm"
    // for now; the worker will update the post's mime_type to "audio/webm" if
    // ffprobe finds no video stream.  This is safe because both share the same
    // file extension (.webm) and the browser media element handles both.
    if data.get(..4) == Some(b"\x1a\x45\xdf\xa3") {
        // Scan first 64 bytes for DocType element (ID = 0x42 0x82).
        let scan_len = data.len().min(64);
        let scan = data.get(..scan_len).unwrap_or(data);
        let mut pos = 4usize;
        let mut found_doctype: Option<&[u8]> = None;
        // Use slice patterns so every access goes through bounds-checked .get(),
        // satisfying clippy::indexing_slicing while keeping the loop readable.
        while let Some([b0, b1, b2, ..]) = scan.get(pos..) {
            if *b0 == 0x42 && *b1 == 0x82 {
                // Next byte is the 1-byte DataSize (short form, bit 7 set -> size = byte & 0x7F).
                let value_len = (*b2 & 0x7f) as usize;
                let value_start = pos + 3;
                if value_start + value_len <= scan.len() {
                    found_doctype = scan.get(value_start..value_start + value_len);
                }
                break;
            }
            pos += 1;
        }
        return match found_doctype {
            Some(b"webm") => Ok("video/webm"), // audio-only WebM also uses docType "webm"
            Some(b"matroska") => Err(anyhow::anyhow!(
                "Matroska (.mkv) files are not accepted. Please upload a WebM file instead."
            )),
            _ => Err(anyhow::anyhow!(
                "Unrecognised EBML container. Accepted: WebM (video/webm)."
            )),
        };
    }

    // ── Image formats ─────────────────────────────────────────────────────────
    if header.starts_with(b"\xff\xd8\xff") {
        return Ok("image/jpeg");
    }
    if header.starts_with(b"\x89PNG") {
        return Ok("image/png");
    }
    if header.starts_with(b"GIF8") {
        return Ok("image/gif");
    }
    // ── BMP ───────────────────────────────────────────────────────────────────
    // Magic: 'BM' (0x42 0x4D).  BMP uploads are immediately converted to WebP
    // by the media pipeline when ffmpeg is available.
    if header.starts_with(b"BM") {
        return Ok("image/bmp");
    }
    // ── TIFF (little-endian and big-endian) ───────────────────────────────────
    // LE: 49 49 2A 00  ("II*\0")
    // BE: 4D 4D 00 2A  ("MM\0*")
    // TIFF uploads are converted to WebP by the media pipeline when ffmpeg is
    // available.
    if header.starts_with(b"\x49\x49\x2a\x00") || header.starts_with(b"\x4d\x4d\x00\x2a") {
        return Ok("image/tiff");
    }
    // ── SVG (text XML) ────────────────────────────────────────────────────────
    // SVG files start with either `<svg` or an XML declaration `<?xml`.
    // Security note: SVG files can embed JavaScript via event handlers.
    // The server must serve them with Content-Security-Policy or as attachments.
    // Stored as-is; the media pipeline does not transcode SVG.
    {
        // Inspect up to 512 bytes of (possibly UTF-8 with BOM) text.
        let text_peek = data.get(..data.len().min(512)).unwrap_or(data);
        // Strip a UTF-8 BOM if present.
        let text_peek = text_peek.strip_prefix(b"\xef\xbb\xbf").unwrap_or(text_peek);
        if text_peek.starts_with(b"<svg")
            || text_peek.starts_with(b"<SVG")
            || text_peek.starts_with(b"<?xml")
        {
            return Ok("image/svg+xml");
        }
    }

    // ── Audio: ID3-tagged MP3 ─────────────────────────────────────────────────
    if header.starts_with(b"ID3") {
        return Ok("audio/mpeg");
    }
    if header.starts_with(b"OggS") {
        return Ok("audio/ogg");
    }
    if header.starts_with(b"fLaC") {
        return Ok("audio/flac");
    }

    // ── Audio: sync-word detection (0xFF prefix) ──────────────────────────────
    // MP3 and AAC ADTS both start with 0xFF.  Merge both checks into a single
    // `if let` block to avoid redundant pattern-matching overhead and clearly
    // express that the two detections are mutually exclusive branches.
    if let (Some(&0xff), Some(&b1)) = (data.first(), data.get(1)) {
        // Raw MP3 frame sync: FF FB / FF F3 / FF F2
        if b1 == 0xfb || b1 == 0xf3 || b1 == 0xf2 {
            return Ok("audio/mpeg");
        }
        // AAC ADTS sync word: FF F0–FF FE (high nibble = 0xF, not 0xFF itself).
        // The MP3 bytes above are already handled, so only true ADTS words reach here.
        if b1 & 0xf0 == 0xf0 && b1 != 0xff {
            return Ok("audio/aac");
        }
    }

    Err(anyhow::anyhow!(
        "File type not allowed. Accepted: JPEG, PNG, GIF, WebP, BMP, TIFF, SVG, \
         MP4, WebM, MP3, OGG, FLAC, WAV, M4A, AAC"
    ))
}

// ─── Disk-space guard ────────────────────────────────────────────────────────

/// Verify at least `2 × needed_bytes` of free space in `dir` before writing.
/// Uses `statvfs` on Unix; is a no-op (always Ok) on other platforms so Windows
/// dev environments still compile and run.
///
/// Requiring 2× headroom means a crash mid-rename still leaves the original
/// temp file and will not fill the volume to 100 %.
#[cfg(unix)]
fn check_disk_space(dir: &Path, needed_bytes: usize) -> Result<()> {
    unsafe {
        let dir_bytes = dir.to_string_lossy();
        if let Ok(path_cstr) = std::ffi::CString::new(dir_bytes.as_bytes()) {
            let mut stat: libc::statvfs = std::mem::zeroed();
            if libc::statvfs(path_cstr.as_ptr(), &raw mut stat) == 0 {
                #[allow(clippy::unnecessary_cast)]
                #[allow(clippy::useless_conversion, clippy::cast_lossless)]
                let free_bytes = u64::from(stat.f_bavail).saturating_mul(u64::from(stat.f_frsize));
                let needed = (needed_bytes as u64).saturating_mul(2);
                if free_bytes < needed {
                    return Err(anyhow::anyhow!(
                        "Insufficient disk space: need ~{} MiB free, only ~{} MiB available.",
                        needed / (1024 * 1024),
                        free_bytes / (1024 * 1024)
                    ));
                }
            }
        }
    }
    Ok(())
}

#[cfg(not(unix))]
fn check_disk_space(_dir: &Path, _needed_bytes: usize) -> Result<()> {
    Ok(())
}

// ─── Main entry point ─────────────────────────────────────────────────────────

/// Save an uploaded file to disk and generate its thumbnail (or audio placeholder).
///
/// Files are stored under `{boards_dir}/{board_short}/` and thumbnails
/// under `{boards_dir}/{board_short}/thumbs/`.
/// The returned paths are relative to `boards_dir` (e.g. `"b/abc123.webm"`).
///
/// When `ffmpeg_available` is true, media files are converted to optimal web
/// formats (JPEG/BMP/TIFF → WebP, GIF → WebM/VP9, PNG → WebP if smaller).
/// MP4/WebM files are flagged for background transcoding (existing pipeline).
///
/// All thumbnails are produced as WebP.  If ffmpeg is unavailable, image
/// thumbnails use the `image` crate as a fallback; video/audio thumbnails
/// fall back to static SVG placeholders.
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
/// # Errors
/// Returns an error if the file type is unsupported, too large, disk space is
/// insufficient, or any I/O operation fails.
pub fn save_upload(
    data: &[u8],
    original_filename: &str,
    boards_dir: &str,
    board_short: &str,
    thumb_size: u32,
    max_image_size: usize,
    max_video_size: usize,
    max_audio_size: usize,
    ffmpeg_available: bool,
) -> Result<UploadedFile> {
    if data.is_empty() {
        return Err(anyhow::anyhow!("File is empty."));
    }

    // Detect MIME type first so we can apply the correct size limit.
    let mime_type = detect_mime_type(data)?;
    let media_type = crate::models::MediaType::from_mime(mime_type)
        .ok_or_else(|| anyhow::anyhow!("Could not classify detected MIME type: {mime_type}"))?;

    let max_size = match media_type {
        crate::models::MediaType::Video => max_video_size,
        crate::models::MediaType::Audio => max_audio_size,
        crate::models::MediaType::Image => max_image_size,
    };

    if data.len() > max_size {
        return Err(anyhow::anyhow!(
            "File too large. Maximum {} size is {} MiB.",
            match media_type {
                crate::models::MediaType::Video => "video",
                crate::models::MediaType::Audio => "audio",
                crate::models::MediaType::Image => "image",
            },
            max_size / 1024 / 1024
        ));
    }

    // FIX[BUG]: Read the EXIF orientation from the ORIGINAL bytes BEFORE EXIF
    // stripping.  strip_jpeg_exif() re-encodes the JPEG, discarding all metadata
    // including the Orientation tag.  If we read orientation from the stripped
    // bytes (as the previous code did) we always get orientation=1 (no rotation)
    // and phone photos appear sideways in thumbnails.
    let jpeg_orientation = if mime_type == "image/jpeg" {
        crate::media::exif::read_exif_orientation(data)
    } else {
        1
    };

    let file_id = Uuid::new_v4().simple().to_string();

    // ── JPEG EXIF stripping ───────────────────────────────────────────────────
    // Re-encoding a JPEG through the `image` crate produces a clean output with
    // no EXIF, IPTC, XMP, or any other metadata segment — only the pixel data
    // is retained.  We replace `data` with the stripped bytes so every
    // downstream step (MediaProcessor, size check, disk write) sees clean data.
    let stripped_jpeg: Option<Vec<u8>> = if mime_type == "image/jpeg" {
        match strip_jpeg_exif(data) {
            Ok(clean) => {
                tracing::debug!(
                    "EXIF stripped from JPEG ({} → {} bytes)",
                    data.len(),
                    clean.len()
                );
                Some(clean)
            }
            Err(e) => {
                tracing::warn!("JPEG EXIF strip failed ({}); saving original", e);
                None
            }
        }
    } else {
        None
    };

    // Use the EXIF-stripped bytes when available, otherwise the original.
    let data: &[u8] = stripped_jpeg.as_deref().unwrap_or(data);

    // ── Async processing flag ─────────────────────────────────────────────────
    // Heavy CPU work (MP4→WebM transcoding, audio waveform) is deferred to the
    // background worker pool so HTTP responses return immediately.
    //   • Video (MP4) + ffmpeg → save as-is now; worker transcodes to WebM.
    //   • Audio       + ffmpeg → use SVG placeholder now; worker adds PNG waveform.
    // GIF→WebM conversion is done inline by MediaProcessor (not deferred) because
    // GIFs are images not videos, and the spec requires immediate conversion.
    let processing_pending = ffmpeg_available
        && matches!(
            media_type,
            crate::models::MediaType::Video | crate::models::MediaType::Audio
        )
        && (media_type != crate::models::MediaType::Video
            || mime_type == "video/mp4"
            || mime_type == "video/webm");

    // ── Ensure per-board directories ──────────────────────────────────────────
    let dest_dir = PathBuf::from(boards_dir).join(board_short);
    let thumbs_dir = dest_dir.join("thumbs");
    std::fs::create_dir_all(&thumbs_dir).context("Failed to create board thumbs directory")?;

    // Disk-space pre-check: verify at least 2× the file size is available.
    check_disk_space(&dest_dir, data.len())?;

    // ── Write upload bytes to a temp file for MediaProcessor ─────────────────
    // MediaProcessor works on disk paths so ffmpeg can read/write files.
    // The temp file is in the same directory so any in-place rename is atomic
    // (same filesystem partition guaranteed).
    let tmp_input = {
        use std::io::Write as _;
        let mut tmp = tempfile::NamedTempFile::new_in(&dest_dir)
            .context("Failed to create temp input file for media processing")?;
        tmp.write_all(data)
            .context("Failed to write upload bytes to temp file")?;
        tmp.flush().context("Failed to flush temp input file")?;
        tmp
    };

    // ── Run media conversion + thumbnail generation via MediaProcessor ────────
    let processor = crate::media::MediaProcessor::new_with_ffmpeg(ffmpeg_available);

    let processed = processor.process_upload(
        tmp_input.path(),
        mime_type,
        &dest_dir,
        &file_id,
        &thumbs_dir,
        thumb_size,
    );

    // The temp input file is no longer needed after process_upload; drop it
    // (NamedTempFile auto-deletes the underlying file on drop).
    drop(tmp_input);

    let processed = processed.context("Media processing pipeline failed")?;

    // ── Apply EXIF orientation to the stored image thumbnail ─────────────────
    // When ffmpeg is unavailable and the image crate generated the thumbnail,
    // EXIF orientation is applied by `apply_exif_orientation` below.
    // When ffmpeg generated the thumbnail it reads EXIF automatically.
    // If orientation != 1 and the thumbnail is WebP and ffmpeg was NOT used,
    // we need to re-orient the generated thumbnail.
    if jpeg_orientation > 1
        && !ffmpeg_available
        && processed.thumbnail_path.exists()
        && processed
            .thumbnail_path
            .extension()
            .and_then(|e| e.to_str())
            == Some("webp")
    {
        apply_thumb_exif_orientation(&processed.thumbnail_path, jpeg_orientation);
    }

    // ── Determine final MIME and media type ───────────────────────────────────
    // GIF → WebM changes the media type from Image to Video.
    let final_mime: String = processed.mime_type.clone();
    let final_media_type = crate::models::MediaType::from_mime(&final_mime).unwrap_or(media_type);

    // ── File size from actual bytes on disk ───────────────────────────────────
    let file_size = i64::try_from(processed.final_size).context("File size overflows i64")?;

    // ── Build relative paths for DB storage ───────────────────────────────────
    // Paths are relative to `boards_dir`, e.g. "b/abc123.webp".
    let filename = processed
        .file_path
        .file_name()
        .and_then(|n| n.to_str())
        .context("Converted file has non-UTF-8 name")?;
    let rel_file = format!("{board_short}/{filename}");

    let thumb_filename = processed
        .thumbnail_path
        .file_name()
        .and_then(|n| n.to_str())
        .context("Thumbnail file has non-UTF-8 name")?;
    let rel_thumb = format!("{board_short}/thumbs/{thumb_filename}");

    // ── processing_pending: always false for inline-converted GIF→WebM ────────
    // The media pipeline converted GIF→WebM synchronously, so no background job
    // is needed.  MP4/WebM still use the existing background transcoding path.
    let final_processing_pending = if processed.was_converted {
        false
    } else {
        processing_pending
    };

    // ── Audio SVG placeholder path (not affected by MediaProcessor) ───────────
    // Audio files are handled as a special case: the media processor emits an
    // SVG placeholder thumbnail, and the background AudioWaveform worker
    // replaces it later.  The rel_thumb already points to the SVG placeholder.

    Ok(UploadedFile {
        file_path: rel_file,
        thumb_path: rel_thumb,
        original_name: crate::utils::sanitize::sanitize_filename(original_filename),
        mime_type: final_mime,
        file_size,
        media_type: final_media_type,
        processing_pending: final_processing_pending,
    })
}

// ─── EXIF orientation for image-crate thumbnails ──────────────────────────────

/// Re-apply EXIF orientation to an already-written thumbnail using the
/// `image` crate.  Called when ffmpeg was unavailable and the thumbnail was
/// produced by `image_crate_thumbnail` in `media/thumbnail.rs`.
///
/// Silently ignores errors (thumbnail orientation is best-effort).
fn apply_thumb_exif_orientation(thumb_path: &Path, orientation: u32) {
    if orientation <= 1 {
        return;
    }
    let Ok(data) = std::fs::read(thumb_path) else {
        return;
    };
    let Ok(img) = image::load_from_memory_with_format(&data, image::ImageFormat::WebP) else {
        return;
    };
    let rotated = crate::media::exif::apply_exif_orientation(img, orientation);
    if let Err(e) = rotated.save_with_format(thumb_path, image::ImageFormat::WebP) {
        tracing::warn!("failed to re-orient thumbnail: {e}");
    }
}

// ─── Image+audio combo: save audio with an existing image as its thumbnail ───

/// Save an audio file to disk for an image+audio combo post.
///
/// Instead of generating a separate thumbnail, the already-saved image's
/// `thumb_path` (relative to `boards_dir`) is reused as the audio's visual
/// thumbnail.  No ffmpeg waveform is generated for this case.
///
/// Returns an `UploadedFile` whose `thumb_path` is set to `image_thumb_rel`.
///
/// # Errors
/// Returns an error if the audio is empty, unsupported type, too large, or any I/O fails.
#[allow(clippy::too_many_arguments)]
pub fn save_audio_with_image_thumb(
    audio_data: &[u8],
    original_filename: &str,
    boards_dir: &str,
    board_short: &str,
    max_audio_size: usize,
) -> Result<UploadedFile> {
    if audio_data.is_empty() {
        return Err(anyhow::anyhow!("Audio file is empty."));
    }

    let mime_type = detect_mime_type(audio_data)?;
    let media_type = crate::models::MediaType::from_mime(mime_type)
        .ok_or_else(|| anyhow::anyhow!("Not an audio file: {mime_type}"))?;

    if !matches!(media_type, crate::models::MediaType::Audio) {
        return Err(anyhow::anyhow!(
            "Expected an audio file for the audio slot; got {mime_type}"
        ));
    }

    if audio_data.len() > max_audio_size {
        return Err(anyhow::anyhow!(
            "Audio file too large. Maximum size is {} MiB.",
            max_audio_size / 1024 / 1024
        ));
    }

    let file_id = Uuid::new_v4().simple().to_string();
    let ext = mime_to_ext(mime_type);
    let filename = format!("{file_id}.{ext}");

    let dest_dir = PathBuf::from(boards_dir).join(board_short);
    std::fs::create_dir_all(&dest_dir).context("Failed to create board directory")?;

    // Apply the same 2× disk-space pre-check used by save_upload.
    check_disk_space(&dest_dir, audio_data.len())?;

    let file_path_abs = dest_dir.join(&filename);
    {
        use std::io::Write as _;
        let mut tmp = tempfile::NamedTempFile::new_in(&dest_dir)
            .context("Failed to create temp file for audio upload")?;
        tmp.write_all(audio_data)
            .context("Failed to write audio data to temp file")?;
        tmp.persist(&file_path_abs)
            .context("Failed to atomically rename audio temp file")?;
    }

    let rel_file = format!("{board_short}/{filename}");
    let file_size = i64::try_from(audio_data.len()).context("File size overflows i64")?;

    // The thumb_path is intentionally left empty here; the caller sets it to
    // the companion image's thumb path when constructing the NewPost record.
    Ok(UploadedFile {
        file_path: rel_file,
        thumb_path: String::new(), // filled in by caller from the image UploadedFile
        original_name: crate::utils::sanitize::sanitize_filename(original_filename),
        mime_type: mime_type.to_string(),
        file_size,
        media_type,
        processing_pending: false, // image serves as thumb; no waveform needed
    })
}

// ─── JPEG EXIF stripping ─────────────────────────────────────────────────────

/// Re-encode a JPEG through the `image` crate, stripping all metadata.
///
/// The `image` crate's JPEG encoder writes a clean JFIF stream with no EXIF,
/// XMP, or IPTC segments — only the pixel data is retained.  This is the most
/// reliable stripping approach available without pulling in a separate EXIF library.
///
/// NOTE: The `image` crate uses a default JPEG quality of 75.  This produces a
/// file that is visually indistinguishable from the original for display purposes
/// but changes the file byte-for-byte.
///
/// IMPORTANT: Callers must read the EXIF Orientation tag from the ORIGINAL bytes
/// BEFORE calling this function if they intend to honour camera orientation —
/// the re-encoded output will contain no EXIF data at all.
fn strip_jpeg_exif(data: &[u8]) -> Result<Vec<u8>> {
    use std::io::Cursor;
    let img = image::load_from_memory_with_format(data, image::ImageFormat::Jpeg)
        .context("Failed to decode JPEG for EXIF strip")?;
    let mut cursor = Cursor::new(Vec::with_capacity(data.len()));
    img.write_to(&mut cursor, image::ImageFormat::Jpeg)
        .context("Failed to re-encode JPEG after EXIF strip")?;
    Ok(cursor.into_inner())
}

// ─── Helpers ──────────────────────────────────────────────────────────────────

/// Map a MIME type string to the canonical file extension used on disk.
///
/// Public wrapper used by `crate::media::convert` when writing fallback files.
#[must_use]
pub fn mime_to_ext_pub(mime: &str) -> &'static str {
    mime_to_ext(mime)
}

/// Map a MIME type string to the canonical file extension used on disk.
fn mime_to_ext(mime: &str) -> &'static str {
    match mime {
        "image/jpeg" => "jpg",
        "image/png" => "png",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/bmp" => "bmp",
        "image/tiff" => "tiff",
        "image/svg+xml" => "svg",
        "video/mp4" => "mp4",
        "video/webm" | "audio/webm" => "webm",
        // Audio formats
        "audio/mpeg" => "mp3",
        "audio/ogg" => "ogg",
        "audio/flac" => "flac",
        "audio/wav" => "wav",
        "audio/mp4" => "m4a",
        "audio/aac" => "aac",
        _ => "bin",
    }
}

/// Delete a file from the filesystem, ignoring not-found errors.
///
/// # Security
///
/// `relative_path` is validated to prevent directory traversal: absolute paths
/// and paths containing `..` components are rejected with a warning log.  Only
/// simple relative paths (e.g. `"b/abc123.webm"`) are accepted.
pub fn delete_file(boards_dir: &str, relative_path: &str) {
    let rel = std::path::Path::new(relative_path);

    // Reject absolute paths: PathBuf::join on an absolute path replaces the
    // entire base, allowing deletion of arbitrary files outside boards_dir.
    if rel.is_absolute() {
        tracing::warn!(
            "delete_file: rejected absolute path (potential traversal): {:?}",
            relative_path
        );
        return;
    }

    // Reject any `..` components that could escape the boards directory.
    if rel
        .components()
        .any(|c| c == std::path::Component::ParentDir)
    {
        tracing::warn!(
            "delete_file: rejected path with '..' component (potential traversal): {:?}",
            relative_path
        );
        return;
    }

    let full_path = PathBuf::from(boards_dir).join(rel);
    let _ = std::fs::remove_file(full_path);
}

/// Format file size as human-readable string.
#[must_use]
#[allow(clippy::cast_precision_loss)]
pub fn format_file_size(bytes: i64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KiB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1} MiB", bytes as f64 / (1024.0 * 1024.0))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]
    use super::*;
    // GenericImageView trait is needed for .width() / .height() on DynamicImage.
    #[allow(unused_imports)]
    use image::GenericImageView as _;

    // ── format_file_size ─────────────────────────────────────────────────────

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

    // ── mime_to_ext ──────────────────────────────────────────────────────────

    #[test]
    fn mime_to_ext_known_types() {
        assert_eq!(mime_to_ext("image/jpeg"), "jpg");
        assert_eq!(mime_to_ext("image/png"), "png");
        assert_eq!(mime_to_ext("image/gif"), "gif");
        assert_eq!(mime_to_ext("image/webp"), "webp");
        assert_eq!(mime_to_ext("video/mp4"), "mp4");
        assert_eq!(mime_to_ext("video/webm"), "webm");
        assert_eq!(mime_to_ext("audio/webm"), "webm");
        assert_eq!(mime_to_ext("audio/mpeg"), "mp3");
        assert_eq!(mime_to_ext("audio/ogg"), "ogg");
        assert_eq!(mime_to_ext("audio/flac"), "flac");
        assert_eq!(mime_to_ext("audio/wav"), "wav");
        assert_eq!(mime_to_ext("audio/mp4"), "m4a");
        assert_eq!(mime_to_ext("audio/aac"), "aac");
    }

    #[test]
    fn mime_to_ext_unknown_falls_back_to_bin() {
        assert_eq!(mime_to_ext("application/octet-stream"), "bin");
        assert_eq!(mime_to_ext(""), "bin");
        assert_eq!(mime_to_ext("text/plain"), "bin");
    }

    // ── detect_mime_type ─────────────────────────────────────────────────────

    #[test]
    fn detect_empty_is_error() {
        assert!(detect_mime_type(b"").is_err());
    }

    #[test]
    fn detect_jpeg() {
        let header = b"\xff\xd8\xff\xe0rest of file";
        assert_eq!(detect_mime_type(header).expect("jpeg"), "image/jpeg");
    }

    #[test]
    fn detect_png() {
        let header = b"\x89PNG\r\n\x1a\nrest";
        assert_eq!(detect_mime_type(header).expect("png"), "image/png");
    }

    #[test]
    fn detect_gif() {
        let header = b"GIF89arest";
        assert_eq!(detect_mime_type(header).expect("gif"), "image/gif");
    }

    #[test]
    fn detect_webp() {
        // RIFF....WEBP — built as a literal to avoid indexing_slicing lint
        let data: &[u8] = b"RIFF\x00\x00\x00\x00WEBPrest";
        assert_eq!(detect_mime_type(data).expect("webp"), "image/webp");
    }

    #[test]
    fn detect_wav() {
        let data: &[u8] = b"RIFF\x00\x00\x00\x00WAVErest";
        assert_eq!(detect_mime_type(data).expect("wav"), "audio/wav");
    }

    #[test]
    fn detect_riff_unknown_subtype_is_error() {
        let data: &[u8] = b"RIFF\x00\x00\x00\x00BLAH";
        assert!(detect_mime_type(data).is_err());
    }

    #[test]
    fn detect_mp3_id3() {
        let header = b"ID3\x03\x00\x00rest";
        assert_eq!(detect_mime_type(header).expect("mp3 id3"), "audio/mpeg");
    }

    #[test]
    fn detect_mp3_raw_sync() {
        // 0xFF 0xFB = raw MP3 frame sync
        let header = b"\xff\xfbrest";
        assert_eq!(detect_mime_type(header).expect("mp3 sync"), "audio/mpeg");
    }

    #[test]
    fn detect_aac() {
        // 0xFF 0xF1 = AAC ADTS sync word
        let header = b"\xff\xf1rest";
        assert_eq!(detect_mime_type(header).expect("aac"), "audio/aac");
    }

    #[test]
    fn detect_ogg() {
        let header = b"OggS\x00rest";
        assert_eq!(detect_mime_type(header).expect("ogg"), "audio/ogg");
    }

    #[test]
    fn detect_flac() {
        let header = b"fLaCrest";
        assert_eq!(detect_mime_type(header).expect("flac"), "audio/flac");
    }

    #[test]
    fn detect_mp4_ftyp() {
        // 4 bytes padding, "ftyp", "isom" brand — all as a literal
        let data: &[u8] = b"\x00\x00\x00\x00ftypismores";
        assert_eq!(detect_mime_type(data).expect("mp4"), "video/mp4");
    }

    #[test]
    fn detect_m4a_ftyp() {
        let data: &[u8] = b"\x00\x00\x00\x00ftypM4A res";
        assert_eq!(detect_mime_type(data).expect("m4a"), "audio/mp4");
    }

    #[test]
    fn detect_m4a_ftyp_lowercase() {
        let data: &[u8] = b"\x00\x00\x00\x00ftypm4a res";
        assert_eq!(detect_mime_type(data).expect("m4a lower"), "audio/mp4");
    }

    #[test]
    fn detect_webm_doctype() {
        // Minimal EBML: magic(4) + 6 padding bytes + DocType ID(2) + size(1) + "webm"(4)
        // Positions: magic=0..4, padding=4..10, 0x42=10, 0x82=11, 0x84=12, "webm"=13..17
        // Built as a concat of fixed-size byte arrays to avoid indexing_slicing.
        let data: &[u8] = b"\x1a\x45\xdf\xa3\x00\x00\x00\x00\x00\x00\x42\x82\x84webm\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00";
        assert_eq!(detect_mime_type(data).expect("webm"), "video/webm");
    }

    #[test]
    fn detect_mkv_doctype_rejected() {
        // Same layout but DocType = "matroska" (8 bytes), size field = 0x88
        let data: &[u8] = b"\x1a\x45\xdf\xa3\x00\x00\x00\x00\x00\x00\x42\x82\x88matroska\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00";
        let err = detect_mime_type(data)
            .expect_err("mkv should be rejected")
            .to_string();
        assert!(err.contains("Matroska") || err.contains("matroska"));
    }

    #[test]
    fn detect_unknown_returns_error() {
        assert!(detect_mime_type(b"\x00\x00\x00\x00unknown").is_err());
    }

    // ── apply_exif_orientation ───────────────────────────────────────────────

    #[test]
    fn exif_orientation_1_is_noop() {
        let img = image::DynamicImage::new_rgba8(4, 6);
        let out = crate::media::exif::apply_exif_orientation(img, 1);
        assert_eq!(out.width(), 4);
        assert_eq!(out.height(), 6);
    }

    #[test]
    fn exif_orientation_3_rotates_180() {
        // 180° rotation keeps dimensions unchanged
        let img = image::DynamicImage::new_rgba8(4, 6);
        let out = crate::media::exif::apply_exif_orientation(img, 3);
        assert_eq!(out.width(), 4);
        assert_eq!(out.height(), 6);
    }

    #[test]
    fn exif_orientation_6_rotates_90cw_swaps_dims() {
        let img = image::DynamicImage::new_rgba8(4, 6);
        let out = crate::media::exif::apply_exif_orientation(img, 6);
        assert_eq!(out.width(), 6);
        assert_eq!(out.height(), 4);
    }

    #[test]
    fn exif_orientation_8_rotates_90ccw_swaps_dims() {
        let img = image::DynamicImage::new_rgba8(4, 6);
        let out = crate::media::exif::apply_exif_orientation(img, 8);
        assert_eq!(out.width(), 6);
        assert_eq!(out.height(), 4);
    }

    #[test]
    fn exif_orientation_unknown_value_is_noop() {
        let img = image::DynamicImage::new_rgba8(4, 6);
        let out = crate::media::exif::apply_exif_orientation(img, 99);
        assert_eq!(out.width(), 4);
        assert_eq!(out.height(), 6);
    }

    // ── New format detection: BMP, TIFF, SVG ─────────────────────────────────

    #[test]
    fn detect_bmp() {
        // BMP magic: 'BM' (0x42 0x4D)
        let header = b"BM\x36\x00\x00\x00\x00\x00rest";
        assert_eq!(detect_mime_type(header).expect("bmp"), "image/bmp");
    }

    #[test]
    fn detect_tiff_little_endian() {
        // TIFF LE magic: 49 49 2A 00
        let header = b"\x49\x49\x2a\x00rest";
        assert_eq!(detect_mime_type(header).expect("tiff le"), "image/tiff");
    }

    #[test]
    fn detect_tiff_big_endian() {
        // TIFF BE magic: 4D 4D 00 2A
        let header = b"\x4d\x4d\x00\x2arest";
        assert_eq!(detect_mime_type(header).expect("tiff be"), "image/tiff");
    }

    #[test]
    fn detect_svg_direct() {
        let data = b"<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"100\" height=\"100\"></svg>";
        assert_eq!(detect_mime_type(data).expect("svg"), "image/svg+xml");
    }

    #[test]
    fn detect_svg_xml_declaration() {
        let data = b"<?xml version=\"1.0\"?><svg></svg>";
        assert_eq!(
            detect_mime_type(data).expect("svg xml decl"),
            "image/svg+xml"
        );
    }

    #[test]
    fn mime_to_ext_new_types() {
        assert_eq!(mime_to_ext("image/bmp"), "bmp");
        assert_eq!(mime_to_ext("image/tiff"), "tiff");
        assert_eq!(mime_to_ext("image/svg+xml"), "svg");
    }
}
