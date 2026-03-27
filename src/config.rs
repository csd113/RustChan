// config.rs — Runtime configuration.
//
// Priority (highest → lowest):
// 1. Environment variables (CHAN_BIND, CHAN_DB, …)
// 2. settings.toml (<exe-dir>/rustchan-data/settings.toml)
// 3. Hard-coded defaults
//
// On first run, settings.toml is generated next to the binary with all
// default values and explanatory comments. Edit it, restart the server.
//
// SECURITY: The cookie_secret is auto-generated on first run and persisted
// to settings.toml. It is never left at a well-known default value.
use rand_core::{OsRng, RngCore};

use serde::Deserialize;
use std::env;
use std::path::PathBuf;
use std::sync::LazyLock;

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
    let data_dir = binary_dir().join("rustchan-data");
    data_dir.join("settings.toml")
}

// ─── Settings file structure ──────────────────────────────────────────────────
#[derive(Deserialize, Default)]
struct SettingsFile {
    forum_name: Option<String>,
    site_subtitle: Option<String>,
    default_theme: Option<String>,
    port: Option<u16>,
    max_image_size_mb: Option<u32>,
    max_video_size_mb: Option<u32>,
    max_audio_size_mb: Option<u32>,
    cookie_secret: Option<String>,
    enable_tor_support: Option<bool>,
    tor_only: Option<bool>,
    tor_bootstrap_timeout_secs: Option<u64>,
    tor_max_concurrent_streams: Option<usize>,
    tor_service_nickname: Option<String>,
    tor_stable_run_threshold_secs: Option<u64>,
    tor_stream_timeout_secs: Option<u64>,
    tor_num_intro_points: Option<u8>,
    require_ffmpeg: Option<bool>,
    wal_checkpoint_interval_secs: Option<u64>,
    auto_vacuum_interval_hours: Option<u64>,
    poll_cleanup_interval_hours: Option<u64>,
    db_warn_threshold_mb: Option<u64>,
    job_queue_capacity: Option<u64>,
    ffmpeg_timeout_secs: Option<u64>,
    archive_before_prune: Option<bool>,
    waveform_cache_max_mb: Option<u64>,
    blocking_threads: Option<usize>,
    db_pool_size: Option<u32>,

    // ── ChanNet / RustWave gateway ────────────────────────────────────────────
    rustwave_url: Option<String>,
    chan_net_bind: Option<String>,
    chan_net_api_key: Option<String>,
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

pub fn generate_settings_file_if_missing() {
    let path = settings_file_path();
    if path.exists() {
        return;
    }

    let mut secret_bytes = [0u8; 32];
    OsRng.fill_bytes(&mut secret_bytes);
    let secret = hex::encode(secret_bytes);

    let content = settings_template(&secret);

    match std::fs::write(&path, content) {
        Ok(()) => println!("Created settings.toml ({})", path.display()),
        Err(e) => eprintln!("Warning: could not write settings.toml: {e}"),
    }
}

fn settings_template(secret: &str) -> String {
    format!(
        r#"# RustChan — Instance Settings
Edit this file to configure your imageboard.
Restart the server after making changes.

# Name shown in the browser tab, page header, and home page title.
forum_name = "RustChan"

# Subtitle shown below the site name on the home page.
# Can also be changed at any time from the admin panel → Site Settings.
site_subtitle = "select board to proceed"

# Default theme for first-time visitors (before they choose their own).
# Valid values: terminal, aero, dorfic, fluorogrid, neoncubicle, chanclassic
# Leave as "terminal" (or empty) for the default dark terminal look.
# Can also be changed at any time from the admin panel → Site Settings.
default_theme = "terminal"

# Port the server listens on (binds to 0.0.0.0:<port>).
port = 8080

# Maximum size for image uploads in megabytes (jpg, png, gif, webp).
max_image_size_mb = 8

# Maximum size for video uploads in megabytes (mp4, webm).
max_video_size_mb = 50

# Maximum size for audio uploads in megabytes (mp3, ogg, flac, wav, m4a, aac).
max_audio_size_mb = 150

# Tor Onion Service support (powered by Arti — no system tor required).
enable_tor_support = true

# When true, the HTTP server binds exclusively to 127.0.0.1 so the site is
# reachable ONLY through the Tor hidden service — clearnet access is blocked.
# Requires enable_tor_support = true.
tor_only = false

# Seconds to wait for Tor to connect to the network before giving up and
# retrying. The default (120 s) works on open networks. On censored networks
# or when using bridges, increase this to 300 or more.
tor_bootstrap_timeout_secs = 120

# Maximum number of simultaneous inbound Tor connections.
# 512 is far too high for a typical hidden service and allows stale half-open
# streams to accumulate, triggering mass circuit teardowns. Start at 64 and
# raise under measured load.
tor_max_concurrent_streams = 64

# Nickname for this instance's Tor hidden service key.
tor_service_nickname = "rustchan"

# Seconds a run must stay alive before its restart-attempt counter resets.
# 60 s (the old default) was too short: a client that bootstrapped and then
# hit guard failures would still reset the counter, causing infinite 30 s
# retries. 600 s (10 min) correctly distinguishes a stable run from a crash.
tor_stable_run_threshold_secs = 600

# Wall-clock timeout (seconds) for each proxied onion-service stream. Stale
# half-open streams are forcibly closed after this many seconds so they do not
# accumulate and exhaust the semaphore. 300 s covers large file uploads.
tor_stream_timeout_secs = 300

# Number of Tor introduction points to establish for the onion service.
# Arti (≥0.40) enforces a hard minimum of 3 and a maximum of 20; values
# outside that range are rejected at startup. 3 is the recommended default.
tor_num_intro_points = 3

# Set to true to hard-exit at startup when ffmpeg is not found.
require_ffmpeg = false

# How often (in seconds) to run PRAGMA wal_checkpoint(TRUNCATE) …
wal_checkpoint_interval_secs = 3600

# How often (in hours) to run VACUUM automatically …
auto_vacuum_interval_hours = 24

# How often (in hours) to purge vote records for expired polls …
poll_cleanup_interval_hours = 72

# Database file size (MiB) above which a warning banner appears …
db_warn_threshold_mb = 2048

# Maximum number of pending background jobs …
job_queue_capacity = 1000

# Maximum seconds a single FFmpeg job may run …
ffmpeg_timeout_secs = 120

# When true, threads that would be hard-deleted are instead archived …
archive_before_prune = true

# Maximum total size (MiB) of all thumbnail/waveform cache files …
waveform_cache_max_mb = 200

# Number of threads in Tokio's blocking pool …
blocking_threads = 0

# Secret key for IP hashing.
# AUTO-GENERATED on first run — do NOT change after your first post.
cookie_secret = "{secret}"

# ── ChanNet / RustWave gateway ────────────────────────────────────────────────
# Uncomment and configure these to enable the ChanNet API (--chan-net flag).

# rustwave_url = "http://localhost:7071"
# chan_net_bind = "127.0.0.1:7070"
"#
    )
}

// ─── Runtime config ───────────────────────────────────────────────────────────
pub static CONFIG: LazyLock<Config> = LazyLock::new(Config::from_env);

#[allow(clippy::struct_excessive_bools)]
pub struct Config {
    pub forum_name: String,
    pub initial_site_subtitle: String,
    pub initial_default_theme: String,
    #[allow(dead_code)]
    pub port: u16,
    pub max_image_size: usize,
    pub max_video_size: usize,
    pub max_audio_size: usize,

    /// When true, Tor is probed at startup and hints are printed.
    pub enable_tor_support: bool,
    /// When true, the HTTP server binds exclusively to 127.0.0.1.
    pub tor_only: bool,
    pub tor_bootstrap_timeout_secs: u64,
    pub tor_max_concurrent_streams: usize,
    pub tor_service_nickname: String,
    /// Seconds a run must stay alive before its attempt counter resets.
    pub tor_stable_run_threshold_secs: u64,
    /// Wall-clock timeout (seconds) for each proxied onion-service stream.
    pub tor_stream_timeout_secs: u64,
    /// Number of introduction points to establish for the onion service.
    pub tor_num_intro_points: u8,
    pub require_ffmpeg: bool,

    pub bind_addr: String,
    pub database_path: String,
    pub upload_dir: String,
    pub thumb_size: u32,
    #[allow(dead_code)]
    pub default_bump_limit: u32,
    #[allow(dead_code)]
    pub max_threads_per_board: u32,
    pub rate_limit_gets: u32,
    pub rate_limit_window: u64,
    pub cookie_secret: String,
    pub session_duration: i64,
    pub behind_proxy: bool,
    pub https_cookies: bool,
    pub wal_checkpoint_interval: u64,
    pub auto_vacuum_interval_hours: u64,
    pub poll_cleanup_interval_hours: u64,
    pub db_warn_threshold_bytes: u64,
    pub job_queue_capacity: u64,
    pub ffmpeg_timeout_secs: u64,
    pub archive_before_prune: bool,
    pub waveform_cache_max_bytes: u64,
    pub blocking_threads: usize,
    pub db_pool_size: u32,

    pub rustwave_url: String,
    pub chan_net_bind: String,
    pub chan_net_max_body: usize,
    pub chan_net_command_max_body: usize,
    pub chan_net_api_key: String,
}

impl Config {
    #[must_use]
    #[allow(clippy::too_many_lines)]
    pub fn from_env() -> Self {
        let s = load_settings_file();
        let data_dir = binary_dir().join("rustchan-data");

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
            s.default_theme.as_deref().unwrap_or("terminal"),
        );
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

        let bind_addr = if tor_only && enable_tor_support {
            let port_str = bind_addr.rsplit_once(':').map_or("8080", |(_, p)| p);
            tracing::info!(
                target: "config",
                bind_addr = %format!("127.0.0.1:{port_str}"),
                "tor_only=true: overriding bind address to loopback"
            );
            format!("127.0.0.1:{port_str}")
        } else {
            bind_addr
        };

        let behind_proxy = env_bool("CHAN_BEHIND_PROXY", false);

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
            let mut b = [0u8; 32];
            OsRng.fill_bytes(&mut b);
            hex::encode(b)
        };

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
            .unwrap_or(10 * 1024 * 1024);
        let chan_net_command_max_body: usize = env::var("CHAN_NET_COMMAND_MAX_BODY")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(8 * 1024);

        Self {
            forum_name,
            initial_site_subtitle,
            initial_default_theme,
            port,
            max_image_size: (max_image_mb as usize).saturating_mul(1024 * 1024),
            max_video_size: (max_video_mb as usize).saturating_mul(1024 * 1024),
            max_audio_size: (max_audio_mb as usize).saturating_mul(1024 * 1024),

            enable_tor_support,
            tor_only,
            tor_bootstrap_timeout_secs: env_parse(
                "CHAN_TOR_BOOTSTRAP_TIMEOUT",
                s.tor_bootstrap_timeout_secs.unwrap_or(120),
            ),
            tor_max_concurrent_streams: env_parse(
                "CHAN_TOR_MAX_STREAMS",
                s.tor_max_concurrent_streams.unwrap_or(64),
            ),
            tor_service_nickname: std::env::var("CHAN_TOR_NICKNAME")
                .ok()
                .or(s.tor_service_nickname)
                .unwrap_or_else(|| "rustchan".to_string()),
            tor_stable_run_threshold_secs: env_parse(
                "CHAN_TOR_STABLE_RUN_SECS",
                s.tor_stable_run_threshold_secs.unwrap_or(600),
            ),
            tor_stream_timeout_secs: env_parse(
                "CHAN_TOR_STREAM_TIMEOUT",
                s.tor_stream_timeout_secs.unwrap_or(300),
            ),
            tor_num_intro_points: env_parse(
                "CHAN_TOR_INTRO_POINTS",
                s.tor_num_intro_points.unwrap_or(3),
            ),
            require_ffmpeg: env_bool("CHAN_REQUIRE_FFMPEG", s.require_ffmpeg.unwrap_or(false)),

            bind_addr,
            database_path: env_str("CHAN_DB", &default_db),
            upload_dir: env_str("CHAN_UPLOADS", &default_uploads),
            thumb_size: env_parse("CHAN_THUMB_SIZE", 250),
            default_bump_limit: env_parse("CHAN_BUMP_LIMIT", 500),
            max_threads_per_board: env_parse("CHAN_MAX_THREADS", 150),
            rate_limit_gets: env_parse("CHAN_RATE_GETS", 60),
            rate_limit_window: env_parse("CHAN_RATE_WINDOW", 60),
            cookie_secret,
            session_duration: env_parse("CHAN_SESSION_SECS", 8 * 3600),
            behind_proxy,
            https_cookies: env_bool("CHAN_HTTPS_COOKIES", behind_proxy),
            wal_checkpoint_interval: env_parse(
                "CHAN_WAL_CHECKPOINT_SECS",
                s.wal_checkpoint_interval_secs.unwrap_or(3600),
            ),
            auto_vacuum_interval_hours: env_parse(
                "CHAN_AUTO_VACUUM_HOURS",
                s.auto_vacuum_interval_hours.unwrap_or(24),
            ),
            poll_cleanup_interval_hours: env_parse(
                "CHAN_POLL_CLEANUP_HOURS",
                s.poll_cleanup_interval_hours.unwrap_or(72),
            ),
            db_warn_threshold_bytes: env_parse::<u64>(
                "CHAN_DB_WARN_THRESHOLD_MB",
                s.db_warn_threshold_mb.unwrap_or(2048),
            )
            .saturating_mul(1024 * 1024),
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
            waveform_cache_max_bytes: env_parse::<u64>(
                "CHAN_WAVEFORM_CACHE_MAX_MB",
                s.waveform_cache_max_mb.unwrap_or(200),
            )
            .saturating_mul(1024 * 1024),
            blocking_threads: {
                let cpus = std::thread::available_parallelism()
                    .map(std::num::NonZero::get)
                    .unwrap_or(4);
                let configured =
                    env_parse("CHAN_BLOCKING_THREADS", s.blocking_threads.unwrap_or(0));
                if configured == 0 {
                    cpus.saturating_mul(4)
                } else {
                    configured
                }
            },
            db_pool_size: env_parse("CHAN_DB_POOL_SIZE", s.db_pool_size.unwrap_or(8)),

            rustwave_url,
            chan_net_bind,
            chan_net_max_body,
            chan_net_command_max_body,
            chan_net_api_key: std::env::var("CHAN_NET_API_KEY")
                .ok()
                .or(s.chan_net_api_key)
                .unwrap_or_default(),
        }
    }

    /// Validate critical configuration values and abort with a clear error
    /// message if any are out of range.  Called once at startup so operators
    /// catch misconfiguration immediately rather than discovering it at runtime.
    ///
    /// # Errors
    /// Returns an error if any configuration value is out of an acceptable range,
    /// the upload directory (or any Tor data directory) is not writable/creatible,
    /// `tor_only` is enabled without `enable_tor_support`, or `chan_net_api_key`
    /// is set but shorter than 32 characters.
    pub fn validate(&self) -> anyhow::Result<()> {
        const MIB: usize = 1024 * 1024;
        const MAX_IMAGE_MIB: usize = 100;
        const MAX_VIDEO_MIB: usize = 2048;
        const MAX_AUDIO_MIB: usize = 512;

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

        let upload_path = std::path::Path::new(&self.upload_dir);
        std::fs::create_dir_all(upload_path).map_err(|e| {
            anyhow::anyhow!(
                "CONFIG ERROR: cannot create upload_dir '{}': {e}",
                self.upload_dir
            )
        })?;
        let probe = upload_path.join(".write_probe");
        if std::fs::write(&probe, b"").is_err() {
            let _ = std::fs::remove_file(&probe);
            anyhow::bail!(
                "CONFIG ERROR: upload_dir '{}' is not writable.",
                self.upload_dir
            );
        }
        let _ = std::fs::remove_file(probe);

        if self.enable_tor_support {
            let exe = std::env::current_exe()
                .ok()
                .and_then(|p| p.parent().map(std::path::Path::to_path_buf))
                .unwrap_or_else(|| std::path::PathBuf::from("."));
            let data_dir = exe.join("rustchan-data");
            for subdir in ["arti_state", "arti_cache"] {
                let dir = data_dir.join(subdir);
                std::fs::create_dir_all(&dir).map_err(|e| {
                    anyhow::anyhow!("CONFIG ERROR: cannot create Tor dir {}: {e}", dir.display())
                })?;

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

        if self.tor_only && !self.enable_tor_support {
            anyhow::bail!("CONFIG ERROR: tor_only=true requires enable_tor_support=true.");
        }
        if !self.chan_net_api_key.is_empty() && self.chan_net_api_key.len() < 32 {
            anyhow::bail!("CONFIG ERROR: chan_net_api_key must be at least 32 characters if set.");
        }

        if !self.rustwave_url.starts_with("http://") && !self.rustwave_url.starts_with("https://") {
            anyhow::bail!(
                "CONFIG ERROR: rustwave_url must begin with http:// or https://, got: {}",
                self.rustwave_url
            );
        }

        Ok(())
    }
}

pub fn update_settings_file_site_names(forum_name: &str, site_subtitle: &str) {
    fn toml_quote(s: &str) -> String {
        let inner = s
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('\n', "\\n")
            .replace('\r', "\\r")
            .replace('\t', "\\t");
        format!("\"{inner}\"")
    }

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

    let trailing_newline = content.ends_with('\n');
    let updated: Vec<String> = content
        .lines()
        .map(|line| {
            if line.trim_start().starts_with("forum_name") && line.contains('=') {
                return format!("forum_name = {}", toml_quote(forum_name));
            }
            if line.trim_start().starts_with("site_subtitle") && line.contains('=') {
                return format!("site_subtitle = {}", toml_quote(site_subtitle));
            }
            line.to_string()
        })
        .collect();

    let mut out = updated.join("\n");
    if trailing_newline {
        out.push('\n');
    }

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
            return;
        }
        tracing::warn!(
            "SECURITY WARNING: cookie_secret has changed since the last run. \
             All IP-based bans are now invalid because all IP hashes have changed. \
             If this was unintentional, restore the previous cookie_secret from \
             settings.toml. If intentional, consider running: \
             DELETE FROM bans; DELETE FROM ban_appeals;"
        );
    }

    let _ = conn.execute(
        "INSERT INTO site_settings (key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        rusqlite::params![KEY, current_hash],
    );
}

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
