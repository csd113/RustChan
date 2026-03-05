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
// FIX[CRITICAL-1]: removed hardcoded default secret; see generate_settings_file_if_missing().

use once_cell::sync::Lazy;
use serde::Deserialize;
use std::env;
use std::path::PathBuf;

/// Absolute path to the directory the running binary lives in.
fn binary_dir() -> PathBuf {
    // FIX[MEDIUM-2]: log a warning when fallback is used so operators
    // are aware that data may land in an unexpected location.
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
    // Store settings.toml in rustrustchan-data/ alongside the database.
    // rustrustchan-data/ is created by run_server before CONFIG is first accessed,
    // so this directory always exists by the time settings are read.
    let data_dir = binary_dir().join("rustchan-data");
    data_dir.join("settings.toml")
}

// ─── Settings file structure ──────────────────────────────────────────────────

#[derive(Deserialize, Default)]
struct SettingsFile {
    forum_name: Option<String>,
    port: Option<u16>,
    max_image_size_mb: Option<u32>,
    max_video_size_mb: Option<u32>,
    max_audio_size_mb: Option<u32>,
    // FIX[CRITICAL-1]: cookie_secret is now persisted in settings.toml so it
    // is generated once and stable across restarts, without being a known default.
    cookie_secret: Option<String>,
    // External tool toggles
    enable_tor_support: Option<bool>,
    require_ffmpeg: Option<bool>,
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
/// FIX[CRITICAL-1]: A cryptographically random cookie_secret is generated on
/// first run and written to settings.toml. Subsequent runs load it from the
/// file. The server never operates with a known/default secret.
pub fn generate_settings_file_if_missing() {
    let path = settings_file_path();
    if path.exists() {
        return;
    }

    // Generate a random 64-hex-char secret (32 bytes of entropy).
    // This runs before CONFIG is initialised, so we call OsRng directly.
    use rand_core::{OsRng, RngCore};
    let mut secret_bytes = [0u8; 32];
    OsRng.fill_bytes(&mut secret_bytes);
    let secret = hex::encode(secret_bytes);

    let content = format!(
        r#"# RustChan — Instance Settings
# Edit this file to configure your imageboard.
# Restart the server after making changes.

# Name shown in the browser tab, page header, and home page title.
forum_name = "RustChan"

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
    #[allow(dead_code)]
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
    #[allow(dead_code)]
    pub default_bump_limit: u32,
    #[allow(dead_code)]
    pub max_threads_per_board: u32,
    pub rate_limit_posts: u32,
    pub rate_limit_window: u64,
    // FIX[CRITICAL-1]: cookie_secret is now loaded from settings.toml or env.
    // It is never left at a hardcoded default string.
    pub cookie_secret: String,
    pub session_duration: i64,
    pub behind_proxy: bool,
    // FIX[MEDIUM-11]: explicit flag for whether to set Secure on cookies.
    // Defaults to true when behind_proxy is true (i.e. TLS is expected).
    pub https_cookies: bool,
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
        let port = env_u16("CHAN_PORT", s.port.unwrap_or(8080));
        let max_image_mb = env_u32("CHAN_MAX_IMAGE_MB", s.max_image_size_mb.unwrap_or(8));
        let max_video_mb = env_u32("CHAN_MAX_VIDEO_MB", s.max_video_size_mb.unwrap_or(50));
        let max_audio_mb = env_u32("CHAN_MAX_AUDIO_MB", s.max_audio_size_mb.unwrap_or(150));

        let host = env_str("CHAN_HOST", "0.0.0.0");
        let bind_addr = env_str("CHAN_BIND", &format!("{host}:{port}"));

        let behind_proxy = env_bool("CHAN_BEHIND_PROXY", false);

        // FIX[CRITICAL-1]: Resolve cookie_secret from env > settings.toml.
        // If neither is set, emit a loud warning. The generate_settings_file_if_missing()
        // call at startup ensures settings.toml always has a generated secret,
        // so this fallback should only be reached in abnormal circumstances.
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
            // Emit a random in-memory secret so each restart invalidates hashes
            // (better than a known empty string, worse than a persisted one).
            let mut b = [0u8; 32];
            rand_core::OsRng.fill_bytes(&mut b);
            hex::encode(b)
        };

        Self {
            forum_name,
            port,
            max_image_size: (max_image_mb as usize) * 1024 * 1024,
            max_video_size: (max_video_mb as usize) * 1024 * 1024,
            max_audio_size: (max_audio_mb as usize) * 1024 * 1024,

            enable_tor_support: env_bool("CHAN_TOR_SUPPORT", s.enable_tor_support.unwrap_or(true)),
            require_ffmpeg: env_bool("CHAN_REQUIRE_FFMPEG", s.require_ffmpeg.unwrap_or(false)),

            bind_addr,
            database_path: env_str("CHAN_DB", &default_db),
            upload_dir: env_str("CHAN_UPLOADS", &default_uploads),
            thumb_size: env_u32("CHAN_THUMB_SIZE", 250),
            default_bump_limit: env_u32("CHAN_BUMP_LIMIT", 500),
            max_threads_per_board: env_u32("CHAN_MAX_THREADS", 150),
            rate_limit_posts: env_u32("CHAN_RATE_POSTS", 10),
            rate_limit_window: env_u64("CHAN_RATE_WINDOW", 60),
            cookie_secret,
            session_duration: env_i64("CHAN_SESSION_SECS", 8 * 3600),
            behind_proxy,
            // FIX[MEDIUM-11]: default Secure=true when running behind a proxy (TLS expected)
            https_cookies: env_bool("CHAN_HTTPS_COOKIES", behind_proxy),
        }
    }
}

// ─── Import needed for OsRng in cookie_secret fallback ───────────────────────
use rand_core::RngCore as _;

fn env_str(key: &str, default: &str) -> String {
    env::var(key).unwrap_or_else(|_| default.to_string())
}
fn env_u16(key: &str, default: u16) -> u16 {
    env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}
fn env_u32(key: &str, default: u32) -> u32 {
    env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}
fn env_u64(key: &str, default: u64) -> u64 {
    env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}
fn env_i64(key: &str, default: i64) -> i64 {
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
