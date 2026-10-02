// Subprocesses receive explicit argument arrays and server-generated paths,
// never shell strings. These helpers run in synchronous or `spawn_blocking`
// contexts, and include bounded stderr in failures for operator diagnostics.

use anyhow::{Context as _, Result};
use std::borrow::Cow;
use std::path::Path;
use std::process::Command;
use std::sync::LazyLock;
use std::thread::available_parallelism;
use std::time::Duration;

/// Construct a command for the configured `FFmpeg` executable.
fn ffmpeg_command() -> Command {
    Command::new(&crate::config::CONFIG.ffmpeg_path)
}

#[derive(Clone, Debug, PartialEq, Eq)]
/// Host-tuned settings for VP9 background transcoding.
pub struct Vp9EncodingProfile {
    /// libvpx speed/quality trade-off selected for this host.
    pub cpu_used: u8,
    /// Number of VP9 tile-column doublings.
    pub tile_columns: u8,
    /// Maximum worker thread count supplied to `FFmpeg`.
    pub threads: usize,
    /// Whether row-based multithreading is enabled.
    pub row_mt: bool,
    /// Human-readable profile description for diagnostics.
    pub label: Cow<'static, str>,
}

/// Pixel format used for broadly compatible VP9 output.
const VP9_COMPAT_PIXEL_FORMAT: &str = "yuv420p";
/// Color metadata used for broadly compatible VP9 output.
const VP9_COMPAT_COLOR_SPACE: &str = "bt709";

/// AV1 decoding and encoding are independent build capabilities.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Av1Capabilities {
    /// Specific AV1 decoder names reported by `FFmpeg`.
    pub decoders: Vec<String>,
    /// Specific AV1 encoder names reported by `FFmpeg`.
    pub encoders: Vec<String>,
}

/// Capabilities of the retained video backend, interrogated independently.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct VideoCapabilities {
    /// Installed AV1 decoders and encoders, not inferred from the version.
    pub av1: Av1Capabilities,
    /// The policy-selected VP9 encoder exists.
    pub vp9: bool,
    /// The policy-selected Opus encoder exists.
    pub opus: bool,
    /// The `WebM` output muxer exists.
    pub webm_muxer: bool,
}

impl VideoCapabilities {
    /// Whether the established VP9/Opus `WebM` output pipeline is available.
    #[must_use]
    pub const fn webm_available(&self) -> bool {
        self.vp9 && self.opus && self.webm_muxer
    }
}

/// Probe the configured build once; missing or failed lists fail closed.
#[must_use]
pub fn video_capabilities() -> &'static VideoCapabilities {
    static CAPS: LazyLock<VideoCapabilities> = LazyLock::new(|| {
        let program = &crate::config::CONFIG.ffmpeg_path;
        capabilities_from_lists(
            output_stdout_with_timeout(program, &["-hide_banner", "-decoders"]).as_deref(),
            output_stdout_with_timeout(program, &["-hide_banner", "-encoders"]).as_deref(),
            output_stdout_with_timeout(program, &["-hide_banner", "-muxers"]).as_deref(),
        )
    });
    &CAPS
}

/// Parse actual capability rows, excluding legends and description-only matches.
fn codec_rows(output: &str, kind: char) -> impl Iterator<Item = (&str, &str)> {
    output.lines().filter_map(move |line| {
        let mut fields = line.split_whitespace();
        let flags = fields.next()?;
        let name = fields.next()?;
        if flags.len() != 6 || !flags.starts_with(kind) || name == "=" {
            return None;
        }
        Some((name, line))
    })
}

/// Recognize codec aliases by their declared codec, including future implementations.
fn av1_names(output: &str) -> Vec<String> {
    codec_rows(output, 'V')
        .filter(|(name, line)| {
            line.contains("(codec av1)")
                || matches!(
                    *name,
                    "av1" | "libaom-av1" | "libsvtav1" | "librav1e" | "libdav1d"
                )
                || name.starts_with("av1_")
                || name.ends_with("_av1")
        })
        .map(|(name, _)| name.to_owned())
        .collect()
}

/// Combine independently probed build lists into video pipeline capabilities.
fn capabilities_from_lists(
    decoders: Option<&str>,
    encoders: Option<&str>,
    muxers: Option<&str>,
) -> VideoCapabilities {
    let encoders = encoders.unwrap_or("");
    VideoCapabilities {
        av1: Av1Capabilities {
            decoders: av1_names(decoders.unwrap_or("")),
            encoders: av1_names(encoders),
        },
        vp9: codec_rows(encoders, 'V').any(|(name, _)| name == "libvpx-vp9"),
        opus: codec_rows(encoders, 'A').any(|(name, _)| name == "libopus"),
        webm_muxer: muxers.unwrap_or("").lines().any(|line| {
            let mut fields = line.split_whitespace();
            fields.next() == Some("E") && fields.next() == Some("webm")
        }),
    }
}

/// Probe whether the `ffmpeg` binary is reachable on the current PATH.
///
/// Runs `ffmpeg -version` and returns `true` if the process exits successfully.
/// This is a synchronous, blocking call and is intended to be invoked at
/// startup (or lazily on first upload) inside a `spawn_blocking` task.
///
/// A `false` return means conversion and video-thumbnail features will be
/// unavailable; all callers must degrade gracefully.
#[must_use]
pub fn detect_ffmpeg() -> bool {
    let mut command = ffmpeg_command();
    command.arg("-version");
    run_command_with_timeout(&mut command, &crate::config::CONFIG.ffmpeg_path, "ffmpeg")
        .is_ok_and(|output| output.status.success())
}

/// Execute `ffmpeg` with the given argument slice.
///
/// All arguments must be pre-constructed, static strings — no user-supplied
/// data may appear in `args` directly.  File paths passed to ffmpeg must be
/// UUID-based names generated by the server.
///
/// Returns `Ok(())` on exit code 0, or an `Err` containing the trimmed
/// stderr output on any non-zero exit.
///
/// # Errors
/// Returns an error if ffmpeg cannot be spawned (binary missing, permission
/// denied) or if the process exits with a non-zero status code.
pub fn run_ffmpeg(args: &[&str]) -> Result<()> {
    let output = run_command_with_timeout(
        ffmpeg_command().args(args),
        &crate::config::CONFIG.ffmpeg_path,
        "ffmpeg",
    )?;

    if output.status.success() {
        Ok(())
    } else {
        Err(anyhow::anyhow!(
            "ffmpeg exited with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

/// Inspect an uncovered audio container using bounded, zero-frame stream maps.
///
/// This compatibility path is used only when the Rust parser reports an
/// unsupported format, never after malformed-input or resource-limit errors.
/// # Errors
/// Returns an error if the tool cannot establish an audio-only stream layout.
pub(crate) fn probe_uncovered_audio(path: &Path) -> Result<super::probe::StreamKind> {
    let path = path_to_str(path)?;
    let has_audio = mapped_stream_exists(path, "a:0", "-frames:a")?;
    let has_video = mapped_stream_exists(path, "v:0", "-frames:v")?;
    if has_video {
        return Ok(super::probe::StreamKind::Video);
    }
    if has_audio {
        return Ok(super::probe::StreamKind::AudioOnly);
    }
    anyhow::bail!("uncovered audio container has no audio/video streams")
}

/// Ask `FFmpeg` to map one stream without decoding any frames.
fn mapped_stream_exists(path: &str, selector: &str, frames: &str) -> Result<bool> {
    let mut command = ffmpeg_command();
    command.args([
        "-v", "error", "-i", path, "-map", selector, frames, "0", "-f", "null", "-",
    ]);
    let output = run_command_with_timeout(
        &mut command,
        &crate::config::CONFIG.ffmpeg_path,
        "audio container compatibility probe",
    )?;
    Ok(output.status.success())
}

/// Extract a video frame with `FFmpeg` and encode its WebP thumbnail in Rust.
///
/// # Errors
/// Returns an error on extraction, bounded image decode, encoding or persistence.
pub fn ffmpeg_thumbnail(input: &Path, output: &Path, max_dim: u32) -> Result<()> {
    let parent = output.parent().context("thumbnail output has no parent")?;
    let frame = tempfile::Builder::new()
        .suffix(".png")
        .tempfile_in(parent)?;
    let thumbnail = tempfile::Builder::new()
        .suffix(".webp")
        .tempfile_in(parent)?;
    let scale = format!("scale='if(gt(iw,ih),{max_dim},-2)':'if(gt(iw,ih),-2,{max_dim})'");
    run_ffmpeg(&[
        "-loglevel",
        "error",
        "-i",
        path_to_str(input)?,
        "-map",
        "0:v:0",
        "-frames:v",
        "1",
        "-vf",
        &scale,
        "-c:v",
        "png",
        "-f",
        "image2",
        "-update",
        "1",
        "-y",
        path_to_str(frame.path())?,
    ])
    .context("video frame extraction failed")?;
    super::thumbnail::image_crate_thumbnail(frame.path(), "image/png", thumbnail.path(), max_dim)?;
    thumbnail
        .persist(output)
        .context("persist Rust-encoded video thumbnail")?;
    Ok(())
}

#[must_use]
/// Return the lazily detected VP9 encoding profile for this host.
pub fn vp9_encoding_profile() -> &'static Vp9EncodingProfile {
    static PROFILE: LazyLock<Vp9EncodingProfile> = LazyLock::new(detect_vp9_encoding_profile);
    &PROFILE
}

#[must_use]
/// Construct `FFmpeg` arguments for a compatibility-oriented VP9/Opus transcode.
pub fn build_vp9_transcode_args(input: &str, output: &str) -> Vec<String> {
    let profile = vp9_encoding_profile();
    let cpu_used = profile.cpu_used.to_string();
    let tile_columns = profile.tile_columns.to_string();
    let threads = profile.threads.to_string();
    let mut args = Vec::with_capacity(34);
    args.extend(
        [
            "-loglevel",
            "error",
            "-i",
            input,
            "-map",
            "0:v:0",
            "-map",
            "0:a:0?",
            "-c:v",
            "libvpx-vp9",
            "-deadline",
            "good",
            "-cpu-used",
        ]
        .into_iter()
        .map(str::to_owned),
    );
    args.push(cpu_used);
    args.extend(
        [
            "-row-mt",
            if profile.row_mt { "1" } else { "0" },
            "-tile-columns",
        ]
        .into_iter()
        .map(str::to_owned),
    );
    args.push(tile_columns);
    args.extend(
        [
            "-threads",
            &threads,
            "-crf",
            "30",
            "-b:v",
            "0",
            "-pix_fmt",
            VP9_COMPAT_PIXEL_FORMAT,
            "-colorspace",
            VP9_COMPAT_COLOR_SPACE,
            "-color_primaries",
            VP9_COMPAT_COLOR_SPACE,
            "-color_trc",
            VP9_COMPAT_COLOR_SPACE,
            "-c:a",
            "libopus",
            "-b:a",
            "128k",
            "-f",
            "webm",
            "-map_metadata",
            "-1",
            "-y",
            output,
        ]
        .into_iter()
        .map(str::to_owned),
    );
    args
}
// Internal helpers
/// Choose bounded VP9 encoder settings from available host parallelism.
fn detect_vp9_encoding_profile() -> Vp9EncodingProfile {
    let threads = available_parallelism()
        .map_or(1, std::num::NonZeroUsize::get)
        .clamp(1, 16);

    let tile_columns = match threads {
        0..=2 => 0,
        3..=4 => 1,
        _ => 2,
    };

    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    {
        if std::arch::is_x86_feature_detected!("avx512f") {
            return Vp9EncodingProfile {
                cpu_used: 1,
                tile_columns,
                threads,
                row_mt: true,
                label: Cow::Borrowed("x86-avx512"),
            };
        }
        if std::arch::is_x86_feature_detected!("avx2") {
            return Vp9EncodingProfile {
                cpu_used: 2,
                tile_columns,
                threads,
                row_mt: true,
                label: Cow::Borrowed("x86-avx2"),
            };
        }
        if std::arch::is_x86_feature_detected!("avx") {
            return Vp9EncodingProfile {
                cpu_used: 3,
                tile_columns: tile_columns.min(1),
                threads,
                row_mt: true,
                label: Cow::Borrowed("x86-avx"),
            };
        }
        if std::arch::is_x86_feature_detected!("sse4.1") {
            return Vp9EncodingProfile {
                cpu_used: 4,
                tile_columns: tile_columns.min(1),
                threads: threads.min(8),
                row_mt: true,
                label: Cow::Borrowed("x86-sse4.1"),
            };
        }
    }

    #[cfg(target_arch = "aarch64")]
    {
        Vp9EncodingProfile {
            cpu_used: 3,
            tile_columns: tile_columns.min(1),
            threads,
            row_mt: true,
            label: Cow::Borrowed("aarch64-neon"),
        }
    }

    #[cfg(target_arch = "arm")]
    {
        return Vp9EncodingProfile {
            cpu_used: 4,
            tile_columns: 0,
            threads: threads.min(4),
            row_mt: true,
            label: Cow::Borrowed("arm"),
        };
    }

    #[cfg(not(any(target_arch = "aarch64", target_arch = "arm")))]
    {
        Vp9EncodingProfile {
            cpu_used: 4,
            tile_columns: 0,
            threads: threads.min(4),
            row_mt: false,
            label: Cow::Owned(format!("generic-{}", std::env::consts::ARCH)),
        }
    }
}

/// Convert a `Path` to a UTF-8 `&str`, returning a descriptive error if the
/// path contains non-UTF-8 bytes (rare on all supported platforms).
fn path_to_str(p: &Path) -> Result<&str> {
    p.to_str()
        .ok_or_else(|| anyhow::anyhow!("path contains non-UTF-8 characters: {}", p.display()))
}

/// Run a subprocess with the configured `FFmpeg` timeout and captured output.
fn run_command_with_timeout(
    command: &mut Command,
    program: &str,
    label: &str,
) -> Result<std::process::Output> {
    let timeout = Duration::from_secs(crate::config::ffmpeg_timeout_secs());
    crate::media::process::run_std_command_with_timeout(
        command,
        timeout,
        label,
        || format!("failed to spawn {label} binary '{program}' — is it installed and executable?"),
        || format!("{label} I/O error"),
    )
}

/// Run a bounded command and return trimmed UTF-8 stdout on success.
fn output_stdout_with_timeout(program: &str, args: &[&str]) -> Option<String> {
    let timeout = Duration::from_secs(10);
    let mut command = Command::new(program);
    command.args(args);
    crate::media::process::run_std_command_with_timeout(
        &mut command,
        timeout,
        "media capability probe",
        || format!("failed to spawn media capability probe '{program}'"),
        || "media capability probe I/O error".to_owned(),
    )
    .ok()
    .filter(|output| output.status.success())
    .map(|output| String::from_utf8_lossy(&output.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::{
        build_vp9_transcode_args, capabilities_from_lists, VP9_COMPAT_COLOR_SPACE,
        VP9_COMPAT_PIXEL_FORMAT,
    };
    use anyhow::{ensure, Context as _, Result};

    #[test]
    fn av1_decoder_does_not_imply_av1_encoder_or_webm_conversion() {
        let caps = capabilities_from_lists(
            Some(" V..... libdav1d dav1d AV1 decoder (codec av1)\n V....D av1 AV1"),
            Some(" A....D wmav1 Windows Media Audio 1"),
            Some(" E webm WebM"),
        );
        assert_eq!(caps.av1.decoders, ["libdav1d", "av1"]);
        assert_eq!(caps.av1.encoders, Vec::<String>::new());
        assert!(!caps.webm_available());
    }

    #[test]
    fn av1_encoder_names_and_webm_requirements_are_independent() {
        let encoders = " V....D libaom-av1 libaom AV1\n V..... libsvtav1 SVT (codec av1)\n V..... librav1e rav1e AV1\n V..... av1_nvenc NVIDIA AV1\n V..... future_encoder future (codec av1)\n V....D libvpx-vp9 VP9\n A....D libopus Opus";
        let caps = capabilities_from_lists(None, Some(encoders), Some(" E webm WebM"));
        assert_eq!(caps.av1.decoders, Vec::<String>::new());
        assert_eq!(
            caps.av1.encoders,
            [
                "libaom-av1",
                "libsvtav1",
                "librav1e",
                "av1_nvenc",
                "future_encoder"
            ]
        );
        assert!(caps.webm_available());
        assert!(!capabilities_from_lists(None, Some(encoders), None).webm_available());
        assert!(!capabilities_from_lists(
            None,
            Some(" V..... libvpx-vp9 VP9"),
            Some(" E webm WebM")
        )
        .webm_available());
        assert!(
            !capabilities_from_lists(None, Some(" A..... libopus Opus"), Some(" E webm WebM"))
                .webm_available()
        );
    }

    #[test]
    fn unavailable_ffmpeg_and_description_matches_fail_closed() {
        let missing = capabilities_from_lists(None, None, None);
        assert_eq!(missing, super::VideoCapabilities::default());
        let misleading = capabilities_from_lists(
            Some(" V..... h264 text mentions av1\n A..... av1 not video"),
            Some(" V..... unrelated mentions libvpx-vp9 libopus libaom-av1\n V..... libvpx-vp9-extra other\n A..... libopus_extra other\n V..... = libaom-av1"),
            Some(" E webm_chunk WebM Chunk\n E other mentions webm"),
        );
        assert_eq!(misleading, missing);
    }

    #[test]
    fn real_av1_inputs_convert_to_vp9_webm_and_native_webp_thumbnails() -> Result<()> {
        if !super::detect_ffmpeg() {
            return Ok(());
        }
        let caps = super::video_capabilities();
        if !caps.webm_available() || caps.av1.decoders.is_empty() {
            return Ok(());
        }
        let dir = tempfile::tempdir()?;
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/media");
        for name in ["av1.webm", "av1.mkv", "av1.mp4"] {
            let input = root.join(name);
            let output = dir.path().join(format!("{name}.webm"));
            let args = build_vp9_transcode_args(
                input.to_str().context("input path")?,
                output.to_str().context("output path")?,
            );
            super::run_ffmpeg(&args.iter().map(String::as_str).collect::<Vec<_>>())?;
            crate::media::probe::validate_webm_output_file(&output)?;
            let thumbnail = dir.path().join(format!("{name}.webp"));
            super::ffmpeg_thumbnail(&input, &thumbnail, 32)?;
            let image = image::open(&thumbnail)?;
            ensure!(
                image.width() == 32 && image.height() == 32,
                "bad video thumbnail"
            );
        }
        Ok(())
    }

    fn paired_arg_index(args: &[String], flag: &str, value: &str) -> Option<usize> {
        args.windows(2).position(|window| {
            matches!(window, [actual_flag, actual_value] if actual_flag == flag && actual_value == value)
        })
    }

    #[expect(
        clippy::panic_in_result_fn,
        reason = "the assertion reports an invalid ordering invariant in generated ffmpeg arguments"
    )]
    fn output_arg_index(args: &[String], flag: &str, value: &str) -> Result<usize> {
        let input_index = paired_arg_index(args, "-i", "input.mp4")
            .with_context(|| format!("missing input marker in ffmpeg args: {args:?}"))?;
        let arg_index = paired_arg_index(args, flag, value)
            .with_context(|| format!("missing ffmpeg arg pair {flag} {value}: {args:?}"))?;
        assert!(
            arg_index > input_index + 1,
            "{flag} {value} must be an output option after the input argument: {args:?}",
        );
        Ok(arg_index)
    }

    #[test]
    fn vp9_transcode_args_include_platform_tuning_flags() {
        let args = build_vp9_transcode_args("input.mp4", "output.webm");
        assert!(args.windows(2).any(|w| w == ["-c:v", "libvpx-vp9"]));
        assert!(args.windows(2).any(|w| w == ["-deadline", "good"]));
        assert!(args
            .windows(2)
            .any(|w| w == ["-row-mt", "1"] || w == ["-row-mt", "0"]));
        assert!(args
            .windows(2)
            .any(|w| w.first().is_some_and(|flag| flag == "-cpu-used")));
        assert!(args
            .windows(2)
            .any(|w| w.first().is_some_and(|flag| flag == "-tile-columns")));
        assert!(args
            .windows(2)
            .any(|w| w.first().is_some_and(|flag| flag == "-threads")));
        assert!(args
            .windows(2)
            .any(|w| w == ["-pix_fmt", VP9_COMPAT_PIXEL_FORMAT]));
    }

    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "test assertions intentionally panic on failure"
    )]
    fn vp9_transcode_args_normalise_yuv420p_color_metadata() -> Result<()> {
        let args = build_vp9_transcode_args("input.mp4", "output.webm");

        let pixel_format_index = output_arg_index(&args, "-pix_fmt", VP9_COMPAT_PIXEL_FORMAT)?;
        let colorspace_index = output_arg_index(&args, "-colorspace", VP9_COMPAT_COLOR_SPACE)?;
        let primaries_index = output_arg_index(&args, "-color_primaries", VP9_COMPAT_COLOR_SPACE)?;
        let transfer_index = output_arg_index(&args, "-color_trc", VP9_COMPAT_COLOR_SPACE)?;

        assert!(
            pixel_format_index < colorspace_index
                && colorspace_index < primaries_index
                && primaries_index < transfer_index,
            "VP9 color metadata should be applied directly after the forced pixel format: {args:?}",
        );
        Ok(())
    }

    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "test assertions intentionally panic on failure"
    )]
    fn vp9_yuv420p_output_does_not_inherit_srgb_gbr_metadata() -> Result<()> {
        let args = build_vp9_transcode_args("input.mp4", "output.webm");

        for (flag, value) in [
            ("-colorspace", VP9_COMPAT_COLOR_SPACE),
            ("-color_primaries", VP9_COMPAT_COLOR_SPACE),
            ("-color_trc", VP9_COMPAT_COLOR_SPACE),
        ] {
            let color_index = output_arg_index(&args, flag, value)?;
            let audio_index = args
                .windows(2)
                .position(|window| matches!(window, [actual_flag, actual_value] if actual_flag == "-c:a" && actual_value == "libopus"))
                .unwrap_or(args.len());
            assert!(
                color_index < audio_index,
                "{flag} {value} should be part of the video output options before audio encoding options: {args:?}",
            );
        }
        Ok(())
    }

    #[test]
    fn ffmpeg_builders_with_forced_compat_pixel_formats_normalise_color_metadata() -> Result<()> {
        let args = build_vp9_transcode_args("input.mp4", "output.webm");

        // Other ffmpeg builders extract PNG video frames or audio waveform
        // PNGs and do not force VP9/H.26x-style
        // compatibility pixel formats. VP9 transcode is the path that combines
        // a forced yuv420p profile-0 output with potentially inherited source
        // color metadata, so it owns the explicit BT.709 normalization contract.
        output_arg_index(&args, "-pix_fmt", VP9_COMPAT_PIXEL_FORMAT)?;
        output_arg_index(&args, "-colorspace", VP9_COMPAT_COLOR_SPACE)?;
        output_arg_index(&args, "-color_primaries", VP9_COMPAT_COLOR_SPACE)?;
        output_arg_index(&args, "-color_trc", VP9_COMPAT_COLOR_SPACE)?;
        Ok(())
    }
}
