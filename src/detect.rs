// detect.rs — Startup detection for optional external tools.
//
// All terminal output goes through crate::logging helpers so it is
// serialised with the tracing terminal layer (CONSOLE_MUTEX).  This
// prevents interleaving with concurrent log events during startup.
//
// Structured events (info!/warn!/error!) capture the detection result in
// the JSON log file with structured fields.  Human-readable install
// instructions are written via console_print_raw so they appear only
// on TTY-mode terminals and never pollute piped / systemd output.

use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Arc;

/// Result of probing for a tool at startup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolStatus {
    Available,
    Missing,
}

// ─── ffmpeg ───────────────────────────────────────────────────────────────────

pub fn detect_ffmpeg(require_ffmpeg: bool) -> ToolStatus {
    let ok = Command::new("ffmpeg")
        .arg("-version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);

    if ok {
        tracing::info!(target: "detect", available = true, "ffmpeg detected — video thumbnails and transcoding enabled");
        ToolStatus::Available
    } else if require_ffmpeg {
        tracing::error!(
            target: "detect",
            available = false,
            "ffmpeg required but not installed — set require_ffmpeg = false in settings.toml to disable this check"
        );
        crate::logging::console_print_raw(
            "  Install ffmpeg from: https://ffmpeg.org/download.html\n\n",
        );
        std::process::exit(1);
    } else {
        tracing::warn!(
            target: "detect",
            available = false,
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

/// Probe whether the detected ffmpeg has `libwebp` compiled in.
pub fn detect_webp_encoder(ffmpeg_ok: bool) -> bool {
    if !ffmpeg_ok {
        return false;
    }

    let has_webp = crate::media::ffmpeg::check_webp_encoder();

    if has_webp {
        tracing::info!(
            target: "detect",
            webp = true,
            "ffmpeg libwebp encoder available — image to WebP conversion enabled"
        );
    } else {
        tracing::warn!(
            target: "detect",
            webp = false,
            "ffmpeg libwebp encoder missing — JPEG/PNG/BMP/TIFF stored in original format"
        );
        if crate::logging::is_tty() {
            crate::logging::console_print_raw(&webp_install_hint());
        }
    }

    has_webp
}

fn webp_install_hint() -> String {
    let mut s = String::new();

    #[cfg(target_os = "macos")]
    {
        s.push_str(
            "  ── macOS: reinstall ffmpeg with libwebp ─────────────────────────────\n\
             \n\
             \x1b[2m  brew uninstall ffmpeg\n\
             \x1b[2m  brew tap homebrew-ffmpeg/ffmpeg\n\
             \x1b[2m  brew install homebrew-ffmpeg/ffmpeg/ffmpeg --with-webp\x1b[0m\n\n",
        );
    }
    #[cfg(target_os = "linux")]
    {
        s.push_str(
            "  ── Linux: install ffmpeg with libwebp ───────────────────────────────\n\
             \n\
             \x1b[2m  sudo apt update && sudo apt install ffmpeg libwebp-dev\x1b[0m\n\n",
        );
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        s.push_str(
            "  Reinstall ffmpeg with libwebp support. See: https://ffmpeg.org/download.html\n\n",
        );
    }

    if !crate::logging::is_tty() {
        // Strip any ANSI codes we added for TTY mode
        s.retain(|c| c != '\x1b');
    }
    s
}

/// Probe whether the detected ffmpeg has `libvpx-vp9` + `libopus` compiled in.
pub fn detect_webm_encoder(ffmpeg_ok: bool) -> bool {
    if !ffmpeg_ok {
        return false;
    }

    let has_vp9 = crate::media::ffmpeg::check_vp9_encoder();
    let has_opus = crate::media::ffmpeg::check_opus_encoder();
    let has_webm = has_vp9 && has_opus;

    if has_webm {
        tracing::info!(
            target: "detect",
            vp9 = true, opus = true,
            "ffmpeg VP9 + Opus encoders available — MP4 to WebM transcoding enabled"
        );
    } else {
        tracing::warn!(
            target: "detect",
            vp9   = has_vp9,
            opus  = has_opus,
            "ffmpeg VP9/Opus encoders missing — MP4 uploads stored as MP4"
        );
        if crate::logging::is_tty() {
            crate::logging::console_print_raw(&webm_install_hint(has_vp9, has_opus));
        }
    }

    has_webm
}

fn webm_install_hint(has_vp9: bool, has_opus: bool) -> String {
    let mut s = String::new();
    if !has_vp9 {
        s.push_str("  Missing: libvpx-vp9 (VP9 video encoder)\n");
    }
    if !has_opus {
        s.push_str("  Missing: libopus   (Opus audio encoder)\n");
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

// ─── Tor (Arti in-process) ────────────────────────────────────────────────────
//
// Previously: spawned the system `tor` binary as a subprocess, wrote a torrc,
// created two directories with chmod 0700, and polled for the hostname file
// for up to 120 seconds.
//
// Now: spawns one Tokio task that bootstraps Arti in-process, launches the
// onion service, and proxies incoming connections to the local HTTP server.
// No subprocess, no torrc, no hostname file, no polling loop.

use arti_client::{config::TorClientConfigBuilder, TorClient};
use futures::StreamExt;
use tokio::net::TcpStream;
use tokio::sync::RwLock;
use tor_cell::relaycell::msg::Connected;
use tor_hsservice::{config::OnionServiceConfigBuilder, handle_rend_requests, HsId, StreamRequest};

/// Spawn the Arti in-process Tor task.
///
/// Returns `Available` immediately. The onion address becomes available in
/// `onion_address` roughly 30 seconds later on first run or ~5 seconds on
/// subsequent runs (consensus served from `arti_cache/`).
///
/// If `enable_tor_support` is false this is a no-op and returns `Missing`.
pub fn detect_tor(
    enable_tor_support: bool,
    bind_port: u16,
    data_dir: &Path,
    onion_address: Arc<RwLock<Option<String>>>,
) -> ToolStatus {
    if !enable_tor_support {
        return ToolStatus::Missing;
    }

    let data_dir = data_dir.to_path_buf();

    tokio::spawn(async move {
        if let Err(e) = run_arti(data_dir, bind_port, onion_address).await {
            tracing::error!(target: "detect", error = %e, "Tor: fatal error in Arti task");
        }
    });

    tracing::info!(
        target: "detect",
        "Tor: Arti task spawned — bootstrapping in background (first run ~30 s)"
    );
    ToolStatus::Available
}

/// No-op. Previously killed the tor subprocess.
/// The [`TorClient`] is owned by the `tokio::spawn` task; dropping the runtime
/// closes all circuits cleanly.
#[allow(dead_code)]
pub const fn kill_tor() {}

// ─── Core Arti task ───────────────────────────────────────────────────────────

async fn run_arti(
    data_dir: std::path::PathBuf,
    bind_port: u16,
    onion_address: Arc<RwLock<Option<String>>>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // arti_cache/ — consensus cache (safe to delete; re-fetched on next start).
    // arti_state/ — service keypair. Back this up.
    //   Delete it only if you want a new .onion address.
    //
    // NOTE: TorClientConfigBuilder::from_directories takes AsRef<Path> directly.
    // Do NOT use the builder().storage().cache_dir(PathBuf) path — CfgPath does
    // not implement From<PathBuf> and it will not compile.
    let config = TorClientConfigBuilder::from_directories(
        data_dir.join("arti_state"),
        data_dir.join("arti_cache"),
    )
    .build()?;

    tracing::info!(
        target: "detect",
        cache_dir  = %data_dir.join("arti_cache").display(),
        state_dir  = %data_dir.join("arti_state").display(),
        "Tor: bootstrapping — first run downloads ~2 MB of directory data"
    );

    let tor_client = TorClient::create_bootstrapped(config)
        .await
        .map_err(|e| format!("Tor bootstrap failed: {e}"))?;

    tracing::info!(target: "detect", "Tor: connected to the Tor network");

    let svc_config = OnionServiceConfigBuilder::default()
        .nickname("rustchan".parse()?)
        .build()?;

    let (onion_service, rend_requests) = tor_client
        .launch_onion_service(svc_config)?
        .ok_or("launch_onion_service returned None — unexpected with code-only config")?;

    let hsid = onion_service
        .onion_address()
        .ok_or("onion_address() returned None immediately after launch")?;
    let onion_name = hsid_to_onion_address(hsid);

    tracing::info!(
        target: "detect",
        onion_address = %onion_name,
        keys_dir = %data_dir.join("arti_state").join("keys").display(),
        "Tor: hidden service active"
    );

    *onion_address.write().await = Some(onion_name.clone());

    if crate::logging::is_tty() {
        crate::logging::console_print_raw(&format!(
            "\n\
            ╔══════════════════════════════════════════════════════╗\n\
            ║  TOR ONION SERVICE ACTIVE  ✓                         ║\n\
            ╠══════════════════════════════════════════════════════╣\n\
            ║  http://{onion:<44}║\n\
            ║                                                      ║\n\
            ║  Keys stored at:                                     ║\n\
            ║    {keys:<50}║\n\
            ║  Back up this directory to preserve your address.    ║\n\
            ╚══════════════════════════════════════════════════════╝\n\n",
            onion = onion_name,
            keys = data_dir.join("arti_state/keys").display(),
        ));
    }

    let mut stream_requests = handle_rend_requests(rend_requests);

    while let Some(stream_req) = stream_requests.next().await {
        let local_addr = format!("127.0.0.1:{bind_port}");
        tokio::spawn(async move {
            if let Err(e) = proxy_tor_stream(stream_req, &local_addr).await {
                tracing::debug!(
                    target: "detect",
                    error = %e,
                    "Tor: stream closed"
                );
            }
        });
    }

    tracing::warn!(
        target: "detect",
        "Tor: rendezvous stream ended — onion service offline"
    );
    Ok(())
}

// ─── Connection proxy ─────────────────────────────────────────────────────────

async fn proxy_tor_stream(
    stream_req: StreamRequest,
    local_addr: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut tor_stream = stream_req.accept(Connected::new_empty()).await?;
    let mut local = TcpStream::connect(local_addr).await?;
    tokio::io::copy_bidirectional(&mut tor_stream, &mut local).await?;
    Ok(())
}

// ─── Onion address encoding ───────────────────────────────────────────────────

/// Encode an [`HsId`] (Ed25519 public key) as a v3 `.onion` address string.
///
/// [`HsId`] does not implement `std::fmt::Display` in arti-client 0.40.
/// Encoded manually using `HsId: AsRef<[u8; 32]>`.
///
/// Format: `base32( pubkey || sha3_256(".onion checksum" || pubkey || version)[..2] || version )`
fn hsid_to_onion_address(hsid: HsId) -> String {
    use sha3::{Digest, Sha3_256};

    let pubkey: &[u8; 32] = hsid.as_ref();
    let version: u8 = 3;

    let mut hasher = Sha3_256::new();
    hasher.update(b".onion checksum");
    hasher.update(pubkey);
    hasher.update([version]);
    let hash = hasher.finalize();

    // Destructure via iterator — avoids the indexing_slicing lint.
    // Safe: Sha3_256 always produces exactly 32 bytes.
    let mut iter = hash.iter().copied();
    let checksum = [iter.next().unwrap_or(0), iter.next().unwrap_or(0)];

    let mut address_bytes = [0u8; 35];
    address_bytes[..32].copy_from_slice(pubkey);
    address_bytes[32..34].copy_from_slice(&checksum);
    address_bytes[34] = version;

    let encoded = data_encoding::BASE32_NOPAD
        .encode(&address_bytes)
        .to_ascii_lowercase();

    format!("{encoded}.onion")
}
