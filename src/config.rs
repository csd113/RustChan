// config.rs — Runtime configuration.
//
// Priority (highest → lowest):
//   1. Environment variables  (CHAN_BIND, CHAN_DB, …)
//   2. settings.toml          (<exe-dir>/rustchan-data/settings.toml)
//   3. Hard-coded defaults
//
// On first run, settings.toml is generated next to the binary with all
// default values and explanatory comments.  Edit it, restart the server.
//
// SECURITY: The cookie_secret is auto-generated on first run and persisted
// to settings.toml. It is never left at a well-known default value.

use once_cell::sync::Lazy;
use rand_core::{OsRng, RngCore};
use serde::Deserialize;
use std::env;
use std::path::PathBuf;

/// Absolute path to the directory the running binary lives in.
fn binary_dir() -> PathBuf {
    match std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.to_path_buf()))
    {
        Some(dir) => dir,
        None => {
            eprintln!(
                "Warning: could not determine binary directory; \
                 using current working directory for data storage. \
                 Set CHAN_DB and CHAN_UPLOADS env vars to override."
            );
            PathBuf::from(".")
        }
    }
}

fn settings_file_path() -> PathBuf {
    // Store settings.toml in rustchan-data/ alongside the database.
    // rustchan-data/ is created by run_server before CONFIG is first accessed,
    // so this directory always exists by the time settings are read.
    let data_dir = binary_dir().join("rustchan-data");
    data_dir.join("settings.toml")
}

// ─── Settings file structure ──────────────────────────────────────────────────

#[derive(Deserialize, Default)]
struct SettingsFile {
    forum_name: Option<String>,
    /// Home page subtitle shown below the site name.
    site_subtitle: Option<String>,
    /// Default theme served to first-time visitors before they pick one.
    /// Valid values: terminal, aero, dorfic, fluorogrid, neoncubicle, chanclassic
    default_theme: Option<String>,
    port: Option<u16>,
    max_image_size_mb: Option<u32>,
    max_video_size_mb: Option<u32>,
    max_audio_size_mb: Option<u32>,
    cookie_secret: Option<String>,
    enable_tor_support: Option<bool>,
    require_ffmpeg: Option<bool>,
    /// How often to run PRAGMA wal_checkpoint(TRUNCATE), in seconds.
    /// Set to 0 to disable. Default: 3600 (hourly).
    wal_checkpoint_interval_secs: Option<u64>,
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
/// A cryptographically random cookie_secret is generated on first run and
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

    let content = format!(
        r#"# RustChan — Instance Settings
# Edit this file to configure your imageboard.
# Restart the server after making changes.

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

# Tor Onion Service support.
# When true, the server probes for `tor` at startup and prints torrc hints.
# The server always starts regardless — this is purely informational.
enable_tor_support = true

# Set to true to hard-exit at startup when ffmpeg is not found.
# When false (default), the server starts normally and video thumbnails
# are replaced with SVG placeholders.
require_ffmpeg = false

# How often (in seconds) to run PRAGMA wal_checkpoint(TRUNCATE) to keep
# the SQLite WAL file from growing unbounded under write load.
# Set to 0 to disable. Default: 3600 (hourly).
wal_checkpoint_interval_secs = 3600

# Secret key for IP hashing.
# AUTO-GENERATED on first run — do NOT change after your first post,
# or all existing IP hashes become invalid (bans will stop working).
# If you must rotate it, also clear the bans table.
cookie_secret = "{secret}"
"#
    );

    match std::fs::write(&path, content) {
        Ok(_) => println!("Created  settings.toml  ({})", path.display()),
        Err(e) => eprintln!("Warning: could not write settings.toml: {e}"),
    }
}

// ─── Runtime config ───────────────────────────────────────────────────────────

pub static CONFIG: Lazy<Config> = Lazy::new(Config::from_env);

pub struct Config {
    // ── Loaded from settings.toml (env vars still override) ──────────────────
    pub forum_name: String,
    /// Initial subtitle shown on the home page (seeds the DB on first run).
    pub initial_site_subtitle: String,
    /// Initial default theme slug (seeds the DB on first run).
    /// Valid: terminal, aero, dorfic, fluorogrid, neoncubicle, chanclassic
    pub initial_default_theme: String,
    #[allow(dead_code)] // read by CLI subcommands and printed at startup
    pub port: u16,
    pub max_image_size: usize, // bytes
    pub max_video_size: usize, // bytes
    pub max_audio_size: usize, // bytes

    // ── External tool settings ────────────────────────────────────────────────
    /// When true, Tor is probed at startup and hints are printed.
    pub enable_tor_support: bool,
    /// When true, the server exits if ffmpeg is missing.
    pub require_ffmpeg: bool,

    // ── Internal / env-only settings ─────────────────────────────────────────
    pub bind_addr: String,
    pub database_path: String,
    pub upload_dir: String,
    pub thumb_size: u32,
    #[allow(dead_code)] // used as default when creating boards via CLI/admin
    pub default_bump_limit: u32,
    #[allow(dead_code)] // used as default when creating boards via CLI/admin
    pub max_threads_per_board: u32,
    /// Maximum GET requests per IP per rate_limit_window.
    pub rate_limit_gets: u32,
    pub rate_limit_window: u64,
    pub cookie_secret: String,
    pub session_duration: i64,
    pub behind_proxy: bool,
    pub https_cookies: bool,
    /// Interval in seconds between WAL checkpoint runs. 0 = disabled.
    pub wal_checkpoint_interval: u64,
}

impl Config {
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

        let behind_proxy = env_bool("CHAN_BEHIND_PROXY", false);

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

        Self {
            forum_name,
            initial_site_subtitle,
            initial_default_theme,
            port,
            max_image_size: (max_image_mb as usize) * 1024 * 1024,
            max_video_size: (max_video_mb as usize) * 1024 * 1024,
            max_audio_size: (max_audio_mb as usize) * 1024 * 1024,

            enable_tor_support: env_bool("CHAN_TOR_SUPPORT", s.enable_tor_support.unwrap_or(true)),
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
        }
    }

    /// Validate critical configuration values and abort with a clear error
    /// message if any are out of range.  Called once at startup so operators
    /// catch misconfiguration immediately rather than discovering it at runtime.
    pub fn validate(&self) -> anyhow::Result<()> {
        // cookie_secret is hex-encoded: 64 hex chars = 32 bytes of entropy.
        if self.cookie_secret.len() < 64 {
            anyhow::bail!(
                "CONFIG ERROR: cookie_secret is too short ({} chars). \
                 It must be at least 64 hex characters (32 bytes). \
                 Delete settings.toml and restart to auto-generate a secure secret.",
                self.cookie_secret.len()
            );
        }

        const MIB: usize = 1024 * 1024;
        const MAX_IMAGE_MIB: usize = 100;
        const MAX_VIDEO_MIB: usize = 2048;
        const MAX_AUDIO_MIB: usize = 512;

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

        Ok(())
    }
}

/// Update `forum_name` and `site_subtitle` in `settings.toml` in-place,
/// preserving all other lines and comments.
///
/// Called by the admin site-settings handler so that changes made via the
/// panel are reflected in the file and survive a restart without the operator
/// needing to hand-edit `settings.toml`.
///
/// If the key is not yet present in the file the function is a no-op for that
/// key (it won't append new lines — the file is only updated if the key already
/// exists).  On a fresh install `generate_settings_file_if_missing` always
/// writes both keys, so this is only a concern for manually-crafted files.
pub fn update_settings_file_site_names(forum_name: &str, site_subtitle: &str) {
    let path = settings_file_path();
    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Warning: could not read settings.toml for update: {e}");
            return;
        }
    };

    // Replace the value portion of `key = "..."` lines while preserving
    // indentation, comments on the same line, and surrounding whitespace.
    // We use a simple line-by-line scan so that file comments are untouched.
    fn toml_quote(s: &str) -> String {
        // Escape backslash and double-quote, then wrap in double quotes.
        let inner = s.replace('\\', "\\\\").replace('"', "\\\"");
        format!("\"{inner}\"")
    }

    let trailing_newline = content.ends_with('\n');
    let updated: Vec<String> = content
        .lines()
        .map(|line| {
            // Match `forum_name = ...` (possibly with surrounding spaces).
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

    if let Err(e) = std::fs::write(&path, out) {
        eprintln!("Warning: could not write updated settings.toml: {e}");
    }
}

// ─── Cookie secret rotation check ────────────────────────────────────────────

/// Check whether the cookie_secret has changed since the last run by comparing
/// a SHA-256 hash stored in the DB against the currently loaded secret.
///
/// Called once at startup after the DB pool is ready.
/// If the secret has rotated, all IP-based bans become invalid — warn loudly.
/// On first run (no stored hash), silently stores the current hash and returns.
pub fn check_cookie_secret_rotation(conn: &rusqlite::Connection) {
    use sha2::{Digest, Sha256};

    let current_hash = {
        let mut h = Sha256::new();
        h.update(CONFIG.cookie_secret.as_bytes());
        hex::encode(h.finalize())
    };

    const KEY: &str = "cookie_secret_hash";

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
    env::var(key)
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(default)
}
