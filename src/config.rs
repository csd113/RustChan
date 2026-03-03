// config.rs — Runtime configuration from env vars or sane local defaults.
//
// Data directory strategy:
//   By default everything lives in  <dir-of-binary>/chan-data/
//   so you can copy the binary anywhere and it just works.
//   Every setting is overridable via an environment variable.

use once_cell::sync::Lazy;
use std::env;
use std::path::PathBuf;

/// Absolute path to the directory the running binary lives in.
/// Falls back to the current working directory if unavailable.
fn binary_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."))
}

pub static CONFIG: Lazy<Config> = Lazy::new(Config::from_env);

pub struct Config {
    pub bind_addr:             String,
    pub database_path:         String,
    pub upload_dir:            String,
    pub max_file_size:         usize,
    pub thumb_size:            u32,
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
        let data_dir = binary_dir().join("chan-data");
        let default_db      = data_dir.join("chan.db").to_string_lossy().into_owned();
        let default_uploads = data_dir.join("uploads").to_string_lossy().into_owned();

        Self {
            bind_addr:             env_str("CHAN_BIND",          "0.0.0.0:8080"),
            database_path:         env_str("CHAN_DB",            &default_db),
            upload_dir:            env_str("CHAN_UPLOADS",       &default_uploads),
            max_file_size:         env_usize("CHAN_MAX_FILE_SIZE", 50 * 1024 * 1024),
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
fn env_usize(key: &str, default: usize) -> usize {
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
