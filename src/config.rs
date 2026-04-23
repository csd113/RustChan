// Runtime configuration and settings-file loading.
use rand_core::{OsRng, RngCore};
use serde::Deserialize;
use std::env;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

mod template;

/// Absolute path to the directory the running binary lives in.
fn binary_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(std::path::Path::to_path_buf))
        .unwrap_or_else(|| {
            eprintln!(
                "Warning: could not determine binary directory; \
                 using current working directory for data storage. \
                 Set CHAN_DB and CHAN_UPLOADS env vars to override."
            );
            PathBuf::from(".")
        })
}

fn settings_file_path() -> PathBuf {
    // Resolve settings.toml next to the running executable, inside
    // <exe-dir>/rustchan-data/. This is the config source of truth for the live
    // process; `CHAN_*` environment variables still override selected fields
    // after the file is loaded.
    // rustchan-data/ is created by run_server before CONFIG is first accessed,
    // so this directory always exists by the time settings are read.
    let data_dir = binary_dir().join("rustchan-data");
    data_dir.join("settings.toml")
}

#[must_use]
pub fn data_dir() -> PathBuf {
    binary_dir().join("rustchan-data")
}

#[must_use]
pub fn runtime_dir() -> PathBuf {
    data_dir().join("runtime")
}

#[must_use]
pub fn logs_dir() -> PathBuf {
    data_dir().join("logs")
}

#[must_use]
pub fn backups_dir() -> PathBuf {
    data_dir().join("backups")
}

#[must_use]
pub fn full_backups_dir() -> PathBuf {
    backups_dir().join("full")
}

#[must_use]
pub fn board_backups_dir() -> PathBuf {
    backups_dir().join("boards")
}

#[must_use]
pub fn runtime_tmp_dir() -> PathBuf {
    runtime_dir().join("tmp")
}

#[must_use]
pub fn runtime_temp_board_downloads_dir() -> PathBuf {
    runtime_tmp_dir().join("board-downloads")
}

#[must_use]
pub fn runtime_tor_dir() -> PathBuf {
    runtime_dir().join("tor")
}

#[must_use]
pub fn runtime_tor_state_dir() -> PathBuf {
    runtime_tor_dir().join("state")
}

#[must_use]
pub fn runtime_tor_hidden_service_keys_dir() -> PathBuf {
    runtime_tor_state_dir().join("keystore")
}

#[must_use]
pub fn configured_tor_hidden_service_keys_dir() -> Option<PathBuf> {
    CONFIG
        .enable_tor_support
        .then(runtime_tor_hidden_service_keys_dir)
}

#[must_use]
pub fn runtime_tor_cache_dir() -> PathBuf {
    runtime_tor_dir().join("cache")
}

#[must_use]
pub fn runtime_tls_dir() -> PathBuf {
    runtime_dir().join("tls")
}

#[must_use]
pub fn runtime_favicon_dir() -> PathBuf {
    runtime_dir().join("favicon")
}

#[must_use]
pub fn runtime_banner_dir() -> PathBuf {
    runtime_dir().join("banner")
}

type RuntimeDirMigration = (&'static str, fn() -> PathBuf);

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
        std::fs::create_dir_all(parent)?;
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
    std::fs::create_dir_all(&data_dir)?;

    for &(legacy_name, destination) in RUNTIME_LAYOUT_MIGRATIONS {
        migrate_dir_if_present(&data_dir.join(legacy_name), &destination())?;
    }

    Ok(())
}

// ─── Settings file structure ──────────────────────────────────────────────────
#[derive(Deserialize, Default)]
struct SettingsFile {
    forum_name: Option<String>,
    /// Home page subtitle shown below the site name.
    site_subtitle: Option<String>,
    /// Default theme served to first-time visitors before they pick one.
    /// Valid values include built-ins and admin-created custom theme slugs.
    default_theme: Option<String>,
    /// Built-in theme whitelist applied when seeding the themes table.
    enabled_builtin_themes: Option<Vec<String>>,
    port: Option<u16>,
    max_image_size_mb: Option<u32>,
    max_video_size_mb: Option<u32>,
    max_audio_size_mb: Option<u32>,
    cookie_secret: Option<String>,
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
    require_ffmpeg: Option<bool>,
    ffmpeg_path: Option<String>,
    ffprobe_path: Option<String>,
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
    /// run before it is killed. Default: 120.
    ffmpeg_timeout_secs: Option<u64>,
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

fn load_settings_file() -> SettingsFile {
    let path = settings_file_path();
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return SettingsFile::default();
    };
    toml::from_str(&raw).unwrap_or_else(|e| {
        eprintln!("Warning: could not parse settings.toml: {e}");
        SettingsFile::default()
    })
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
        return;
    }
    // Generate a random 64-hex-char secret (32 bytes of entropy).
    let mut secret_bytes = [0u8; 32];
    OsRng.fill_bytes(&mut secret_bytes);
    let secret = hex::encode(secret_bytes);
    let content = template::settings_template(&secret);
    match std::fs::write(&path, content) {
        Ok(()) => println!("Created settings.toml ({})", path.display()),
        Err(e) => eprintln!("Warning: could not write settings.toml: {e}"),
    }
}

// ─── TLS configuration ───────────────────────────────────────────────────────
#[derive(Debug, Clone, serde::Deserialize)]
pub struct TlsConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_https_port")]
    pub port: u16,
    #[serde(default)]
    pub redirect_http: bool,
    #[serde(default = "default_http_port")]
    pub http_port: u16,
    #[serde(default)]
    pub acme: AcmeConfig,
    pub manual_cert: Option<ManualCertConfig>,
}

impl Default for TlsConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            port: default_https_port(),
            redirect_http: false,
            http_port: default_http_port(),
            acme: AcmeConfig::default(),
            manual_cert: None,
        }
    }
}

#[derive(Debug, Clone, serde::Deserialize, Default)]
pub struct AcmeConfig {
    #[serde(default)]
    pub enabled: bool,

    // These fields are only read when the `tls-acme` Cargo feature is enabled
    // (the Let's Encrypt implementation lives in a separate module).
    // They are intentionally kept here so the `[tls.acme]` section in
    // settings.toml deserializes cleanly even when the feature is off.
    // The dead-code allow exists because the config shape is stable even when
    // this build cannot act on the fields.
    #[serde(default)]
    // Feature-gated, but still part of the stable settings shape.
    #[allow(dead_code)]
    pub domains: Vec<String>,
    #[serde(default)]
    // Feature-gated, but still part of the stable settings shape.
    #[allow(dead_code)]
    pub email: Option<String>,
    #[serde(default = "default_true")]
    // Feature-gated, but still part of the stable settings shape.
    #[allow(dead_code)]
    pub staging: bool,
    #[serde(default = "default_acme_dir")]
    // Feature-gated, but still part of the stable settings shape.
    #[allow(dead_code)]
    pub cache_dir: String,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct ManualCertConfig {
    pub cert_path: String,
    pub key_path: String,
}

const fn default_https_port() -> u16 {
    8443
}

const fn default_http_port() -> u16 {
    8080
}

const fn default_true() -> bool {
    true
}

fn default_acme_dir() -> String {
    "runtime/tls/acme".into()
}

// ─── Runtime config ───────────────────────────────────────────────────────────
pub static CONFIG: LazyLock<Config> = LazyLock::new(Config::from_env);

// This type mirrors serialized or render state, so the boolean count is an intentional tradeoff.
#[allow(clippy::struct_excessive_bools)]
pub struct Config {
    // ── Loaded from settings.toml (env vars still override) ──────────────────
    pub forum_name: String,
    /// Initial subtitle shown on the home page; seeds the DB on first run and
    /// then the Admin -> Site Settings DB value becomes the live source of truth.
    pub initial_site_subtitle: String,
    /// Initial default theme slug; seeds the DB on first run and later the
    /// Admin -> Site Settings DB value becomes the live source of truth.
    /// Valid: built-in or custom theme slug present in the themes table.
    pub initial_default_theme: String,
    /// Built-in themes enabled by default when the site seeds its theme catalog.
    /// After seeding, the theme catalog in the DB owns the enabled/disabled set.
    pub initial_enabled_builtin_themes: Vec<String>,
    pub port: u16,
    pub max_image_size: usize, // bytes
    pub max_video_size: usize, // bytes
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
    pub bind_addr: String,
    pub database_path: String,
    pub upload_dir: String,
    pub thumb_size: u32,
    /// Maximum GET requests per IP per `rate_limit_window`.
    pub rate_limit_gets: u32,
    pub rate_limit_window: u64,
    pub cookie_secret: String,
    pub session_duration: i64,
    pub behind_proxy: bool,
    /// Trusted proxy CIDR allowlist for forwarding headers.
    pub trusted_proxy_cidrs: Vec<String>,
    pub https_cookies: bool,
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
    /// Interval in hours between expired poll vote cleanup runs. 0 = disabled.
    pub poll_cleanup_interval_hours: u64,
    /// DB file size threshold in bytes above which admin panel shows a warning.
    /// 0 = disabled.
    pub db_warn_threshold_bytes: u64,
    /// Maximum number of pending jobs before new ones are dropped.
    pub job_queue_capacity: u64,
    /// Maximum seconds a single `FFmpeg` job may run before being killed.
    pub ffmpeg_timeout_secs: u64,
    /// When true, threads are always archived (never hard-deleted) on prune,
    /// overriding individual board settings.
    pub archive_before_prune: bool,
    /// Total thumbnail/waveform cache size limit in bytes. 0 = disabled.
    pub waveform_cache_max_bytes: u64,
    /// Number of threads in Tokio's blocking pool. Default: logical CPUs × 4.
    pub blocking_threads: usize,
    /// `SQLite` `r2d2` connection pool size (default 8).
    pub db_pool_size: u32,
    // ── ChanNet / RustWave gateway (Step 1.2) ────────────────────────────────
    /// Base URL of the connected `RustWave` instance (must begin with http:// or https://).
    /// Validated at startup by `Config::validate()`.
    pub rustwave_url: String,
    /// Address to bind the second `ChanNet` TCP listener (default 127.0.0.1:7070).
    /// Only used when the server is started with `--chan-net`.
    pub chan_net_bind: String,
    /// Maximum request body size for `/chan/import` (ZIP snapshots). Default: 10 MiB.
    pub chan_net_max_body: usize,
    /// Maximum request body size for `/chan/command` (raw JSON). Default: 8 KiB.
    pub chan_net_command_max_body: usize,
    /// Pre-shared key required on X-ChanNet-Key header for /chan/refresh and
    /// /chan/poll. An empty string means those endpoints are disabled entirely.
    pub chan_net_api_key: String,
    // ── TLS / HTTPS ───────────────────────────────────────────────────────────
    /// TLS configuration. Defaults to disabled so existing installs are unaffected.
    pub tls: TlsConfig,
}

impl Config {
    #[must_use]
    // This function/module is intentionally long; splitting it further would make the routing or template flow harder to follow.
    #[allow(clippy::too_many_lines)]
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
        let initial_default_theme = env_str(
            "CHAN_DEFAULT_THEME",
            s.default_theme
                .as_deref()
                .unwrap_or(crate::theme::HARD_DEFAULT_THEME),
        );
        let initial_enabled_builtin_themes = s.enabled_builtin_themes.unwrap_or_else(|| {
            crate::theme::builtin_theme_slugs()
                .into_iter()
                .map(str::to_string)
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
        let behind_proxy = env_bool("CHAN_BEHIND_PROXY", false);
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
            eprintln!(
                "SECURITY WARNING: No cookie_secret found in environment or settings.toml. \
                 IP hashing is using an empty secret. Run the server once to auto-generate, \
                 or set CHAN_COOKIE_SECRET."
            );
            // Random in-memory secret so each restart invalidates hashes
            // (better than a known empty string, worse than a persisted one).
            let mut b = [0u8; 32];
            OsRng.fill_bytes(&mut b);
            hex::encode(b)
        };
        // ── ChanNet fields (Step 1.2) — computed after cookie_secret ──────────
        // Use as_deref() to borrow rather than move the Option<String> fields.
        let rustwave_url = env::var("CHAN_RUSTWAVE_URL").unwrap_or_else(|_| {
            s.rustwave_url
                .as_deref()
                .unwrap_or("http://localhost:7071")
                .to_string()
        });
        let chan_net_bind = env::var("CHAN_NET_BIND").unwrap_or_else(|_| {
            s.chan_net_bind
                .as_deref()
                .unwrap_or("127.0.0.1:7070")
                .to_string()
        });
        let chan_net_max_body: usize = env::var("CHAN_NET_MAX_BODY")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(10 * 1024 * 1024); // 10 MiB default
        let chan_net_command_max_body: usize = env::var("CHAN_NET_COMMAND_MAX_BODY")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(8 * 1024); // 8 KiB default — commands are raw JSON, never ZIPs
        Self {
            forum_name,
            initial_site_subtitle,
            initial_default_theme,
            initial_enabled_builtin_themes,
            port,
            max_image_size: (max_image_mb as usize)
                .saturating_mul(1024)
                .saturating_mul(1024),
            max_video_size: (max_video_mb as usize)
                .saturating_mul(1024)
                .saturating_mul(1024),
            max_audio_size: (max_audio_mb as usize)
                .saturating_mul(1024)
                .saturating_mul(1024),
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
            tor_service_nickname: std::env::var("CHAN_TOR_NICKNAME")
                .ok()
                .or(s.tor_service_nickname)
                .unwrap_or_else(|| "rustchan".to_string()),
            require_ffmpeg: env_bool("CHAN_REQUIRE_FFMPEG", s.require_ffmpeg.unwrap_or(false)),
            ffmpeg_path: env::var("CHAN_FFMPEG_PATH")
                .ok()
                .or(s.ffmpeg_path)
                .unwrap_or_else(|| "ffmpeg".to_string()),
            ffprobe_path: env::var("CHAN_FFPROBE_PATH")
                .ok()
                .or(s.ffprobe_path)
                .unwrap_or_else(|| "ffprobe".to_string()),
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
            https_cookies: env_bool("CHAN_HTTPS_COOKIES", https_cookies_default),
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
                s.ffmpeg_timeout_secs.unwrap_or(120),
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
            chan_net_api_key: std::env::var("CHAN_NET_API_KEY")
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
    #[allow(clippy::too_many_lines)]
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
        let upload_path = std::path::Path::new(&self.upload_dir);
        if upload_path.exists() {
            let probe = upload_path.join(".write_probe");
            if std::fs::write(&probe, b"").is_err() {
                anyhow::bail!(
                    "CONFIG ERROR: upload_dir '{}' is not writable.",
                    self.upload_dir
                );
            }
            let _ = std::fs::remove_file(probe);
        }
        // F-13: Pre-flight writability check for Arti data directories.
        // Without this, a permissions error on these dirs only surfaces ~30 s
        // into bootstrap as a cryptic internal error — invisible at startup.
        if self.enable_tor_support {
            for dir in [runtime_tor_state_dir(), runtime_tor_cache_dir()] {
                std::fs::create_dir_all(&dir).map_err(|e| {
                    anyhow::anyhow!("CONFIG ERROR: cannot create Tor dir {}: {e}", dir.display())
                })?;
                // Arti requires runtime/tor/state/ to have permissions 0700 (no group
                // or other read access) for its key material. create_dir_all
                // respects the process umask, typically yielding 0755, which
                // Arti rejects with "problem with filesystem permissions".
                // Explicitly set 0700 on Unix so Arti accepts the directory.
                // runtime/tor/cache/ holds no sensitive data and is left at normal
                // permissions, but we restrict it too for defence-in-depth.
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let perms = std::fs::Permissions::from_mode(0o700);
                    std::fs::set_permissions(&dir, perms).map_err(|e| {
                        anyhow::anyhow!(
                            "CONFIG ERROR: cannot set permissions on Tor dir {}: {e}",
                            dir.display()
                        )
                    })?;
                }
                let probe = dir.join(".write_probe");
                std::fs::write(&probe, b"").map_err(|_| {
                    anyhow::anyhow!(
                        "CONFIG ERROR: Tor dir {} is not writable — check permissions",
                        dir.display()
                    )
                })?;
                let _ = std::fs::remove_file(probe);
            }
        }
        // Step 1.2: Validate rustwave_url scheme so operators catch
        // misconfiguration at startup rather than at first federation call.
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
        Ok(())
    }

    #[must_use]
    pub fn bind_addr_with_port(&self, port: u16) -> String {
        bind_addr_for_port(&self.bind_addr, port)
    }

    #[must_use]
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
fn toml_quote(s: &str) -> String {
    let inner = s.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{inner}\"")
}

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
            line.to_string()
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

fn update_settings_file_entries(updates: &[(&str, String)], insert_missing_before: Option<&str>) {
    // Escape backslash and double-quote, then wrap in double quotes.
    let path = settings_file_path();
    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(
                target: "config",
                path = %path.display(),
                error = %e,
                "Could not read settings.toml for update"
            );
            return;
        }
    };
    // Replace the value portion of `key = ...` lines while preserving file
    // order and unrelated comments.
    let out = rewrite_settings_file_lines(&content, updates, insert_missing_before);
    // Atomic write: write to a temp file in the same directory, then rename
    // over the target. This prevents a partial write from corrupting settings.toml
    // if the process is killed mid-write (rename(2) is atomic on POSIX).
    let dir = path.parent().unwrap_or_else(|| std::path::Path::new("."));
    match tempfile::Builder::new()
        .prefix(".settings_")
        .suffix(".tmp")
        .tempfile_in(dir)
    {
        Err(e) => {
            tracing::warn!(
                target: "config",
                path = %path.display(),
                error = %e,
                "Could not create temp file for settings.toml update"
            );
        }
        Ok(mut tmp) => {
            use std::io::Write as _;
            let write_result = tmp
                .write_all(out.as_bytes())
                .and_then(|()| tmp.as_file().sync_all());
            if let Err(e) = write_result {
                tracing::warn!(
                    target: "config",
                    path = %path.display(),
                    error = %e,
                    "Could not write settings.toml temp file"
                );
            } else if let Err(e) = tmp.persist(&path) {
                tracing::warn!(
                    target: "config",
                    path = %path.display(),
                    error = %e.error,
                    "Could not atomically replace settings.toml"
                );
            }
        }
    }
}

pub fn update_settings_file_site_settings(
    forum_name: &str,
    site_subtitle: &str,
    default_theme: &str,
) {
    update_settings_file_entries(
        &[
            ("forum_name", toml_quote(forum_name)),
            ("site_subtitle", toml_quote(site_subtitle)),
            ("default_theme", toml_quote(default_theme)),
        ],
        Some("# ── Network / web server"),
    );
}

pub fn update_settings_file_auto_full_backup(
    interval_hours: u64,
    copies_to_keep: u64,
    include_tor_hidden_service_keys: bool,
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
        ],
        Some("# ── Federation / ChanNet gateway"),
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
    use sha2::{Digest, Sha256};
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
    if let Some(ref h) = stored {
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
    let _ = conn.execute(
        "INSERT INTO site_settings (key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        rusqlite::params![KEY, current_hash],
    );
}

// ─── Env helpers ──────────────────────────────────────────────────────────────
fn env_str(key: &str, default: &str) -> String {
    env::var(key).unwrap_or_else(|_| default.to_string())
}

fn env_parse<T: std::str::FromStr>(key: &str, default: T) -> T {
    env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn env_bool(key: &str, default: bool) -> bool {
    env::var(key).map_or(default, |v| v == "1" || v.eq_ignore_ascii_case("true"))
}

fn env_list(key: &str, file_value: Option<Vec<String>>, default: &[&str]) -> Vec<String> {
    env::var(key)
        .ok()
        .map(|value| split_list(&value))
        .or(file_value)
        .unwrap_or_else(|| default.iter().map(|value| (*value).to_string()).collect())
}

fn split_list(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .collect()
}

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
        return Some(unbracketed.to_string());
    }

    if unbracketed.contains(':') || unbracketed.contains(char::is_whitespace) {
        return None;
    }

    Some(unbracketed.to_string())
}

fn split_bind_addr(addr: &str) -> Option<(&str, &str)> {
    if let Some(rest) = addr.strip_prefix('[') {
        let (host, port) = rest.split_once("]:")?;
        Some((host, port))
    } else {
        addr.rsplit_once(':')
    }
}

fn bind_host_for_family(addr: &str) -> &str {
    split_bind_addr(addr).map_or("0.0.0.0", |(host, _)| host)
}

fn host_is_ipv6(host: &str) -> bool {
    host.contains(':')
}

fn format_bind_addr(host: &str, port: u16) -> String {
    if host_is_ipv6(host) {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

fn bind_addr_for_port(addr: &str, port: u16) -> String {
    format_bind_addr(bind_host_for_family(addr), port)
}

fn loopback_addr_for_family(addr: &str, port: u16) -> String {
    let host = if host_is_ipv6(bind_host_for_family(addr)) {
        "::1"
    } else {
        "127.0.0.1"
    };
    format_bind_addr(host, port)
}

fn port_from_bind_addr(addr: &str) -> Option<u16> {
    split_bind_addr(addr).and_then(|(_, port)| port.parse().ok())
}

#[cfg(test)]
mod tests {
    use super::{
        rewrite_settings_file_lines, runtime_tor_hidden_service_keys_dir,
        template::settings_template, Config, TlsConfig,
    };

    fn valid_config() -> Config {
        const MIB: usize = 1024 * 1024;
        Config {
            forum_name: "RustChan".to_string(),
            initial_site_subtitle: "select board to proceed".to_string(),
            initial_default_theme: crate::theme::HARD_DEFAULT_THEME.to_string(),
            initial_enabled_builtin_themes: crate::theme::builtin_theme_slugs()
                .into_iter()
                .map(str::to_string)
                .collect(),
            port: 8080,
            max_image_size: 8 * MIB,
            max_video_size: 50 * MIB,
            max_audio_size: 150 * MIB,
            enable_tor_support: false,
            tor_only: false,
            tor_bootstrap_timeout_secs: 120,
            tor_max_concurrent_streams: 512,
            tor_service_nickname: "rustchan".to_string(),
            require_ffmpeg: false,
            ffmpeg_path: "ffmpeg".to_string(),
            ffprobe_path: "ffprobe".to_string(),
            enable_any_file_uploads_feature: false,
            bind_addr: "0.0.0.0:8080".to_string(),
            database_path: "chan.db".to_string(),
            upload_dir: "__rustchan_test_uploads_does_not_exist__".to_string(),
            thumb_size: 250,
            rate_limit_gets: 60,
            rate_limit_window: 60,
            cookie_secret: "a".repeat(64),
            session_duration: 8 * 3600,
            behind_proxy: false,
            trusted_proxy_cidrs: vec!["127.0.0.1/32".to_string(), "::1/128".to_string()],
            https_cookies: false,
            public_hosts: Vec::new(),
            wal_checkpoint_interval: 3600,
            auto_vacuum_interval_hours: 24,
            auto_full_backup_interval_hours: 24,
            auto_full_backup_copies_to_keep: 1,
            auto_full_backup_include_tor_hidden_service_keys: false,
            poll_cleanup_interval_hours: 72,
            db_warn_threshold_bytes: 2048 * MIB as u64,
            job_queue_capacity: 1000,
            ffmpeg_timeout_secs: 120,
            archive_before_prune: true,
            waveform_cache_max_bytes: 200 * MIB as u64,
            blocking_threads: 4,
            db_pool_size: 8,
            rustwave_url: "http://localhost:7071".to_string(),
            chan_net_bind: "127.0.0.1:7070".to_string(),
            chan_net_max_body: 10 * MIB,
            chan_net_command_max_body: 8 * 1024,
            chan_net_api_key: String::new(),
            tls: TlsConfig::default(),
        }
    }

    fn validation_error(config: &Config) -> String {
        config
            .validate()
            .expect_err("config should fail validation")
            .to_string()
    }

    #[test]
    fn tor_hidden_service_keys_dir_matches_arti_native_keystore_location() {
        assert!(runtime_tor_hidden_service_keys_dir().ends_with("runtime/tor/state/keystore"));
    }

    #[test]
    fn rewrite_settings_file_lines_updates_requested_keys_and_preserves_comments() {
        let input = r#"# RustChan settings.toml
forum_name = "RustChan"
site_subtitle = "select board to proceed"
default_theme = "forest"
auto_full_backup_interval_hours = 24
auto_full_backup_copies_to_keep = 1
"#;

        let output = rewrite_settings_file_lines(
            input,
            &[
                ("forum_name", "\"BackupChan\"".to_string()),
                ("default_theme", "\"terminal\"".to_string()),
                ("auto_full_backup_interval_hours", "12".to_string()),
                ("auto_full_backup_copies_to_keep", "3".to_string()),
                (
                    "auto_full_backup_include_tor_hidden_service_keys",
                    "true".to_string(),
                ),
            ],
            None,
        );

        assert!(output.starts_with("# RustChan settings.toml\n"));
        assert!(output.contains("forum_name = \"BackupChan\"\n"));
        assert!(output.contains("site_subtitle = \"select board to proceed\"\n"));
        assert!(output.contains("default_theme = \"terminal\"\n"));
        assert!(output.contains("auto_full_backup_interval_hours = 12\n"));
        assert!(output.contains("auto_full_backup_copies_to_keep = 3\n"));
        assert!(output.contains("auto_full_backup_include_tor_hidden_service_keys = true\n"));
        assert!(output.ends_with('\n'));
    }

    #[test]
    fn rewrite_settings_file_lines_inserts_missing_root_keys_before_anchor_section() {
        let input = r#"# RustChan settings.toml
forum_name = "RustChan"

# ── Federation / ChanNet gateway ──────────────────────────────────────────────
[tls]
enabled = true
"#;

        let output = rewrite_settings_file_lines(
            input,
            &[
                ("auto_full_backup_interval_hours", "24".to_string()),
                ("auto_full_backup_copies_to_keep", "1".to_string()),
                (
                    "auto_full_backup_include_tor_hidden_service_keys",
                    "false".to_string(),
                ),
            ],
            Some("# ── Federation / ChanNet gateway"),
        );

        let backup_hours_idx = output
            .find("auto_full_backup_interval_hours = 24")
            .expect("backup hours key inserted");
        let backup_copies_idx = output
            .find("auto_full_backup_copies_to_keep = 1")
            .expect("backup copies key inserted");
        let backup_tor_idx = output
            .find("auto_full_backup_include_tor_hidden_service_keys = false")
            .expect("backup Tor key option inserted");
        let anchor_idx = output
            .find("# ── Federation / ChanNet gateway")
            .expect("anchor comment present");
        let tls_idx = output.find("[tls]").expect("tls section present");

        assert!(backup_hours_idx < anchor_idx);
        assert!(backup_copies_idx < anchor_idx);
        assert!(backup_tor_idx < anchor_idx);
        assert!(anchor_idx < tls_idx);
    }

    #[test]
    fn rewrite_settings_file_lines_inserts_missing_default_theme_before_network_section() {
        let input = r#"# RustChan settings.toml
forum_name = "RustChan"
site_subtitle = "select board to proceed"

# ── Network / web server ──────────────────────────────────────────────────────
port = 8080
"#;

        let output = rewrite_settings_file_lines(
            input,
            &[
                ("forum_name", "\"NewChan\"".to_string()),
                ("site_subtitle", "\"new subtitle\"".to_string()),
                ("default_theme", "\"terminal\"".to_string()),
            ],
            Some("# ── Network / web server"),
        );

        let theme_idx = output
            .find("default_theme = \"terminal\"")
            .expect("default_theme inserted");
        let network_idx = output
            .find("# ── Network / web server")
            .expect("network section present");

        assert!(theme_idx < network_idx);
        assert!(output.contains("forum_name = \"NewChan\"\n"));
        assert!(output.contains("site_subtitle = \"new subtitle\"\n"));
    }

    #[test]
    fn validate_rejects_tls_port_matching_main_http_port() {
        let mut config = valid_config();
        config.tls.enabled = true;
        config.tls.port = 8080;

        let error = validation_error(&config);

        assert_eq!(
            error,
            "CONFIG ERROR: tls.port (8080) must differ from the main HTTP port (8080)."
        );
    }

    #[test]
    fn validate_rejects_redirect_http_port_matching_main_http_port() {
        let mut config = valid_config();
        config.tls.enabled = true;
        config.tls.port = 8443;
        config.tls.redirect_http = true;
        config.tls.http_port = 8080;

        let error = validation_error(&config);

        assert_eq!(
            error,
            "CONFIG ERROR: tls.http_port (8080) must differ from the main HTTP port (8080) when tls.redirect_http=true."
        );
    }

    #[test]
    fn validate_rejects_redirect_http_port_matching_tls_port() {
        let mut config = valid_config();
        config.tls.enabled = true;
        config.tls.port = 8443;
        config.tls.redirect_http = true;
        config.tls.http_port = 8443;

        let error = validation_error(&config);

        assert_eq!(
            error,
            "CONFIG ERROR: tls.http_port (8443) must differ from tls.port (8443) when tls.redirect_http=true."
        );
    }

    #[test]
    fn validate_accepts_distinct_tls_and_redirect_ports() {
        let mut config = valid_config();
        config.tls.enabled = true;
        config.tls.port = 8443;
        config.tls.redirect_http = true;
        config.tls.http_port = 8081;

        config.validate().expect("config should validate");
    }

    #[test]
    fn validate_rejects_short_chan_net_api_key() {
        let mut config = valid_config();
        config.chan_net_api_key = "short-key".to_string();

        let error = validation_error(&config);

        assert_eq!(
            error,
            "CONFIG ERROR: chan_net_api_key must be empty to disable ChanNet auth-protected endpoints or at least 32 characters long."
        );
    }

    #[test]
    fn validate_accepts_empty_or_long_chan_net_api_key() {
        let mut config = valid_config();
        config.chan_net_api_key.clear();
        config.validate().expect("empty key disables endpoints");

        config.chan_net_api_key = "x".repeat(32);
        config.validate().expect("32-char key is accepted");
    }

    #[test]
    fn validate_rejects_tor_only_without_tor_support() {
        let mut config = valid_config();
        config.enable_tor_support = false;
        config.tor_only = true;

        let error = validation_error(&config);

        assert_eq!(
            error,
            "CONFIG ERROR: tor_only=true requires enable_tor_support=true. Tor-only mode needs the built-in onion service to be active."
        );
    }

    #[test]
    fn settings_template_uses_forest_and_featured_theme_order() {
        let template = settings_template("secret");

        assert!(template.contains(r#"default_theme = "forest""#));
        assert!(template.contains(
            r#"enabled_builtin_themes = ["forest", "blue-sky", "deep-orbit", "terminal", "dorfic", "chanclassic", "aero", "neoncubicle", "fluorogrid"]"#
        ));
    }

    #[test]
    fn settings_template_marks_enabled_builtins_as_first_start_seeded() {
        let template = settings_template("secret");

        assert!(
            template.contains("# Built-in themes enabled when the theme catalog is first seeded.")
        );
        assert!(template.contains(
            "# After first startup, Admin -> Theme Catalog owns the live enabled/disabled state."
        ));
    }
}
