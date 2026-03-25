// detect.rs — Startup detection for optional external tools.
//
// All terminal output goes through crate::logging helpers so it is
// serialised with the tracing terminal layer (CONSOLE_MUTEX). This
// prevents interleaving with concurrent log events during startup.
//
// Structured events (info!/warn!/error!) capture the detection result in
// the JSON log file with structured fields. Human-readable install
// instructions are written via console_print_raw so they appear only
// on TTY-mode terminals and never pollute piped / systemd output.
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Arc;

/// Result of probing for a tool at startup.
///
/// Note: the `Spawning` variant that existed in v1.0 has been removed.
/// `detect_tor` now returns `Option<JoinHandle<()>>` directly; it no longer
/// uses this enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolStatus {
    /// Tool is ready for use immediately (e.g. ffmpeg).
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
        tracing::info!(target: "rustchan::detect", available = true, "ffmpeg detected — video thumbnails and transcoding enabled");
        ToolStatus::Available
    } else if require_ffmpeg {
        tracing::error!(
            target: "rustchan::detect",
            available = false,
            "ffmpeg required but not installed — set require_ffmpeg = false in settings.toml to disable this check"
        );
        crate::logging::console_print_raw(
            " Install ffmpeg from: https://ffmpeg.org/download.html\n\n",
        );
        std::process::exit(1);
    } else {
        tracing::warn!(
            target: "rustchan::detect",
            available = false,
            "ffmpeg not detected — video thumbnails and transcoding disabled"
        );
        if crate::logging::is_tty() {
            crate::logging::console_print_raw(
                " Install from: https://ffmpeg.org/download.html\n\n",
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
            target: "rustchan::detect",
            webp = true,
            "ffmpeg libwebp encoder available — image to WebP conversion enabled"
        );
    } else {
        tracing::warn!(
            target: "rustchan::detect",
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
            " ── macOS: reinstall ffmpeg with libwebp ─────────────────────────────\n\
             \n\
             \x1b[2m brew uninstall ffmpeg\n\
             \x1b[2m brew tap homebrew-ffmpeg/ffmpeg\n\
             \x1b[2m brew install homebrew-ffmpeg/ffmpeg/ffmpeg --with-webp\x1b[0m\n\n",
        );
    }
    #[cfg(target_os = "linux")]
    {
        s.push_str(
            " ── Linux: install ffmpeg with libwebp ───────────────────────────────\n\
             \n\
             \x1b[2m sudo apt update && sudo apt install ffmpeg libwebp-dev\x1b[0m\n\n",
        );
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        s.push_str(
            " Reinstall ffmpeg with libwebp support. See: https://ffmpeg.org/download.html\n\n",
        );
    }
    if !crate::logging::is_tty() {
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
            target: "rustchan::detect",
            vp9 = true, opus = true,
            "ffmpeg VP9 + Opus encoders available — MP4 to WebM transcoding enabled"
        );
    } else {
        tracing::warn!(
            target: "rustchan::detect",
            vp9 = has_vp9,
            opus = has_opus,
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
        s.push_str(" Missing: libvpx-vp9 (VP9 video encoder)\n");
    }
    if !has_opus {
        s.push_str(" Missing: libopus (Opus audio encoder)\n");
    }
    s.push('\n');
    #[cfg(target_os = "macos")]
    {
        s.push_str(
            " ── macOS: reinstall ffmpeg with VP9 + Opus ──────────────────────────\n\
             \n\
             \x1b[2m brew install ffmpeg\x1b[0m\n\n",
        );
    }
    #[cfg(target_os = "linux")]
    {
        s.push_str(
            " ── Linux: install ffmpeg with VP9 + Opus ────────────────────────────\n\
             \n\
             \x1b[2m sudo apt update && sudo apt install ffmpeg libvpx-dev libopus-dev\x1b[0m\n\n",
        );
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        s.push_str(
            " Reinstall ffmpeg with libvpx-vp9 and libopus support.\n\
             See: https://ffmpeg.org/download.html\n\n",
        );
    }
    if !crate::logging::is_tty() {
        s.retain(|c| c != '\x1b');
    }
    s
}
// ─── Tor (Arti in-process) ────────────────────────────────────────────────────
use arti_client::{config::TorClientConfigBuilder, TorClient};
use dashmap::DashMap;
use futures::StreamExt;
use rand_core::{OsRng, RngCore};
use std::sync::LazyLock;
use tokio::net::TcpStream;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;
use tor_cell::relaycell::msg::Connected;
use tor_hsservice::{config::OnionServiceConfigBuilder, handle_rend_requests, HsId, StreamRequest};
use tor_rtcompat::PreferredRuntime;
// ─── Per-stream identity map ──────────────────────────────────────────────────
pub static TOR_STREAM_TOKENS: LazyLock<DashMap<u16, Arc<str>>> = LazyLock::new(DashMap::new);

/// Removes a port→token entry from `TOR_STREAM_TOKENS` when dropped.
struct TokenGuard(u16);
impl Drop for TokenGuard {
    fn drop(&mut self) {
        TOR_STREAM_TOKENS.remove(&self.0);
    }
}

/// Spawn the Arti in-process Tor task.
pub fn detect_tor(
    enable_tor_support: bool,
    bind_port: u16,
    data_dir: &Path,
    onion_address: Arc<RwLock<Option<String>>>,
    cancel: CancellationToken,
) -> Option<tokio::task::JoinHandle<()>> {
    if !enable_tor_support {
        return None;
    }
    let data_dir = data_dir.to_path_buf();
    let handle = tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        let mut attempt = 0u32;
        loop {
            tracing::info!(target: "rustchan::detect", attempt, "Tor: starting Arti");
            let run_start = std::time::Instant::now();
            let result = tokio::select! {
                r = run_arti(data_dir.clone(), bind_port, onion_address.clone()) => r,
                () = cancel.cancelled() => {
                    tracing::info!(target: "rustchan::detect", "Tor: shutdown signal — exiting");
                    *onion_address.write().await = None;
                    return;
                }
            };
            match result {
                Ok(()) => {
                    if run_start.elapsed() >= std::time::Duration::from_secs(60) {
                        attempt = 0;
                    }
                    tracing::warn!(target: "rustchan::detect", "Tor: Arti exited cleanly");
                }
                Err(e) => {
                    tracing::error!(target: "rustchan::detect", error = %e, attempt, "Tor: fatal error");
                }
            }
            *onion_address.write().await = None;
            let backoff =
                std::time::Duration::from_secs(30_u64.saturating_mul(1 << attempt.min(4)));
            tracing::warn!(target: "rustchan::detect", retry_in = ?backoff, "Tor: scheduling restart");
            tokio::select! {
                () = tokio::time::sleep(backoff) => {}
                () = cancel.cancelled() => {
                    tracing::info!(target: "rustchan::detect", "Tor: shutdown during backoff — exiting");
                    return;
                }
            }
            attempt = attempt.saturating_add(1);
        }
    });
    tracing::info!(
        target: "rustchan::detect",
        "Tor: Arti task spawned — bootstrapping in background (first run ~30 s)"
    );
    Some(handle)
}

#[deprecated(
    since = "1.1.0",
    note = "Arti lifecycle is managed by the runtime. Dropping TorClient closes circuits cleanly. This fn is a no-op."
)]
#[allow(dead_code)]
pub const fn kill_tor() {}
// ─── Arti helper types ───────────────────────────────────────────────────────
type ArtiResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;
type DynError = Box<dyn std::error::Error + Send + Sync>;
// ─── Arti helper: bootstrap client ───────────────────────────────────────────
async fn bootstrap_tor_client(
    data_dir: &std::path::Path,
) -> Result<TorClient<PreferredRuntime>, DynError> {
    let config = TorClientConfigBuilder::from_directories(
        data_dir.join("arti_state"),
        data_dir.join("arti_cache"),
    )
    .build()
    .map_err(|e| Box::new(e) as DynError)?;

    tracing::info!(
        target: "rustchan::detect",
        cache_dir = %data_dir.join("arti_cache").display(),
        state_dir = %data_dir.join("arti_state").display(),
        "Tor: bootstrapping — first run downloads ~2 MB of directory data"
    );

    let bootstrap_timeout =
        std::time::Duration::from_secs(crate::config::CONFIG.tor_bootstrap_timeout_secs);

    let tor_client =
        tokio::time::timeout(bootstrap_timeout, TorClient::create_bootstrapped(config))
            .await
            .map_err(|_| {
                let msg = format!(
                    "Tor bootstrap timed out after {} s — check network connectivity \
             (increase tor_bootstrap_timeout_secs in settings.toml for censored networks)",
                    crate::config::CONFIG.tor_bootstrap_timeout_secs,
                );
                Box::new(std::io::Error::new(std::io::ErrorKind::TimedOut, msg)) as DynError
            })?
            .map_err(|e| {
                let msg = format!("Tor bootstrap failed: {e}");
                Box::new(std::io::Error::other(msg)) as DynError
            })?;

    tracing::info!(target: "rustchan::detect", "Tor: connected to the Tor network");
    Ok(tor_client)
}
// ─── Arti helper: launch onion service ────────────────────────────────────────
fn launch_onion_service(
    tor_client: &TorClient<PreferredRuntime>,
) -> Result<
    (
        Arc<tor_hsservice::RunningOnionService>,
        impl futures::Stream<Item = tor_hsservice::RendRequest>,
    ),
    DynError,
> {
    let svc_config = OnionServiceConfigBuilder::default()
        .nickname(
            crate::config::CONFIG
                .tor_service_nickname
                .parse()
                .map_err(|e| Box::new(e) as DynError)?,
        )
        .build()
        .map_err(|e| Box::new(e) as DynError)?;

    let (onion_service, rend_requests) = tor_client
        .launch_onion_service(svc_config)
        .map_err(|e| Box::new(e) as DynError)?
        .ok_or_else(|| {
            let msg = "launch_onion_service returned None — unexpected with code-only config";
            Box::new(std::io::Error::other(msg)) as DynError
        })?;

    Ok((onion_service, rend_requests))
}
// ─── Arti helper: resolve HsId with retries ──────────────────────────────────
async fn resolve_hsid(
    onion_service: &tor_hsservice::RunningOnionService,
) -> Result<HsId, DynError> {
    let mut found = None;
    for i in 0..10u32 {
        found = onion_service.onion_address();
        if found.is_some() {
            break;
        }
        tracing::debug!(target: "rustchan::detect", attempt = i, "Tor: waiting for HsId availability");
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
    found.ok_or_else(|| {
        let msg = "onion_address() still None after 5 s — service key unavailable";
        Box::new(std::io::Error::other(msg)) as DynError
    })
}
// ─── Arti helper: accept stream loop ─────────────────────────────────────────
async fn accept_streams(
    rend_requests: impl futures::Stream<Item = tor_hsservice::RendRequest>,
    bind_port: u16,
) {
    let mut stream_requests = std::pin::pin!(handle_rend_requests(rend_requests));
    let max_streams = crate::config::CONFIG.tor_max_concurrent_streams;
    let sem = std::sync::Arc::new(tokio::sync::Semaphore::new(max_streams));
    let local_addr: Arc<str> = Arc::from(format!("127.0.0.1:{bind_port}").as_str());

    while let Some(stream_req) = stream_requests.next().await {
        match std::sync::Arc::clone(&sem).try_acquire_owned() {
            Ok(permit) => {
                let addr = Arc::clone(&local_addr);
                tokio::spawn(async move {
                    let _permit = permit;
                    if let Err(e) = proxy_tor_stream(stream_req, &addr).await {
                        let msg = e.to_string();
                        if msg.contains("local TCP connect failed")
                            || msg.contains("timed out connecting to local HTTP server")
                        {
                            tracing::error!(
                                target: "rustchan::detect",
                                error = %e,
                                "Tor: cannot reach local HTTP server — is axum running?"
                            );
                        } else {
                            tracing::debug!(
                                target: "rustchan::detect",
                                error = %e,
                                "Tor: stream closed"
                            );
                        }
                    }
                });
            }
            Err(_) => {
                tracing::warn!(
                    target: "rustchan::detect",
                    limit = max_streams,
                    "Tor: stream limit reached — dropping connection"
                );
            }
        }
    }
}
// ─── Core Arti task ───────────────────────────────────────────────────────────
async fn run_arti(
    data_dir: std::path::PathBuf,
    bind_port: u16,
    onion_address: Arc<RwLock<Option<String>>>,
) -> ArtiResult {
    let tor_client = bootstrap_tor_client(&data_dir).await?;
    let (onion_service, rend_requests) = launch_onion_service(&tor_client)?;
    let hsid = resolve_hsid(&onion_service).await?;

    let onion_name = hsid_to_onion_address(hsid);
    publish_onion_address(&onion_name, &data_dir, &onion_address).await;

    accept_streams(rend_requests, bind_port).await;

    let _ = &tor_client;
    let _ = &onion_service;
    tracing::warn!(
        target: "rustchan::detect",
        "Tor: rendezvous stream ended — onion service offline"
    );
    Ok(())
}
// ─── Onion address publication ────────────────────────────────────────────────
async fn publish_onion_address(
    onion_name: &str,
    data_dir: &std::path::Path,
    onion_address: &RwLock<Option<String>>,
) {
    tracing::debug!(
        target: "rustchan::detect",
        onion_address = %onion_name,
        keys_dir = %data_dir.join("arti_state").join("keys").display(),
        "Tor: hidden service active"
    );
    tracing::info!(target: "rustchan::detect", "Tor: hidden service active");
    *onion_address.write().await = Some(onion_name.to_string());

    if crate::logging::is_tty() {
        let keys_path = data_dir.join("arti_state/keys");
        let keys_display = keys_path.display().to_string();
        let keys = if keys_display.len() > 60 {
            let start = keys_display.len().saturating_sub(57);
            format!("...{}", &keys_display[start..])
        } else {
            keys_display
        };

        let url_line = format!("http://{onion_name}");

        // Inner width between ║ and ║ is 78 characters (80 total with borders)
        crate::logging::console_print_raw(&format!(
            "\n\
            ╔══════════════════════════════════════════════════════════════════════════════╗\n\
            ║ {:<76} ║\n\
            ╠══════════════════════════════════════════════════════════════════════════════╣\n\
            ║ {:<76} ║\n\
            ║ {:<76} ║\n\
            ║ {:<76} ║\n\
            ║ {:<76} ║\n\
            ║ {:<76} ║\n\
            ╚══════════════════════════════════════════════════════════════════════════════╝\n\n",
            "TOR ONION SERVICE ACTIVE ✓",
            url_line,
            "",
            "Keys stored at:",
            keys,
            "Back up this directory to preserve your address.",
        ));
    }
}
// ─── Connection proxy ─────────────────────────────────────────────────────────
async fn proxy_tor_stream(
    stream_req: StreamRequest,
    local_addr: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut tor_stream = stream_req
        .accept(Connected::new_empty())
        .await
        .map_err(|e| {
            Box::new(std::io::Error::other(format!(
                "Tor stream accept failed: {e}"
            ))) as Box<dyn std::error::Error + Send + Sync>
        })?;

    let mut local = tokio::time::timeout(
        std::time::Duration::from_secs(15),
        TcpStream::connect(local_addr),
    )
    .await
    .map_err(|_| {
        Box::new(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "timed out connecting to local HTTP server",
        )) as Box<dyn std::error::Error + Send + Sync>
    })?
    .map_err(|e| {
        let msg = format!("local TCP connect failed: {e}");
        Box::new(std::io::Error::other(msg)) as Box<dyn std::error::Error + Send + Sync>
    })?;

    let local_port = local.local_addr().map(|a| a.port()).unwrap_or(0);
    let token: Arc<str> = {
        let mut bytes = [0u8; 16];
        OsRng.fill_bytes(&mut bytes);
        let hex = hex::encode(bytes);
        Arc::from(format!("tor:{hex}").as_str())
    };
    let _guard = if local_port != 0 {
        TOR_STREAM_TOKENS.insert(local_port, Arc::clone(&token));
        Some(TokenGuard(local_port))
    } else {
        tracing::debug!(target: "rustchan::detect", "Tor: could not determine local port — stream uses shared bucket");
        None
    };

    tokio::io::copy_bidirectional(&mut tor_stream, &mut local)
        .await
        .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)?;

    Ok(())
}
// ─── Onion address encoding ───────────────────────────────────────────────────
fn hsid_to_onion_address(hsid: HsId) -> String {
    use sha3::{Digest, Sha3_256};
    let pubkey: &[u8; 32] = hsid.as_ref();
    let version: u8 = 3;
    let mut hasher = Sha3_256::new();
    hasher.update(b".onion checksum");
    hasher.update(pubkey);
    hasher.update([version]);
    let hash: [u8; 32] = hasher.finalize().into();
    let checksum = [hash[0], hash[1]];
    let mut address_bytes = [0u8; 35];
    address_bytes[..32].copy_from_slice(pubkey);
    address_bytes[32..34].copy_from_slice(&checksum);
    address_bytes[34] = version;
    let encoded = data_encoding::BASE32_NOPAD
        .encode(&address_bytes)
        .to_ascii_lowercase();
    format!("{encoded}.onion")
}
// ─── Tests ────────────────────────────────────────────────────────────────────
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn onion_address_format_is_56_chars_plus_dot_onion() {
        let zeroed: [u8; 32] = [0u8; 32];
        let hsid = HsId::from(zeroed);
        let addr = hsid_to_onion_address(hsid);
        assert_eq!(
            addr, "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaam2dqd.onion",
            "zero-key address must match Python reference implementation"
        );
        assert_eq!(addr.len(), 62, "v3 onion address must be 62 chars total");
        assert_eq!(&addr[56..], ".onion", "must end with .onion");
        let base32_part = &addr[..56];
        assert_eq!(base32_part.len(), 56, "base32 part must be 56 chars");
        assert!(
            base32_part
                .chars()
                .all(|c| matches!(c, 'a'..='z' | '2'..='7')),
            "base32 part must be lowercase a-z2-7 only"
        );
    }
}
