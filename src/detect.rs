// detect.rs — Startup detection for optional external tools.
//
// Responsibilities:
//   • detect_ffmpeg() — probe ffmpeg via `ffmpeg -version`
//   • detect_tor()    — probe tor via `tor --version`
//
// Design:
//   • NO shell string invocation.  All calls use std::process::Command with
//     explicit argument arrays to prevent any shell injection.
//   • Neither check blocks startup; both degrade gracefully.
//   • Tor detection is skipped entirely when enable_tor_support = false.
//   • ffmpeg detection can optionally hard-exit when require_ffmpeg = true.
//
// Security:
//   • Command arguments are passed as separate &str values, never concatenated.
//   • We only inspect the exit-code; we do NOT eval or execute any output.

use std::process::Command;

/// Result of probing for a tool at startup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolStatus {
    /// The tool responded correctly (exit 0).
    Available,
    /// The tool could not be found or returned a non-zero exit code.
    Missing,
}

// ─── ffmpeg detection ─────────────────────────────────────────────────────────

/// Probe for `ffmpeg` on PATH.
///
/// Runs: ffmpeg -version
///
/// If `require_ffmpeg` is true and ffmpeg is missing, the process exits with
/// a clear error message rather than returning.
///
/// Returns `ToolStatus::Available` when ffmpeg is present, otherwise
/// `ToolStatus::Missing`.  Never panics.
pub fn detect_ffmpeg(require_ffmpeg: bool) -> ToolStatus {
    // Use an argument array — no shell, no injection risk.
    let result = Command::new("ffmpeg")
        .arg("-version")
        // Suppress output; we only care about the exit code.
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();

    let available = matches!(result, Ok(s) if s.success());

    if available {
        println!("[INFO] ffmpeg detected. Video thumbnails and transcoding enabled.");
        ToolStatus::Available
    } else if require_ffmpeg {
        // Hard requirement not met — exit cleanly with a descriptive message.
        eprintln!("[ERROR] ffmpeg required but not installed.");
        eprintln!("        Install ffmpeg or set require_ffmpeg = false in settings.toml.");
        std::process::exit(1);
    } else {
        println!("[WARN] ffmpeg not detected. Video thumbnails and transcoding disabled.");
        println!("       Install ffmpeg from: https://ffmpeg.org/download.html");
        ToolStatus::Missing
    }
}

// ─── Tor detection ────────────────────────────────────────────────────────────

/// Probe for `tor` on PATH and print guidance for onion service configuration.
///
/// Runs: tor --version
///
/// This is purely informational — the server always starts regardless of the
/// result.  No torrc editing, no networking, no port binding.
///
/// If `enable_tor_support` is false in config, this function is a no-op.
pub fn detect_tor(enable_tor_support: bool, bind_port: u16) {
    // Skip detection entirely when the operator has disabled it.
    if !enable_tor_support {
        return;
    }

    // Brew on Apple Silicon installs to /opt/homebrew/bin; Intel Macs use
    // /usr/local/bin.  Fall back to a bare "tor" for Linux / custom PATH.
    let candidates = ["/opt/homebrew/bin/tor", "/usr/local/bin/tor", "tor"];

    // Some tor builds exit with code 1 for `--version` even when present, so
    // we treat *any* successful spawn (including non-zero exit) as "available".
    // The only failure case we care about is the binary not being found at all.
    let available = candidates.iter().any(|bin| {
        Command::new(bin)
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok() // is_ok() = the binary was found and spawned; ignores exit code
    });

    if available {
        println!("[INFO] Tor detected.");
        println!("[HINT] To enable onion access, add the following to your torrc:");
        println!("           HiddenServiceDir /var/lib/tor/rustchan/");
        println!("           HiddenServicePort 80 127.0.0.1:{}", bind_port);
        println!("       Then restart Tor and check the hostname file for your .onion address.");
    } else {
        println!("[INFO] Tor not detected.");
        println!("       This server supports Tor Onion Services.");
        println!("       Install Tor from: https://www.torproject.org/");
        println!("       Then configure:");
        println!("           HiddenServicePort 80 127.0.0.1:{}", bind_port);
    }
}
