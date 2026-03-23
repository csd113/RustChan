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
            "  Install ffmpeg from: https://ffmpeg.org/download.html\n\n",
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
            target: "rustchan::detect",
            vp9 = true, opus = true,
            "ffmpeg VP9 + Opus encoders available — MP4 to WebM transcoding enabled"
        );
    } else {
        tracing::warn!(
            target: "rustchan::detect",
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
use dashmap::DashMap;
use futures::StreamExt;
use rand_core::{OsRng, RngCore};
use std::sync::LazyLock;
use tokio::net::TcpStream;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;
use tor_cell::relaycell::msg::Connected;
use tor_hsservice::{config::OnionServiceConfigBuilder, handle_rend_requests, HsId, StreamRequest};

// ─── Per-stream identity map ──────────────────────────────────────────────────
//
// CRIT-2A fix: maps the ephemeral local port of each Tor proxy connection to a
// random pseudonymous token.
//
// How it works:
//   1. proxy_tor_stream() connects to 127.0.0.1:{bind_port}. The OS assigns
//      an ephemeral source port on the connecting end.
//   2. That ephemeral port is what axum's ConnectInfo sees as the peer port
//      on the accepted socket.
//   3. A random "tor:<hex>" token is inserted into this map keyed by that port.
//   4. ClientIp / extract_ip look up the peer port in this map and return the
//      token as the request's "IP address".
//   5. A RAII TokenGuard removes the entry when the proxy task ends.
//
// Result: every Tor stream gets its own isolated bucket for rate limiting, post
// cooldowns, and bans — one Tor user's actions no longer affect all others.
// The token is random per-stream so it cannot track users across reconnections.
pub static TOR_STREAM_TOKENS: LazyLock<DashMap<u16, Arc<str>>> = LazyLock::new(DashMap::new);

/// Removes a port→token entry from `TOR_STREAM_TOKENS` when dropped.
struct TokenGuard(u16);
impl Drop for TokenGuard {
    fn drop(&mut self) {
        TOR_STREAM_TOKENS.remove(&self.0);
    }
}

/// Spawn the Arti in-process Tor task.
///
/// Returns `Some(JoinHandle)` when Tor support is enabled — the handle should
/// be stored and awaited during graceful shutdown (F-04). The onion address
/// becomes available in `onion_address` roughly 30 seconds after startup on
/// first run, or ~5 seconds on subsequent runs (consensus served from cache).
///
/// Returns `None` when `enable_tor_support` is false.
///
/// # CRIT-3 fix
/// Accepts a `CancellationToken` so the retry loop and backoff sleep both
/// respond to graceful shutdown. Without this the task had no cancel path and
/// was abandoned with a hard 10-second timeout, leaving Tor circuits open.
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

    // F-02: Retry loop with cancellation support.
    // Backoff: 30 s, 60 s, 120 s, 240 s, 480 s (capped at 2^4 × 30 s).
    // HIGH-7 fix: attempt resets to 0 after a healthy long-running session so
    // clean exits don't accumulate backoff identically to crash loops.
    let handle = tokio::spawn(async move {
        // Yield briefly before emitting any output. detect_tor() is called
        // after the first-run admin wizard (which is synchronous), but the Tokio
        // runtime schedules this task immediately after tokio::spawn returns.
        // A short sleep lets the startup banner and any queued tracing events
        // flush to the terminal before Tor starts printing, preventing
        // interleaving with interactive prompts or the keyboard handler startup.
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        let mut attempt = 0u32;
        loop {
            tracing::info!(target: "rustchan::detect", attempt, "Tor: starting Arti");
            let run_start = std::time::Instant::now();

            // CRIT-3: race run_arti against the shutdown token.
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
                    // HIGH-7: a clean exit after ≥60 s of healthy operation is not
                    // a crash — reset attempt so backoff stays short on reconnect.
                    if run_start.elapsed() >= std::time::Duration::from_secs(60) {
                        attempt = 0;
                    }
                    tracing::warn!(target: "rustchan::detect", "Tor: Arti exited cleanly");
                }
                Err(e) => {
                    tracing::error!(target: "rustchan::detect", error = %e, attempt, "Tor: fatal error");
                }
            }

            // Clear stale address so UI correctly shows Tor as offline.
            *onion_address.write().await = None;

            let backoff =
                std::time::Duration::from_secs(30_u64.saturating_mul(1 << attempt.min(4)));
            tracing::warn!(target: "rustchan::detect", retry_in = ?backoff, "Tor: scheduling restart");

            // CRIT-3: also cancel-aware during the backoff sleep.
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

/// Previously killed the tor subprocess.
/// The [`TorClient`] is owned by the `tokio::spawn` task; dropping the runtime
/// closes all circuits cleanly. This function is a no-op and should not be called.
#[deprecated(
    since = "1.1.0",
    note = "Arti lifecycle is managed by the runtime. Dropping TorClient closes circuits cleanly. This fn is a no-op."
)]
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
        target: "rustchan::detect",
        cache_dir  = %data_dir.join("arti_cache").display(),
        state_dir  = %data_dir.join("arti_state").display(),
        "Tor: bootstrapping — first run downloads ~2 MB of directory data"
    );

    // F-01: Wrap bootstrap in a timeout. Without this, a captive portal,
    // strict firewall, or Tor directory downtime hangs this task forever.
    // HIGH-2 fix: timeout sourced from CONFIG instead of a hardcoded constant
    // so operators on censored networks can increase it via settings.toml.
    let bootstrap_timeout =
        std::time::Duration::from_secs(crate::config::CONFIG.tor_bootstrap_timeout_secs);
    // KEEP ALIVE: dropping tor_client closes all Tor circuits and kills the onion service.
    let tor_client =
        tokio::time::timeout(bootstrap_timeout, TorClient::create_bootstrapped(config))
            .await
            .map_err(|_| {
                format!(
                    "Tor bootstrap timed out after {} s — check network connectivity \
             (increase tor_bootstrap_timeout_secs in settings.toml for censored networks)",
                    crate::config::CONFIG.tor_bootstrap_timeout_secs,
                )
            })?
            .map_err(|e| format!("Tor bootstrap failed: {e}"))?;

    tracing::info!(target: "rustchan::detect", "Tor: connected to the Tor network");

    // Security hardening options available in OnionServiceConfigBuilder (Arti 0.40):
    //   .pow_resistance(...)          — proof-of-work DoS resistance
    //   .rate_limit_num_intro_points  — cap introduction point abuse
    // Currently left at defaults. Consider exposing these in settings.toml (F-18).
    //
    // MED-3 fix: nickname sourced from CONFIG.tor_service_nickname (default
    // "rustchan") so operators running multiple instances with a shared
    // arti_state/ directory can assign distinct names and avoid key collisions.
    let svc_config = OnionServiceConfigBuilder::default()
        .nickname(crate::config::CONFIG.tor_service_nickname.parse()?)
        .build()?;

    // KEEP ALIVE: dropping onion_service deregisters the service from the Tor network.
    let (onion_service, rend_requests) = tor_client
        .launch_onion_service(svc_config)?
        .ok_or("launch_onion_service returned None — unexpected with code-only config")?;

    // F-03: onion_address() can return None during early bringup in Arti 0.40;
    // key material is not guaranteed to be readable synchronously at launch time.
    // Retry up to 10 times at 500 ms intervals (5 s total) before failing.
    let hsid = {
        let mut found = None;
        for i in 0..10u32 {
            found = onion_service.onion_address();
            if found.is_some() {
                break;
            }
            tracing::debug!(target: "rustchan::detect", attempt = i, "Tor: waiting for HsId availability");
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }
        found.ok_or("onion_address() still None after 5 s — service key unavailable")?
    };
    let onion_name = hsid_to_onion_address(hsid);
    publish_onion_address(&onion_name, &data_dir, &onion_address).await;

    let mut stream_requests = handle_rend_requests(rend_requests);

    // F-05: Cap concurrent proxy tasks to prevent file-descriptor exhaustion
    // under a connection flood. Excess connections are dropped (Arti sends
    // RELAY_END automatically when stream_req is dropped).
    // HIGH-3 fix: limit sourced from CONFIG so operators can tune it without
    // recompiling, e.g. to reduce FD pressure on resource-constrained hosts.
    let max_streams = crate::config::CONFIG.tor_max_concurrent_streams;
    let sem = std::sync::Arc::new(tokio::sync::Semaphore::new(max_streams));

    // HIGH-5 fix: Arc<str> avoids a String heap allocation per connection.
    let local_addr: Arc<str> = Arc::from(format!("127.0.0.1:{bind_port}").as_str());

    while let Some(stream_req) = stream_requests.next().await {
        match std::sync::Arc::clone(&sem).try_acquire_owned() {
            Ok(permit) => {
                let addr = Arc::clone(&local_addr);
                tokio::spawn(async move {
                    let _permit = permit; // released on drop
                                          // HIGH-4 fix: distinguish infrastructure failures (axum
                                          // unreachable) from normal stream closure (client disconnect,
                                          // EOF). Infrastructure errors go to ERROR; everything else
                                          // stays at DEBUG so the log isn't flooded by normal traffic.
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
                // Dropping stream_req causes Arti to send a RELAY_END cell.
            }
        }
    }

    // CRIT-5/6: belt-and-suspenders keepalives. Named let-bindings in Rust
    // drop at end of their enclosing scope (the function body), not at
    // last-use — so tor_client and onion_service are already live here.
    // These explicit borrows make the intent unambiguous to future readers
    // and guard against any tooling that might warn about "unused" bindings.
    let _ = &tor_client;
    let _ = &onion_service;

    tracing::warn!(
        target: "rustchan::detect",
        "Tor: rendezvous stream ended — onion service offline"
    );
    Ok(())
}

// ─── Onion address publication ────────────────────────────────────────────────

/// Log the active onion address, write it to the shared state, and print the
/// TTY banner. Extracted from `run_arti` to keep that function under the
/// clippy line-count limit.
///
/// MED-7/MED-11: the address is logged at DEBUG only so it never appears in
/// plaintext in JSON log files forwarded to aggregators. Set
/// `RUST_LOG=detect=debug` to see it in logs; the TTY banner and admin panel
/// always show the full address.
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
}

// ─── Connection proxy ─────────────────────────────────────────────────────────

async fn proxy_tor_stream(
    stream_req: StreamRequest,
    local_addr: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut tor_stream = stream_req.accept(Connected::new_empty()).await?;

    // MED-10 fix: increased from 5 s to 15 s — under load the axum TCP accept
    // queue can fill and connect() can legitimately take several seconds.
    let mut local = tokio::time::timeout(
        std::time::Duration::from_secs(15),
        TcpStream::connect(local_addr),
    )
    .await
    .map_err(|_| "timed out connecting to local HTTP server")?
    .map_err(|e| format!("local TCP connect failed: {e}"))?;

    // CRIT-2A: Register a per-stream pseudonymous token keyed on the ephemeral
    // local port. axum's ConnectInfo sees this port as the peer port on the
    // incoming socket, so ClientIp / extract_ip can retrieve the token without
    // any HTTP parsing, through keep-alive, across all content types.
    let local_port = local.local_addr().map(|a| a.port()).unwrap_or(0);
    let token: Arc<str> = {
        let mut bytes = [0u8; 16];
        OsRng.fill_bytes(&mut bytes);
        Arc::from(format!("tor:{}", hex::encode(bytes)).as_str())
    };
    // _guard removes the map entry when this task ends (connection closed or error).
    let _guard = if local_port != 0 {
        TOR_STREAM_TOKENS.insert(local_port, Arc::clone(&token));
        Some(TokenGuard(local_port))
    } else {
        tracing::debug!(target: "rustchan::detect", "Tor: could not determine local port — stream uses shared bucket");
        None
    };

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
    // HIGH-1 fix: use a typed [u8;32] array instead of an iterator with
    // unwrap_or(0). Sha3_256 always produces 32 bytes — the fallback was dead
    // code that masked potential logic errors if the digest size ever changed.
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

    /// Verify the v3 onion address encoder against a Python-computed reference
    /// value for the all-zeros Ed25519 key.
    ///
    /// Reference:
    /// ```python
    /// import hashlib, base64
    /// pub = bytes(32); ver = bytes([3])
    /// chk = hashlib.sha3_256(b'.onion checksum' + pub + ver).digest()[:2]
    /// raw = pub + chk + ver
    /// print(base64.b32encode(raw).decode().lower().rstrip('=') + '.onion')
    /// # → aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaam2dqd.onion
    /// ```
    #[test]
    fn onion_address_format_is_56_chars_plus_dot_onion() {
        let zeroed: [u8; 32] = [0u8; 32];
        let hsid = HsId::from(zeroed);
        let addr = hsid_to_onion_address(hsid);

        // Verified test vector: Python sha3_256 reference + base32 encoding.
        assert_eq!(
            addr, "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaam2dqd.onion",
            "zero-key address must match Python reference implementation"
        );

        assert_eq!(addr.len(), 62, "v3 onion address must be 62 chars total");
        // Use index slicing instead of ends_with / trim_end_matches to avoid
        // the clippy::case_sensitive_file_extension_comparisons lint.
        // The encoder always produces lowercase output, and the length is
        // already asserted above, so fixed-index slicing is safe here.
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
