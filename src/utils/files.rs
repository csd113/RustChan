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
// Security notes:
//   • We NEVER trust the Content-Type header alone — we check magic bytes.
//   • Filenames are never used as filesystem paths — UUIDs only.
//   • Files are stored flat (no user-supplied path components).
//   • We keep the original filename only for display purposes.
//   • ffmpeg is called with an explicit argument array (not via shell).
//   • Audio files that fail magic-byte detection are rejected.
//   • delete_file validates the relative path to prevent directory traversal.

use anyhow::{Context, Result};
use image::{imageops::FilterType, GenericImageView, ImageFormat};
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
        "File type not allowed. Accepted: JPEG, PNG, GIF, WebP, MP4, WebM, \
         MP3, OGG, FLAC, WAV, M4A, AAC"
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
                let free_bytes = u64::from(stat.f_bavail) * stat.f_frsize;
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
/// When `ffmpeg_available` is true, uploaded MP4/WebM files are flagged for
/// background transcoding.  Audio files receive an SVG placeholder immediately;
/// the waveform PNG is generated by the background worker.
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
        read_exif_orientation(data)
    } else {
        1
    };

    // Use `Uuid::simple()` to produce a 32-char hex string directly, avoiding the
    // intermediate hyphenated string and the extra allocation of `.replace('-', "")`.
    let file_id = Uuid::new_v4().simple().to_string();

    // ── JPEG EXIF stripping ───────────────────────────────────────────────────
    // Re-encoding a JPEG through the `image` crate produces a clean output with
    // no EXIF, IPTC, XMP, or any other metadata segment — only the pixel data
    // is retained.  This is the recommended approach when the crate is already
    // in the dependency tree.  We replace `data` with the stripped bytes so
    // every downstream step (transcoding, size check, disk write) sees clean data.
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
    // The handler enqueues the correct Job after db::create_post gives it post_id.
    let processing_pending = ffmpeg_available
        && matches!(
            media_type,
            // MP4 always needs transcoding to WebM.
            // WebM is flagged pending so the worker can probe the codec and
            // transcode AV1 → VP9 when necessary.  VP8/VP9 WebM files are
            // detected and skipped cheaply inside the worker.
            crate::models::MediaType::Video | crate::models::MediaType::Audio
        )
        && (media_type != crate::models::MediaType::Video
            || mime_type == "video/mp4"
            || mime_type == "video/webm");

    // Always save the original bytes — no inline transcoding.
    let (final_data, final_mime): (&[u8], &'static str) = (data, mime_type);

    // File size is derived from the data we're actually saving.
    let file_size = i64::try_from(final_data.len()).context("File size overflows i64")?;

    let ext = mime_to_ext(final_mime);
    let filename = format!("{file_id}.{ext}");

    // Ensure per-board directories exist: boards_dir/{board_short}/ and thumbs/
    let dest_dir = PathBuf::from(boards_dir).join(board_short);
    let thumbs_dir = dest_dir.join("thumbs");
    std::fs::create_dir_all(&thumbs_dir).context("Failed to create board thumbs directory")?;

    let file_path_abs = dest_dir.join(&filename);

    // Disk-space pre-check: verify at least 2× the file size is available
    // before writing.  Uses statvfs on Unix; skipped on other platforms.
    check_disk_space(&dest_dir, final_data.len())?;

    // Write via a temp file in the same directory, then atomically rename.
    // This guarantees no partial/corrupt file survives a crash or OOM mid-write.
    // tempfile::NamedTempFile::new_in writes to a UUID-named .tmp in the same
    // directory, ensuring the rename is always on the same filesystem (POSIX atomic).
    {
        use std::io::Write as _;
        let mut tmp = tempfile::NamedTempFile::new_in(&dest_dir)
            .context("Failed to create temp file for upload")?;
        tmp.write_all(final_data)
            .context("Failed to write upload data to temp file")?;
        tmp.persist(&file_path_abs)
            .context("Failed to atomically rename upload temp file")?;
    }

    // Paths relative to boards_dir (e.g. "b/abc123.webm", "b/thumbs/abc123.jpg")
    let rel_file = format!("{board_short}/{filename}");
    let board_dir_str = dest_dir.to_string_lossy().into_owned();

    // Generate thumbnail / placeholder based on media type.
    let thumb_rel = match media_type {
        crate::models::MediaType::Video => {
            // Use ffmpeg for first-frame thumbnail; SVG fallback if unavailable.
            let (name, _) = generate_video_thumb(
                final_data,
                &board_dir_str,
                board_short,
                &file_id,
                thumb_size,
                ffmpeg_available,
            );
            name
        }
        crate::models::MediaType::Audio => {
            // When ffmpeg is available the background worker will generate a
            // waveform PNG and update this post's thumb_path.  For now write
            // the SVG placeholder so the post is immediately renderable.
            let svg_rel = format!("{board_short}/thumbs/{file_id}.svg");
            let svg_path = PathBuf::from(boards_dir).join(&svg_rel);
            if let Err(e) = generate_audio_placeholder(&svg_path) {
                tracing::warn!("Failed to write audio SVG placeholder: {}", e);
            }
            svg_rel
        }
        crate::models::MediaType::Image => {
            let thumb_ext = match mime_type {
                "image/png" => "png",
                "image/gif" => "gif",
                "image/webp" => "webp",
                _ => "jpg",
            };
            let thumb_rel_name = format!("{board_short}/thumbs/{file_id}.{thumb_ext}");
            let thumb_abs = PathBuf::from(boards_dir).join(&thumb_rel_name);
            // Pass the pre-read orientation so the thumbnail reflects the
            // camera's physical orientation even after EXIF has been stripped.
            generate_image_thumb(data, mime_type, &thumb_abs, thumb_size, jpeg_orientation)
                .context("Failed to generate image thumbnail")?;
            thumb_rel_name
        }
    };

    Ok(UploadedFile {
        file_path: rel_file,
        thumb_path: thumb_rel,
        original_name: crate::utils::sanitize::sanitize_filename(original_filename),
        mime_type: final_mime.to_string(),
        file_size,
        media_type,
        processing_pending,
    })
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

// ─── Audio waveform thumbnail ─────────────────────────────────────────────────

/// Generate a waveform PNG thumbnail for an audio file using ffmpeg's
/// `showwavespic` filter.
///
/// The output is a `width × height` greyscale-on-dark PNG that gives the
/// post a visual identity without revealing anything about the audio content
/// beyond its amplitude envelope.
///
/// Security: arguments are passed as an explicit array — no shell invocation.
///
/// Temp files: the input is written via `tempfile::NamedTempFile` so it is
/// automatically deleted on drop even if the function returns early.
fn ffmpeg_audio_waveform(
    audio_data: &[u8],
    output_path: &Path,
    width: u32,
    height: u32,
) -> Result<()> {
    use std::io::Write as _;
    use std::process::Command;

    // Auto-cleaned temp input (deleted when `temp_in` is dropped).
    let mut temp_in = tempfile::Builder::new()
        .prefix("chan_aud_")
        .suffix(".tmp")
        .tempfile()
        .context("Failed to create temp audio input file")?;
    temp_in
        .write_all(audio_data)
        .context("Failed to write temp audio for waveform")?;
    temp_in.flush().context("Failed to flush temp audio file")?;

    let temp_in_str = temp_in
        .path()
        .to_str()
        .context("Temp audio path contains non-UTF-8 characters")?;

    // Output uses a UUID-named path; cleaned up manually after rename.
    let temp_dir = std::env::temp_dir();
    let tmp_id = Uuid::new_v4().simple().to_string();
    let temp_out = temp_dir.join(format!("chan_wav_{tmp_id}.png"));
    let temp_out_str = temp_out
        .to_str()
        .context("Temp waveform output path contains non-UTF-8 characters")?;

    // showwavespic: renders the entire file as a single static image.
    // split_channels=0 → mono composite.
    let vf = format!("showwavespic=s={width}x{height}:colors=#00c840|#007020:split_channels=0");

    let output = Command::new("ffmpeg")
        .args([
            "-loglevel",
            "error",
            "-i",
            temp_in_str,
            "-lavfi",
            &vf,
            "-frames:v",
            "1",
            "-y",
            temp_out_str,
        ])
        .output();

    // `temp_in` (NamedTempFile) is dropped here, auto-deleting the input file.
    drop(temp_in);

    let out = output.context("ffmpeg not found or failed to spawn")?;

    if out.status.success() && temp_out.exists() {
        std::fs::rename(&temp_out, output_path).context("Failed to move waveform PNG")?;
        Ok(())
    } else {
        let _ = std::fs::remove_file(&temp_out);
        Err(anyhow::anyhow!(
            "ffmpeg waveform exit {}: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

// ─── Video transcoding ───────────────────────────────────────────────────────

/// Transcode any video file to `WebM` (VP9 + Opus) using ffmpeg.
///
/// Returns the transcoded `WebM` bytes on success, or an error on failure.
/// The caller is responsible for falling back to the original data on error.
///
/// Encoding settings:
///   VP9  — `-deadline good -cpu-used 4` for a good quality/speed balance.
///           CRF 33 with unconstrained average bitrate (`-b:v 0`) — pure CRF
///           mode is the correct libvpx-vp9 approach; combining `-b:v 0` with
///           `-maxrate` / `-bufsize` is not supported by the encoder and causes
///           exit-234 "Rate control parameters set without a bitrate".
///           `-row-mt 1` enables row-based multithreading (significant speedup
///           on multi-core hosts, no quality cost).
///           `-tile-columns 2` improves both encode parallelism and decode
///           performance on multi-core playback devices (including mobile).
///   Opus — `-b:a 96k` — transparent quality for speech and music.
///
/// Security: all arguments passed as separate array elements — no shell.
///
/// Temp files: the input is written via `tempfile::NamedTempFile` so it is
/// automatically deleted on drop even if the function returns early.
fn ffmpeg_transcode_webm(video_data: &[u8]) -> Result<Vec<u8>> {
    use std::io::Write as _;
    use std::process::Command;

    // Auto-cleaned temp input.
    let mut temp_in = tempfile::Builder::new()
        .prefix("chan_in_")
        .suffix(".tmp")
        .tempfile()
        .context("Failed to create temp video input file")?;
    temp_in
        .write_all(video_data)
        .context("Failed to write temp video for transcoding")?;
    temp_in.flush().context("Failed to flush temp video file")?;

    let in_str = temp_in
        .path()
        .to_str()
        .context("Temp input path is non-UTF-8")?;

    let temp_dir = std::env::temp_dir();
    let tmp_id = Uuid::new_v4().simple().to_string();
    let temp_out = temp_dir.join(format!("chan_out_{tmp_id}.webm"));
    let out_str = temp_out.to_str().context("Temp output path is non-UTF-8")?;

    let output = Command::new("ffmpeg")
        .args([
            "-loglevel",
            "error",
            "-i",
            in_str,
            "-c:v",
            "libvpx-vp9",
            "-crf",
            "33",
            "-b:v",
            "0",
            "-deadline",
            "good",
            "-cpu-used",
            "4",
            "-row-mt",
            "1",
            "-tile-columns",
            "2",
            "-threads",
            "0",
            "-c:a",
            "libopus",
            "-b:a",
            "96k",
            "-y",
            out_str,
        ])
        .output();

    // Drop the NamedTempFile to auto-delete the input.
    drop(temp_in);

    let out = output.context("ffmpeg not found or failed to spawn")?;

    if out.status.success() && temp_out.exists() {
        let webm = std::fs::read(&temp_out).context("Failed to read transcoded WebM output")?;
        let _ = std::fs::remove_file(&temp_out);
        Ok(webm)
    } else {
        let _ = std::fs::remove_file(&temp_out);
        Err(anyhow::anyhow!(
            "ffmpeg transcode exit {}: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

// ─── Video thumbnail ──────────────────────────────────────────────────────────

/// Try ffmpeg first-frame extraction; fall back to SVG placeholder on failure.
/// Returns (boards_dir-relative thumb path, absolute path).
fn generate_video_thumb(
    video_data: &[u8],
    board_dir: &str,
    board_short: &str,
    file_id: &str,
    thumb_size: u32,
    ffmpeg_available: bool,
) -> (String, PathBuf) {
    // Only attempt ffmpeg if it was detected at startup.
    if ffmpeg_available {
        let jpg_rel = format!("{board_short}/thumbs/{file_id}.jpg");
        let jpg_path = PathBuf::from(board_dir).join(format!("thumbs/{file_id}.jpg"));

        match ffmpeg_first_frame(video_data, &jpg_path, thumb_size) {
            Ok(()) => {
                tracing::debug!("ffmpeg thumbnail generated for {}", file_id);
                return (jpg_rel, jpg_path);
            }
            Err(e) => {
                tracing::warn!("ffmpeg thumbnail failed ({}), using SVG placeholder", e);
            }
        }
    } else {
        tracing::debug!(
            "ffmpeg not available — using video SVG placeholder for {}",
            file_id
        );
    }

    // Fall back to SVG placeholder
    let svg_rel = format!("{board_short}/thumbs/{file_id}.svg");
    let svg_path = PathBuf::from(board_dir).join(format!("thumbs/{file_id}.svg"));
    if let Err(e) = generate_video_placeholder(&svg_path) {
        tracing::error!("Failed to write video SVG placeholder: {}", e);
    }
    (svg_rel, svg_path)
}

/// Shell out to ffmpeg to extract the first frame as a scaled JPEG.
///
/// Security: all arguments are passed as separate array elements — no shell
/// invocation, no injection surface.
///
/// The video bytes are written to a temp file via `tempfile::NamedTempFile`
/// (auto-deleted on drop), ffmpeg runs on that file, the JPEG output is moved
/// to `output_path`, and the temp file is cleaned up.
fn ffmpeg_first_frame(video_data: &[u8], output_path: &Path, thumb_size: u32) -> Result<()> {
    use std::io::Write as _;
    use std::process::Command;

    // Auto-cleaned temp input.
    let mut temp_in = tempfile::Builder::new()
        .prefix("chan_vid_")
        .suffix(".tmp")
        .tempfile()
        .context("Failed to create temp video input file")?;
    temp_in
        .write_all(video_data)
        .context("Failed to write temp video for ffmpeg")?;
    temp_in.flush().context("Failed to flush temp video file")?;

    let temp_in_str = temp_in
        .path()
        .to_str()
        .context("Temp video path contains non-UTF-8 characters")?;

    let temp_dir = std::env::temp_dir();
    let tmp_id = Uuid::new_v4().simple().to_string();
    let temp_out = temp_dir.join(format!("chan_thm_{tmp_id}.jpg"));
    let temp_out_str = temp_out
        .to_str()
        .context("Temp output path contains non-UTF-8 characters")?;

    // scale=W:-2 : scale width to thumb_size, height to nearest even number
    let vf = format!("scale={thumb_size}:-2");

    // No shell invocation — explicit argument array only.
    let output = Command::new("ffmpeg")
        .args([
            "-loglevel",
            "error",
            "-i",
            temp_in_str,
            "-vframes",
            "1",
            "-ss",
            "0",
            "-vf",
            &vf,
            "-y",
            temp_out_str,
        ])
        .output();

    // Drop NamedTempFile — auto-deletes the temp input regardless of outcome.
    drop(temp_in);

    let out = output.context("ffmpeg not found or failed to spawn")?;

    if out.status.success() && temp_out.exists() {
        std::fs::rename(&temp_out, output_path).context("Failed to move ffmpeg output")?;
        Ok(())
    } else {
        let _ = std::fs::remove_file(&temp_out);
        Err(anyhow::anyhow!(
            "ffmpeg exit {}: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

/// Minimal SVG play-button fallback (used when ffmpeg is unavailable).
fn generate_video_placeholder(output_path: &Path) -> Result<()> {
    let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="250" height="250" viewBox="0 0 250 250">
  <rect width="250" height="250" fill="#0a0f0a"/>
  <circle cx="125" cy="125" r="60" fill="#0d120d" stroke="#00c840" stroke-width="2"/>
  <polygon points="108,95 108,155 165,125" fill="#00c840"/>
  <text x="125" y="215" text-anchor="middle" fill="#3a4a3a" font-family="monospace" font-size="12">VIDEO</text>
</svg>"##;
    std::fs::write(output_path, svg)?;
    Ok(())
}

// ─── Audio placeholder ────────────────────────────────────────────────────────

/// SVG music-note placeholder written for every audio upload.
/// No real thumbnail is generated for audio files.
fn generate_audio_placeholder(output_path: &Path) -> Result<()> {
    let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="250" height="250" viewBox="0 0 250 250">
  <rect width="250" height="250" fill="#0a0f0a"/>
  <circle cx="125" cy="125" r="60" fill="#0d120d" stroke="#00c840" stroke-width="2"/>
  <text x="125" y="140" text-anchor="middle" fill="#00c840" font-family="monospace" font-size="48">&#9835;</text>
  <text x="125" y="215" text-anchor="middle" fill="#3a4a3a" font-family="monospace" font-size="12">AUDIO</text>
</svg>"##;
    std::fs::write(output_path, svg)?;
    Ok(())
}

// ─── Image thumbnail ──────────────────────────────────────────────────────────

/// Read the EXIF Orientation tag from JPEG bytes.
///
/// Returns the orientation value (1–8) or 1 (no rotation) if the tag is
/// absent or unreadable.  Only JPEG files carry reliable EXIF orientation;
/// PNG/WebP/GIF do not use this tag.
///
/// Values follow the EXIF spec:
///   1 = normal (0°), 2 = flip-H, 3 = 180°, 4 = flip-V,
///   5 = transpose, 6 = 90° CW, 7 = transverse, 8 = 90° CCW
///
/// NOTE: This must be called on the ORIGINAL bytes before any EXIF-stripping
/// re-encode.  Once EXIF has been stripped, this function will always return 1.
fn read_exif_orientation(data: &[u8]) -> u32 {
    use std::io::Cursor;
    let Ok(exif) = exif::Reader::new().read_from_container(&mut Cursor::new(data)) else {
        return 1;
    };
    exif.get_field(exif::Tag::Orientation, exif::In::PRIMARY)
        .and_then(|f| {
            if let exif::Value::Short(ref v) = f.value {
                v.first().copied().map(u32::from)
            } else {
                None
            }
        })
        .unwrap_or(1)
}

/// Apply an EXIF orientation transformation to a decoded `DynamicImage`.
///
/// This corrects the pixel layout so that thumbnails appear upright regardless
/// of which way the camera was held when the photo was taken.  The `image`
/// crate operations used here are pure in-memory pixel transforms — no I/O.
fn apply_exif_orientation(img: image::DynamicImage, orientation: u32) -> image::DynamicImage {
    use image::imageops;
    match orientation {
        2 => image::DynamicImage::ImageRgba8(imageops::flip_horizontal(&img)),
        3 => image::DynamicImage::ImageRgba8(imageops::rotate180(&img)),
        4 => image::DynamicImage::ImageRgba8(imageops::flip_vertical(&img)),
        5 => {
            // Transpose = rotate 90° CW then flip horizontally
            let rot = imageops::rotate90(&img);
            image::DynamicImage::ImageRgba8(imageops::flip_horizontal(&rot))
        }
        6 => image::DynamicImage::ImageRgba8(imageops::rotate90(&img)),
        7 => {
            // Transverse = rotate 90° CW then flip vertically
            let rot = imageops::rotate90(&img);
            image::DynamicImage::ImageRgba8(imageops::flip_vertical(&rot))
        }
        8 => image::DynamicImage::ImageRgba8(imageops::rotate270(&img)),
        _ => img, // 1 = normal, or unknown value — no transform
    }
}

/// Generate a scaled thumbnail for an image file.
///
/// `exif_orientation` must be pre-read from the ORIGINAL (pre-strip) bytes so
/// that JPEG thumbnails are oriented correctly even after EXIF has been removed
/// from the stored file.  Pass `1` for non-JPEG formats.
fn generate_image_thumb(
    data: &[u8],
    mime_type: &str,
    output_path: &Path,
    max_dim: u32,
    exif_orientation: u32,
) -> Result<()> {
    let format = match mime_type {
        "image/jpeg" => ImageFormat::Jpeg,
        "image/png" => ImageFormat::Png,
        "image/gif" => ImageFormat::Gif, // NOTE: first frame only for animated GIFs
        "image/webp" => ImageFormat::WebP,
        _ => return Err(anyhow::anyhow!("Unsupported image format")),
    };

    let img =
        image::load_from_memory_with_format(data, format).context("Failed to decode image")?;

    // Apply EXIF orientation using the value read from the original bytes before
    // EXIF stripping.  Only JPEG carries reliable orientation data; for other
    // formats exif_orientation will always be 1 (no-op).
    let img = if exif_orientation > 1 {
        tracing::debug!("EXIF orientation {} applied to thumbnail", exif_orientation);
        apply_exif_orientation(img, exif_orientation)
    } else {
        img
    };

    let (w, h) = img.dimensions();
    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss
    )]
    let (tw, th) = if w > h {
        let r = max_dim as f32 / w as f32;
        (max_dim, (h as f32 * r) as u32)
    } else {
        let r = max_dim as f32 / h as f32;
        ((w as f32 * r) as u32, max_dim)
    };

    let thumb = if w <= tw && h <= th {
        img
    } else {
        img.resize(tw, th, FilterType::Triangle)
    };

    let save_format = match mime_type {
        "image/png" => ImageFormat::Png,
        "image/gif" => ImageFormat::Gif,
        "image/webp" => ImageFormat::WebP,
        _ => ImageFormat::Jpeg,
    };

    thumb
        .save_with_format(output_path, save_format)
        .context("Failed to save thumbnail")?;

    Ok(())
}

// ─── Helpers ──────────────────────────────────────────────────────────────────

/// Map a MIME type string to the canonical file extension used on disk.
fn mime_to_ext(mime: &str) -> &'static str {
    match mime {
        "image/jpeg" => "jpg",
        "image/png" => "png",
        "image/gif" => "gif",
        "image/webp" => "webp",
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

// ─── Video codec probing ──────────────────────────────────────────────────────

/// Use ffprobe to determine the video codec of the first video stream in a
/// file.  Returns the codec name in lower-case (e.g. `"av1"`, `"vp9"`,
/// `"vp8"`, `"h264"`).
///
/// Security: arguments are passed as a separate array — no shell invocation.
/// `file_path` must be a path we control (UUID-based); it is never derived
/// from user input.
fn ffprobe_video_codec(file_path: &str) -> Result<String> {
    use std::process::Command;

    let out = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=codec_name",
            "-of",
            "csv=p=0",
            file_path,
        ])
        .output()
        .context("ffprobe not found or failed to spawn")?;

    if !out.status.success() {
        return Err(anyhow::anyhow!(
            "ffprobe exit {}: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }

    let codec = String::from_utf8_lossy(&out.stdout)
        .trim()
        .to_ascii_lowercase();

    if codec.is_empty() {
        return Err(anyhow::anyhow!(
            "ffprobe returned no codec for: {file_path}"
        ));
    }

    Ok(codec)
}

/// Probe the video codec of a file on disk.
///
/// Returns the codec name in lower-case (e.g. `"av1"`, `"vp9"`, `"vp8"`).
/// Called by the `VideoTranscode` background worker to decide whether a `WebM`
/// upload needs AV1 → VP9 re-encoding.
///
/// # Errors
/// Returns an error if ffprobe cannot be run or returns no codec.
pub fn probe_video_codec(file_path: &str) -> anyhow::Result<String> {
    ffprobe_video_codec(file_path)
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

// ─── Public wrappers used by background workers ───────────────────────────────

/// Transcode any video (typically MP4) to `WebM` (VP9 + Opus) via ffmpeg.
/// Called by the `VideoTranscode` background worker.
///
/// # Errors
/// Returns an error if ffmpeg cannot be run or transcoding fails.
pub fn transcode_to_webm(data: &[u8]) -> anyhow::Result<Vec<u8>> {
    ffmpeg_transcode_webm(data)
}

/// Generate a waveform PNG for an audio file via ffmpeg.
/// Called by the `AudioWaveform` background worker.
///
/// # Errors
/// Returns an error if ffmpeg cannot be run or waveform generation fails.
pub fn gen_waveform_png(
    data: &[u8],
    output_path: &Path,
    width: u32,
    height: u32,
) -> anyhow::Result<()> {
    ffmpeg_audio_waveform(data, output_path, width, height)
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let out = apply_exif_orientation(img, 1);
        assert_eq!(out.width(), 4);
        assert_eq!(out.height(), 6);
    }

    #[test]
    fn exif_orientation_3_rotates_180() {
        // 180° rotation keeps dimensions unchanged
        let img = image::DynamicImage::new_rgba8(4, 6);
        let out = apply_exif_orientation(img, 3);
        assert_eq!(out.width(), 4);
        assert_eq!(out.height(), 6);
    }

    #[test]
    fn exif_orientation_6_rotates_90cw_swaps_dims() {
        let img = image::DynamicImage::new_rgba8(4, 6);
        let out = apply_exif_orientation(img, 6);
        assert_eq!(out.width(), 6);
        assert_eq!(out.height(), 4);
    }

    #[test]
    fn exif_orientation_8_rotates_90ccw_swaps_dims() {
        let img = image::DynamicImage::new_rgba8(4, 6);
        let out = apply_exif_orientation(img, 8);
        assert_eq!(out.width(), 6);
        assert_eq!(out.height(), 4);
    }

    #[test]
    fn exif_orientation_unknown_value_is_noop() {
        let img = image::DynamicImage::new_rgba8(4, 6);
        let out = apply_exif_orientation(img, 99);
        assert_eq!(out.width(), 4);
        assert_eq!(out.height(), 6);
    }
}
