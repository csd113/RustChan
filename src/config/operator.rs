//! Bounded operator policies with historical behavior as the default.

use super::Environment;
use crate::config::admin::{InputKind, SettingDefinition};
use anyhow::ensure;

/// Additional policies consumed by authentication, paging and request boundaries.
#[derive(Debug, Clone)]
pub struct OperatorSettings {
    /// Administrator failed-login allowance.
    pub admin_login_fail_limit: u32,
    /// Administrator failed-login window (seconds).
    pub admin_login_fail_window_secs: u64,
    /// Board-password failed-attempt allowance.
    pub board_password_fail_limit: u32,
    /// Board-password failed-attempt window (seconds).
    pub board_password_fail_window_secs: u64,
    /// Board-password access lifetime (days).
    pub board_access_cookie_days: i64,
    /// Self-edit and self-delete window (seconds).
    pub self_action_window_secs: i64,
    /// Board index threads per page.
    pub index_threads_per_page: i64,
    /// Board index reply previews.
    pub index_reply_previews: i64,
    /// Normal GET/HEAD timeout (seconds).
    pub read_timeout_secs: u64,
    /// Normal write timeout (seconds).
    pub write_timeout_secs: u64,
    /// Optional tracing filter; None preserves the built-in INFO filter.
    pub log_filter: Option<String>,
}

/// File values; missing entries retain existing defaults.
#[derive(Debug, Default, serde::Deserialize)]
pub(super) struct SavedOperatorSettings {
    /// Optional saved admin login fail limit policy.
    pub(super) admin_login_fail_limit: Option<u32>,
    /// Optional saved admin login fail window secs policy.
    pub(super) admin_login_fail_window_secs: Option<u64>,
    /// Optional saved board password fail limit policy.
    pub(super) board_password_fail_limit: Option<u32>,
    /// Optional saved board password fail window secs policy.
    pub(super) board_password_fail_window_secs: Option<u64>,
    /// Optional saved board access cookie days policy.
    pub(super) board_access_cookie_days: Option<i64>,
    /// Optional saved self action window secs policy.
    pub(super) self_action_window_secs: Option<i64>,
    /// Optional saved index threads per page policy.
    pub(super) index_threads_per_page: Option<i64>,
    /// Optional saved index reply previews policy.
    pub(super) index_reply_previews: Option<i64>,
    /// Optional saved read timeout secs policy.
    pub(super) read_timeout_secs: Option<u64>,
    /// Optional saved write timeout secs policy.
    pub(super) write_timeout_secs: Option<u64>,
    /// Optional saved log filter policy.
    pub(super) log_filter: Option<String>,
}

impl OperatorSettings {
    /// Resolve startup environment precedence and historical defaults.
    pub(super) fn load(saved: &SavedOperatorSettings, env: &Environment<'_>) -> Self {
        Self {
            admin_login_fail_limit: env.parse(
                "CHAN_ADMIN_LOGIN_FAIL_LIMIT",
                saved.admin_login_fail_limit.unwrap_or(5),
            ),
            admin_login_fail_window_secs: env.parse(
                "CHAN_ADMIN_LOGIN_FAIL_WINDOW_SECS",
                saved.admin_login_fail_window_secs.unwrap_or(900),
            ),
            board_password_fail_limit: env.parse(
                "CHAN_BOARD_PASSWORD_FAIL_LIMIT",
                saved.board_password_fail_limit.unwrap_or(5),
            ),
            board_password_fail_window_secs: env.parse(
                "CHAN_BOARD_PASSWORD_FAIL_WINDOW_SECS",
                saved.board_password_fail_window_secs.unwrap_or(900),
            ),
            board_access_cookie_days: env.parse(
                "CHAN_BOARD_ACCESS_COOKIE_DAYS",
                saved.board_access_cookie_days.unwrap_or(30),
            ),
            self_action_window_secs: env.parse(
                "CHAN_SELF_ACTION_WINDOW_SECS",
                saved.self_action_window_secs.unwrap_or(60),
            ),
            index_threads_per_page: env.parse(
                "CHAN_INDEX_THREADS_PER_PAGE",
                saved.index_threads_per_page.unwrap_or(10),
            ),
            index_reply_previews: env.parse(
                "CHAN_INDEX_REPLY_PREVIEWS",
                saved.index_reply_previews.unwrap_or(3),
            ),
            read_timeout_secs: env.parse(
                "CHAN_READ_TIMEOUT_SECS",
                saved.read_timeout_secs.unwrap_or(30),
            ),
            write_timeout_secs: env.parse(
                "CHAN_WRITE_TIMEOUT_SECS",
                saved.write_timeout_secs.unwrap_or(300),
            ),
            log_filter: env
                .var("RUST_LOG")
                .ok()
                .filter(|f| tracing_subscriber::EnvFilter::try_new(f).is_ok())
                .or_else(|| saved.log_filter.clone()),
        }
    }
    /// Validate bounded policies before startup or persistence.
    ///
    /// # Errors
    /// Rejects unbounded security/display/timeouts and malformed tracing filters.
    pub fn validate(&self) -> anyhow::Result<()> {
        ensure!(
            (1..=100).contains(&self.admin_login_fail_limit),
            "Administrator failed-login allowance must be 1–100"
        );
        ensure!(
            (30..=86400).contains(&self.admin_login_fail_window_secs),
            "Administrator failed-login window (seconds) must be 30–86400"
        );
        ensure!(
            (1..=100).contains(&self.board_password_fail_limit),
            "Board-password failed-attempt allowance must be 1–100"
        );
        ensure!(
            (30..=86400).contains(&self.board_password_fail_window_secs),
            "Board-password failed-attempt window (seconds) must be 30–86400"
        );
        ensure!(
            (1..=365).contains(&self.board_access_cookie_days),
            "Board-password access lifetime (days) must be 1–365"
        );
        ensure!(
            (1..=3600).contains(&self.self_action_window_secs),
            "Self-edit and self-delete window (seconds) must be 1–3600"
        );
        ensure!(
            (1..=100).contains(&self.index_threads_per_page),
            "Board index threads per page must be 1–100"
        );
        ensure!(
            (0..=20).contains(&self.index_reply_previews),
            "Board index reply previews must be 0–20"
        );
        ensure!(
            (1..=300).contains(&self.read_timeout_secs),
            "Normal GET/HEAD timeout (seconds) must be 1–300"
        );
        ensure!(
            (1..=3600).contains(&self.write_timeout_secs),
            "Normal write timeout (seconds) must be 1–3600"
        );
        if let Some(filter) = &self.log_filter {
            ensure!(filter.len() <= 1024, "log filter is too long");
            tracing_subscriber::EnvFilter::try_new(filter)
                .map_err(|_| anyhow::anyhow!("invalid tracing filter"))?;
        }
        Ok(())
    }
}

impl SavedOperatorSettings {
    /// Whether this policy was explicitly present in settings.toml.
    pub(super) fn contains(&self, key: &str) -> bool {
        match key {
            "admin_login_fail_limit" => self.admin_login_fail_limit.is_some(),
            "admin_login_fail_window_secs" => self.admin_login_fail_window_secs.is_some(),
            "board_password_fail_limit" => self.board_password_fail_limit.is_some(),
            "board_password_fail_window_secs" => self.board_password_fail_window_secs.is_some(),
            "board_access_cookie_days" => self.board_access_cookie_days.is_some(),
            "self_action_window_secs" => self.self_action_window_secs.is_some(),
            "index_threads_per_page" => self.index_threads_per_page.is_some(),
            "index_reply_previews" => self.index_reply_previews.is_some(),
            "read_timeout_secs" => self.read_timeout_secs.is_some(),
            "write_timeout_secs" => self.write_timeout_secs.is_some(),
            "log_filter" => self.log_filter.is_some(),
            _ => false,
        }
    }
}

/// Additional access settings.
pub static ACCESS_SETTINGS: &[SettingDefinition] = &[
    SettingDefinition { application: crate::config::admin::ApplicationMode::Restart, key: "admin_login_fail_limit", label: "Administrator failed-login allowance", help: "Failed login attempts per visitor before lockout; account reauthentication uses the same protection.", environment: "CHAN_ADMIN_LOGIN_FAIL_LIMIT", kind: InputKind::Number(1, 100), value: |c| c.operator.admin_login_fail_limit.to_string() },
    SettingDefinition { application: crate::config::admin::ApplicationMode::Restart, key: "admin_login_fail_window_secs", label: "Administrator failed-login window (seconds)", help: "Separate from browsing limits. Counter resets after this interval.", environment: "CHAN_ADMIN_LOGIN_FAIL_WINDOW_SECS", kind: InputKind::Number(30, 86400), value: |c| c.operator.admin_login_fail_window_secs.to_string() },
    SettingDefinition { application: crate::config::admin::ApplicationMode::Restart, key: "board_password_fail_limit", label: "Board-password failed-attempt allowance", help: "Per visitor and board; leaves browsing and administrator protections separate.", environment: "CHAN_BOARD_PASSWORD_FAIL_LIMIT", kind: InputKind::Number(1, 100), value: |c| c.operator.board_password_fail_limit.to_string() },
    SettingDefinition { application: crate::config::admin::ApplicationMode::Restart, key: "board_password_fail_window_secs", label: "Board-password failed-attempt window (seconds)", help: "Board unlock counters expire after this interval.", environment: "CHAN_BOARD_PASSWORD_FAIL_WINDOW_SECS", kind: InputKind::Number(30, 86400), value: |c| c.operator.board_password_fail_window_secs.to_string() },
    SettingDefinition { application: crate::config::admin::ApplicationMode::Restart, key: "board_access_cookie_days", label: "Board-password access lifetime (days)", help: "Browser cookie lifetime for new password grants. Existing browser cookies retain their issued expiry; password or secret changes revoke grants.", environment: "CHAN_BOARD_ACCESS_COOKIE_DAYS", kind: InputKind::Number(1, 365), value: |c| c.operator.board_access_cookie_days.to_string() },
    SettingDefinition { application: crate::config::admin::ApplicationMode::Restart, key: "self_action_window_secs", label: "Self-edit and self-delete window (seconds)", help: "Signed ownership grants and server authorization use this window. Board permissions still apply; legacy edit_window_secs remains inactive.", environment: "CHAN_SELF_ACTION_WINDOW_SECS", kind: InputKind::Number(1, 3600), value: |c| c.operator.self_action_window_secs.to_string() },
];

/// Public index display preferences.
pub static DISPLAY_SETTINGS: &[SettingDefinition] = &[
    SettingDefinition {
        application: crate::config::admin::ApplicationMode::Restart,
        key: "index_threads_per_page",
        label: "Board index threads per page",
        help: "Index pagination only. Catalog behavior is unchanged.",
        environment: "CHAN_INDEX_THREADS_PER_PAGE",
        kind: InputKind::Number(1, 100),
        value: |c| c.operator.index_threads_per_page.to_string(),
    },
    SettingDefinition {
        application: crate::config::admin::ApplicationMode::Restart,
        key: "index_reply_previews",
        label: "Board index reply previews",
        help: "Latest reply previews per thread; 0 hides previews.",
        environment: "CHAN_INDEX_REPLY_PREVIEWS",
        kind: InputKind::Number(0, 20),
        value: |c| c.operator.index_reply_previews.to_string(),
    },
];

/// Normal request deadline preferences.
pub static TIMEOUT_SETTINGS: &[SettingDefinition] = &[
    SettingDefinition {
        application: crate::config::admin::ApplicationMode::Restart,
        key: "read_timeout_secs",
        label: "Normal GET/HEAD timeout (seconds)",
        help: "Uploads, backup, restore and maintenance retain their existing timeout exemptions.",
        environment: "CHAN_READ_TIMEOUT_SECS",
        kind: InputKind::Number(1, 300),
        value: |c| c.operator.read_timeout_secs.to_string(),
    },
    SettingDefinition {
        application: crate::config::admin::ApplicationMode::Restart,
        key: "write_timeout_secs",
        label: "Normal write timeout (seconds)",
        help: "Other methods use this timeout; existing operation exemptions remain available.",
        environment: "CHAN_WRITE_TIMEOUT_SECS",
        kind: InputKind::Number(1, 3600),
        value: |c| c.operator.write_timeout_secs.to_string(),
    },
];

/// Startup tracing filter.
pub static LOG_SETTINGS: &[SettingDefinition] = &[
    SettingDefinition { application: crate::config::admin::ApplicationMode::Restart, key: "log_filter", label: "Log filter / verbosity", help: "Tracing directives such as info,rustchan=debug. Blank keeps the built-in INFO application/dependency filter. RUST_LOG takes precedence. Restart required; excessive detail can increase log volume.", environment: "RUST_LOG", kind: InputKind::OptionalText, value: |c| c.operator.log_filter.clone().unwrap_or_else(|| "Built-in INFO filter".to_owned()) },
];
