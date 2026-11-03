//! Startup detection for optional tools and the in-process Tor service.

mod tor;
pub use tor::{detect_tor, tor_stream_token_identity, TOR_STREAM_TOKENS};

use std::process::{Command, Stdio};
use std::time::Duration;

/// Result of probing for a tool at startup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolStatus {
    /// Tool is ready for use immediately (e.g. ffmpeg).
    Available,
    /// Tool is not available at its configured path.
    Missing,
}

/// Availability of the encoders required for `WebM` transcoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WebmEncoderStatus {
    /// Whether the `libvpx-vp9` video encoder is available.
    pub vp9: bool,
    /// Whether the `libopus` audio encoder is available.
    pub opus: bool,
    /// Whether the `WebM` output muxer is available.
    pub muxer: bool,
}

impl WebmEncoderStatus {
    /// Returns whether both selected encoders and the `WebM` muxer are available.
    #[must_use]
    pub const fn is_available(self) -> bool {
        self.vp9 && self.opus && self.muxer
    }
}

// ffmpeg
/// Probes the configured `ffmpeg` executable and reports its status.
pub fn detect_ffmpeg(require_ffmpeg: bool) -> ToolStatus {
    let ok = probe_tool(&crate::config::CONFIG.ffmpeg_path);

    if ok {
        tracing::info!(target: "rustchan::detect", available = true, "ffmpeg detected — video thumbnails and transcoding enabled");
        ToolStatus::Available
    } else if require_ffmpeg {
        tracing::error!(
            target: "rustchan::detect",
            available = false,
            ffmpeg_path = %crate::config::CONFIG.ffmpeg_path,
            "ffmpeg required but not installed — set require_ffmpeg = false in settings.toml to disable this check"
        );
        crate::logging::console_print_raw(
            "  Install ffmpeg from: https://ffmpeg.org/download.html\n\n",
        );
        ToolStatus::Missing
    } else {
        tracing::warn!(
            target: "rustchan::detect",
            available = false,
            ffmpeg_path = %crate::config::CONFIG.ffmpeg_path,
            "ffmpeg not detected — video thumbnails and transcoding disabled"
        );
        if crate::logging::is_tty() {
            crate::logging::console_print_raw(
                "  Install from: https://ffmpeg.org/download.html\n\n",
            );
        }
        ToolStatus::Missing
    }
}

/// Reports the built-in pure-Rust PDF preview renderer.
#[must_use]
pub fn detect_pdf_thumbnail_renderers() -> Vec<crate::media::thumbnail::PdfRenderer> {
    crate::media::thumbnail::detect_pdf_renderers()
}

/// Probes an external tool using its conventional version argument.
fn probe_tool(program: &str) -> bool {
    probe_tool_with_args(program, &["-version"])
}

/// Probes an external tool with explicit arguments and a bounded timeout.
fn probe_tool_with_args(program: &str, args: &[&str]) -> bool {
    let mut command = Command::new(program);
    let _configured = command
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    status_with_timeout(&mut command, Duration::from_secs(10))
        .is_some_and(|status| status.success())
}

/// Waits for a child command up to `timeout`, terminating it on expiry.
fn status_with_timeout(
    command: &mut Command,
    timeout: Duration,
) -> Option<std::process::ExitStatus> {
    crate::media::process::run_std_command_with_timeout(
        command,
        timeout,
        "external tool probe",
        || "failed to spawn external tool probe".to_owned(),
        || "external tool probe I/O error".to_owned(),
    )
    .ok()
    .map(|output| output.status)
}

/// Probe whether the detected ffmpeg has `libvpx-vp9` + `libopus` compiled in.
pub fn detect_webm_encoder(ffmpeg_ok: bool) -> WebmEncoderStatus {
    if !ffmpeg_ok {
        return WebmEncoderStatus {
            vp9: false,
            opus: false,
            muxer: false,
        };
    }

    let caps = crate::media::ffmpeg::video_capabilities();
    tracing::info!(
        target: "rustchan::detect",
        av1_decoders = ?caps.av1.decoders,
        av1_encoders = ?caps.av1.encoders,
        "ffmpeg AV1 build capabilities; RustChan output policy remains VP9 + Opus"
    );
    let has_vp9 = caps.vp9;
    let has_opus = caps.opus;
    let status = WebmEncoderStatus {
        vp9: has_vp9,
        opus: has_opus,
        muxer: caps.webm_muxer,
    };

    if status.is_available() {
        let profile = crate::media::ffmpeg::vp9_encoding_profile();
        tracing::info!(
            target: "rustchan::detect",
            vp9 = true,
            opus = true,
            profile = %profile.label,
            cpu_used = profile.cpu_used,
            tile_columns = profile.tile_columns,
            threads = profile.threads,
            row_mt = profile.row_mt,
            "ffmpeg VP9 + Opus encoders available — MP4 to WebM transcoding enabled with platform-tuned settings"
        );
    } else {
        tracing::warn!(
            target: "rustchan::detect",
            vp9   = has_vp9,
            opus  = has_opus,
            webm_muxer = caps.webm_muxer,
            "ffmpeg VP9/Opus encoders or WebM muxer missing — videos stored as uploaded"
        );
        if crate::logging::is_tty() {
            crate::logging::console_print_raw(&webm_install_hint(
                has_vp9,
                has_opus,
                caps.webm_muxer,
            ));
        }
    }

    status
}

/// Builds a platform-specific installation hint for missing `WebM` encoders.
fn webm_install_hint(has_vp9: bool, has_opus: bool, has_muxer: bool) -> String {
    let mut s = String::new();
    if !has_vp9 {
        s.push_str("  Missing: libvpx-vp9 (VP9 video encoder)\n");
    }
    if !has_opus {
        s.push_str("  Missing: libopus   (Opus audio encoder)\n");
    }
    if !has_muxer {
        s.push_str("  Missing: webm (WebM output muxer)\n");
    }
    s.push('\n');

    #[cfg(target_os = "macos")]
    {
        s.push_str(
            "  ── macOS: reinstall ffmpeg with VP9 + Opus ──────────────────────────\n\
             \n\
             \x1b[2m  brew install ffmpeg\x1b[0m\n\n",
        );
    }
    #[cfg(target_os = "linux")]
    {
        s.push_str(
            "  ── Linux: install ffmpeg with VP9 + Opus ────────────────────────────\n\
             \n\
             \x1b[2m  sudo apt update && sudo apt install ffmpeg libvpx-dev libopus-dev\x1b[0m\n\n",
        );
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        s.push_str(
            "  Reinstall ffmpeg with libvpx-vp9 and libopus support.\n\
             See: https://ffmpeg.org/download.html\n\n",
        );
    }

    if !crate::logging::is_tty() {
        s.retain(|c| c != '\x1b');
    }
    s
}
