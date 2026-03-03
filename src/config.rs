// config.rs — Runtime configuration.
//
// Priority (highest → lowest):
//   1. Environment variables  (CHAN_BIND, CHAN_DB, …)
//   2. settings.toml          (<exe-dir>/settings.toml)
//   3. Hard-coded defaults
//
// On first run, settings.toml is generated next to the binary with all
// default values and explanatory comments.  Edit it, restart the server.

use once_cell::sync::Lazy;
use serde::Deserialize;
use std::env;
use std::path::PathBuf;

/// Absolute path to the directory the running binary lives in.
fn binary_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."))
}

fn settings_file_path() -> PathBuf {
    // Store settings.toml in chan-data/ alongside the database.
    // chan-data/ is created by run_server before CONFIG is first accessed,
    // so this directory always exists by the time settings are read.
    let data_dir = binary_dir().join("chan-data");
    data_dir.join("settings.toml")
}

// ─── Settings file structure ──────────────────────────────────────────────────

#[derive(Deserialize, Default)]
struct SettingsFile {
    forum_name:        Option<String>,
    port:              Option<u16>,
    max_image_size_mb: Option<u32>,
    max_video_size_mb: Option<u32>,
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
pub fn generate_settings_file_if_missing() {
    let path = settings_file_path();
    if path.exists() {
        return;
    }
    let content = r#"# RustChan — Instance Settings
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
"#;
    match std::fs::write(&path, content) {
        Ok(_)  => println!("Created  settings.toml  ({})", path.display()),
        Err(e) => eprintln!("Warning: could not write settings.toml: {e}"),
    }
}

// ─── Runtime config ───────────────────────────────────────────────────────────

pub static CONFIG: Lazy<Config> = Lazy::new(Config::from_env);

pub struct Config {
    // ── Loaded from settings.toml (env vars still override) ──────────────────
    pub forum_name:            String,
    #[allow(dead_code)]
    pub port:                  u16,
    pub max_image_size:        usize,   // bytes
    pub max_video_size:        usize,   // bytes

    // ── Internal / env-only settings ─────────────────────────────────────────
    pub bind_addr:             String,
    pub database_path:         String,
    pub upload_dir:            String,
    pub thumb_size:            u32,
    #[allow(dead_code)]
    pub default_bump_limit:    u32,
    pub max_threads_per_board: u32,
    pub rate_limit_posts:      u32,
    pub rate_limit_window:     u64,
    pub cookie_secret:         String,
    pub session_duration:      i64,
    pub behind_proxy:          bool,
}

impl Config {
    pub fn from_env() -> Self {
        let s        = load_settings_file();
        let data_dir = binary_dir().join("chan-data");

        let default_db      = data_dir.join("chan.db").to_string_lossy().into_owned();
        let default_uploads = data_dir.join("uploads").to_string_lossy().into_owned();

        let forum_name   = env_str("CHAN_FORUM_NAME",  s.forum_name.as_deref().unwrap_or("RustChan"));
        let port         = env_u16("CHAN_PORT",         s.port.unwrap_or(8080));
        let max_image_mb = env_u32("CHAN_MAX_IMAGE_MB", s.max_image_size_mb.unwrap_or(8));
        let max_video_mb = env_u32("CHAN_MAX_VIDEO_MB", s.max_video_size_mb.unwrap_or(50));

        let host      = env_str("CHAN_HOST", "0.0.0.0");
        let bind_addr = env_str("CHAN_BIND",  &format!("{host}:{port}"));

        Self {
            forum_name,
            port,
            max_image_size:        (max_image_mb as usize) * 1024 * 1024,
            max_video_size:        (max_video_mb  as usize) * 1024 * 1024,

            bind_addr,
            database_path:         env_str("CHAN_DB",            &default_db),
            upload_dir:            env_str("CHAN_UPLOADS",       &default_uploads),
            thumb_size:            env_u32("CHAN_THUMB_SIZE",    250),
            default_bump_limit:    env_u32("CHAN_BUMP_LIMIT",    500),
            max_threads_per_board: env_u32("CHAN_MAX_THREADS",   150),
            rate_limit_posts:      env_u32("CHAN_RATE_POSTS",    10),
            rate_limit_window:     env_u64("CHAN_RATE_WINDOW",   60),
            cookie_secret:         env_str("CHAN_COOKIE_SECRET", "CHANGE_THIS_SECRET_IN_PRODUCTION"),
            session_duration:      env_i64("CHAN_SESSION_SECS",  8 * 3600),
            behind_proxy:          env_bool("CHAN_BEHIND_PROXY", false),
        }
    }
}

fn env_str(key: &str, default: &str) -> String {
    env::var(key).unwrap_or_else(|_| default.to_string())
}
fn env_u16(key: &str, default: u16) -> u16 {
    env::var(key).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}
fn env_u32(key: &str, default: u32) -> u32 {
    env::var(key).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}
fn env_u64(key: &str, default: u64) -> u64 {
    env::var(key).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}
fn env_i64(key: &str, default: i64) -> i64 {
    env::var(key).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}
fn env_bool(key: &str, default: bool) -> bool {
    env::var(key)
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(default)
}
