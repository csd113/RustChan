//! Runtime configuration and settings-file loading.
use anyhow::Context as _;
use serde::Deserialize;
use std::env;
use std::io::{stderr, stdout, Write as _};
use std::path::Component;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
#[cfg(test)]
use std::sync::Mutex;
use std::sync::{LazyLock, OnceLock};

/// Settings-file template rendering.
mod template;

#[cfg(test)]
/// Serializes tests that mutate the process-wide runtime directory layout.
pub static RUNTIME_LAYOUT_TEST_LOCK: Mutex<()> = Mutex::new(());

/// Explicit process-wide data-directory override.
static DATA_DIR_OVERRIDE: OnceLock<PathBuf> = OnceLock::new();

/// Absolute path to the directory the running binary lives in.
fn binary_dir() -> PathBuf {
    env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| {
            drop(writeln!(
                stderr().lock(),
                "Warning: could not determine binary directory; \
                 using current working directory for data storage. \
                 Pass --data-dir with an absolute path to override."
            ));
            PathBuf::from(".")
        })
}

/// Return the active settings-file path.
fn settings_file_path() -> PathBuf {
    // Resolve settings.toml inside the configured data directory. By default
    // that remains <exe-dir>/rustchan-data/; service installations can select
    // an explicit writable directory with --data-dir.
    // The selected directory is created during CLI startup before CONFIG is
    // first accessed, so it exists by the time settings are read.
    data_dir().join("settings.toml")
}

/// Resolve and validate an operator-provided data-directory override.
fn resolve_data_dir_override(path: &Path) -> anyhow::Result<PathBuf> {
    if !path.is_absolute() {
        anyhow::bail!("--data-dir must be an absolute path");
    }
    if path
        .components()
        .any(|component| component == Component::ParentDir)
    {
        anyhow::bail!("--data-dir must not contain '..' components");
    }

    let mut existing_ancestor = path;
    let mut missing_components = Vec::new();
    let mut resolved = loop {
        match std::fs::canonicalize(existing_ancestor) {
            Ok(resolved) => {
                if resolved.parent().is_none() && existing_ancestor.parent().is_some() {
                    anyhow::bail!(
                        "--data-dir ancestor must not resolve to a filesystem root: {}",
                        existing_ancestor.display()
                    );
                }
                if !resolved.is_dir() {
                    anyhow::bail!(
                        "--data-dir ancestor is not a directory: {}",
                        existing_ancestor.display()
                    );
                }
                break resolved;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                match std::fs::symlink_metadata(existing_ancestor) {
                    Ok(_) => {
                        return Err(error).with_context(|| {
                            format!(
                                "could not resolve --data-dir path {}",
                                existing_ancestor.display()
                            )
                        });
                    }
                    Err(metadata_error)
                        if metadata_error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(metadata_error) => {
                        return Err(metadata_error).with_context(|| {
                            format!(
                                "could not inspect --data-dir path {}",
                                existing_ancestor.display()
                            )
                        });
                    }
                }

                let component = existing_ancestor.file_name().ok_or_else(|| {
                    anyhow::anyhow!(
                        "could not find an existing parent for --data-dir {}",
                        path.display()
                    )
                })?;
                missing_components.push(component.to_os_string());
                existing_ancestor = existing_ancestor.parent().ok_or_else(|| {
                    anyhow::anyhow!(
                        "could not find an existing parent for --data-dir {}",
                        path.display()
                    )
                })?;
            }
            Err(error) => {
                return Err(error).with_context(|| {
                    format!(
                        "could not resolve --data-dir path {}",
                        existing_ancestor.display()
                    )
                });
            }
        }
    };

    for component in missing_components.iter().rev() {
        resolved.push(component);
    }
    if resolved.parent().is_none() {
        anyhow::bail!("--data-dir must not resolve to a filesystem root");
    }
    Ok(resolved)
}

/// Configure an explicit runtime data directory before configuration is loaded.
///
/// # Errors
/// Returns an error when the path is relative, is a filesystem root, contains
/// parent-directory traversal, cannot be resolved safely, or an override was
/// already configured.
pub fn configure_data_dir(path: Option<&Path>) -> anyhow::Result<()> {
    let Some(path) = path else {
        return Ok(());
    };
    let resolved = resolve_data_dir_override(path)?;
    DATA_DIR_OVERRIDE
        .set(resolved)
        .map_err(|_| anyhow::anyhow!("runtime data directory was already configured"))
}

#[must_use]
/// Return the configured root directory for mutable `RustChan` data.
pub fn data_dir() -> PathBuf {
    DATA_DIR_OVERRIDE
        .get()
        .cloned()
        .unwrap_or_else(|| binary_dir().join("rustchan-data"))
}

#[must_use]
/// Return the root for generated runtime state.
pub fn runtime_dir() -> PathBuf {
    data_dir().join("runtime")
}

#[must_use]
/// Return the directory containing application log files.
pub fn logs_dir() -> PathBuf {
    data_dir().join("logs")
}

#[must_use]
/// Return the root directory for all backups.
pub fn backups_dir() -> PathBuf {
    data_dir().join("backups")
}

#[must_use]
/// Return the directory containing full-site backups.
pub fn full_backups_dir() -> PathBuf {
    backups_dir().join("full")
}

#[must_use]
/// Return the directory containing board-scoped backups.
pub fn board_backups_dir() -> PathBuf {
    backups_dir().join("boards")
}

#[must_use]
/// Return the directory for ephemeral runtime files.
pub fn runtime_tmp_dir() -> PathBuf {
    runtime_dir().join("tmp")
}

#[must_use]
/// Return the directory for temporary board-backup downloads.
pub fn runtime_temp_board_downloads_dir() -> PathBuf {
    runtime_tmp_dir().join("board-downloads")
}

#[must_use]
/// Return the root directory for Arti runtime state.
pub fn runtime_tor_dir() -> PathBuf {
    runtime_dir().join("tor")
}

#[must_use]
/// Return the persistent Arti state directory.
pub fn runtime_tor_state_dir() -> PathBuf {
    runtime_tor_dir().join("state")
}

#[must_use]
/// Return the keystore that holds the onion-service identity.
pub fn runtime_tor_hidden_service_keys_dir() -> PathBuf {
    runtime_tor_state_dir().join("keystore")
}

#[must_use]
/// Return the onion-service keystore when Tor support is enabled.
pub fn configured_tor_hidden_service_keys_dir() -> Option<PathBuf> {
    CONFIG
        .enable_tor_support
        .then(runtime_tor_hidden_service_keys_dir)
}

#[must_use]
/// Return the Arti directory-cache path.
pub fn runtime_tor_cache_dir() -> PathBuf {
    runtime_tor_dir().join("cache")
}

#[must_use]
/// Return the runtime directory for TLS material.
pub fn runtime_tls_dir() -> PathBuf {
    runtime_dir().join("tls")
}

#[must_use]
/// Return the directory containing the global generated favicon set.
pub fn runtime_favicon_dir() -> PathBuf {
    runtime_dir().join("favicon")
}

#[must_use]
/// Return the directory containing non-board banner assets.
pub fn runtime_banner_dir() -> PathBuf {
    runtime_dir().join("banner")
}

/// Create a private directory and repair its mode on Unix.
///
/// On non-Unix platforms this preserves the existing platform behavior.
///
/// # Errors
/// Returns an error if the directory cannot be created or its mode cannot be set.
pub fn ensure_private_dir(path: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(path)?;
    restrict_private_dir_permissions(path)
}

/// Repair a secret-bearing directory mode on Unix.
///
/// # Errors
/// Returns an error if the mode cannot be changed.
pub fn restrict_private_dir_permissions(path: &Path) -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }

    #[cfg(not(unix))]
    let _ = path;

    Ok(())
}

/// Repair a secret-bearing file mode on Unix.
///
/// # Errors
/// Returns an error if the mode cannot be changed.
pub fn restrict_private_file_permissions(path: &Path) -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }

    #[cfg(not(unix))]
    let _ = path;

    Ok(())
}

/// Write a secret-bearing file and repair its mode on Unix.
///
/// # Errors
/// Returns an error if the parent directory cannot be created, the file cannot
/// be written, or its mode cannot be set.
pub fn write_private_file(path: &Path, contents: impl AsRef<[u8]>) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        ensure_private_dir(parent)?;
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;

        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .mode(0o600)
            .open(path)?;
        file.write_all(contents.as_ref())?;
        file.sync_all()?;
    }

    #[cfg(not(unix))]
    std::fs::write(path, contents)?;

    restrict_private_file_permissions(path)?;
    Ok(())
}

/// Legacy directory name and its current destination resolver.
type RuntimeDirMigration = (&'static str, fn() -> PathBuf);

/// Legacy runtime directories migrated into the grouped layout.
const RUNTIME_LAYOUT_MIGRATIONS: &[RuntimeDirMigration] = &[
    ("full-backups", full_backups_dir),
    ("board-backups", board_backups_dir),
    ("tmp-board-downloads", runtime_temp_board_downloads_dir),
    ("arti_state", runtime_tor_state_dir),
    ("arti_cache", runtime_tor_cache_dir),
    ("tls", runtime_tls_dir),
    ("favicon", runtime_favicon_dir),
    ("banner", runtime_banner_dir),
];

/// Recursively migrate one legacy runtime path when it exists.
fn migrate_dir_if_present(old_path: &Path, new_path: &Path) -> anyhow::Result<()> {
    if !old_path.exists() {
        return Ok(());
    }
    if new_path.exists() {
        if !old_path.is_dir() || !new_path.is_dir() {
            anyhow::bail!(
                "cannot migrate {} to {} because one path is not a directory",
                old_path.display(),
                new_path.display()
            );
        }
        for entry in std::fs::read_dir(old_path)? {
            let entry = entry?;
            let source = entry.path();
            let destination = new_path.join(entry.file_name());
            migrate_dir_if_present(&source, &destination)?;
        }
        std::fs::remove_dir(old_path)?;
        return Ok(());
    }
    if let Some(parent) = new_path.parent() {
        ensure_private_dir(parent)?;
    }
    std::fs::rename(old_path, new_path)?;
    Ok(())
}

/// Move legacy top-level runtime folders into the newer grouped layout.
///
/// # Errors
/// Returns an error if a filesystem move fails, if a directory cannot be
/// created or removed, or if an old and new path conflict by type.
pub fn migrate_runtime_layout_if_needed() -> anyhow::Result<()> {
    let data_dir = data_dir();
    ensure_private_dir(&data_dir)?;
    for dir in [
        runtime_dir(),
        runtime_tmp_dir(),
        runtime_temp_board_downloads_dir(),
        runtime_favicon_dir(),
        runtime_banner_dir(),
        backups_dir(),
        full_backups_dir(),
        board_backups_dir(),
    ] {
        ensure_private_dir(&dir)?;
    }

    for &(legacy_name, destination) in RUNTIME_LAYOUT_MIGRATIONS {
        migrate_dir_if_present(&data_dir.join(legacy_name), &destination())?;
    }

    Ok(())
}

// ─── Settings file structure ──────────────────────────────────────────────────
#[derive(Deserialize, Default)]
/// Optional values deserialized from `settings.toml`.
struct SettingsFile {
    /// Public forum name.
    forum_name: Option<String>,
    /// Home page subtitle shown below the site name.
    site_subtitle: Option<String>,
    /// Legacy initial state for browser-local new-activity badges.
    new_activity_notifications_enabled: Option<bool>,
    /// Initial state for homepage board-card new-thread badges.
    homepage_new_thread_badges_enabled: Option<bool>,
    /// Initial state for homepage board-card new-reply badges.
    homepage_new_reply_badges_enabled: Option<bool>,
    /// Initial state for board/catalog thread-card new-reply badges.
    thread_new_reply_badges_enabled: Option<bool>,
    /// Default theme served to first-time visitors before they pick one.
    /// Valid values include built-ins and admin-created custom theme slugs.
    default_theme: Option<String>,
    /// Built-in theme whitelist applied when seeding the themes table.
    enabled_builtin_themes: Option<Vec<String>>,
    /// Primary HTTP listener port.
    port: Option<u16>,
    /// Maximum image upload size in mebibytes.
    max_image_size_mb: Option<u32>,
    /// Maximum video upload size in mebibytes.
    max_video_size_mb: Option<u32>,
    /// Maximum audio upload size in mebibytes.
    max_audio_size_mb: Option<u32>,
    /// Secret used for authentication cookies and privacy-preserving hashes.
    cookie_secret: Option<String>,
    /// Whether trusted reverse-proxy forwarding headers are enabled.
    behind_proxy: Option<bool>,
    /// Whether authentication cookies carry the `Secure` attribute.
    https_cookies: Option<bool>,
    /// Expose detailed `/readyz` internals such as schema, backup, media queue,
    /// maintenance, and Tor readiness to unauthenticated clients. Default: false.
    public_readiness_details: Option<bool>,
    /// Expose unauthenticated Prometheus metrics at `/metrics`. Default: false.
    public_metrics_enabled: Option<bool>,
    /// Whether the built-in Arti onion service is enabled.
    enable_tor_support: Option<bool>,
    /// When true, the HTTP server binds exclusively to 127.0.0.1 so it is
    /// reachable only through the Tor hidden service. Overrides the host
    /// portion of `bind_addr` (the configured port is preserved).
    /// Default: false (clearnet and Tor both active when `enable_tor_support=true`).
    tor_only: Option<bool>,
    /// Seconds to wait for Tor bootstrap before timing out and retrying.
    /// Increase to 300+ on heavily censored networks or when using bridges.
    /// Default: 120.
    tor_bootstrap_timeout_secs: Option<u64>,
    /// Maximum simultaneous inbound Tor streams (proxy tasks).
    /// Each stream holds one file descriptor. Reduce if the process runs low
    /// on FDs; excess connections are dropped with a `RELAY_END` cell.
    /// Default: 512.
    tor_max_concurrent_streams: Option<usize>,
    /// Nickname for the Arti onion service key.
    /// Must be unique per `runtime/tor/state/` directory. Change this when running
    /// multiple instances that share the same storage to avoid key collisions.
    /// Default: "rustchan".
    tor_service_nickname: Option<String>,
    /// Whether startup fails when `FFmpeg` or `FFprobe` is unavailable.
    require_ffmpeg: Option<bool>,
    /// Configured `FFmpeg` executable path.
    ffmpeg_path: Option<String>,
    /// Configured `FFprobe` executable path.
    ffprobe_path: Option<String>,
    /// Whether boards may enable arbitrary-file uploads.
    enable_any_file_uploads_feature: Option<bool>,
    /// How often to run PRAGMA `wal_checkpoint(TRUNCATE)`, in seconds.
    /// Set to 0 to disable. Default: 3600 (hourly).
    wal_checkpoint_interval_secs: Option<u64>,
    /// How often to run VACUUM to reclaim disk space, in hours.
    /// Set to 0 to disable. Default: 24 (daily).
    auto_vacuum_interval_hours: Option<u64>,
    /// How often to create a saved full-site backup automatically, in hours.
    /// Set to 0 to disable. Default: 24 (daily).
    auto_full_backup_interval_hours: Option<u64>,
    /// How many saved full-site backups to keep on disk after a new saved
    /// backup completes. Minimum 1. Default: 1.
    auto_full_backup_copies_to_keep: Option<u64>,
    /// Whether automatic full-site backups should include Tor hidden service
    /// identity keys. Default: false.
    auto_full_backup_include_tor_hidden_service_keys: Option<bool>,
    /// Output format for automatic full-site backups: `directory` or `split_zip`.
    auto_full_backup_storage_mode: Option<String>,
    /// Split ZIP part size in GiB for automatic full-site backups.
    auto_full_backup_split_zip_part_size_gib: Option<u64>,
    /// How often to purge vote records for expired polls, in hours.
    /// Set to 0 to disable. Default: 72 (every 3 days).
    poll_cleanup_interval_hours: Option<u64>,
    /// Database file size (MB) above which a warning banner is shown in the
    /// admin panel. Set to 0 to disable. Default: 2048 (2 GiB).
    db_warn_threshold_mb: Option<u64>,
    /// Maximum number of pending jobs in the background job queue.
    /// When this limit is reached, new jobs are dropped (with a warning) rather
    /// than accepted. Default: 1000.
    job_queue_capacity: Option<u64>,
    /// Maximum seconds to allow a single `FFmpeg` transcode or waveform job to
    /// run before it is killed. Default: 600.
    ffmpeg_timeout_secs: Option<u64>,
    /// Initial state for automatic post-media pruning.
    /// This seeds the DB on first run; after that Admin -> Media Settings owns
    /// the live value.
    media_auto_prune_enabled: Option<bool>,
    /// Initial maximum active post-media size in bytes. 0 disables pruning.
    /// This seeds the DB on first run; after that Admin -> Media Settings owns
    /// the live value.
    media_max_active_content_size_bytes: Option<u64>,
    /// Explicit proxy CIDR allowlist for trusted forwarding headers.
    /// Examples include `127.0.0.1/32`, `::1/128`, and `10.0.0.0/8`.
    trusted_proxy_cidrs: Option<Vec<String>>,
    /// Public hostnames accepted by the HTTP→HTTPS redirect listener.
    /// Needed when `RustChan` binds to a wildcard address but serves a manual-cert
    /// public domain.
    public_hosts: Option<Vec<String>>,
    /// When true, overflow threads are always archived rather than hard-deleted,
    /// even on boards with `allow_archive` = false. Default: true.
    archive_before_prune: Option<bool>,
    /// Maximum total size (MiB) of all thumbnail/waveform cache files across all
    /// boards. A background task evicts the oldest files when exceeded.
    /// Set to 0 to disable. Default: 200.
    waveform_cache_max_mb: Option<u64>,
    /// Number of threads in Tokio's blocking pool (`spawn_blocking`).
    /// Defaults to logical CPUs × 4. Increase if DB/render latency is a bottleneck
    /// under load.
    blocking_threads: Option<usize>,
    /// `SQLite` connection pool size. Default: 8.
    /// Increase on high-traffic deployments; each connection uses ~32 MiB page cache.
    db_pool_size: Option<u32>,
    // ── ChanNet / RustWave gateway ────────────────────────────────────────────
    /// Base URL of the connected `RustWave` instance.
    /// Must begin with http:// or https://. Default: <http://localhost:7071>.
    rustwave_url: Option<String>,
    /// Address to bind the second `ChanNet` TCP listener.
    /// Default: 127.0.0.1:7070 (loopback-only; not exposed to the internet).
    chan_net_bind: Option<String>,
    /// Pre-shared API key required for /chan/refresh and /chan/poll endpoints.
    /// Must be at least 32 characters. Leave empty to disable the endpoints.
    /// Set via `CHAN_NET_API_KEY` environment variable or `settings.toml`.
    chan_net_api_key: Option<String>,
    /// TLS/HTTPS configuration. Omitting this section keeps TLS disabled.
    tls: Option<TlsConfig>,
}

/// Load the settings file or return defaults when it does not exist.
fn load_settings_file() -> SettingsFile {
    let path = settings_file_path();
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return SettingsFile::default();
    };
    parse_settings_file_str(&raw).unwrap_or_else(|_| settings_file_parse_error(&path))
}

/// Print a secret-safe parse failure and terminate with `EX_CONFIG`.
#[expect(
    clippy::exit,
    reason = "invalid configuration must terminate with the standard EX_CONFIG status"
)]
fn settings_file_parse_error(path: &Path) -> ! {
    drop(writeln!(
        stderr().lock(),
        "CONFIG ERROR: could not parse {}. Refusing to start with fallback defaults; fix or remove settings.toml. Details are omitted to avoid leaking secrets.",
        path.display()
    ));
    std::process::exit(78);
}

/// Deserialize a settings file from TOML text.
fn parse_settings_file_str(raw: &str) -> Result<SettingsFile, toml::de::Error> {
    toml::from_str(raw)
}

/// Return whether a listener address is a valid loopback endpoint.
fn bind_addr_is_loopback(bind_addr: &str) -> bool {
    if let Ok(addr) = bind_addr.parse::<std::net::SocketAddr>() {
        return addr.ip().is_loopback();
    }

    bind_addr
        .strip_prefix("localhost:")
        .is_some_and(|port| port.parse::<u16>().is_ok())
}

/// Create settings.toml with defaults if it does not exist yet.
/// Call this once at startup (before CONFIG is accessed for the first time).
///
/// A cryptographically random `cookie_secret` is generated on first run and
/// written to settings.toml. Subsequent runs load it from the file.
/// The server never operates with a known/default secret.
pub fn generate_settings_file_if_missing() {
    let path = settings_file_path();
    if path.exists() {
        if let Err(error) = restrict_private_file_permissions(&path) {
            drop(writeln!(
                stderr().lock(),
                "Warning: could not repair settings.toml permissions: {error}"
            ));
        }
        return;
    }
    // Generate a random 64-hex-char secret (32 bytes of entropy).
    let mut secret_bytes = [0u8; 32];
    crate::utils::crypto::fill_os_random_or_exit(
        &mut secret_bytes,
        "generating the initial cookie secret",
    );
    let secret = hex::encode(secret_bytes);
    let content = template::settings_template(&secret);
    match write_private_file(&path, content) {
        Ok(()) => {
            drop(writeln!(
                stdout().lock(),
                "Created settings.toml ({})",
                path.display()
            ));
        }
        Err(e) => {
            drop(writeln!(
                stderr().lock(),
                "Warning: could not write settings.toml: {e}"
            ));
        }
    }
}

// ─── TLS configuration ───────────────────────────────────────────────────────
#[derive(Debug, Clone, serde::Deserialize)]
/// HTTPS listener and certificate-source configuration.
pub struct TlsConfig {
    #[serde(default)]
    /// Whether the HTTPS listener is enabled.
    pub enabled: bool,
    #[serde(default)]
    /// Whether enabling HTTPS also disables public plaintext application access.
    pub require_https: bool,
    #[serde(default = "default_https_port")]
    /// HTTPS listener port.
    pub port: u16,
    #[serde(default)]
    /// Whether a companion HTTP listener redirects to HTTPS.
    pub redirect_http: bool,
    #[serde(default = "default_http_port")]
    /// HTTP redirect-listener port.
    pub http_port: u16,
    #[serde(default)]
    /// ACME certificate settings.
    pub acme: AcmeConfig,
    /// Optional explicit certificate and private-key paths.
    pub manual_cert: Option<ManualCertConfig>,
}

impl Default for TlsConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            require_https: false,
            port: default_https_port(),
            redirect_http: false,
            http_port: default_http_port(),
            acme: AcmeConfig::default(),
            manual_cert: None,
        }
    }
}

#[derive(Debug, Clone, serde::Deserialize, Default)]
/// Automatic Certificate Management Environment settings.
pub struct AcmeConfig {
    #[serde(default)]
    /// Whether ACME certificate acquisition is enabled.
    pub enabled: bool,

    // These fields are only read when the `tls-acme` Cargo feature is enabled
    // (the Let's Encrypt implementation lives in a separate module).
    // They are intentionally kept here so the `[tls.acme]` section in
    // settings.toml deserializes cleanly even when the feature is off.
    // The dead-code allow exists because the config shape is stable even when
    // this build cannot act on the fields.
    #[serde(default)]
    // Feature-gated, but still part of the stable settings shape.
    /// DNS names requested in the ACME certificate.
    pub domains: Vec<String>,
    #[serde(default)]
    // Feature-gated, but still part of the stable settings shape.
    /// Optional ACME account contact email.
    pub email: Option<String>,
    #[serde(default = "default_true")]
    // Feature-gated, but still part of the stable settings shape.
    /// Whether to use the ACME staging directory.
    pub staging: bool,
    #[serde(default = "default_acme_dir")]
    // Feature-gated, but still part of the stable settings shape.
    /// Directory used to cache ACME account and certificate state.
    pub cache_dir: String,
}

#[derive(Debug, Clone, serde::Deserialize)]
/// Paths to an operator-managed TLS certificate and private key.
pub struct ManualCertConfig {
    /// PEM certificate-chain path.
    pub cert_path: String,
    /// PEM private-key path.
    pub key_path: String,
}

/// Return the default HTTPS listener port.
const fn default_https_port() -> u16 {
    8443
}

/// Return the default HTTP listener port.
const fn default_http_port() -> u16 {
    8080
}

/// Return `true` for Serde defaults.
const fn default_true() -> bool {
    true
}

/// Return the default ACME state directory.
fn default_acme_dir() -> String {
    "runtime/tls/acme".into()
}

// ─── Runtime config ───────────────────────────────────────────────────────────
/// Lazily loaded process-wide runtime configuration.
pub static CONFIG: LazyLock<Config> = LazyLock::new(Config::from_env);
/// Runtime-adjustable `FFmpeg` timeout.
static LIVE_FFMPEG_TIMEOUT_SECS: LazyLock<AtomicU64> =
    LazyLock::new(|| AtomicU64::new(CONFIG.ffmpeg_timeout_secs));

/// Default upper bound for one `FFmpeg` subprocess.
pub const DEFAULT_FFMPEG_TIMEOUT_SECS: u64 = 600;
/// Minimum configurable `FFmpeg` subprocess timeout.
pub const MIN_FFMPEG_TIMEOUT_SECS: u64 = 30;
/// Maximum configurable `FFmpeg` subprocess timeout.
pub const MAX_FFMPEG_TIMEOUT_SECS: u64 = 86_400;

#[must_use]
/// Return the current live `FFmpeg` timeout in seconds.
pub fn ffmpeg_timeout_secs() -> u64 {
    LIVE_FFMPEG_TIMEOUT_SECS.load(Ordering::Relaxed)
}

/// # Errors
/// Returns an error when `timeout_secs` falls outside the supported range.
pub fn set_live_ffmpeg_timeout_secs(timeout_secs: u64) -> anyhow::Result<()> {
    let timeout_secs = validate_ffmpeg_timeout_secs(timeout_secs)?;
    LIVE_FFMPEG_TIMEOUT_SECS.store(timeout_secs, Ordering::Relaxed);
    Ok(())
}

/// # Errors
/// Returns an error when `timeout_secs` falls outside the supported range.
pub fn validate_ffmpeg_timeout_secs(timeout_secs: u64) -> anyhow::Result<u64> {
    if !(MIN_FFMPEG_TIMEOUT_SECS..=MAX_FFMPEG_TIMEOUT_SECS).contains(&timeout_secs) {
        anyhow::bail!(
            "CONFIG ERROR: ffmpeg_timeout_secs must be between {MIN_FFMPEG_TIMEOUT_SECS} and {MAX_FFMPEG_TIMEOUT_SECS} seconds."
        );
    }
    Ok(timeout_secs)
}

#[must_use]
/// Format a timeout as a compact human-readable duration.
pub fn describe_timeout_secs(timeout_secs: u64) -> String {
    let minutes = timeout_secs / 60;
    let seconds = timeout_secs % 60;
    match (minutes, seconds) {
        (0, secs) => format!("{secs} seconds"),
        (1, 0) => "1 minute".to_owned(),
        (mins, 0) => format!("{mins} minutes"),
        (1, secs) => format!("1 minute {secs} seconds"),
        (mins, secs) => format!("{mins} minutes {secs} seconds"),
    }
}

// This type mirrors serialized or render state, so the boolean count is an intentional tradeoff.
#[expect(
    clippy::struct_excessive_bools,
    reason = "runtime configuration intentionally models independent operator-controlled feature switches"
)]
/// Fully resolved runtime configuration.
pub struct Config {
    // ── Loaded from settings.toml (env vars still override) ──────────────────
    /// Public forum name.
    pub forum_name: String,
    /// Initial subtitle shown on the home page; seeds the DB on first run and
    /// then the Admin -> Site Settings DB value becomes the live source of truth.
    pub initial_site_subtitle: String,
    /// Initial state for homepage board-card new-thread badges; seeds the DB
    /// on first run and then the Admin -> Site Settings DB value becomes the
    /// live source of truth.
    pub initial_homepage_new_thread_badges_enabled: bool,
    /// Initial state for homepage board-card new-reply badges; seeds the DB
    /// on first run and then the Admin -> Site Settings DB value becomes the
    /// live source of truth.
    pub initial_homepage_new_reply_badges_enabled: bool,
    /// Initial state for board/catalog thread-card new-reply badges; seeds the
    /// DB on first run and then the Admin -> Site Settings DB value becomes the
    /// live source of truth.
    pub initial_thread_new_reply_badges_enabled: bool,
    /// Initial default theme slug; seeds the DB on first run and later the
    /// Admin -> Site Settings DB value becomes the live source of truth.
    /// Valid: built-in or custom theme slug present in the themes table.
    pub initial_default_theme: String,
    /// Built-in themes enabled by default when the site seeds its theme catalog.
    /// After seeding, the theme catalog in the DB owns the enabled/disabled set.
    pub initial_enabled_builtin_themes: Vec<String>,
    /// Primary HTTP listener port.
    pub port: u16,
    /// Maximum accepted image upload size in bytes.
    pub max_image_size: usize, // bytes
    /// Maximum accepted video upload size in bytes.
    pub max_video_size: usize, // bytes
    /// Maximum accepted audio upload size in bytes.
    pub max_audio_size: usize, // bytes,
    // ── External tool settings ────────────────────────────────────────────────
    /// When true, Tor is probed at startup and hints are printed.
    pub enable_tor_support: bool,
    /// When true, the server binds to loopback only and is reachable exclusively
    /// via the Tor hidden service. Requires `enable_tor_support = true`.
    pub tor_only: bool,
    /// Seconds before a bootstrap attempt is considered failed and retried.
    pub tor_bootstrap_timeout_secs: u64,
    /// Maximum simultaneous inbound Tor proxy tasks.
    pub tor_max_concurrent_streams: usize,
    /// Nickname for the Arti onion service. Unique per `runtime/tor/state/` directory.
    pub tor_service_nickname: String,
    /// When true, the server exits if ffmpeg is missing.
    pub require_ffmpeg: bool,
    /// Explicit ffmpeg binary path, or plain "ffmpeg" for PATH lookup.
    pub ffmpeg_path: String,
    /// Explicit ffprobe binary path, or plain "ffprobe" for PATH lookup.
    pub ffprobe_path: String,
    /// Global feature gate for arbitrary uploads. Boards can only enable the
    /// per-board toggle when this is true.
    pub enable_any_file_uploads_feature: bool,
    // ── Internal / env-only settings ─────────────────────────────────────────
    /// Interface or host used by the primary listener.
    pub bind_addr: String,
    /// `SQLite` database file path.
    pub database_path: String,
    /// Root directory for board uploads.
    pub upload_dir: String,
    /// Maximum generated thumbnail dimension in pixels.
    pub thumb_size: u32,
    /// Maximum GET requests per IP per `rate_limit_window`.
    pub rate_limit_gets: u32,
    /// Rate-limit window duration in seconds.
    pub rate_limit_window: u64,
    /// Secret used for cookies, CSRF signatures, and privacy-preserving hashes.
    pub cookie_secret: String,
    /// Administrator session lifetime in seconds.
    pub session_duration: i64,
    /// Whether trusted reverse-proxy forwarding headers are enabled.
    pub behind_proxy: bool,
    /// Trusted proxy CIDR allowlist for forwarding headers.
    pub trusted_proxy_cidrs: Vec<String>,
    /// Whether authentication cookies carry the `Secure` attribute.
    pub https_cookies: bool,
    /// When true, `/readyz` includes detailed operational internals for public clients.
    pub public_readiness_details: bool,
    /// When true, `/metrics` exposes Prometheus metrics to unauthenticated clients.
    pub public_metrics_enabled: bool,
    /// Public hostnames accepted by the HTTP→HTTPS redirect listener.
    pub public_hosts: Vec<String>,
    /// Interval in seconds between WAL checkpoint runs. 0 = disabled.
    pub wal_checkpoint_interval: u64,
    /// Interval in hours between automatic VACUUM runs. 0 = disabled.
    pub auto_vacuum_interval_hours: u64,
    /// Interval in hours between automatic saved full backups. 0 = disabled.
    pub auto_full_backup_interval_hours: u64,
    /// Maximum number of saved full backups kept on disk after each new saved
    /// full backup completes. Minimum 1.
    pub auto_full_backup_copies_to_keep: u64,
    /// Whether automatic saved full backups include Tor hidden service identity keys.
    pub auto_full_backup_include_tor_hidden_service_keys: bool,
    /// Output format for automatic saved full backups.
    pub auto_full_backup_storage_mode: String,
    /// Split ZIP part size in bytes for automatic saved full backups.
    pub auto_full_backup_split_zip_part_size_bytes: u64,
    /// Interval in hours between expired poll vote cleanup runs. 0 = disabled.
    pub poll_cleanup_interval_hours: u64,
    /// DB file size threshold in bytes above which admin panel shows a warning.
    /// 0 = disabled.
    pub db_warn_threshold_bytes: u64,
    /// Maximum number of pending jobs before new ones are dropped.
    pub job_queue_capacity: u64,
    /// Maximum seconds a single `FFmpeg` job may run before being killed.
    pub ffmpeg_timeout_secs: u64,
    /// Initial state for automatic active post-media pruning.
    pub initial_media_auto_prune_enabled: bool,
    /// Initial active post-media size cap in bytes. 0 = unset/disabled.
    pub initial_media_max_active_content_size_bytes: u64,
    /// When true, threads are always archived (never hard-deleted) on prune,
    /// overriding individual board settings.
    pub archive_before_prune: bool,
    /// Total thumbnail/waveform cache size limit in bytes. 0 = disabled.
    pub waveform_cache_max_bytes: u64,
    /// Explicitly permit managed-media reconciliation to schedule safe repairs.
    pub media_reconcile_repair_enabled: bool,
    /// Hours between bounded managed-media audit pages. 0 disables periodic passes.
    pub media_reconcile_interval_hours: u64,
    /// Maximum managed filesystem entries inspected per reconciliation pass.
    pub media_reconcile_files_per_pass: usize,
    /// Maximum database rows loaded into one authoritative reference snapshot.
    pub media_reconcile_database_rows_per_pass: usize,
    /// Maximum file bytes hashed during one reconciliation pass.
    pub media_reconcile_hash_bytes_per_pass: u64,
    /// Maximum safe repair attempts during one reconciliation pass.
    pub media_reconcile_repairs_per_pass: usize,
    /// Number of threads in Tokio's blocking pool. Default: logical CPUs × 4.
    pub blocking_threads: usize,
    /// `SQLite` `r2d2` connection pool size (default 8).
    pub db_pool_size: u32,
    // ── ChanNet / RustWave gateway ───────────────────────────────────────────
    /// Base URL of the connected `RustWave` instance (must begin with http:// or https://).
    /// Validated at startup by `Config::validate()`.
    pub rustwave_url: String,
    /// Address to bind the second `ChanNet` TCP listener (default 127.0.0.1:7070).
    /// Only used when the server is started with `--chan-net`.
    pub chan_net_bind: String,
    /// Maximum request body size for `/chan/import` (ZIP snapshots). Default: 10 MiB.
    /// Maximum request body size for non-command `ChanNet` endpoints.
    pub chan_net_max_body: usize,
    /// Maximum request body size for `/chan/command` (raw JSON). Default: 512 KiB.
    pub chan_net_command_max_body: usize,
    /// Pre-shared key required on X-ChanNet-Key header for /chan/refresh and
    /// /chan/poll. An empty string means those endpoints are disabled entirely.
    pub chan_net_api_key: String,
    // ── TLS / HTTPS ───────────────────────────────────────────────────────────
    /// TLS configuration. Defaults to disabled so existing installs are unaffected.
    pub tls: TlsConfig,
}

impl std::fmt::Debug for Config {
    #[expect(
        clippy::too_many_lines,
        reason = "listing every configuration field keeps secret redaction and Debug completeness auditable"
    )]
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Config")
            .field("forum_name", &self.forum_name)
            .field("initial_site_subtitle", &self.initial_site_subtitle)
            .field(
                "initial_homepage_new_thread_badges_enabled",
                &self.initial_homepage_new_thread_badges_enabled,
            )
            .field(
                "initial_homepage_new_reply_badges_enabled",
                &self.initial_homepage_new_reply_badges_enabled,
            )
            .field(
                "initial_thread_new_reply_badges_enabled",
                &self.initial_thread_new_reply_badges_enabled,
            )
            .field("initial_default_theme", &self.initial_default_theme)
            .field(
                "initial_enabled_builtin_themes",
                &self.initial_enabled_builtin_themes,
            )
            .field("port", &self.port)
            .field("max_image_size", &self.max_image_size)
            .field("max_video_size", &self.max_video_size)
            .field("max_audio_size", &self.max_audio_size)
            .field("enable_tor_support", &self.enable_tor_support)
            .field("tor_only", &self.tor_only)
            .field(
                "tor_bootstrap_timeout_secs",
                &self.tor_bootstrap_timeout_secs,
            )
            .field(
                "tor_max_concurrent_streams",
                &self.tor_max_concurrent_streams,
            )
            .field("tor_service_nickname", &self.tor_service_nickname)
            .field("require_ffmpeg", &self.require_ffmpeg)
            .field("ffmpeg_path", &self.ffmpeg_path)
            .field("ffprobe_path", &self.ffprobe_path)
            .field(
                "enable_any_file_uploads_feature",
                &self.enable_any_file_uploads_feature,
            )
            .field("bind_addr", &self.bind_addr)
            .field("database_path", &self.database_path)
            .field("upload_dir", &self.upload_dir)
            .field("thumb_size", &self.thumb_size)
            .field("rate_limit_gets", &self.rate_limit_gets)
            .field("rate_limit_window", &self.rate_limit_window)
            .field("cookie_secret", &"[REDACTED]")
            .field("session_duration", &self.session_duration)
            .field("behind_proxy", &self.behind_proxy)
            .field("trusted_proxy_cidrs", &self.trusted_proxy_cidrs)
            .field("https_cookies", &self.https_cookies)
            .field("public_readiness_details", &self.public_readiness_details)
            .field("public_metrics_enabled", &self.public_metrics_enabled)
            .field("public_hosts", &self.public_hosts)
            .field("wal_checkpoint_interval", &self.wal_checkpoint_interval)
            .field(
                "auto_vacuum_interval_hours",
                &self.auto_vacuum_interval_hours,
            )
            .field(
                "auto_full_backup_interval_hours",
                &self.auto_full_backup_interval_hours,
            )
            .field(
                "auto_full_backup_copies_to_keep",
                &self.auto_full_backup_copies_to_keep,
            )
            .field(
                "auto_full_backup_include_tor_hidden_service_keys",
                &self.auto_full_backup_include_tor_hidden_service_keys,
            )
            .field(
                "auto_full_backup_storage_mode",
                &self.auto_full_backup_storage_mode,
            )
            .field(
                "auto_full_backup_split_zip_part_size_bytes",
                &self.auto_full_backup_split_zip_part_size_bytes,
            )
            .field(
                "poll_cleanup_interval_hours",
                &self.poll_cleanup_interval_hours,
            )
            .field("db_warn_threshold_bytes", &self.db_warn_threshold_bytes)
            .field("job_queue_capacity", &self.job_queue_capacity)
            .field("ffmpeg_timeout_secs", &self.ffmpeg_timeout_secs)
            .field(
                "initial_media_auto_prune_enabled",
                &self.initial_media_auto_prune_enabled,
            )
            .field(
                "initial_media_max_active_content_size_bytes",
                &self.initial_media_max_active_content_size_bytes,
            )
            .field("archive_before_prune", &self.archive_before_prune)
            .field("waveform_cache_max_bytes", &self.waveform_cache_max_bytes)
            .field(
                "media_reconcile_repair_enabled",
                &self.media_reconcile_repair_enabled,
            )
            .field(
                "media_reconcile_interval_hours",
                &self.media_reconcile_interval_hours,
            )
            .field(
                "media_reconcile_files_per_pass",
                &self.media_reconcile_files_per_pass,
            )
            .field(
                "media_reconcile_database_rows_per_pass",
                &self.media_reconcile_database_rows_per_pass,
            )
            .field(
                "media_reconcile_hash_bytes_per_pass",
                &self.media_reconcile_hash_bytes_per_pass,
            )
            .field(
                "media_reconcile_repairs_per_pass",
                &self.media_reconcile_repairs_per_pass,
            )
            .field("blocking_threads", &self.blocking_threads)
            .field("db_pool_size", &self.db_pool_size)
            .field("rustwave_url", &"[REDACTED]")
            .field("chan_net_bind", &self.chan_net_bind)
            .field("chan_net_max_body", &self.chan_net_max_body)
            .field("chan_net_command_max_body", &self.chan_net_command_max_body)
            .field("chan_net_api_key", &"[REDACTED]")
            .field("tls", &self.tls)
            .finish()
    }
}

impl Config {
    /// Load settings and environment overrides into one validated runtime shape.
    #[must_use]
    // This function/module is intentionally long; splitting it further would make the routing or template flow harder to follow.
    #[expect(
        clippy::too_many_lines,
        reason = "configuration loading keeps precedence and defaults together for auditability"
    )]
    pub fn from_env() -> Self {
        let s = load_settings_file();
        let tls = s.tls.clone().unwrap_or_default();
        let data_dir = data_dir();
        let default_db = data_dir.join("chan.db").to_string_lossy().into_owned();
        let default_uploads = data_dir.join("boards").to_string_lossy().into_owned();
        let forum_name = env_str(
            "CHAN_FORUM_NAME",
            s.forum_name.as_deref().unwrap_or("RustChan"),
        );
        let initial_site_subtitle = env_str(
            "CHAN_SITE_SUBTITLE",
            s.site_subtitle
                .as_deref()
                .unwrap_or("select board to proceed"),
        );
        let legacy_new_activity_notifications_enabled = env::var("CHAN_NEW_ACTIVITY_NOTIFICATIONS")
            .ok()
            .map(|value| value == "1" || value.eq_ignore_ascii_case("true"))
            .or(s.new_activity_notifications_enabled);
        let initial_homepage_new_thread_badges_enabled =
            env::var("CHAN_HOMEPAGE_NEW_THREAD_BADGES")
                .ok()
                .map(|value| value == "1" || value.eq_ignore_ascii_case("true"))
                .or(s.homepage_new_thread_badges_enabled)
                .or(legacy_new_activity_notifications_enabled)
                .unwrap_or(true);
        let initial_homepage_new_reply_badges_enabled = env::var("CHAN_HOMEPAGE_NEW_REPLY_BADGES")
            .ok()
            .map(|value| value == "1" || value.eq_ignore_ascii_case("true"))
            .or(s.homepage_new_reply_badges_enabled)
            .or(legacy_new_activity_notifications_enabled)
            .unwrap_or(true);
        let initial_thread_new_reply_badges_enabled = env::var("CHAN_THREAD_NEW_REPLY_BADGES")
            .ok()
            .map(|value| value == "1" || value.eq_ignore_ascii_case("true"))
            .or(s.thread_new_reply_badges_enabled)
            .or(legacy_new_activity_notifications_enabled)
            .unwrap_or(true);
        let initial_default_theme = env_str(
            "CHAN_DEFAULT_THEME",
            s.default_theme
                .as_deref()
                .unwrap_or(crate::theme::HARD_DEFAULT_THEME),
        );
        let initial_enabled_builtin_themes = s.enabled_builtin_themes.unwrap_or_else(|| {
            crate::theme::builtin_theme_slugs()
                .into_iter()
                .map(str::to_owned)
                .collect()
        });
        let port: u16 = env_parse("CHAN_PORT", s.port.unwrap_or(8080));
        let max_image_mb: u32 = env_parse("CHAN_MAX_IMAGE_MB", s.max_image_size_mb.unwrap_or(8));
        let max_video_mb: u32 = env_parse("CHAN_MAX_VIDEO_MB", s.max_video_size_mb.unwrap_or(50));
        let max_audio_mb: u32 = env_parse("CHAN_MAX_AUDIO_MB", s.max_audio_size_mb.unwrap_or(150));
        let bind_addr = env_str(
            "CHAN_BIND",
            &format!("{}:{}", env_str("CHAN_HOST", "0.0.0.0"), port),
        );
        let tor_only = env_bool("CHAN_TOR_ONLY", s.tor_only.unwrap_or(false));
        let enable_tor_support = env_bool("CHAN_TOR_SUPPORT", s.enable_tor_support.unwrap_or(true));
        // When tor_only=true, force the bind host to loopback while preserving
        // the configured address family and port. Validation later rejects a
        // tor-only request if Tor support itself is disabled.
        let bind_addr = if tor_only {
            let port_num = port_from_bind_addr(&bind_addr).unwrap_or(8080);
            let tor_bind_addr = loopback_addr_for_family(&bind_addr, port_num);
            tracing::info!(
                target: "config",
                bind_addr = %tor_bind_addr,
                "tor_only=true: overriding bind address to loopback"
            );
            tor_bind_addr
        } else {
            bind_addr
        };
        let behind_proxy = env_bool("CHAN_BEHIND_PROXY", s.behind_proxy.unwrap_or(false));
        let https_cookies_default = behind_proxy || tls.enabled;
        let trusted_proxy_cidrs = env_list(
            "CHAN_TRUSTED_PROXY_CIDRS",
            s.trusted_proxy_cidrs,
            &["127.0.0.1/32", "::1/128"],
        );
        let public_hosts = env_list("CHAN_PUBLIC_HOSTS", s.public_hosts, &[]);
        // Resolve cookie_secret from env > settings.toml.
        // generate_settings_file_if_missing() ensures settings.toml always has
        // a generated secret, so this fallback should only fire in abnormal cases.
        let cookie_secret = if let Ok(v) = env::var("CHAN_COOKIE_SECRET") {
            v
        } else if let Some(v) = s.cookie_secret {
            v
        } else {
            drop(writeln!(
                stderr().lock(),
                "SECURITY WARNING: No cookie_secret found in environment or settings.toml. \
                 IP hashing is using an empty secret. Run the server once to auto-generate, \
                 or set CHAN_COOKIE_SECRET."
            ));
            // Random in-memory secret so each restart invalidates hashes
            // (better than a known empty string, worse than a persisted one).
            let mut b = [0u8; 32];
            crate::utils::crypto::fill_os_random_or_exit(
                &mut b,
                "generating a fallback in-memory cookie secret",
            );
            hex::encode(b)
        };
        // ── ChanNet fields ───────────────────────────────────────────────────
        // Use as_deref() to borrow rather than move the Option<String> fields.
        let rustwave_url = env::var("CHAN_RUSTWAVE_URL").unwrap_or_else(|_| {
            s.rustwave_url
                .as_deref()
                .unwrap_or("http://localhost:7071")
                .to_owned()
        });
        let chan_net_bind = env::var("CHAN_NET_BIND").unwrap_or_else(|_| {
            s.chan_net_bind
                .as_deref()
                .unwrap_or("127.0.0.1:7070")
                .to_owned()
        });
        let chan_net_max_body: usize = env::var("CHAN_NET_MAX_BODY")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(10 * 1024 * 1024); // 10 MiB default
        let chan_net_command_max_body: usize = env::var("CHAN_NET_COMMAND_MAX_BODY")
            .ok()
            .and_then(|v| v.parse().ok())
            // The legal 32,768-character reply envelope can approach 384 KiB
            // when four-byte Unicode scalar values use JSON surrogate escapes.
            .unwrap_or(512 * 1024);
        Self {
            forum_name,
            initial_site_subtitle,
            initial_homepage_new_thread_badges_enabled,
            initial_homepage_new_reply_badges_enabled,
            initial_thread_new_reply_badges_enabled,
            initial_default_theme,
            initial_enabled_builtin_themes,
            port,
            max_image_size: mebibytes_to_bytes(max_image_mb),
            max_video_size: mebibytes_to_bytes(max_video_mb),
            max_audio_size: mebibytes_to_bytes(max_audio_mb),
            enable_tor_support,
            tor_only,
            tor_bootstrap_timeout_secs: env_parse(
                "CHAN_TOR_BOOTSTRAP_TIMEOUT",
                s.tor_bootstrap_timeout_secs.unwrap_or(120),
            ),
            tor_max_concurrent_streams: env_parse(
                "CHAN_TOR_MAX_STREAMS",
                s.tor_max_concurrent_streams.unwrap_or(512),
            ),
            tor_service_nickname: env::var("CHAN_TOR_NICKNAME")
                .ok()
                .or(s.tor_service_nickname)
                .unwrap_or_else(|| "rustchan".to_owned()),
            require_ffmpeg: env_bool("CHAN_REQUIRE_FFMPEG", s.require_ffmpeg.unwrap_or(false)),
            ffmpeg_path: env::var("CHAN_FFMPEG_PATH")
                .ok()
                .or(s.ffmpeg_path)
                .unwrap_or_else(|| "ffmpeg".to_owned()),
            ffprobe_path: env::var("CHAN_FFPROBE_PATH")
                .ok()
                .or(s.ffprobe_path)
                .unwrap_or_else(|| "ffprobe".to_owned()),
            enable_any_file_uploads_feature: env_bool(
                "CHAN_ENABLE_ANY_FILE_UPLOADS_FEATURE",
                s.enable_any_file_uploads_feature.unwrap_or(false),
            ),
            bind_addr,
            database_path: env_str("CHAN_DB", &default_db),
            upload_dir: env_str("CHAN_UPLOADS", &default_uploads),
            thumb_size: env_parse("CHAN_THUMB_SIZE", 250),
            rate_limit_gets: env_parse("CHAN_RATE_GETS", 60),
            rate_limit_window: env_parse("CHAN_RATE_WINDOW", 60),
            cookie_secret,
            session_duration: env_parse("CHAN_SESSION_SECS", 8 * 3600),
            behind_proxy,
            trusted_proxy_cidrs,
            https_cookies: env_bool(
                "CHAN_HTTPS_COOKIES",
                s.https_cookies.unwrap_or(https_cookies_default),
            ),
            public_readiness_details: env_bool(
                "CHAN_PUBLIC_READINESS_DETAILS",
                s.public_readiness_details.unwrap_or(false),
            ),
            public_metrics_enabled: env_bool(
                "CHAN_PUBLIC_METRICS_ENABLED",
                s.public_metrics_enabled.unwrap_or(false),
            ),
            public_hosts,
            wal_checkpoint_interval: env_parse(
                "CHAN_WAL_CHECKPOINT_SECS",
                s.wal_checkpoint_interval_secs.unwrap_or(3600),
            ),
            auto_vacuum_interval_hours: env_parse(
                "CHAN_AUTO_VACUUM_HOURS",
                s.auto_vacuum_interval_hours.unwrap_or(24),
            ),
            auto_full_backup_interval_hours: env_parse(
                "CHAN_AUTO_FULL_BACKUP_HOURS",
                s.auto_full_backup_interval_hours.unwrap_or(24),
            ),
            auto_full_backup_copies_to_keep: env_parse::<u64>(
                "CHAN_AUTO_FULL_BACKUP_COPIES",
                s.auto_full_backup_copies_to_keep.unwrap_or(1),
            )
            .max(1),
            auto_full_backup_include_tor_hidden_service_keys: env_bool(
                "CHAN_AUTO_FULL_BACKUP_INCLUDE_TOR_KEYS",
                s.auto_full_backup_include_tor_hidden_service_keys
                    .unwrap_or(false),
            ),
            auto_full_backup_storage_mode: env::var("CHAN_AUTO_FULL_BACKUP_STORAGE_MODE")
                .ok()
                .filter(|value| matches!(value.as_str(), "directory" | "split_zip"))
                .or_else(|| {
                    s.auto_full_backup_storage_mode
                        .filter(|value| matches!(value.as_str(), "directory" | "split_zip"))
                })
                .unwrap_or_else(|| "directory".to_owned()),
            auto_full_backup_split_zip_part_size_bytes: env_parse::<u64>(
                "CHAN_AUTO_FULL_BACKUP_SPLIT_ZIP_PART_SIZE_GIB",
                s.auto_full_backup_split_zip_part_size_gib.unwrap_or(4),
            )
            .clamp(1, 64)
            .saturating_mul(1024 * 1024 * 1024),
            poll_cleanup_interval_hours: env_parse(
                "CHAN_POLL_CLEANUP_HOURS",
                s.poll_cleanup_interval_hours.unwrap_or(72),
            ),
            db_warn_threshold_bytes: {
                let mb = env_parse::<u64>(
                    "CHAN_DB_WARN_THRESHOLD_MB",
                    s.db_warn_threshold_mb.unwrap_or(2048),
                );
                mb.saturating_mul(1024).saturating_mul(1024)
            },
            job_queue_capacity: env_parse(
                "CHAN_JOB_QUEUE_CAPACITY",
                s.job_queue_capacity.unwrap_or(1000),
            ),
            ffmpeg_timeout_secs: env_parse(
                "CHAN_FFMPEG_TIMEOUT_SECS",
                s.ffmpeg_timeout_secs.unwrap_or(DEFAULT_FFMPEG_TIMEOUT_SECS),
            ),
            initial_media_auto_prune_enabled: env_bool(
                "CHAN_MEDIA_AUTO_PRUNE_ENABLED",
                s.media_auto_prune_enabled.unwrap_or(false),
            ),
            initial_media_max_active_content_size_bytes: env_parse(
                "CHAN_MEDIA_MAX_ACTIVE_CONTENT_SIZE_BYTES",
                s.media_max_active_content_size_bytes.unwrap_or(0),
            ),
            archive_before_prune: env_bool(
                "CHAN_ARCHIVE_BEFORE_PRUNE",
                s.archive_before_prune.unwrap_or(true),
            ),
            waveform_cache_max_bytes: {
                let mb = env_parse::<u64>(
                    "CHAN_WAVEFORM_CACHE_MAX_MB",
                    s.waveform_cache_max_mb.unwrap_or(200),
                );
                mb.saturating_mul(1024).saturating_mul(1024)
            },
            media_reconcile_repair_enabled: env_bool("CHAN_MEDIA_RECONCILE_REPAIR_ENABLED", false),
            media_reconcile_interval_hours: env_parse("CHAN_MEDIA_RECONCILE_INTERVAL_HOURS", 24),
            media_reconcile_files_per_pass: env_parse("CHAN_MEDIA_RECONCILE_FILES_PER_PASS", 512),
            media_reconcile_database_rows_per_pass: env_parse(
                "CHAN_MEDIA_RECONCILE_DATABASE_ROWS_PER_PASS",
                16_384,
            ),
            media_reconcile_hash_bytes_per_pass: env_parse(
                "CHAN_MEDIA_RECONCILE_HASH_BYTES_PER_PASS",
                64 * 1024 * 1024,
            ),
            media_reconcile_repairs_per_pass: env_parse(
                "CHAN_MEDIA_RECONCILE_REPAIRS_PER_PASS",
                32,
            ),
            blocking_threads: {
                let cpus = std::thread::available_parallelism().map_or(4, std::num::NonZero::get);
                let configured =
                    env_parse("CHAN_BLOCKING_THREADS", s.blocking_threads.unwrap_or(0));
                if configured == 0 {
                    cpus.saturating_mul(4)
                } else {
                    configured
                }
            },
            db_pool_size: env_parse("CHAN_DB_POOL_SIZE", s.db_pool_size.unwrap_or(8)),
            // ChanNet fields
            rustwave_url,
            chan_net_bind,
            chan_net_max_body,
            chan_net_command_max_body,
            chan_net_api_key: env::var("CHAN_NET_API_KEY")
                .ok()
                .or(s.chan_net_api_key)
                .unwrap_or_default(),
            // TLS — loaded from [tls] section in settings.toml; defaults to disabled.
            tls,
        }
    }

    /// Validate critical configuration values and abort with a clear error
    /// message if any are out of range. Called once at startup so operators
    /// catch misconfiguration immediately rather than discovering it at runtime.
    ///
    /// # Errors
    /// Returns an error if any configuration value is out of an acceptable range,
    /// or if the upload directory is not writable.
    #[expect(
        clippy::too_many_lines,
        reason = "startup validation keeps the complete fail-closed configuration audit in one routine"
    )]
    pub fn validate(&self) -> anyhow::Result<()> {
        fn url_host_is_loopback(url: &str) -> bool {
            reqwest::Url::parse(url).ok().is_some_and(|parsed| {
                parsed.host_str().is_some_and(|host| {
                    host.eq_ignore_ascii_case("localhost")
                        || host
                            .parse::<std::net::IpAddr>()
                            .ok()
                            .is_some_and(|ip| ip.is_loopback())
                })
            })
        }

        const MIB: usize = 1024 * 1024;
        const MAX_IMAGE_MIB: usize = 100;
        const MAX_VIDEO_MIB: usize = 2048;
        const MAX_AUDIO_MIB: usize = 512;
        // cookie_secret is hex-encoded: 64 hex chars = 32 bytes of entropy.
        if self.cookie_secret.len() < 64 {
            anyhow::bail!(
                "CONFIG ERROR: cookie_secret is too short ({} chars). \
                 It must be at least 64 hex characters (32 bytes). \
                 Delete settings.toml and restart to auto-generate a secure secret.",
                self.cookie_secret.len()
            );
        }
        if self.max_image_size < MIB || self.max_image_size > MAX_IMAGE_MIB * MIB {
            anyhow::bail!(
                "CONFIG ERROR: max_image_size_mb must be between 1 and {} MiB (got {} MiB).",
                MAX_IMAGE_MIB,
                self.max_image_size / MIB
            );
        }
        if self.max_video_size < MIB || self.max_video_size > MAX_VIDEO_MIB * MIB {
            anyhow::bail!(
                "CONFIG ERROR: max_video_size_mb must be between 1 and {} MiB (got {} MiB).",
                MAX_VIDEO_MIB,
                self.max_video_size / MIB
            );
        }
        if self.max_audio_size < MIB || self.max_audio_size > MAX_AUDIO_MIB * MIB {
            anyhow::bail!(
                "CONFIG ERROR: max_audio_size_mb must be between 1 and {} MiB (got {} MiB).",
                MAX_AUDIO_MIB,
                self.max_audio_size / MIB
            );
        }
        if self.port == 0 {
            anyhow::bail!("CONFIG ERROR: port must not be 0.");
        }
        if self.tls.enabled && self.tls.port == 0 {
            anyhow::bail!(
                "CONFIG ERROR: tls.port must not be 0. \
                 Add `port = 8443` under [tls] in settings.toml, or remove the explicit `port = 0`."
            );
        }
        let Some(http_port) = port_from_bind_addr(&self.bind_addr) else {
            anyhow::bail!(
                "CONFIG ERROR: bind_addr '{}' is not a valid host:port address.",
                self.bind_addr
            );
        };
        if self.tls.enabled && self.tls.port == http_port {
            anyhow::bail!(
                "CONFIG ERROR: tls.port ({}) must differ from the main HTTP port ({}).",
                self.tls.port,
                http_port
            );
        }
        if self.tls.enabled && self.tls.redirect_http {
            if self.tls.http_port == http_port {
                anyhow::bail!(
                    "CONFIG ERROR: tls.http_port ({}) must differ from the main HTTP port ({}) \
                     when tls.redirect_http=true.",
                    self.tls.http_port,
                    http_port
                );
            }
            if self.tls.http_port == self.tls.port {
                anyhow::bail!(
                    "CONFIG ERROR: tls.http_port ({}) must differ from tls.port ({}) \
                     when tls.redirect_http=true.",
                    self.tls.http_port,
                    self.tls.port
                );
            }
        }
        for cidr in &self.trusted_proxy_cidrs {
            cidr.parse::<ipnet::IpNet>().map_err(|error| {
                anyhow::anyhow!(
                    "CONFIG ERROR: trusted_proxy_cidrs entry '{cidr}' is not valid CIDR: {error}"
                )
            })?;
        }
        for host in &self.public_hosts {
            normalize_public_host(host).ok_or_else(|| {
                anyhow::anyhow!(
                    "CONFIG ERROR: public_hosts entry '{host}' must be a bare hostname or IP literal."
                )
            })?;
        }
        // Verify the upload directory is writable.
        let upload_path = Path::new(&self.upload_dir);
        if upload_path.exists() {
            let probe = upload_path.join(".write_probe");
            if std::fs::write(&probe, b"").is_err() {
                anyhow::bail!(
                    "CONFIG ERROR: upload_dir '{}' is not writable.",
                    self.upload_dir
                );
            }
            drop(std::fs::remove_file(probe));
        }
        // F-13: Pre-flight writability check for Arti data directories.
        // Without this, a permissions error on these dirs only surfaces ~30 s
        // into bootstrap as a cryptic internal error — invisible at startup.
        if self.enable_tor_support {
            for dir in [runtime_tor_state_dir(), runtime_tor_cache_dir()] {
                ensure_private_dir(&dir).map_err(|e| {
                    anyhow::anyhow!("CONFIG ERROR: cannot create Tor dir {}: {e}", dir.display())
                })?;
                let probe = dir.join(".write_probe");
                std::fs::write(&probe, b"").map_err(|_error| {
                    anyhow::anyhow!(
                        "CONFIG ERROR: Tor dir {} is not writable — check permissions",
                        dir.display()
                    )
                })?;
                drop(std::fs::remove_file(probe));
            }
        }
        // Validate rustwave_url at startup rather than at first federation call.
        if !self.rustwave_url.starts_with("http://") && !self.rustwave_url.starts_with("https://") {
            return Err(anyhow::anyhow!(
                "CONFIG ERROR: rustwave_url must begin with http:// or https://, got: {}",
                self.rustwave_url
            ));
        }
        if !self.chan_net_api_key.is_empty() && self.chan_net_api_key.len() < 32 {
            anyhow::bail!(
                "CONFIG ERROR: chan_net_api_key must be empty to disable ChanNet auth-protected endpoints \
                 or at least 32 characters long."
            );
        }
        if self.tor_only && !self.enable_tor_support {
            anyhow::bail!(
                "CONFIG ERROR: tor_only=true requires enable_tor_support=true. \
                 Tor-only mode needs the built-in onion service to be active."
            );
        }
        if self.enable_tor_support && !self.tor_only {
            tracing::warn!(
                target: "config",
                "Tor support is enabled, but tor_only=false. RustChan will accept both clearnet and Tor traffic."
            );
        }
        if self.tor_only && self.tls.acme.enabled {
            anyhow::bail!(
                "CONFIG ERROR: tor_only=true cannot be combined with [tls.acme]. \
                 ACME validation requires public HTTPS reachability, but tor_only binds RustChan to loopback."
            );
        }
        if self.tor_only && !url_host_is_loopback(&self.rustwave_url) {
            anyhow::bail!(
                "CONFIG ERROR: tor_only=true requires rustwave_url to point at localhost/loopback. \
                 Current rustwave_url '{}' would send federation traffic directly off-host.",
                self.rustwave_url
            );
        }
        validate_ffmpeg_timeout_secs(self.ffmpeg_timeout_secs)?;
        Ok(())
    }

    /// Validate `ChanNet` listener settings when the `ChanNet` service is enabled.
    ///
    /// # Errors
    /// Returns an error if `ChanNet` would bind to a non-loopback address without
    /// a configured pre-shared key.
    pub fn validate_chan_net_listener(&self) -> anyhow::Result<()> {
        if self.chan_net_api_key.is_empty() && !bind_addr_is_loopback(&self.chan_net_bind) {
            anyhow::bail!(
                "CONFIG ERROR: --chan-net with chan_net_bind '{}' requires chan_net_api_key when binding outside loopback.",
                self.chan_net_bind
            );
        }
        Ok(())
    }

    #[must_use]
    /// Format the primary listener address with a replacement port.
    pub fn bind_addr_with_port(&self, port: u16) -> String {
        bind_addr_for_port(&self.bind_addr, port)
    }

    #[must_use]
    /// Format the matching loopback address family with a replacement port.
    pub fn loopback_addr_with_port(&self, port: u16) -> String {
        loopback_addr_for_family(&self.bind_addr, port)
    }
}

/// Update site identity fields in `settings.toml` in-place,
/// preserving all other lines and comments.
///
/// Called by the admin site-settings handler so that changes made via the
/// panel are reflected in the file and survive a restart without the operator
/// needing to hand-edit `settings.toml`.
///
/// If a key is not yet present in the file, it is inserted before the requested
/// anchor section. On a fresh install `generate_settings_file_if_missing`
/// already writes these keys, so insertion mainly covers manually-crafted files.
/// Quote and escape one TOML basic string.
fn toml_quote(s: &str) -> String {
    let inner = s.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{inner}\"")
}

/// Rewrite selected root settings while preserving unrelated text and comments.
fn rewrite_settings_file_lines(
    content: &str,
    updates: &[(&str, String)],
    insert_missing_before: Option<&str>,
) -> String {
    use std::collections::BTreeSet;

    let trailing_newline = content.ends_with('\n');
    let mut seen_keys = BTreeSet::new();
    let mut updated_lines: Vec<String> = content
        .lines()
        .map(|line| {
            let trimmed = line.trim_start();
            for (key, value) in updates {
                if trimmed.starts_with(key) && line.contains('=') {
                    seen_keys.insert(*key);
                    return format!("{key} = {value}");
                }
            }
            line.to_owned()
        })
        .collect();

    let missing = updates
        .iter()
        .filter(|(key, _)| !seen_keys.contains(key))
        .map(|(key, value)| format!("{key} = {value}"))
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        let insert_idx = insert_missing_before
            .and_then(|anchor| {
                updated_lines
                    .iter()
                    .position(|line| line.trim_start().starts_with(anchor))
            })
            .or_else(|| {
                updated_lines
                    .iter()
                    .position(|line| line.trim_start().starts_with('['))
            })
            .unwrap_or(updated_lines.len());
        let mut insertion_block = missing;
        let previous_line = insert_idx
            .checked_sub(1)
            .and_then(|index| updated_lines.get(index));
        if previous_line.is_some_and(|line| !line.trim().is_empty()) {
            insertion_block.insert(0, String::new());
        }
        let next_line = updated_lines.get(insert_idx);
        if next_line.is_some_and(|line| !line.trim().is_empty()) {
            insertion_block.push(String::new());
        }
        updated_lines.splice(insert_idx..insert_idx, insertion_block);
    }

    let mut out = updated_lines.join("\n");
    if trailing_newline {
        out.push('\n');
    }
    out
}

/// Atomically persist selected root settings.
fn update_settings_file_entries_result(
    updates: &[(&str, String)],
    insert_missing_before: Option<&str>,
) -> anyhow::Result<()> {
    // Escape backslash and double-quote, then wrap in double quotes.
    let path = settings_file_path();
    let content = match std::fs::read_to_string(&path) {
        Ok(content) => content,
        Err(error) => anyhow::bail!("could not read {}: {error}", path.display()),
    };
    // Replace the value portion of `key = ...` lines while preserving file
    // order and unrelated comments.
    let out = rewrite_settings_file_lines(&content, updates, insert_missing_before);
    // Atomic write: write to a temp file in the same directory, then rename
    // over the target. This prevents a partial write from corrupting settings.toml
    // if the process is killed mid-write (rename(2) is atomic on POSIX).
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let mut tmp = tempfile::Builder::new()
        .prefix(".settings_")
        .suffix(".tmp")
        .tempfile_in(dir)
        .with_context(|| format!("could not create temp file for {}", path.display()))?;
    tmp.write_all(out.as_bytes())
        .and_then(|()| tmp.as_file().sync_all())
        .with_context(|| format!("could not write temp settings file for {}", path.display()))?;
    tmp.persist(&path)
        .map_err(|error| error.error)
        .with_context(|| format!("could not atomically replace {}", path.display()))?;
    restrict_private_file_permissions(&path)
        .with_context(|| format!("could not repair permissions on {}", path.display()))?;
    Ok(())
}

/// Persist selected root settings and log failures.
fn update_settings_file_entries(updates: &[(&str, String)], insert_missing_before: Option<&str>) {
    if let Err(error) = update_settings_file_entries_result(updates, insert_missing_before) {
        tracing::warn!(
            target: "config",
            error = %error,
            "Could not update settings.toml"
        );
    }
}

/// Persist site identity, activity-badge, and default-theme settings.
pub fn update_settings_file_site_settings(
    forum_name: &str,
    site_subtitle: &str,
    homepage_new_thread_badges_enabled: bool,
    homepage_new_reply_badges_enabled: bool,
    thread_new_reply_badges_enabled: bool,
    default_theme: &str,
) {
    update_settings_file_entries(
        &[
            ("forum_name", toml_quote(forum_name)),
            ("site_subtitle", toml_quote(site_subtitle)),
            (
                "homepage_new_thread_badges_enabled",
                homepage_new_thread_badges_enabled.to_string(),
            ),
            (
                "homepage_new_reply_badges_enabled",
                homepage_new_reply_badges_enabled.to_string(),
            ),
            (
                "thread_new_reply_badges_enabled",
                thread_new_reply_badges_enabled.to_string(),
            ),
            ("default_theme", toml_quote(default_theme)),
        ],
        Some("# ── Network / web server"),
    );
}

/// Persist automatic full-backup scheduling and retention settings.
pub fn update_settings_file_auto_full_backup(
    interval_hours: u64,
    copies_to_keep: u64,
    include_tor_hidden_service_keys: bool,
    storage_mode: &str,
    split_zip_part_size_gib: u64,
) {
    update_settings_file_entries(
        &[
            (
                "auto_full_backup_interval_hours",
                interval_hours.to_string(),
            ),
            (
                "auto_full_backup_copies_to_keep",
                copies_to_keep.max(1).to_string(),
            ),
            (
                "auto_full_backup_include_tor_hidden_service_keys",
                include_tor_hidden_service_keys.to_string(),
            ),
            ("auto_full_backup_storage_mode", toml_quote(storage_mode)),
            (
                "auto_full_backup_split_zip_part_size_gib",
                split_zip_part_size_gib.to_string(),
            ),
        ],
        Some("# ── Federation / ChanNet gateway"),
    );
}

#[derive(Clone, Copy, Debug)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "settings update mirrors independent runtime setup toggles"
)]
/// Runtime toggles and upload limits collected by the setup flow.
pub struct SetupRuntimeSettingsUpdate {
    /// Whether to start the Tor onion service.
    pub enable_tor_support: bool,
    /// Whether the primary listener must remain loopback-only.
    pub tor_only: bool,
    /// Whether trusted reverse-proxy forwarding headers are enabled.
    pub behind_proxy: bool,
    /// Whether authentication cookies carry the `Secure` attribute.
    pub https_cookies: bool,
    /// Maximum image upload size in mebibytes.
    pub max_image_size_mb: u64,
    /// Maximum video upload size in mebibytes.
    pub max_video_size_mb: u64,
    /// Maximum audio upload size in mebibytes.
    pub max_audio_size_mb: u64,
}

#[derive(Debug)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "settings update mirrors independent first-run setup toggles"
)]
/// Complete settings-file update collected by first-run setup.
pub struct SetupSettingsFileUpdate<'a> {
    /// Public forum name.
    pub forum_name: &'a str,
    /// Public home-page subtitle.
    pub site_subtitle: &'a str,
    /// Initial home-page new-thread badge state.
    pub homepage_new_thread_badges_enabled: bool,
    /// Initial home-page new-reply badge state.
    pub homepage_new_reply_badges_enabled: bool,
    /// Initial thread-card new-reply badge state.
    pub thread_new_reply_badges_enabled: bool,
    /// Initial default theme slug.
    pub default_theme: &'a str,
    /// Automatic full-backup interval in hours.
    pub auto_full_backup_interval_hours: u64,
    /// Number of automatic full backups retained.
    pub auto_full_backup_copies_to_keep: u64,
    /// Whether automatic full backups contain onion-service identity keys.
    pub auto_full_backup_include_tor_hidden_service_keys: bool,
    /// Saved-backup storage mode.
    pub auto_full_backup_storage_mode: &'a str,
    /// Maximum split-archive part size in gibibytes.
    pub auto_full_backup_split_zip_part_size_gib: u64,
    /// Runtime-specific setup values.
    pub runtime: SetupRuntimeSettingsUpdate,
}

/// Persist all setup-owned runtime settings in one atomic settings.toml rewrite.
///
/// # Errors
/// Returns an error if the settings file cannot be read or atomically replaced.
pub fn update_settings_file_setup(update: &SetupSettingsFileUpdate<'_>) -> anyhow::Result<()> {
    update_settings_file_entries_result(
        &[
            ("forum_name", toml_quote(update.forum_name)),
            ("site_subtitle", toml_quote(update.site_subtitle)),
            (
                "homepage_new_thread_badges_enabled",
                update.homepage_new_thread_badges_enabled.to_string(),
            ),
            (
                "homepage_new_reply_badges_enabled",
                update.homepage_new_reply_badges_enabled.to_string(),
            ),
            (
                "thread_new_reply_badges_enabled",
                update.thread_new_reply_badges_enabled.to_string(),
            ),
            ("default_theme", toml_quote(update.default_theme)),
            (
                "enable_tor_support",
                update.runtime.enable_tor_support.to_string(),
            ),
            ("tor_only", update.runtime.tor_only.to_string()),
            ("behind_proxy", update.runtime.behind_proxy.to_string()),
            ("https_cookies", update.runtime.https_cookies.to_string()),
            (
                "max_image_size_mb",
                update.runtime.max_image_size_mb.to_string(),
            ),
            (
                "max_video_size_mb",
                update.runtime.max_video_size_mb.to_string(),
            ),
            (
                "max_audio_size_mb",
                update.runtime.max_audio_size_mb.to_string(),
            ),
            (
                "auto_full_backup_interval_hours",
                update.auto_full_backup_interval_hours.to_string(),
            ),
            (
                "auto_full_backup_copies_to_keep",
                update.auto_full_backup_copies_to_keep.max(1).to_string(),
            ),
            (
                "auto_full_backup_include_tor_hidden_service_keys",
                update
                    .auto_full_backup_include_tor_hidden_service_keys
                    .to_string(),
            ),
            (
                "auto_full_backup_storage_mode",
                toml_quote(update.auto_full_backup_storage_mode),
            ),
            (
                "auto_full_backup_split_zip_part_size_gib",
                update.auto_full_backup_split_zip_part_size_gib.to_string(),
            ),
        ],
        Some("# Optional explicit ffmpeg binary path."),
    )
}

/// Persist the configured `FFmpeg` subprocess timeout.
pub fn update_settings_file_ffmpeg_timeout(timeout_secs: u64) {
    let timeout_secs = timeout_secs.clamp(MIN_FFMPEG_TIMEOUT_SECS, MAX_FFMPEG_TIMEOUT_SECS);
    update_settings_file_entries(
        &[("ffmpeg_timeout_secs", timeout_secs.to_string())],
        Some("# Optional explicit ffmpeg binary path."),
    );
}

/// Persist active-media pruning enablement and size limit.
pub fn update_settings_file_media_pruning(enabled: bool, max_size_bytes: u64) {
    update_settings_file_entries(
        &[
            ("media_auto_prune_enabled", enabled.to_string()),
            (
                "media_max_active_content_size_bytes",
                max_size_bytes.to_string(),
            ),
        ],
        Some("# Optional explicit ffmpeg binary path."),
    );
}

// ─── Cookie secret rotation check ────────────────────────────────────────────
/// Check whether the `cookie_secret` has changed since the last run by comparing
/// a SHA-256 hash stored in the DB against the currently loaded secret.
///
/// Called once at startup after the DB pool is ready.
/// If the secret has rotated, all IP-based bans become invalid — warn loudly.
/// On first run (no stored hash), silently stores the current hash and returns.
pub fn check_cookie_secret_rotation(conn: &rusqlite::Connection) {
    use sha2::{Digest as _, Sha256};
    const KEY: &str = "cookie_secret_hash";
    let current_hash = {
        let mut h = Sha256::new();
        h.update(CONFIG.cookie_secret.as_bytes());
        hex::encode(h.finalize())
    };
    let stored = conn
        .query_row(
            "SELECT value FROM site_settings WHERE key = ?1",
            rusqlite::params![KEY],
            |r| r.get::<_, String>(0),
        )
        .ok();
    if let Some(h) = &stored {
        if h == &current_hash {
            return; // Secret unchanged — nothing to do.
        }
        tracing::warn!(
            "SECURITY WARNING: cookie_secret has changed since the last run. \
             All IP-based bans are now invalid because all IP hashes have changed. \
             If this was unintentional, restore the previous cookie_secret from \
             settings.toml. If intentional, consider running: \
             DELETE FROM bans; DELETE FROM ban_appeals;"
        );
    }
    // First run (None) or rotated secret (Some) — store the current hash.
    drop(conn.execute(
        "INSERT INTO site_settings (key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        rusqlite::params![KEY, current_hash],
    ));
}

// ─── Env helpers ──────────────────────────────────────────────────────────────
/// Read a string environment override or clone its default.
fn env_str(key: &str, default: &str) -> String {
    env::var(key).unwrap_or_else(|_| default.to_owned())
}

/// Parse an environment override or retain its typed default.
fn env_parse<T: std::str::FromStr>(key: &str, default: T) -> T {
    env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// Read a boolean environment override using the accepted truthy spellings.
fn env_bool(key: &str, default: bool) -> bool {
    env::var(key).map_or(default, |v| v == "1" || v.eq_ignore_ascii_case("true"))
}

/// Resolve a comma-separated list from environment, file, or defaults.
fn env_list(key: &str, file_value: Option<Vec<String>>, default: &[&str]) -> Vec<String> {
    env::var(key)
        .ok()
        .map(|value| split_list(&value))
        .or(file_value)
        .unwrap_or_else(|| default.iter().map(|value| (*value).to_owned()).collect())
}

/// Split a comma-separated setting, trimming and discarding empty values.
fn split_list(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Convert a mebibyte setting to the platform's byte-count type without wrapping.
fn mebibytes_to_bytes(mebibytes: u32) -> usize {
    usize::try_from(mebibytes)
        .unwrap_or(usize::MAX)
        .saturating_mul(1024)
        .saturating_mul(1024)
}

/// Validate and normalize a public host name or IP literal.
pub fn normalize_public_host(host: &str) -> Option<String> {
    let trimmed = host.trim();
    if trimmed.is_empty() || trimmed.contains('/') || trimmed.contains('@') {
        return None;
    }

    let unbracketed = trimmed
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
        .unwrap_or(trimmed);

    if unbracketed.parse::<std::net::IpAddr>().is_ok() {
        return Some(unbracketed.to_owned());
    }

    if unbracketed.contains(':') || unbracketed.contains(char::is_whitespace) {
        return None;
    }

    Some(unbracketed.to_owned())
}

/// Split an IPv4, hostname, or bracketed-IPv6 listener address.
fn split_bind_addr(addr: &str) -> Option<(&str, &str)> {
    if let Some(rest) = addr.strip_prefix('[') {
        let (host, port) = rest.split_once("]:")?;
        Some((host, port))
    } else {
        addr.rsplit_once(':')
    }
}

/// Extract the listener host, defaulting to an IPv4 wildcard.
fn bind_host_for_family(addr: &str) -> &str {
    split_bind_addr(addr).map_or("0.0.0.0", |(host, _)| host)
}

/// Return whether a host string uses IPv6 notation.
fn host_is_ipv6(host: &str) -> bool {
    host.contains(':')
}

/// Format a host and port, adding IPv6 brackets when necessary.
fn format_bind_addr(host: &str, port: u16) -> String {
    if host_is_ipv6(host) {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

/// Reuse a listener's address family and host with another port.
fn bind_addr_for_port(addr: &str, port: u16) -> String {
    format_bind_addr(bind_host_for_family(addr), port)
}

/// Format a loopback listener matching an address's IP family.
fn loopback_addr_for_family(addr: &str, port: u16) -> String {
    let host = if host_is_ipv6(bind_host_for_family(addr)) {
        "::1"
    } else {
        "127.0.0.1"
    };
    format_bind_addr(host, port)
}

/// Parse the port from a listener address.
fn port_from_bind_addr(addr: &str) -> Option<u16> {
    let (_, port) = split_bind_addr(addr)?;
    port.parse().ok()
}

#[cfg(test)]
/// Configuration unit tests.
mod tests {
    use super::{
        describe_timeout_secs, ffmpeg_timeout_secs, resolve_data_dir_override,
        rewrite_settings_file_lines, runtime_tor_hidden_service_keys_dir,
        set_live_ffmpeg_timeout_secs, settings_file_path, template::settings_template,
        update_settings_file_ffmpeg_timeout, validate_ffmpeg_timeout_secs, Config, TlsConfig,
        DEFAULT_FFMPEG_TIMEOUT_SECS, MAX_FFMPEG_TIMEOUT_SECS, MIN_FFMPEG_TIMEOUT_SECS,
    };
    use anyhow::Context as _;
    use std::path::{Path, PathBuf};
    use std::sync::{Mutex, MutexGuard, PoisonError};

    /// Serializes tests that replace the process-wide settings file.
    static SETTINGS_FILE_TEST_LOCK: Mutex<()> = Mutex::new(());

    /// Standard fallible test result.
    type TestResult = anyhow::Result<()>;

    /// Original settings-file state restored after a mutating test.
    #[derive(Debug)]
    struct SettingsFileSnapshot {
        /// Settings-file path being protected.
        path: PathBuf,
        /// Original file bytes, or `None` when no file existed.
        previous: Option<Vec<u8>>,
        /// Whether explicit restoration already succeeded.
        restored: bool,
    }

    impl SettingsFileSnapshot {
        /// Capture the current settings-file state.
        ///
        /// # Errors
        ///
        /// Returns an error when an existing settings file cannot be read.
        fn capture(path: PathBuf) -> anyhow::Result<Self> {
            let previous = match std::fs::read(&path) {
                Ok(contents) => Some(contents),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(error) => {
                    return Err(error).with_context(|| {
                        format!("could not capture settings fixture {}", path.display())
                    });
                }
            };
            Ok(Self {
                path,
                previous,
                restored: false,
            })
        }

        /// Restore the captured settings-file state.
        ///
        /// # Errors
        ///
        /// Returns an error when the original file cannot be restored or a
        /// test-created file cannot be removed.
        fn restore(mut self) -> TestResult {
            self.restore_inner()?;
            self.restored = true;
            Ok(())
        }

        /// Perform restoration without changing the completion flag.
        fn restore_inner(&self) -> TestResult {
            if let Some(contents) = &self.previous {
                std::fs::write(&self.path, contents).with_context(|| {
                    format!("could not restore settings file {}", self.path.display())
                })?;
            } else if let Err(error) = std::fs::remove_file(&self.path) {
                if error.kind() != std::io::ErrorKind::NotFound {
                    return Err(error).with_context(|| {
                        format!("could not remove settings fixture {}", self.path.display())
                    });
                }
            }
            Ok(())
        }
    }

    impl Drop for SettingsFileSnapshot {
        fn drop(&mut self) {
            if !self.restored {
                drop(self.restore_inner());
            }
        }
    }

    /// Live timeout value restored after a mutating test.
    #[derive(Debug)]
    struct LiveFfmpegTimeoutSnapshot(u64);

    impl LiveFfmpegTimeoutSnapshot {
        /// Capture the current live timeout.
        fn capture() -> Self {
            Self(ffmpeg_timeout_secs())
        }
    }

    impl Drop for LiveFfmpegTimeoutSnapshot {
        fn drop(&mut self) {
            super::LIVE_FFMPEG_TIMEOUT_SECS.store(self.0, std::sync::atomic::Ordering::Relaxed);
        }
    }

    /// Hold the settings-file test lock, recovering after a prior test panic.
    fn lock_settings_file() -> MutexGuard<'static, ()> {
        SETTINGS_FILE_TEST_LOCK
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "assertion failures are the intended failure mechanism for this test"
    )]
    /// Accepts scoped absolute data paths and rejects unsafe path shapes.
    fn data_dir_override_requires_a_scoped_absolute_path() -> TestResult {
        let absolute = std::env::current_dir()
            .context("could not read current directory")?
            .join("rustchan-test-data");
        let filesystem_root = absolute
            .ancestors()
            .last()
            .context("absolute path did not have a filesystem root")?;
        let with_parent = absolute.join("nested").join("..").join("data");

        assert!(
            resolve_data_dir_override(&absolute).is_ok(),
            "scoped absolute paths should be accepted"
        );
        assert!(
            resolve_data_dir_override(Path::new("relative/data")).is_err(),
            "relative paths should be rejected"
        );
        assert!(
            resolve_data_dir_override(filesystem_root).is_err(),
            "filesystem roots should be rejected"
        );
        assert!(
            resolve_data_dir_override(&with_parent).is_err(),
            "parent-directory components should be rejected"
        );
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "assertion failures are the intended failure mechanism for this test"
    )]
    /// Rejects root symlinks without changing the containing directory.
    fn data_dir_override_rejects_symlink_to_root_without_mutation() -> TestResult {
        use std::os::unix::fs::symlink;

        let temp_dir = tempfile::tempdir().context("could not create temporary directory")?;
        let root_link = temp_dir.path().join("root-link");
        symlink("/", &root_link).context("could not create root symlink")?;
        let entries_before = std::fs::read_dir(temp_dir.path())
            .context("could not read temporary directory before validation")?
            .map(|entry| entry.map(|entry| entry.file_name()))
            .collect::<Result<Vec<_>, _>>()
            .context("could not read a temporary-directory entry before validation")?;

        let error = resolve_data_dir_override(&root_link)
            .err()
            .context("root symlink should have been rejected")?;
        let nested_error = resolve_data_dir_override(&root_link.join("new-data"))
            .err()
            .context("root symlink ancestor should have been rejected")?;

        assert!(
            error.to_string().contains("filesystem root"),
            "direct root symlink error should identify the filesystem root"
        );
        assert!(
            nested_error.to_string().contains("filesystem root"),
            "nested root symlink error should identify the filesystem root"
        );
        assert_eq!(
            std::fs::read_link(&root_link).context("root symlink should remain")?,
            Path::new("/"),
            "validation should not replace the root symlink"
        );
        let entries_after = std::fs::read_dir(temp_dir.path())
            .context("could not read temporary directory after validation")?
            .map(|entry| entry.map(|entry| entry.file_name()))
            .collect::<Result<Vec<_>, _>>()
            .context("could not read a temporary-directory entry after validation")?;
        assert_eq!(
            entries_after, entries_before,
            "validation should not mutate the temporary directory"
        );
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "assertion failures are the intended failure mechanism for this test"
    )]
    /// Resolves an existing symlink ancestor before appending missing components.
    fn data_dir_override_resolves_existing_symlink_ancestor_before_new_components() -> TestResult {
        use std::os::unix::fs::symlink;

        let temp_dir = tempfile::tempdir().context("could not create temporary directory")?;
        let actual_parent = temp_dir.path().join("actual");
        std::fs::create_dir(&actual_parent).context("could not create actual parent")?;
        let parent_link = temp_dir.path().join("parent-link");
        symlink(&actual_parent, &parent_link).context("could not create parent symlink")?;
        let requested = parent_link.join("nested").join("data");

        let resolved =
            resolve_data_dir_override(&requested).context("could not resolve data directory")?;

        assert_eq!(
            resolved,
            actual_parent
                .canonicalize()
                .context("could not canonicalize actual parent")?
                .join("nested")
                .join("data"),
            "missing components should be appended to the canonical ancestor"
        );
        assert!(
            !actual_parent.join("nested").exists(),
            "validation should not create missing components"
        );
        Ok(())
    }

    /// Build a complete configuration that passes validation.
    fn valid_config() -> Config {
        const MIB: usize = 1024 * 1024;
        const MIB_U64: u64 = 1024 * 1024;
        Config {
            forum_name: "RustChan".to_owned(),
            initial_site_subtitle: "select board to proceed".to_owned(),
            initial_homepage_new_thread_badges_enabled: true,
            initial_homepage_new_reply_badges_enabled: true,
            initial_thread_new_reply_badges_enabled: true,
            initial_default_theme: crate::theme::HARD_DEFAULT_THEME.to_owned(),
            initial_enabled_builtin_themes: crate::theme::builtin_theme_slugs()
                .into_iter()
                .map(str::to_owned)
                .collect(),
            port: 8080,
            max_image_size: 8 * MIB,
            max_video_size: 50 * MIB,
            max_audio_size: 150 * MIB,
            enable_tor_support: false,
            tor_only: false,
            tor_bootstrap_timeout_secs: 120,
            tor_max_concurrent_streams: 512,
            tor_service_nickname: "rustchan".to_owned(),
            require_ffmpeg: false,
            ffmpeg_path: "ffmpeg".to_owned(),
            ffprobe_path: "ffprobe".to_owned(),
            enable_any_file_uploads_feature: false,
            bind_addr: "0.0.0.0:8080".to_owned(),
            database_path: "chan.db".to_owned(),
            upload_dir: "__rustchan_test_uploads_does_not_exist__".to_owned(),
            thumb_size: 250,
            rate_limit_gets: 60,
            rate_limit_window: 60,
            cookie_secret: "a".repeat(64),
            session_duration: 8 * 3600,
            behind_proxy: false,
            trusted_proxy_cidrs: vec!["127.0.0.1/32".to_owned(), "::1/128".to_owned()],
            https_cookies: false,
            public_readiness_details: false,
            public_metrics_enabled: false,
            public_hosts: Vec::new(),
            wal_checkpoint_interval: 3600,
            auto_vacuum_interval_hours: 24,
            auto_full_backup_interval_hours: 24,
            auto_full_backup_copies_to_keep: 1,
            auto_full_backup_include_tor_hidden_service_keys: false,
            auto_full_backup_storage_mode: "directory".to_owned(),
            auto_full_backup_split_zip_part_size_bytes: 4 * 1024 * 1024 * 1024,
            poll_cleanup_interval_hours: 72,
            db_warn_threshold_bytes: 2_048 * MIB_U64,
            job_queue_capacity: 1000,
            ffmpeg_timeout_secs: 120,
            initial_media_auto_prune_enabled: false,
            initial_media_max_active_content_size_bytes: 0,
            archive_before_prune: true,
            waveform_cache_max_bytes: 200 * MIB_U64,
            media_reconcile_repair_enabled: false,
            media_reconcile_interval_hours: 24,
            media_reconcile_files_per_pass: 512,
            media_reconcile_database_rows_per_pass: 16_384,
            media_reconcile_hash_bytes_per_pass: 64 * MIB_U64,
            media_reconcile_repairs_per_pass: 32,
            blocking_threads: 4,
            db_pool_size: 8,
            rustwave_url: "http://localhost:7071".to_owned(),
            chan_net_bind: "127.0.0.1:7070".to_owned(),
            chan_net_max_body: 10 * MIB,
            chan_net_command_max_body: 512 * 1024,
            chan_net_api_key: String::new(),
            tls: TlsConfig::default(),
        }
    }

    #[test]
    /// Redacts authentication secrets while retaining useful configuration context.
    fn config_debug_redacts_secrets() -> TestResult {
        const COOKIE_SECRET: &str = "cookie-secret-debug-sentinel";
        const CHAN_NET_API_KEY: &str = "chan-net-key-debug-sentinel";
        const RUSTWAVE_URL: &str = "http://debug-user:rw-secret-sentinel@localhost:7071";
        const RUSTWAVE_CREDENTIAL: &str = "rw-secret-sentinel";
        let mut config = valid_config();
        config.cookie_secret = COOKIE_SECRET.to_owned();
        config.chan_net_api_key = CHAN_NET_API_KEY.to_owned();
        config.rustwave_url = RUSTWAVE_URL.to_owned();
        let rendered = format!("{config:?}");
        anyhow::ensure!(
            !rendered.contains(COOKIE_SECRET),
            "Config Debug output must not expose the cookie secret"
        );
        anyhow::ensure!(
            !rendered.contains(CHAN_NET_API_KEY) && !rendered.contains(RUSTWAVE_CREDENTIAL),
            "Config Debug output must not expose the ChanNet API key or RustWave URL credentials"
        );
        anyhow::ensure!(
            rendered.contains("cookie_secret: \"[REDACTED]\"")
                && rendered.contains("chan_net_api_key: \"[REDACTED]\"")
                && rendered.contains("rustwave_url: \"[REDACTED]\""),
            "Config Debug output should identify redacted fields"
        );
        anyhow::ensure!(
            rendered.contains("forum_name: \"RustChan\""),
            "Config Debug output should retain useful nonsecret context"
        );
        Ok(())
    }

    /// Return a configuration validation failure as text.
    fn validation_error(config: &Config) -> Option<String> {
        config.validate().err().map(|error| error.to_string())
    }

    #[test]
    /// Uses the Arti-native keystore location for onion-service identity keys.
    fn tor_hidden_service_keys_dir_matches_arti_native_keystore_location() {
        assert!(
            runtime_tor_hidden_service_keys_dir().ends_with("runtime/tor/state/keystore"),
            "the identity path should match Arti's native keystore layout"
        );
    }

    #[test]
    /// Updates selected root keys without disturbing comments or other values.
    fn rewrite_settings_file_lines_updates_requested_keys_and_preserves_comments() {
        let input = r#"# RustChan settings.toml
forum_name = "RustChan"
site_subtitle = "select board to proceed"
homepage_new_thread_badges_enabled = true
homepage_new_reply_badges_enabled = true
thread_new_reply_badges_enabled = true
default_theme = "forest"
auto_full_backup_interval_hours = 24
auto_full_backup_copies_to_keep = 1
"#;

        let output = rewrite_settings_file_lines(
            input,
            &[
                ("forum_name", "\"BackupChan\"".to_owned()),
                ("default_theme", "\"terminal\"".to_owned()),
                ("auto_full_backup_interval_hours", "12".to_owned()),
                ("auto_full_backup_copies_to_keep", "3".to_owned()),
                (
                    "auto_full_backup_include_tor_hidden_service_keys",
                    "true".to_owned(),
                ),
                ("auto_full_backup_storage_mode", "\"split_zip\"".to_owned()),
                ("auto_full_backup_split_zip_part_size_gib", "8".to_owned()),
            ],
            None,
        );

        assert!(
            output.starts_with("# RustChan settings.toml\n"),
            "the leading comment should be preserved"
        );
        for expected in [
            "forum_name = \"BackupChan\"\n",
            "site_subtitle = \"select board to proceed\"\n",
            "homepage_new_thread_badges_enabled = true\n",
            "homepage_new_reply_badges_enabled = true\n",
            "thread_new_reply_badges_enabled = true\n",
            "default_theme = \"terminal\"\n",
            "auto_full_backup_interval_hours = 12\n",
            "auto_full_backup_copies_to_keep = 3\n",
            "auto_full_backup_include_tor_hidden_service_keys = true\n",
            "auto_full_backup_storage_mode = \"split_zip\"\n",
            "auto_full_backup_split_zip_part_size_gib = 8\n",
        ] {
            assert!(
                output.contains(expected),
                "rewritten settings should contain {expected:?}"
            );
        }
        assert!(
            output.ends_with('\n'),
            "the trailing newline should be preserved"
        );
    }

    #[test]
    /// Inserts missing backup keys before the requested root-section anchor.
    fn rewrite_settings_file_lines_inserts_missing_root_keys_before_anchor_section() {
        let input = r#"# RustChan settings.toml
forum_name = "RustChan"

# ── Federation / ChanNet gateway ──────────────────────────────────────────────
[tls]
enabled = false
"#;

        let output = rewrite_settings_file_lines(
            input,
            &[
                ("auto_full_backup_interval_hours", "24".to_owned()),
                ("auto_full_backup_copies_to_keep", "1".to_owned()),
                (
                    "auto_full_backup_include_tor_hidden_service_keys",
                    "true".to_owned(),
                ),
                ("auto_full_backup_storage_mode", "\"directory\"".to_owned()),
                ("auto_full_backup_split_zip_part_size_gib", "4".to_owned()),
            ],
            Some("# ── Federation / ChanNet gateway"),
        );

        let positions = [
            output.find("auto_full_backup_interval_hours = 24"),
            output.find("auto_full_backup_copies_to_keep = 1"),
            output.find("auto_full_backup_include_tor_hidden_service_keys = true"),
            output.find("auto_full_backup_storage_mode = \"directory\""),
            output.find("auto_full_backup_split_zip_part_size_gib = 4"),
            output.find("# ── Federation / ChanNet gateway"),
            output.find("[tls]"),
        ];

        assert!(
            positions.iter().all(Option::is_some),
            "all inserted keys, the anchor, and the TLS section should be present"
        );
        assert!(
            positions
                .windows(2)
                .all(|pair| matches!(pair, [Some(left), Some(right)] if left < right)),
            "inserted backup keys should retain request order before the anchor and TLS section"
        );
    }

    #[test]
    /// Inserts a missing default theme before the network section.
    fn rewrite_settings_file_lines_inserts_missing_default_theme_before_network_section() {
        let input = r#"# RustChan settings.toml
forum_name = "RustChan"
site_subtitle = "select board to proceed"
homepage_new_thread_badges_enabled = true
homepage_new_reply_badges_enabled = true
thread_new_reply_badges_enabled = true

# ── Network / web server ──────────────────────────────────────────────────────
port = 8080
"#;

        let output = rewrite_settings_file_lines(
            input,
            &[
                ("forum_name", "\"NewChan\"".to_owned()),
                ("site_subtitle", "\"new subtitle\"".to_owned()),
                ("homepage_new_thread_badges_enabled", "false".to_owned()),
                ("homepage_new_reply_badges_enabled", "true".to_owned()),
                ("thread_new_reply_badges_enabled", "true".to_owned()),
                ("default_theme", "\"terminal\"".to_owned()),
            ],
            Some("# ── Network / web server"),
        );

        let positions = [
            output.find("homepage_new_thread_badges_enabled = false"),
            output.find("homepage_new_reply_badges_enabled = true"),
            output.find("thread_new_reply_badges_enabled = true"),
            output.find("default_theme = \"terminal\""),
            output.find("# ── Network / web server"),
        ];

        assert!(
            positions.iter().all(Option::is_some),
            "updated activity keys, the inserted theme, and the network section should be present"
        );
        assert!(
            positions
                .windows(2)
                .all(|pair| matches!(pair, [Some(left), Some(right)] if left < right)),
            "activity settings and the inserted theme should precede the network section"
        );
        assert!(
            output.contains("forum_name = \"NewChan\"\n"),
            "forum name should be updated"
        );
        assert!(
            output.contains("site_subtitle = \"new subtitle\"\n"),
            "site subtitle should be updated"
        );
    }

    #[test]
    /// Rejects an `FFmpeg` timeout below the supported minimum.
    fn validate_rejects_ffmpeg_timeout_below_minimum() {
        let error = validate_ffmpeg_timeout_secs(MIN_FFMPEG_TIMEOUT_SECS - 1)
            .err()
            .map(|error| error.to_string());
        let expected = format!(
            "CONFIG ERROR: ffmpeg_timeout_secs must be between {MIN_FFMPEG_TIMEOUT_SECS} and {MAX_FFMPEG_TIMEOUT_SECS} seconds."
        );
        assert_eq!(
            error.as_deref(),
            Some(expected.as_str()),
            "a below-minimum timeout should return the bounded-range error"
        );
    }

    #[test]
    /// Rejects an `FFmpeg` timeout above the supported maximum.
    fn validate_rejects_ffmpeg_timeout_above_maximum() {
        let error = validate_ffmpeg_timeout_secs(MAX_FFMPEG_TIMEOUT_SECS + 1)
            .err()
            .map(|error| error.to_string());
        let expected = format!(
            "CONFIG ERROR: ffmpeg_timeout_secs must be between {MIN_FFMPEG_TIMEOUT_SECS} and {MAX_FFMPEG_TIMEOUT_SECS} seconds."
        );
        assert_eq!(
            error.as_deref(),
            Some(expected.as_str()),
            "an above-maximum timeout should return the bounded-range error"
        );
    }

    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "assertion failures are the intended failure mechanism for this test"
    )]
    /// Persists a live timeout update and reloads the written value.
    fn update_settings_file_ffmpeg_timeout_persists_and_reloads() -> TestResult {
        let _settings_lock = lock_settings_file();
        let path = settings_file_path();
        let snapshot = SettingsFileSnapshot::capture(path.clone())?;
        let parent = path
            .parent()
            .context("settings path should have a parent directory")?;
        std::fs::create_dir_all(parent).context("could not create settings directory")?;
        std::fs::write(
            &path,
            format!(
                "forum_name = \"RustChan\"\nffmpeg_timeout_secs = {DEFAULT_FFMPEG_TIMEOUT_SECS}\n"
            ),
        )
        .context("could not write settings fixture")?;

        update_settings_file_ffmpeg_timeout(1_800);
        let updated =
            std::fs::read_to_string(&path).context("could not read updated settings file")?;
        assert!(
            updated.contains("ffmpeg_timeout_secs = 1800\n"),
            "the persisted settings should contain the updated timeout"
        );

        let reloaded = Config::from_env();
        assert_eq!(
            reloaded.ffmpeg_timeout_secs, 1_800,
            "configuration reload should observe the persisted timeout"
        );

        snapshot.restore()
    }

    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "assertion failures are the intended failure mechanism for this test"
    )]
    /// Persists the distinct homepage reply-badge toggle.
    fn update_settings_file_site_settings_persists_homepage_reply_badge_toggle() -> TestResult {
        let _settings_lock = lock_settings_file();
        let path = settings_file_path();
        let snapshot = SettingsFileSnapshot::capture(path.clone())?;
        let parent = path
            .parent()
            .context("settings path should have a parent directory")?;
        std::fs::create_dir_all(parent).context("could not create settings directory")?;
        std::fs::write(
            &path,
            "forum_name = \"RustChan\"\nsite_subtitle = \"select board to proceed\"\nhomepage_new_thread_badges_enabled = true\nhomepage_new_reply_badges_enabled = true\nthread_new_reply_badges_enabled = true\ndefault_theme = \"forest\"\n",
        )
        .context("could not write settings fixture")?;

        super::update_settings_file_site_settings(
            "RustChan",
            "select board to proceed",
            true,
            false,
            true,
            "forest",
        );
        let updated =
            std::fs::read_to_string(&path).context("could not read updated settings file")?;
        for expected in [
            "homepage_new_thread_badges_enabled = true\n",
            "homepage_new_reply_badges_enabled = false\n",
            "thread_new_reply_badges_enabled = true\n",
        ] {
            assert!(
                updated.contains(expected),
                "updated site settings should contain {expected:?}"
            );
        }

        snapshot.restore()
    }

    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "assertion failures are the intended failure mechanism for this test"
    )]
    /// Updates the live timeout and formats a mixed-minute duration.
    fn live_ffmpeg_timeout_setting_updates_and_formats_human_text() -> TestResult {
        let _snapshot = LiveFfmpegTimeoutSnapshot::capture();
        set_live_ffmpeg_timeout_secs(90).context("could not set live timeout")?;
        assert_eq!(
            ffmpeg_timeout_secs(),
            90,
            "live timeout should reflect the accepted update"
        );
        assert_eq!(
            describe_timeout_secs(90),
            "1 minute 30 seconds",
            "mixed-minute durations should use compact human text"
        );
        Ok(())
    }

    #[test]
    /// Rejects an HTTPS port that matches the primary HTTP listener.
    fn validate_rejects_tls_port_matching_main_http_port() {
        let mut config = valid_config();
        config.tls.enabled = true;
        config.tls.port = 8080;

        let error = validation_error(&config);

        assert_eq!(
            error.as_deref(),
            Some("CONFIG ERROR: tls.port (8080) must differ from the main HTTP port (8080)."),
            "matching primary and HTTPS ports should fail validation"
        );
    }

    #[test]
    /// Rejects a redirect port that matches the primary HTTP listener.
    fn validate_rejects_redirect_http_port_matching_main_http_port() {
        let mut config = valid_config();
        config.tls.enabled = true;
        config.tls.port = 8443;
        config.tls.redirect_http = true;
        config.tls.http_port = 8080;

        let error = validation_error(&config);

        assert_eq!(
            error.as_deref(),
            Some(
                "CONFIG ERROR: tls.http_port (8080) must differ from the main HTTP port (8080) when tls.redirect_http=true."
            ),
            "matching primary and redirect ports should fail validation"
        );
    }

    #[test]
    /// Rejects a redirect port that matches the HTTPS listener.
    fn validate_rejects_redirect_http_port_matching_tls_port() {
        let mut config = valid_config();
        config.tls.enabled = true;
        config.tls.port = 8443;
        config.tls.redirect_http = true;
        config.tls.http_port = 8443;

        let error = validation_error(&config);

        assert_eq!(
            error.as_deref(),
            Some(
                "CONFIG ERROR: tls.http_port (8443) must differ from tls.port (8443) when tls.redirect_http=true."
            ),
            "matching HTTPS and redirect ports should fail validation"
        );
    }

    #[test]
    /// Accepts distinct primary, HTTPS, and redirect ports.
    fn validate_accepts_distinct_tls_and_redirect_ports() -> TestResult {
        let mut config = valid_config();
        config.tls.enabled = true;
        config.tls.port = 8443;
        config.tls.redirect_http = true;
        config.tls.http_port = 8081;

        config
            .validate()
            .context("distinct TLS and redirect ports should validate")
    }

    #[test]
    /// Rejects an enabled `ChanNet` API key below the minimum length.
    fn validate_rejects_short_chan_net_api_key() {
        let mut config = valid_config();
        config.chan_net_api_key = "short-key".to_owned();

        let error = validation_error(&config);

        assert_eq!(
            error.as_deref(),
            Some(
                "CONFIG ERROR: chan_net_api_key must be empty to disable ChanNet auth-protected endpoints or at least 32 characters long."
            ),
            "a short ChanNet API key should fail validation"
        );
    }

    #[test]
    /// Accepts either a disabled or sufficiently long `ChanNet` API key.
    fn validate_accepts_empty_or_long_chan_net_api_key() -> TestResult {
        let mut config = valid_config();
        config.chan_net_api_key.clear();
        config
            .validate()
            .context("an empty key should disable protected endpoints")?;

        config.chan_net_api_key = "x".repeat(32);
        config
            .validate()
            .context("a 32-character key should be accepted")
    }

    #[test]
    /// Requires an API key only when `ChanNet` listens outside loopback.
    fn validate_chan_net_listener_requires_key_for_non_loopback_bind() {
        let mut config = valid_config();
        config.chan_net_api_key.clear();
        config.chan_net_bind = "0.0.0.0:7070".to_owned();

        let error = config
            .validate_chan_net_listener()
            .err()
            .map(|error| error.to_string());
        assert_eq!(
            error.as_deref(),
            Some(
                "CONFIG ERROR: --chan-net with chan_net_bind '0.0.0.0:7070' requires chan_net_api_key when binding outside loopback."
            ),
            "a non-loopback listener without a key should fail validation"
        );

        config.chan_net_bind = "127.0.0.1:7070".to_owned();
        assert!(
            config.validate_chan_net_listener().is_ok(),
            "an IPv4 loopback listener may disable protected endpoints"
        );

        config.chan_net_bind = "localhost:7070".to_owned();
        assert!(
            config.validate_chan_net_listener().is_ok(),
            "a localhost listener may disable protected endpoints"
        );

        config.chan_net_bind = "0.0.0.0:7070".to_owned();
        config.chan_net_api_key = "x".repeat(32);
        assert!(
            config.validate_chan_net_listener().is_ok(),
            "a non-loopback listener with a sufficiently long key should validate"
        );
    }

    #[cfg(unix)]
    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "assertion failures are the intended failure mechanism for this test"
    )]
    /// Applies owner-only modes to private directories and files on Unix.
    fn private_file_helpers_set_owner_only_modes() -> TestResult {
        use std::os::unix::fs::PermissionsExt as _;

        let temp_dir = tempfile::tempdir().context("could not create temporary directory")?;
        let private_dir = temp_dir.path().join("private");
        super::ensure_private_dir(&private_dir).context("could not create private directory")?;
        assert_eq!(
            std::fs::metadata(&private_dir)
                .context("could not read private-directory metadata")?
                .permissions()
                .mode()
                & 0o777,
            0o700,
            "private directories should be owner-accessible only"
        );

        let private_file = private_dir.join("settings.toml");
        super::write_private_file(&private_file, b"secret")
            .context("could not write private file")?;
        assert_eq!(
            std::fs::metadata(&private_file)
                .context("could not read private-file metadata")?
                .permissions()
                .mode()
                & 0o777,
            0o600,
            "private files should be owner-readable and owner-writable only"
        );
        Ok(())
    }

    #[test]
    /// Rejects Tor-only mode when the onion service is disabled.
    fn validate_rejects_tor_only_without_tor_support() {
        let mut config = valid_config();
        config.enable_tor_support = false;
        config.tor_only = true;

        let error = validation_error(&config);

        assert_eq!(
            error.as_deref(),
            Some(
                "CONFIG ERROR: tor_only=true requires enable_tor_support=true. Tor-only mode needs the built-in onion service to be active."
            ),
            "Tor-only mode without Tor support should fail validation"
        );
    }

    #[test]
    /// Keeps plaintext application access enabled for pre-setting TLS configurations.
    fn tls_config_without_require_https_defaults_to_optional_https() -> TestResult {
        let parsed = super::parse_settings_file_str(
            "[tls]\nenabled = true\nport = 8443\nredirect_http = false\nhttp_port = 8081\n",
        )
        .context("could not parse legacy TLS settings")?;
        let tls = parsed.tls.context("TLS settings should be present")?;

        anyhow::ensure!(
            !tls.require_https,
            "omitting tls.require_https must preserve the plaintext listener"
        );
        Ok(())
    }

    #[test]
    /// Renders the expected first-run defaults and featured-theme order.
    fn settings_template_uses_forest_and_featured_theme_order() {
        let template = settings_template("secret");

        for expected in [
            "homepage_new_thread_badges_enabled = true",
            "homepage_new_reply_badges_enabled = true",
            "thread_new_reply_badges_enabled = true",
            "media_auto_prune_enabled = false",
            "media_max_active_content_size_bytes = 0",
            r#"default_theme = "forest""#,
            "require_https = false",
            "auto_full_backup_include_tor_hidden_service_keys = true",
            r#"auto_full_backup_storage_mode = "directory""#,
            "auto_full_backup_split_zip_part_size_gib = 4",
            "public_readiness_details = false",
            "public_metrics_enabled = false",
            r#"enabled_builtin_themes = ["forest", "blue-sky", "deep-orbit", "terminal", "dorfic", "chanclassic", "aero", "neoncubicle", "fluorogrid"]"#,
        ] {
            assert!(
                template.contains(expected),
                "generated settings should contain {expected:?}"
            );
        }
    }

    #[test]
    /// Explains that enabled built-ins seed only the first-start theme catalog.
    fn settings_template_marks_enabled_builtins_as_first_start_seeded() {
        let template = settings_template("secret");

        assert!(
            template.contains("# Built-in themes enabled when the theme catalog is first seeded."),
            "template should explain when the enabled built-ins are seeded"
        );
        assert!(
            template.contains(
                "# After first startup, Admin -> Theme Catalog owns the live enabled/disabled state."
            ),
            "template should explain post-startup theme ownership"
        );
    }

    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "assertion failures are the intended failure mechanism for this test"
    )]
    /// Round-trips generated root and TLS settings through parsing and reload.
    fn generated_settings_template_round_trips_root_and_tls_values() -> TestResult {
        let _settings_lock = lock_settings_file();
        let path = settings_file_path();
        let snapshot = SettingsFileSnapshot::capture(path.clone())?;
        let parent = path
            .parent()
            .context("settings path should have a parent directory")?;
        let secret = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".to_owned();
        let template = settings_template(&secret);

        std::fs::create_dir_all(parent).context("could not create settings directory")?;
        std::fs::write(&path, &template).context("could not write generated settings template")?;

        let parsed = super::parse_settings_file_str(&template)
            .context("could not parse generated settings template")?;
        assert_eq!(
            parsed.cookie_secret.as_deref(),
            Some(secret.as_str()),
            "generated cookie secret should parse unchanged"
        );
        assert_eq!(
            parsed.enable_tor_support,
            Some(true),
            "generated settings should enable Tor support"
        );
        assert_eq!(
            parsed.tor_only,
            Some(false),
            "generated settings should retain clearnet access"
        );
        assert_eq!(
            parsed.public_readiness_details,
            Some(false),
            "generated settings should keep readiness details private"
        );
        assert_eq!(
            parsed.public_metrics_enabled,
            Some(false),
            "generated settings should keep metrics private"
        );
        assert!(
            parsed.public_hosts.is_none(),
            "commented public hosts should not deserialize"
        );
        assert_eq!(
            parsed.tls.as_ref().map(|tls| tls.enabled),
            Some(false),
            "generated settings should disable TLS by default"
        );
        assert_eq!(
            parsed.tls.as_ref().map(|tls| tls.port),
            Some(8443),
            "generated settings should retain the default HTTPS port"
        );
        assert_eq!(
            parsed.tls.as_ref().map(|tls| tls.redirect_http),
            Some(false),
            "generated settings should disable HTTP redirection by default"
        );
        assert!(
            template.contains("runtime/tor/state/keystore/"),
            "template should document the Arti-native keystore path"
        );

        let reloaded = Config::from_env();
        assert_eq!(
            reloaded.cookie_secret, secret,
            "runtime reload should retain the generated cookie secret"
        );
        assert!(
            reloaded.enable_tor_support,
            "runtime reload should enable Tor support"
        );
        assert!(
            !reloaded.tor_only,
            "runtime reload should retain clearnet access"
        );
        assert!(
            !reloaded.public_readiness_details,
            "runtime reload should keep readiness details private"
        );
        assert!(
            !reloaded.public_metrics_enabled,
            "runtime reload should keep metrics private"
        );
        assert!(
            reloaded.public_hosts.is_empty(),
            "runtime reload should not add commented public hosts"
        );
        assert!(
            !reloaded.tls.enabled,
            "runtime reload should keep TLS disabled"
        );
        assert_eq!(
            reloaded.tls.port, 8443,
            "runtime reload should retain the HTTPS port"
        );
        assert!(
            !reloaded.tls.redirect_http,
            "runtime reload should keep HTTP redirection disabled"
        );

        snapshot.restore()
    }
}
