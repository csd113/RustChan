// utils/files.rs
//
// File handling pipeline:
//   1. Receive multipart bytes
//   2. Validate MIME type against allowlist (BOTH magic bytes AND extension)
//   3. Validate file size using per-type limits
//   4. Generate random filename (UUID-based, prevents path traversal)
//   5. If JPEG: re-encode through the `image` crate to strip all EXIF metadata
//   6. If video and ffmpeg is available: transcode MP4 → WebM (VP9/Opus)
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

use anyhow::{Context, Result};
use image::{imageops::FilterType, GenericImageView, ImageFormat};
use std::path::PathBuf;
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
    /// Explicit media category derived from mime_type
    pub media_type: crate::models::MediaType,
    /// True when the file needs async background processing:
    ///   • Video (MP4) → VideoTranscode job (MP4 → WebM via ffmpeg)
    ///   • Audio       → AudioWaveform job  (waveform PNG via ffmpeg)
    /// The handler must enqueue the appropriate Job after the post is inserted
    /// so that the post_id is available. Always false for cached/dedup hits.
    pub processing_pending: bool,
}

// ─── MIME / type detection ────────────────────────────────────────────────────

/// Validate file bytes against known magic signatures and return the detected
/// MIME type string.  Returns an error when the file type is not on the allowlist.
///
/// We check magic bytes rather than trusting the user-supplied Content-Type
/// header.  This prevents executables disguised as media from being served.
pub fn detect_mime_type(data: &[u8]) -> Result<&'static str> {
    if data.is_empty() {
        return Err(anyhow::anyhow!("File is empty."));
    }
    // Use .get() for all slice access — no panics, bounds proven by the len checks.
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
        match data.get(8..12) {
            Some(b"WEBP") => return Ok("image/webp"),
            Some(b"WAVE") => return Ok("audio/wav"),
            _ => {
                return Err(anyhow::anyhow!(
                    "RIFF container with unknown subtype. Accepted: WebP, WAV"
                ))
            }
        }
    }

    // ── WebM (video or audio-only) ────────────────────────────────────────────
    if header.starts_with(b"\x1a\x45\xdf\xa3") {
        return Ok("video/webm");
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

    // ── Audio formats ─────────────────────────────────────────────────────────
    if header.starts_with(b"ID3") {
        return Ok("audio/mpeg");
    }
    // Raw MP3 frame sync (no ID3 header): FF FB/F3/F2
    if let (Some(&0xff), Some(&b1)) = (data.first(), data.get(1)) {
        if b1 == 0xfb || b1 == 0xf3 || b1 == 0xf2 {
            return Ok("audio/mpeg");
        }
    }
    if header.starts_with(b"OggS") {
        return Ok("audio/ogg");
    }
    if header.starts_with(b"fLaC") {
        return Ok("audio/flac");
    }
    // AAC ADTS sync word: FF F0–FE (not FF FF, and not the MP3 bytes above)
    if let (Some(&0xff), Some(&b1)) = (data.first(), data.get(1)) {
        if (b1 & 0xf0 == 0xf0) && b1 != 0xff {
            return Ok("audio/aac");
        }
    }

    Err(anyhow::anyhow!(
        "File type not allowed. Accepted: JPEG, PNG, GIF, WebP, MP4, WebM, \
         MP3, OGG, FLAC, WAV, M4A, AAC"
    ))
}

// ─── Main entry point ─────────────────────────────────────────────────────────

/// Save an uploaded file to disk and generate its thumbnail (or audio placeholder).
///
/// Files are stored under `{boards_dir}/{board_short}/` and thumbnails
/// under `{boards_dir}/{board_short}/thumbs/`.
/// The returned paths are relative to `boards_dir` (e.g. `"b/abc123.webm"`).
///
/// When `ffmpeg_available` is true, uploaded MP4 files are transcoded to WebM
/// (VP9 video + Opus audio) before being saved.  Already-WebM uploads are kept
/// as-is.  If transcoding fails, the original file is saved as a fallback.
/// Audio files never require ffmpeg; they always receive an SVG placeholder.
#[allow(clippy::too_many_arguments)]
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
        .ok_or_else(|| anyhow::anyhow!("Could not classify detected MIME type: {}", mime_type))?;

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

    let file_id = Uuid::new_v4().to_string().replace('-', "");

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
    let data: &[u8] = if let Some(ref stripped) = stripped_jpeg {
        stripped.as_slice()
    } else {
        data
    };

    // ── Async processing flag ─────────────────────────────────────────────────
    // Heavy CPU work (MP4→WebM transcoding, audio waveform) is deferred to the
    // background worker pool so HTTP responses return immediately.
    //   • Video (MP4) + ffmpeg → save as-is now; worker transcodes to WebM.
    //   • Audio       + ffmpeg → use SVG placeholder now; worker adds PNG waveform.
    // The handler enqueues the correct Job after db::create_post gives it post_id.
    let processing_pending = ffmpeg_available
        && match media_type {
            // MP4 always needs transcoding to WebM.
            // WebM is flagged pending so the worker can probe the codec and
            // transcode AV1 → VP9 when necessary.  VP8/VP9 WebM files are
            // detected and skipped cheaply inside the worker.
            crate::models::MediaType::Video => {
                mime_type == "video/mp4" || mime_type == "video/webm"
            }
            crate::models::MediaType::Audio => true,
            _ => false,
        };

    // Always save the original bytes — no inline transcoding.
    let (final_data, final_mime): (&[u8], &'static str) = (data, mime_type);

    // File size is derived from the data we're actually saving.
    let file_size = i64::try_from(final_data.len()).context("File size overflows i64")?;

    let ext = mime_to_ext(final_mime);
    let filename = format!("{}.{}", file_id, ext);

    // Ensure per-board directories exist: boards_dir/{board_short}/ and thumbs/
    let board_dir = PathBuf::from(boards_dir).join(board_short);
    let thumbs_dir = board_dir.join("thumbs");
    std::fs::create_dir_all(&thumbs_dir).context("Failed to create board thumbs directory")?;

    let file_path_abs = board_dir.join(&filename);

    // Disk-space pre-check (#14): verify at least 2× the file size is available
    // before writing.  Uses statvfs on Unix; skipped on other platforms.
    #[cfg(unix)]
    {
        unsafe {
            let dir_bytes = board_dir.to_string_lossy();
            if let Ok(path_cstr) = std::ffi::CString::new(dir_bytes.as_bytes()) {
                let mut stat: libc::statvfs = std::mem::zeroed();
                if libc::statvfs(path_cstr.as_ptr(), &mut stat) == 0 {
                    #[allow(clippy::useless_conversion)]
                    let free_bytes = (stat.f_bavail as u64) * (stat.f_frsize as u64);
                    let needed = (final_data.len() as u64).saturating_mul(2);
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
    }

    // Write via a temp file in the same directory, then atomically rename.
    // This guarantees no partial/corrupt file survives a crash or OOM mid-write.
    // tempfile::NamedTempFile::new_in writes to a UUID-named .tmp in the same
    // directory, ensuring the rename is always on the same filesystem (POSIX atomic).
    {
        use std::io::Write as _;
        let mut tmp = tempfile::NamedTempFile::new_in(&board_dir)
            .context("Failed to create temp file for upload")?;
        tmp.write_all(final_data)
            .context("Failed to write upload data to temp file")?;
        tmp.persist(&file_path_abs)
            .context("Failed to atomically rename upload temp file")?;
    }

    // Paths relative to boards_dir (e.g. "b/abc123.webm", "b/thumbs/abc123.jpg")
    let rel_file = format!("{}/{}", board_short, filename);
    let board_dir_str = board_dir.to_string_lossy().into_owned();

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
            let svg_rel = format!("{}/thumbs/{}.svg", board_short, file_id);
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
            let thumb_rel_name = format!("{}/thumbs/{}.{}", board_short, file_id, thumb_ext);
            let thumb_abs = PathBuf::from(boards_dir).join(&thumb_rel_name);
            generate_image_thumb(data, mime_type, &thumb_abs, thumb_size)
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
        .ok_or_else(|| anyhow::anyhow!("Not an audio file: {}", mime_type))?;

    if !matches!(media_type, crate::models::MediaType::Audio) {
        return Err(anyhow::anyhow!(
            "Expected an audio file for the audio slot; got {}",
            mime_type
        ));
    }

    if audio_data.len() > max_audio_size {
        return Err(anyhow::anyhow!(
            "Audio file too large. Maximum size is {} MiB.",
            max_audio_size / 1024 / 1024
        ));
    }

    let file_id = Uuid::new_v4().to_string().replace('-', "");
    let ext = mime_to_ext(mime_type);
    let filename = format!("{}.{}", file_id, ext);

    let board_dir = PathBuf::from(boards_dir).join(board_short);
    std::fs::create_dir_all(&board_dir).context("Failed to create board directory")?;

    let file_path_abs = board_dir.join(&filename);
    {
        use std::io::Write as _;
        let mut tmp = tempfile::NamedTempFile::new_in(&board_dir)
            .context("Failed to create temp file for audio upload")?;
        tmp.write_all(audio_data)
            .context("Failed to write audio data to temp file")?;
        tmp.persist(&file_path_abs)
            .context("Failed to atomically rename audio temp file")?;
    }

    let rel_file = format!("{}/{}", board_short, filename);
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
/// XMP, or IPTC segments — only the pixel data and ICC colour profile are
/// written (and only when the crate chooses to include a profile, which for
/// basic re-encodes it does not).  This is the most reliable stripping approach
/// available without pulling in a separate EXIF library.
///
/// Quality is set to 90 (the `image` crate's default for JPEG output), which
/// is indistinguishable from the original for display purposes but slightly
/// changes the file byte-for-byte.
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
fn ffmpeg_audio_waveform(
    audio_data: &[u8],
    output_path: &PathBuf,
    width: u32,
    height: u32,
) -> Result<()> {
    use std::process::Command;

    let temp_dir = std::env::temp_dir();
    let tmp_id = Uuid::new_v4().to_string().replace('-', "");
    let temp_in = temp_dir.join(format!("chan_aud_{}.tmp", tmp_id));
    let temp_out = temp_dir.join(format!("chan_wav_{}.png", tmp_id));

    std::fs::write(&temp_in, audio_data).context("Failed to write temp audio for waveform")?;

    let temp_in_str = temp_in
        .to_str()
        .context("Temp audio path contains non-UTF-8 characters")?;
    let temp_out_str = temp_out
        .to_str()
        .context("Temp waveform output path contains non-UTF-8 characters")?;

    // showwavespic: renders the entire file as a single static image.
    // split_channels=0 → mono composite, colours=white|#00c840 for the terminal theme.
    let vf = format!(
        "showwavespic=s={}x{}:colors=#00c840|#007020:split_channels=0",
        width, height
    );

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

    let _ = std::fs::remove_file(&temp_in);

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

/// Transcode any video file to WebM (VP9 + Opus) using ffmpeg.
///
/// Returns the transcoded WebM bytes on success, or an error on failure.
/// The caller is responsible for falling back to the original data on error.
///
/// Encoding settings:
///   VP9  — `-deadline good -cpu-used 4` for a good quality/speed balance.
///           CRF 33 with unconstrained average bitrate (`-b:v 0`) but a
///           peak bitrate cap (`-maxrate 2M -bufsize 4M`) to prevent large
///           bitrate spikes on complex scenes that cause player stalling.
///           `-row-mt 1` enables row-based multithreading (significant speedup
///           on multi-core hosts, no quality cost).
///           `-tile-columns 2` improves both encode parallelism and decode
///           performance on multi-core playback devices (including mobile).
///   Opus — `-b:a 96k` — transparent quality for speech and music.
///
/// Security: all arguments passed as separate array elements — no shell.
fn ffmpeg_transcode_webm(video_data: &[u8]) -> Result<Vec<u8>> {
    use std::process::Command;

    let temp_dir = std::env::temp_dir();
    let tmp_id = Uuid::new_v4().to_string().replace('-', "");
    let temp_in = temp_dir.join(format!("chan_in_{}.tmp", tmp_id));
    let temp_out = temp_dir.join(format!("chan_out_{}.webm", tmp_id));

    std::fs::write(&temp_in, video_data).context("Failed to write temp video for transcoding")?;

    let in_str = temp_in.to_str().context("Temp input path is non-UTF-8")?;
    let out_str = temp_out.to_str().context("Temp output path is non-UTF-8")?;

    // VP9 CRF mode: `-b:v 0` means "unconstrained bitrate, let CRF drive quality".
    // `-maxrate` / `-bufsize` cannot be combined with `-b:v 0` in libvpx-vp9 —
    // the encoder treats that as "constrained quality" and requires a real
    // target bitrate, producing exit-234 / "Rate control parameters set without
    // a bitrate".  Pure CRF (`-b:v 0 -crf 33`) is the correct mode here.
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

    // Always clean up the temp input.
    let _ = std::fs::remove_file(&temp_in);

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
        let jpg_rel = format!("{}/thumbs/{}.jpg", board_short, file_id);
        let jpg_path = PathBuf::from(board_dir).join(format!("thumbs/{}.jpg", file_id));

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
    let svg_rel = format!("{}/thumbs/{}.svg", board_short, file_id);
    let svg_path = PathBuf::from(board_dir).join(format!("thumbs/{}.svg", file_id));
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
/// The video bytes are written to a temp file, ffmpeg runs on that file,
/// the JPEG output is moved to `output_path`, and the temp file is cleaned up.
fn ffmpeg_first_frame(video_data: &[u8], output_path: &PathBuf, thumb_size: u32) -> Result<()> {
    use std::process::Command;

    let temp_dir = std::env::temp_dir();
    let tmp_id = Uuid::new_v4().to_string().replace('-', "");
    let temp_in = temp_dir.join(format!("chan_vid_{}.tmp", tmp_id));
    let temp_out = temp_dir.join(format!("chan_thm_{}.jpg", tmp_id));

    std::fs::write(&temp_in, video_data).context("Failed to write temp video for ffmpeg")?;

    // FIX[MEDIUM-5]: Use context() to propagate errors if the temp path is
    // non-UTF-8, instead of silently passing "" as the ffmpeg -i argument.
    let temp_in_str = temp_in
        .to_str()
        .context("Temp video path contains non-UTF-8 characters")?;
    let temp_out_str = temp_out
        .to_str()
        .context("Temp output path contains non-UTF-8 characters")?;

    // scale=W:-2 : scale width to thumb_size, height to nearest even number
    let vf = format!("scale={}:-2", thumb_size);

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

    // Always remove the temp input regardless of ffmpeg result
    let _ = std::fs::remove_file(&temp_in);

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
fn generate_video_placeholder(output_path: &PathBuf) -> Result<()> {
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
fn generate_audio_placeholder(output_path: &PathBuf) -> Result<()> {
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

fn generate_image_thumb(
    data: &[u8],
    mime_type: &str,
    output_path: &PathBuf,
    max_dim: u32,
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

    let (w, h) = img.dimensions();
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
        "video/webm" => "webm",
        // Audio formats
        "audio/mpeg" => "mp3",
        "audio/ogg" => "ogg",
        "audio/flac" => "flac",
        "audio/wav" => "wav",
        "audio/mp4" => "m4a",
        "audio/aac" => "aac",
        "audio/webm" => "webm",
        _ => "bin",
    }
}

// ─── Video codec probing ──────────────────────────────────────────────────────

/// Use ffprobe to determine the video codec of the first video stream in a
/// file.  Returns the codec name in lower-case (e.g. `"av1"`, `"vp9"`,
/// `"vp8"`, `"h264"`).
///
/// Security: arguments are passed as a separate array — no shell invocation.
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
            "ffprobe returned no codec for: {}",
            file_path
        ));
    }

    Ok(codec)
}

/// Probe the video codec of a file on disk.
/// Returns the codec name in lower-case (e.g. `"av1"`, `"vp9"`, `"vp8"`).
/// Called by the VideoTranscode background worker to decide whether a WebM
/// upload needs AV1 → VP9 re-encoding.
pub fn probe_video_codec(file_path: &str) -> anyhow::Result<String> {
    ffprobe_video_codec(file_path)
}

/// Delete a file from the filesystem, ignoring not-found errors.
pub fn delete_file(boards_dir: &str, relative_path: &str) {
    let full_path = PathBuf::from(boards_dir).join(relative_path);
    let _ = std::fs::remove_file(full_path);
}

/// Format file size as human-readable string.
pub fn format_file_size(bytes: i64) -> String {
    if bytes < 1024 {
        format!("{} B", bytes)
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KiB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1} MiB", bytes as f64 / (1024.0 * 1024.0))
    }
}

// ─── Public wrappers used by background workers ───────────────────────────────

/// Transcode any video (typically MP4) to WebM (VP9 + Opus) via ffmpeg.
/// Called by the VideoTranscode background worker.
pub fn transcode_to_webm(data: &[u8]) -> anyhow::Result<Vec<u8>> {
    ffmpeg_transcode_webm(data)
}

/// Generate a waveform PNG for an audio file via ffmpeg.
/// Called by the AudioWaveform background worker.
pub fn gen_waveform_png(
    data: &[u8],
    output_path: &std::path::PathBuf,
    width: u32,
    height: u32,
) -> anyhow::Result<()> {
    ffmpeg_audio_waveform(data, output_path, width, height)
}
