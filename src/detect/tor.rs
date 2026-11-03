//! In-process onion service, loopback proxy, and ephemeral stream identities.

use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

// Arti onion service

use arti_client::{config::TorClientConfigBuilder, TorClient};
use dashmap::DashMap;
use futures::StreamExt as _;
use std::sync::LazyLock;
use tokio::net::TcpStream;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;
use tor_cell::relaycell::msg::Connected;
use tor_hsservice::{config::OnionServiceConfigBuilder, handle_rend_requests, HsId, StreamRequest};

// Per-stream identity map
// random pseudonymous token.
//
// How it works:
//   1. proxy_tor_stream() connects to 127.0.0.1:{bind_port}. The OS assigns
//      an ephemeral source port on the connecting end.
//   2. That ephemeral port is what axum's ConnectInfo sees as the peer port
//      on the accepted socket.
//   3. A random "tor:<hex>" token is inserted into this map keyed by that exact
//      source socket address.
//   4. ClientIp / extract_ip look up the peer address in this map and return the
//      token as the request's "IP address".
//   5. A RAII TokenGuard removes the entry when the proxy task ends.
//
// Result: every Tor stream gets its own isolated bucket for rate limiting, post
// cooldowns, and bans — one Tor user's actions no longer affect all others.
// The token is random per-stream so it cannot track users across reconnections.
/// Active loopback proxy peers mapped to their ephemeral Tor identity token.
pub static TOR_STREAM_TOKENS: LazyLock<DashMap<SocketAddr, Arc<str>>> = LazyLock::new(DashMap::new);

/// Resolves an eligible loopback peer to its per-stream Tor identity token.
#[must_use]
pub fn tor_stream_token_identity(
    peer: Option<SocketAddr>,
    enable_tor_support: bool,
) -> Option<String> {
    if !enable_tor_support {
        return None;
    }
    let addr = peer?;
    if !addr.ip().is_loopback() {
        return None;
    }
    TOR_STREAM_TOKENS
        .get(&addr)
        .map(|token| token.value().to_string())
}

/// Removes a peer-address→token entry from `TOR_STREAM_TOKENS` when dropped.
struct TokenGuard(SocketAddr);
impl Drop for TokenGuard {
    fn drop(&mut self) {
        let _previous_value = TOR_STREAM_TOKENS.remove(&self.0);
    }
}

/// Spawn the Arti in-process Tor task.
///
/// Returns a handle when Tor is enabled. Retain and await it during graceful
/// shutdown; the onion address is published asynchronously through
/// `onion_address`.
///
/// Returns `None` when `enable_tor_support` is false.
///
/// The cancellation token stops both the retry loop and backoff sleep so Tor
/// circuits can close during graceful shutdown.
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

    // Crash loops back off from 30 to 480 seconds. Runs lasting at least one
    // minute reset the backoff before reconnecting.
    let handle = tokio::spawn(async move {
        // Let queued startup output flush before Tor begins logging.
        tokio::time::sleep(Duration::from_millis(200)).await;

        let mut attempt = 0u32;
        loop {
            tracing::info!(
                target: "rustchan::detect",
                attempt = attempt.saturating_add(1),
                "Starting Tor"
            );
            let run_start = Instant::now();
            let result = tokio::select! {
                r = run_arti(data_dir.clone(), bind_port, Arc::clone(&onion_address)) => r,
                () = cancel.cancelled() => {
                    tracing::info!(target: "rustchan::detect", "Tor: shutdown signal — exiting");
                    *onion_address.write().await = None;
                    return;
                }
            };

            match result {
                Ok(()) => {
                    // Stable runs reconnect without inheriting an old crash-loop backoff.
                    if run_start.elapsed() >= Duration::from_mins(1) {
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

            let backoff = Duration::from_secs(30_u64.saturating_mul(1 << attempt.min(4)));
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

// Core Arti task
#[expect(
    clippy::cognitive_complexity,
    reason = "the Tor bootstrap and onion-service lifecycle is a single fail-closed state machine"
)]
/// Runs one complete Arti client and onion-service lifecycle.
async fn run_arti(
    _data_dir: std::path::PathBuf,
    bind_port: u16,
    onion_address: Arc<RwLock<Option<String>>>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // runtime/tor/cache/ — consensus cache (safe to delete; re-fetched on next start).
    // runtime/tor/state/keystore/ — service keypair. Back this up.
    //   Delete it only if you want a new .onion address.
    //
    // NOTE: TorClientConfigBuilder::from_directories takes AsRef<Path> directly.
    // Do NOT use the builder().storage().cache_dir(PathBuf) path — CfgPath does
    // not implement From<PathBuf> and it will not compile.
    let config = TorClientConfigBuilder::from_directories(
        crate::config::runtime_tor_state_dir(),
        crate::config::runtime_tor_cache_dir(),
    )
    .build()?;

    let cache_dir = crate::config::runtime_tor_cache_dir();
    let state_dir = crate::config::runtime_tor_state_dir();
    let service_key_dir = crate::config::runtime_tor_hidden_service_keys_dir();
    tracing::info!(
        target: "rustchan::detect",
        cache_dir  = %cache_dir.display(),
        state_dir  = %state_dir.display(),
        key_dir    = %service_key_dir.display(),
        "Tor: bootstrapping — first run downloads ~2 MB of directory data"
    );

    // Bound captive-portal, firewall, and directory-outage stalls. Operators on
    // censored networks can raise the timeout in settings.toml.
    let bootstrap_timeout = Duration::from_secs(crate::config::CONFIG.tor_bootstrap_timeout_secs);
    // This handle owns the Tor circuits and must outlive the onion service.
    let tor_client: Arc<TorClient<_>> =
        tokio::time::timeout(bootstrap_timeout, TorClient::create_bootstrapped(config))
            .await
            .map_err(|_error| {
                format!(
                    "Tor bootstrap timed out after {} s — check network connectivity \
             (increase tor_bootstrap_timeout_secs in settings.toml for censored networks)",
                    crate::config::CONFIG.tor_bootstrap_timeout_secs,
                )
            })?
            .map_err(|e| format!("Tor bootstrap failed: {e}"))?;

    tracing::info!(target: "rustchan::detect", "Tor: connected to the Tor network");

    // Configurable nicknames distinguish instances that share Arti state.
    let onion_key_dir = crate::config::runtime_tor_hidden_service_keys_dir();
    tracing::info!(
        target: "rustchan::detect",
        nickname = %crate::config::CONFIG.tor_service_nickname,
        key_dir = %onion_key_dir.display(),
        "Tor: launching onion service"
    );
    let svc_config = OnionServiceConfigBuilder::default()
        .nickname(crate::config::CONFIG.tor_service_nickname.parse()?)
        .build()?;

    // This handle keeps the onion service registered with the Tor network.
    let (onion_service, rend_requests) = tor_client
        .launch_onion_service(svc_config)?
        .ok_or("launch_onion_service returned None — unexpected with code-only config")?;

    // Arti may expose the service before its address is synchronously readable.
    let hsid = {
        let mut found = None;
        for i in 0..10u32 {
            found = onion_service.onion_address();
            if found.is_some() {
                break;
            }
            tracing::debug!(target: "rustchan::detect", attempt = i, "Tor: waiting for HsId availability");
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        found.ok_or("onion_address() still None after 5 s — service key unavailable")?
    };
    let onion_name = hsid_to_onion_address(hsid);

    // Bound proxy tasks to cap file-descriptor use during connection floods.
    // Dropping excess requests makes Arti send RELAY_END automatically.
    let max_streams = crate::config::CONFIG.tor_max_concurrent_streams;
    let sem = Arc::new(tokio::sync::Semaphore::new(max_streams));

    let local_addr: Arc<str> = Arc::from(
        crate::config::CONFIG
            .loopback_addr_with_port(bind_port)
            .into_boxed_str(),
    );

    tracing::info!(
        target: "rustchan::detect",
        local_target = %local_addr.as_ref(),
        max_streams,
        "Tor: onion service ready and forwarding to local HTTP server"
    );
    publish_onion_address(&onion_name, &onion_address).await;

    let mut stream_requests = handle_rend_requests(rend_requests);

    loop {
        let next_request = stream_requests.next().await;
        let Some(stream_req) = next_request else {
            break;
        };
        let permit_result = Arc::clone(&sem).try_acquire_owned();
        match permit_result {
            Ok(permit) => {
                spawn_tor_stream_proxy(stream_req, permit, Arc::clone(&local_addr));
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
    // Explicitly drop both keep-alive handles only after the request stream
    // ends; dropping either earlier would tear down the onion service.
    drop(onion_service);
    drop(tor_client);

    tracing::warn!(
        target: "rustchan::detect",
        "Tor: rendezvous stream ended — onion service offline"
    );
    Ok(())
}

// Onion address publication
/// Announce service activation, write the onion address to shared state, and
/// print the TTY banner when the full-screen TUI is inactive.
///
/// Activation logs omit the address at every level. The terminal dashboard,
/// eligible TTY banner and admin panel display the full address from shared state.
async fn publish_onion_address(onion_name: &str, onion_address: &RwLock<Option<String>>) {
    tracing::debug!(target: "rustchan::detect", "Tor: hidden service active");
    tracing::info!(target: "rustchan::detect", "Tor: hidden service active");

    *onion_address.write().await = Some(onion_name.to_owned());

    // When the full-screen TUI is active the dashboard shows the onion address
    // on its next render tick.  Printing the banner box here would corrupt the
    // alternate screen, so we skip it.  In non-TUI mode (piped/systemd) or when
    // the user is on a plain TTY without the TUI, print the full box as before.
    if crate::logging::is_tty() && !crate::logging::is_tui_active() {
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
            keys = crate::config::runtime_tor_hidden_service_keys_dir().display(),
        ));
    }
}

/// Spawns one bounded proxy task for an accepted Tor stream.
fn spawn_tor_stream_proxy(
    stream_req: StreamRequest,
    permit: tokio::sync::OwnedSemaphorePermit,
    local_addr: Arc<str>,
) {
    drop(tokio::spawn(async move {
        let result = proxy_tor_stream(stream_req, &local_addr).await;
        drop(permit);
        if let Err(e) = result {
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
    }));
}

// Connection proxy
/// Proxies one accepted Tor stream to the local HTTP listener.
async fn proxy_tor_stream(
    stream_req: StreamRequest,
    local_addr: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut tor_stream = stream_req.accept(Connected::new_empty()).await?;
    // queue can fill and connect() can legitimately take several seconds.
    let mut local = tokio::time::timeout(Duration::from_secs(15), TcpStream::connect(local_addr))
        .await
        .map_err(|_error| "timed out connecting to local HTTP server")?
        .map_err(|e| format!("local TCP connect failed: {e}"))?;
    // local address. axum's ConnectInfo sees this exact socket address as the
    // peer on the accepted connection, so a different loopback address cannot
    // impersonate the Tor proxy merely by reusing its source port.
    let local_peer = local
        .local_addr()
        .map_err(|error| format!("could not determine local Tor proxy address: {error}"))?;
    let token: Arc<str> = {
        let mut bytes = [0u8; 16];
        crate::utils::crypto::fill_os_random_or_exit(
            &mut bytes,
            "generating a Tor stream isolation token",
        );
        Arc::from(format!("tor:{}", hex::encode(bytes)).as_str())
    };
    // _guard removes the map entry when this task ends (connection closed or error).
    let _previous_value = TOR_STREAM_TOKENS.insert(local_peer, Arc::clone(&token));
    let _guard = TokenGuard(local_peer);

    tokio::io::copy_bidirectional(&mut tor_stream, &mut local)
        .await
        .map(|_completed_value| ())?;
    Ok(())
}

// Onion address encoding
/// Encode an [`HsId`] (Ed25519 public key) as a v3 `.onion` address string.
///
/// [`HsId`] does not implement `std::fmt::Display` in arti-client.
/// Encoded manually using `HsId: AsRef<[u8; 32]>`.
///
/// Format: `base32( pubkey || sha3_256(".onion checksum" || pubkey || version)[..2] || version )`.
fn hsid_to_onion_address(hsid: HsId) -> String {
    use sha3::{Digest as _, Sha3_256};

    let pubkey: &[u8; 32] = hsid.as_ref();
    let version: u8 = 3;

    let mut hasher = Sha3_256::new();
    hasher.update(b".onion checksum");
    hasher.update(pubkey);
    hasher.update([version]);
    let hash: [u8; 32] = hasher.finalize().into();
    let [checksum_first, checksum_second, ..] = hash;

    let mut address_bytes = Vec::with_capacity(35);
    address_bytes.extend_from_slice(pubkey);
    address_bytes.extend_from_slice(&[checksum_first, checksum_second]);
    address_bytes.push(version);

    let encoded = data_encoding::BASE32_NOPAD
        .encode(&address_bytes)
        .to_ascii_lowercase();

    format!("{encoded}.onion")
}
