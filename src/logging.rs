// logging.rs — Structured logging initialisation.
//
// Two output channels:
//
//   1. Terminal (stdout, TTY-aware)
//      • When stdout is a TTY (interactive terminal):
//          HH:MM:SS.mmm [LEVEL] [component] message  key=val …
//        Coloured level tags; component tag in cyan; fits in 80 columns.
//      • When stdout is not a TTY (piped, systemd, Docker, nohup):
//          YYYY-MM-DD HH:MM:SS.mmm [LEVEL] [component] message  key=val …
//        Zero ANSI codes — clean for log shippers and `grep`.
//
//      All writes go through `CONSOLE_MUTEX` so `console.rs` interactive
//      output never interleaves with log events.
//
//   2. Log file (logs/rustchan.YYYY-MM-DD.log, human-readable text, daily rotation)
//      Same fixed-column format as the non-TTY terminal output, with two extras:
//        • Millisecond precision on every timestamp.
//        • WARN and ERROR lines append  (src/file.rs:line)  at the end so you
//          can jump straight to the source without grepping the codebase.
//      One event per line — easy to tail, grep, and read in any text editor.
//      If you need machine-parseable output for a log shipper (Loki, Datadog,
//      etc.) swap the FileFormatter layer for .json() — see the comment in
//      init_logging().
//
// `CONSOLE_MUTEX`
// ───────────────
// A `parking_lot::Mutex<()>` used as a coordinated write-lock on stdout.
// Both this module's `ConsoleLock` `MakeWriter` (used by the tracing
// terminal layer) and `console.rs`'s helpers acquire this same lock before
// writing. This eliminates byte-level interleaving between log events and
// stats/prompt output regardless of concurrent async activity.
//
// `IS_TTY`
// ────────
// Set once during `init_logging()` via `std::io::IsTerminal`.
// Read at every log event and every `console.rs` write to decide whether
// to emit ANSI escape codes. Exposed via `is_tty()`.
//
// Respects the RUST_LOG environment variable if set.

use std::fmt;
use std::io::{self, Write as _};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, OnceLock};
use std::time::{Duration, Instant};

use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::fmt::format::{FormatEvent, FormatFields, Writer};
use tracing_subscriber::fmt::{FmtContext, MakeWriter};
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::{layer::SubscriberExt as _, util::SubscriberInitExt as _, EnvFilter};

// ─── Shared console write lock ────────────────────────────────────────────────

/// Global mutex that serialises all stdout writes across the process.
///
/// Both the tracing terminal layer (via [`ConsoleLock`]) and the `console.rs`
/// interactive helpers acquire this before writing. The result: log events
/// and stats/prompt blocks never interleave, even under high async concurrency.
static CONSOLE_MUTEX: LazyLock<parking_lot::Mutex<()>> =
    LazyLock::new(|| parking_lot::Mutex::new(()));

// ─── Non-blocking file writer guard ─────────────────────────────────────────────
//
// `tracing_appender::non_blocking()` spawns a background thread that drains a
// channel and flushes writes to disk after every batch.  The returned
// `WorkerGuard` MUST stay alive for the entire process lifetime — dropping it
// stops the background thread, which silently discards any buffered log lines
// that have not yet been flushed, producing blank or truncated log files.
//
// Using a module-level OnceLock means the guard is stored here without
// requiring callers to thread it through their own state, and without changing
// the public `init_logging(&Path)` signature.
static FILE_GUARD: OnceLock<tracing_appender::non_blocking::WorkerGuard> = OnceLock::new();

// ─── TTY detection ────────────────────────────────────────────────────────────

static IS_TTY: AtomicBool = AtomicBool::new(false);
static ANSI_ENABLED: AtomicBool = AtomicBool::new(false);

/// Returns `true` when stdout was a real interactive terminal at startup.
///
/// Used by the log formatter and `console.rs` to decide whether to emit ANSI
/// escape codes. Always `false` when output is piped, redirected, or captured
/// by a process supervisor like systemd.
pub fn is_tty() -> bool {
    IS_TTY.load(Ordering::Relaxed)
}

/// Returns `true` when stdout is an interactive terminal that can consume ANSI
/// escape sequences. On Windows this also probes/enables virtual terminal
/// processing so raw colour sequences are not printed literally.
pub fn ansi_enabled() -> bool {
    ANSI_ENABLED.load(Ordering::Relaxed)
}

#[cfg(windows)]
fn detect_ansi_enabled(tty: bool) -> bool {
    if !tty {
        return false;
    }

    crossterm::ansi_support::supports_ansi()
}

#[cfg(not(windows))]
const fn detect_ansi_enabled(tty: bool) -> bool {
    tty
}

/// Set to `true` once the full-screen TUI alternate screen is active.
/// Any code that would print banners or boxes to stdout (e.g. the Tor onion
/// address box in detect.rs) must check this and skip its output — the TUI
/// dashboard owns the screen and will display the information itself.
static TUI_ACTIVE: AtomicBool = AtomicBool::new(false);

const TOR_DESCRIPTOR_TIMEOUT_SUPPRESSION_WINDOW: Duration = Duration::from_secs(10 * 60);
static TOR_DESCRIPTOR_TIMEOUT_LIMITER: LazyLock<parking_lot::Mutex<TorDescriptorTimeoutLimiter>> =
    LazyLock::new(|| {
        parking_lot::Mutex::new(TorDescriptorTimeoutLimiter::new(
            TOR_DESCRIPTOR_TIMEOUT_SUPPRESSION_WINDOW,
        ))
    });

pub fn set_tui_active(v: bool) {
    TUI_ACTIVE.store(v, Ordering::SeqCst);
}

pub fn is_tui_active() -> bool {
    TUI_ACTIVE.load(Ordering::Relaxed)
}

// ─── Component name extraction ────────────────────────────────────────────────

/// Extract the useful short name from a tracing target (module path).
///
/// `rustchan::server::server` → `server`
/// `rustchan::db::mod`        → `db`
/// `rustchan::workers`        → `workers`
/// `tower_http::trace`        → `trace`
fn extract_component(target: &str) -> &str {
    let mut parts = target.rsplit("::");
    let last = parts.next().unwrap_or(target);
    if last == "mod" {
        parts.next().unwrap_or(last)
    } else {
        last
    }
}

fn display_component(target: &str) -> String {
    if is_tor_target(target) {
        match extract_component(target) {
            "bootstrap" | "dirmgr" | "state" | "sqlite" => "tor-dir".to_owned(),
            "guard" | "guardmgr" => "tor-net".to_owned(),
            "chanmgr" => "tor-chan".to_owned(),
            "circmgr" | "mgr" | "reactor" => "tor-circ".to_owned(),
            "hspool" | "publish" | "descriptor" => "onion".to_owned(),
            "config" => "tor-cfg".to_owned(),
            other => other.to_owned(),
        }
    } else {
        extract_component(target).to_owned()
    }
}

// ─── Shared formatting helpers ────────────────────────────────────────────────

/// Write the fixed-width level tag.  Returns the ANSI open/close codes for
/// the tag (empty strings when `ansi` is false).
fn write_level_tag(writer: &mut Writer<'_>, level: Level, ansi: bool) -> fmt::Result {
    let (tag, open, close) = if ansi {
        match level {
            Level::ERROR => ("[ERROR]", "\x1b[1;31m", "\x1b[0m"),
            Level::WARN => ("[WARN ]", "\x1b[33m", "\x1b[0m"),
            Level::INFO => ("[INFO ]", "", ""),
            Level::DEBUG => ("[DEBUG]", "\x1b[2m", "\x1b[0m"),
            Level::TRACE => ("[TRACE]", "\x1b[2m", "\x1b[0m"),
        }
    } else {
        match level {
            Level::ERROR => ("[ERROR]", "", ""),
            Level::WARN => ("[WARN ]", "", ""),
            Level::INFO => ("[INFO ]", "", ""),
            Level::DEBUG => ("[DEBUG]", "", ""),
            Level::TRACE => ("[TRACE]", "", ""),
        }
    };
    write!(writer, "{open}{tag}{close} ")
}

/// Write the fixed-width (8-char) component tag, cyan when `ansi` is true.
fn write_component_tag(writer: &mut Writer<'_>, target: &str, ansi: bool) -> fmt::Result {
    let component = display_component(target);
    let chars: Vec<char> = component.chars().collect();
    let len = chars.len().min(8);
    let display: String = chars.get(..len).unwrap_or(&chars).iter().collect();

    let (open, close) = if ansi {
        ("\x1b[36m", "\x1b[0m")
    } else {
        ("", "")
    };
    write!(writer, "{open}[{display:<8}]{close} ")
}

fn strip_ansi(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' && chars.next_if_eq(&'[').is_some() {
            for next in chars.by_ref() {
                if ('@'..='~').contains(&next) {
                    break;
                }
            }
            continue;
        }

        out.push(ch);
    }

    out
}

fn humanize_field_name(name: &str) -> String {
    match name {
        "addr" => "address".to_owned(),
        "admin_id" => "admin ID".to_owned(),
        "archived_cap" => "archive limit".to_owned(),
        "board_id" => "board ID".to_owned(),
        "bytes" => "size".to_owned(),
        "error" => "error".to_owned(),
        "failure_rate" => "failure rate".to_owned(),
        "files_removed" => "files removed".to_owned(),
        "freed_kib" => "freed KiB".to_owned(),
        "has_csrf_cookie" => "CSRF cookie".to_owned(),
        "has_session_cookie" => "session cookie".to_owned(),
        "attempts" => "attempts".to_owned(),
        "reason" => "reason".to_owned(),
        "uri" => "URI".to_owned(),
        "id" => "ID".to_owned(),
        "latency_ms" => "latency".to_owned(),
        "mime" => "MIME type".to_owned(),
        "post_id" => "post ID".to_owned(),
        "remaining_kib" => "remaining KiB".to_owned(),
        "retry_in" => "retry in".to_owned(),
        "saved_as" => "saved as".to_owned(),
        "thread_id" => "thread ID".to_owned(),
        "thumb" => "thumbnail".to_owned(),
        "url" => "URL".to_owned(),
        other => other.replace('_', " "),
    }
}

fn is_external_source(file: &str) -> bool {
    file.contains("/.cargo/registry/src/") || file.contains("/rustc/")
}

fn is_tor_component(component: &str) -> bool {
    matches!(
        component,
        "bootstrap"
            | "chanmgr"
            | "circmgr"
            | "config"
            | "descriptor"
            | "dirmgr"
            | "guard"
            | "guardmgr"
            | "hspool"
            | "lib"
            | "mgr"
            | "publish"
            | "reactor"
            | "sqlite"
            | "state"
    )
}

fn is_tor_target(target: &str) -> bool {
    target == "arti_client"
        || target.starts_with("arti_client::")
        || target.starts_with("tor_")
        || target.starts_with("tor-")
}

fn title_case_message(message: &str) -> String {
    let mut chars = message.chars();
    match chars.next() {
        Some(first) if first.is_ascii_lowercase() => {
            format!("{}{}", first.to_ascii_uppercase(), chars.as_str())
        }
        _ => message.to_owned(),
    }
}

fn parse_formatted_duration(value: &str) -> Option<String> {
    let inner = value
        .strip_prefix("FormattedDuration(")?
        .strip_suffix(')')?
        .strip_suffix('s')?;
    let seconds = inner.parse::<f64>().ok()?;

    if seconds >= 60.0 {
        let total = std::time::Duration::from_secs_f64(seconds.round()).as_secs();
        let minutes = total / 60;
        let secs = total % 60;
        if secs == 0 {
            Some(format!("{minutes}m"))
        } else {
            Some(format!("{minutes}m {secs}s"))
        }
    } else if seconds >= 10.0 {
        Some(format!(
            "{}s",
            std::time::Duration::from_secs_f64(seconds.round()).as_secs()
        ))
    } else {
        Some(format!("{seconds:.1}s"))
    }
}

fn format_duration_parts(total_nanos: u128) -> String {
    let total_millis = (total_nanos.saturating_add(500_000)) / 1_000_000;
    if total_millis == 0 {
        return "0s".to_owned();
    }

    let total_secs = (total_millis.saturating_add(500)) / 1000;
    if total_secs >= 60 {
        let minutes = total_secs / 60;
        let secs = total_secs % 60;
        if secs == 0 {
            format!("{minutes}m")
        } else {
            format!("{minutes}m {secs}s")
        }
    } else if total_secs >= 10 {
        format!("{total_secs}s")
    } else {
        format!(
            "{}.{:01}s",
            total_millis / 1000,
            (total_millis % 1000) / 100
        )
    }
}

fn parse_compound_duration(value: &str) -> Option<String> {
    let trimmed = value.trim().trim_end_matches('.');
    if !trimmed.contains(char::is_whitespace) {
        return None;
    }

    let mut total_nanos = 0u128;
    let mut saw_unit = false;
    for part in trimmed.split_whitespace() {
        let split = part
            .find(|ch: char| !ch.is_ascii_digit())
            .filter(|&index| index > 0)?;
        let (number, unit) = part.split_at(split);
        let value = number.parse::<u128>().ok()?;
        let nanos = match unit {
            "h" => value.saturating_mul(3_600_000_000_000),
            "m" => value.saturating_mul(60_000_000_000),
            "s" => value.saturating_mul(1_000_000_000),
            "ms" => value.saturating_mul(1_000_000),
            "us" | "µs" => value.saturating_mul(1_000),
            "ns" => value,
            _ => return None,
        };
        total_nanos = total_nanos.saturating_add(nanos);
        saw_unit = true;
    }

    saw_unit.then(|| format_duration_parts(total_nanos))
}

fn normalize_duration(value: &str) -> Option<String> {
    parse_formatted_duration(value).or_else(|| parse_compound_duration(value))
}

fn scrub_tor_value(value: &str) -> String {
    let mut out = value.to_owned();

    while let Some(start) = out.find("GuardId(") {
        let Some(end) = out[start..].find(')') else {
            break;
        };
        out.replace_range(start..=(start + end), "[scrubbed guard]");
    }

    while let Some(start) = out.find(" via Circ ") {
        let token_value_start = start + " via Circ ".len();
        let end = out[token_value_start..]
            .find(char::is_whitespace)
            .map_or(out.len(), |offset| token_value_start + offset);
        out.replace_range(start..end, "");
    }

    out
}

fn normalize_field_value(name: &str, value: &str) -> String {
    if let Some(duration) = normalize_duration(value) {
        return duration;
    }

    if name == "guard" && value.starts_with("GuardId(") {
        return "[scrubbed]".to_owned();
    }

    if name == "latency_ms" {
        return format!("{value} ms");
    }

    scrub_tor_value(value)
}

fn should_hide_field(name: &str, value: &str) -> bool {
    (matches!(value, "" | "<missing>")
        && matches!(name, "content_type" | "content_length" | "mime" | "path"))
        || (name == "guard" && value == "[scrubbed]")
}

#[derive(Default)]
struct LogEventFields {
    message: Option<String>,
    fields: Vec<(String, String)>,
}

impl LogEventFields {
    fn push_field(&mut self, field: &Field, value: &str) {
        let clean = normalize_field_value(field.name(), &strip_ansi(value.trim()));
        if field.name() == "message" {
            if !clean.is_empty() {
                self.message = Some(clean);
            }
            return;
        }
        if should_hide_field(field.name(), &clean) {
            return;
        }
        self.fields.push((field.name().to_owned(), clean));
    }
}

impl Visit for LogEventFields {
    fn record_str(&mut self, field: &Field, value: &str) {
        self.push_field(field, value);
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        self.push_field(field, if value { "yes" } else { "no" });
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        self.push_field(field, &value.to_string());
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.push_field(field, &value.to_string());
    }

    fn record_i128(&mut self, field: &Field, value: i128) {
        self.push_field(field, &value.to_string());
    }

    fn record_u128(&mut self, field: &Field, value: u128) {
        self.push_field(field, &value.to_string());
    }

    fn record_f64(&mut self, field: &Field, value: f64) {
        self.push_field(field, &value.to_string());
    }

    fn record_error(&mut self, field: &Field, value: &(dyn std::error::Error + 'static)) {
        self.push_field(field, &value.to_string());
    }

    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        self.push_field(field, &format!("{value:?}"));
    }
}

fn upsert_field(fields: &mut Vec<(String, String)>, name: &str, value: String) {
    if let Some((_, existing)) = fields.iter_mut().find(|(field_name, _)| field_name == name) {
        *existing = value;
    } else {
        fields.push((name.to_owned(), value));
    }
}

fn remove_field(fields: &mut Vec<(String, String)>, name: &str) -> Option<String> {
    fields
        .iter()
        .position(|(field_name, _)| field_name == name)
        .map(|index| fields.remove(index).1)
}

fn extract_percent(message: &str) -> Option<String> {
    let percent_index = message.find('%')?;
    let number = message[..percent_index]
        .rsplit_once(' ')
        .map_or(&message[..percent_index], |(_, value)| value);
    Some(format!("{number}%"))
}

fn extract_attempt_count(message: &str) -> Option<String> {
    let before = message
        .strip_prefix("We failed ")?
        .split_once(" times to bootstrap")?
        .0;
    before.parse::<u64>().ok().map(|count| count.to_string())
}

fn short_tor_error(error: &str) -> String {
    let lower = error.to_ascii_lowercase();
    if lower.contains("invalid document from directory server") {
        "directory server sent invalid data".to_owned()
    } else if lower.contains("timed out") || lower.contains("timeout") {
        "network timeout".to_owned()
    } else if lower.contains("unusable guard") || lower.contains("could not connect to guard") {
        "could not connect to a Tor guard".to_owned()
    } else if lower.contains("consensus") && lower.contains("expired") {
        "network directory consensus expired".to_owned()
    } else {
        scrub_tor_value(error)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TorDescriptorTimeoutDecision {
    EmitOriginal,
    Suppress,
    EmitSummary { suppressed: u64, window: Duration },
}

#[derive(Debug)]
struct TorDescriptorTimeoutLimiter {
    window: Duration,
    window_started_at: Option<Instant>,
    suppressed: u64,
    duplicate_formatter_decision: Option<TorDescriptorTimeoutDecision>,
}

impl TorDescriptorTimeoutLimiter {
    const fn new(window: Duration) -> Self {
        Self {
            window,
            window_started_at: None,
            suppressed: 0,
            duplicate_formatter_decision: None,
        }
    }

    fn decide(&mut self, now: Instant) -> TorDescriptorTimeoutDecision {
        // The terminal and file formatters both see the same tracing event.
        // Reuse one decision for the second formatter so the limiter counts
        // the event once while keeping both outputs consistent.
        if let Some(decision) = self.duplicate_formatter_decision.take() {
            return decision;
        }

        let decision = self.decide_once(now);
        self.duplicate_formatter_decision = Some(decision);
        decision
    }

    fn decide_once(&mut self, now: Instant) -> TorDescriptorTimeoutDecision {
        let Some(window_started_at) = self.window_started_at else {
            self.window_started_at = Some(now);
            return TorDescriptorTimeoutDecision::EmitOriginal;
        };

        if now.duration_since(window_started_at) >= self.window {
            let suppressed = self.suppressed;
            self.window_started_at = Some(now);
            self.suppressed = 0;
            if suppressed == 0 {
                TorDescriptorTimeoutDecision::EmitOriginal
            } else {
                TorDescriptorTimeoutDecision::EmitSummary {
                    suppressed,
                    window: self.window,
                }
            }
        } else {
            self.suppressed = self.suppressed.saturating_add(1);
            TorDescriptorTimeoutDecision::Suppress
        }
    }
}

fn is_tor_descriptor_upload_timeout_event(target: &str, fields: &LogEventFields) -> bool {
    if !is_tor_target(target) {
        return false;
    }

    let message = fields.message.as_deref().unwrap_or_default();
    let message_lower = message.to_ascii_lowercase();
    let joined_fields = fields
        .fields
        .iter()
        .map(|(_, value)| value.as_str())
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase();
    let haystack = format!("{message_lower} {joined_fields}");

    haystack.contains("descriptor")
        && (haystack.contains("failed to upload descriptor")
            || haystack.contains("unable to upload")
            || haystack.contains("descriptor upload"))
        && (haystack.contains("timeout exceeded")
            || haystack.contains("timed out")
            || haystack.contains("network timeout"))
}

fn apply_tor_descriptor_timeout_rate_limit(fields: &mut LogEventFields) -> bool {
    let decision = TOR_DESCRIPTOR_TIMEOUT_LIMITER.lock().decide(Instant::now());
    match decision {
        TorDescriptorTimeoutDecision::EmitOriginal => true,
        TorDescriptorTimeoutDecision::Suppress => false,
        TorDescriptorTimeoutDecision::EmitSummary { suppressed, window } => {
            fields.message = Some(format!(
                "Tor descriptor upload timeouts are still occurring; suppressed {suppressed} repeated warnings in the last {} minutes",
                window.as_secs() / 60
            ));
            fields.fields.clear();
            true
        }
    }
}

fn normalize_message_text(message: &str) -> String {
    let mut cleaned = message.trim().replace('—', "-").replace('…', "...");
    if cleaned.contains('→') {
        cleaned = cleaned.replace('→', " to ");
    }
    while cleaned.contains("  ") {
        cleaned = cleaned.replace("  ", " ");
    }
    title_case_message(&cleaned)
}

// This function/module is intentionally long; splitting it further would make the routing or template flow harder to follow.
#[expect(clippy::too_many_lines)]
fn rewrite_message(target: &str, file: Option<&str>, fields: &mut LogEventFields) {
    let Some(message) = fields.message.clone() else {
        return;
    };

    let component = extract_component(target);
    fields.message = Some(normalize_message_text(&message));

    match component {
        "guard" => {
            if let Some((_, retry)) = message.split_once("Retrying in ") {
                let retry = retry.trim_end_matches('.');
                let retry = normalize_duration(retry).unwrap_or_else(|| retry.to_owned());
                fields.message = Some("Tor guard connection failed".to_owned());
                upsert_field(&mut fields.fields, "retry_in", retry);
            } else if message.contains("Next retry time unknown") {
                fields.message = Some("Tor guard connection failed".to_owned());
                upsert_field(&mut fields.fields, "retry_in", "unknown".to_owned());
            } else if message.starts_with("Questionable guard:") {
                fields.message = Some("Tor marked a guard as unstable".to_owned());
                if let Some(rate) = extract_percent(&message) {
                    upsert_field(&mut fields.fields, "failure_rate", rate);
                }
            } else if message.starts_with("Disabling guard:") {
                fields.message = Some("Tor disabled an unstable guard".to_owned());
                if let Some(rate) = extract_percent(&message) {
                    upsert_field(&mut fields.fields, "failure_rate", rate);
                }
            }
        }
        "hspool" => {
            if message == "Too many preemptive onion service circuits failed; waiting a while." {
                fields.message = Some(
                    "Tor onion-service circuits are failing; waiting before retrying".to_owned(),
                );
            } else if message.starts_with("unknown vanguard mode") {
                fields.message = Some("Tor onion-service vanguard mode is unknown".to_owned());
            }
        }
        "reactor" => {
            if message.eq_ignore_ascii_case("removing circuit leg") {
                fields.message = Some("Tor circuit closed".to_owned());
                remove_field(&mut fields.fields, "tunnel_id");
            } else if message.contains("descriptor upload")
                || message.contains("Unable to upload")
                || message.contains("publish")
            {
                fields.message =
                    Some(normalize_message_text(&message).replace("HS", "onion service"));
            }
        }
        "bootstrap" => {
            if message == "Unable to advance downloading state" {
                fields.message = Some("Tor directory bootstrap stalled".to_owned());
            } else if message.starts_with("We failed ") && message.contains("times to bootstrap") {
                fields.message = Some("Tor directory bootstrap failed".to_owned());
                if let Some(attempts) = extract_attempt_count(&message) {
                    upsert_field(&mut fields.fields, "attempts", attempts);
                }
            }
        }
        "lib" => {
            if message == "Bootstrapping task exited before finishing." {
                fields.message =
                    Some("Tor directory bootstrap stopped before finishing".to_owned());
            } else if message == "Got a new NetDir, but it's older than the one we currently have!"
            {
                fields.message = Some(
                    "Tor received an older network directory; keeping current copy".to_owned(),
                );
            }
        }
        "state" => {
            if message.starts_with("Problem with certificate received from") {
                fields.message = Some("Tor directory certificate was rejected".to_owned());
            } else if message.starts_with("Discarding certificates") {
                fields.message =
                    Some("Tor discarded unrequested directory certificates".to_owned());
            } else if message.starts_with("Received microdescriptor") {
                fields.message = Some("Tor discarded an unrequested relay descriptor".to_owned());
            } else if message == "Found a mismatched microdescriptor in cache; ignoring" {
                fields.message =
                    Some("Tor ignored a mismatched cached relay descriptor".to_owned());
            }
        }
        "sqlite" if message.starts_with("Removing unreferenced file") => {
            fields.message = Some("Tor removed an unreferenced cache file".to_owned());
        }
        "mgr" => {
            if message == "All tunnel attempts failed due to timeout" {
                fields.message = Some("Tor circuit build timed out".to_owned());
            } else if message == "Reached circuit build retry limit, exiting..." {
                fields.message = Some("Tor circuit build retry limit reached".to_owned());
            } else if message == "Request failed" {
                fields.message = Some("Tor circuit request failed".to_owned());
            }
        }
        _ => {}
    }

    if message.starts_with("Error while adding directory info") {
        fields.message = Some("Tor directory update failed".to_owned());
        if let Some(error) = remove_field(&mut fields.fields, "error") {
            upsert_field(&mut fields.fields, "reason", short_tor_error(&error));
        }
    }

    if let Some(file) = file {
        if (is_external_source(file) || is_tor_target(target)) && is_tor_component(component) {
            if let Some(msg) = fields.message.as_mut() {
                if !msg.starts_with("Tor ") {
                    *msg = format!("Tor: {msg}");
                }
            }
        }
    }
}

fn should_suppress_event(target: &str, fields: &LogEventFields) -> bool {
    matches!(
        (extract_component(target), fields.message.as_deref()),
        (
            "hspool",
            Some("Tor onion-service circuits are failing; waiting before retrying")
        )
    )
}

fn prepare_event_fields(
    target: &str,
    file: Option<&str>,
    event: &Event<'_>,
) -> Option<LogEventFields> {
    let mut fields = LogEventFields::default();
    event.record(&mut fields);
    rewrite_message(target, file, &mut fields);
    if *event.metadata().level() == Level::WARN
        && is_tor_descriptor_upload_timeout_event(target, &fields)
        && !apply_tor_descriptor_timeout_rate_limit(&mut fields)
    {
        return None;
    }
    (!should_suppress_event(target, &fields)).then_some(fields)
}

fn write_event_fields(writer: &mut Writer<'_>, fields: &LogEventFields) -> fmt::Result {
    let has_message = fields.message.is_some();

    if let Some(message) = fields.message.as_deref() {
        write!(writer, "{message}")?;
    }

    if !fields.fields.is_empty() {
        if has_message {
            write!(writer, " - ")?;
        }

        for (index, (name, value)) in fields.fields.iter().enumerate() {
            if index > 0 {
                write!(writer, ", ")?;
            }
            write!(writer, "{}: {}", humanize_field_name(name), value)?;
        }
    }

    Ok(())
}

// ─── Terminal formatter ───────────────────────────────────────────────────────

/// Writes one compact line per log event to the terminal.
///
/// TTY mode (local dev):
///   `14:22:01.123 [INFO ] [server  ] HTTP server listening  addr=0.0.0.0:8080`
///
/// Non-TTY mode (piped / systemd / Docker):
///   `2026-03-18 14:22:01.123 [INFO ] [server  ] HTTP server listening  addr=0.0.0.0:8080`
///
/// Columns are fixed-width so the message text always starts at the same
/// horizontal position, making it easy to scan down a busy log stream.
struct TerminalFormatter;

impl<S, N> FormatEvent<S, N> for TerminalFormatter
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    N: for<'a> FormatFields<'a> + 'static,
{
    fn format_event(
        &self,
        ctx: &FmtContext<'_, S, N>,
        mut writer: Writer<'_>,
        event: &Event<'_>,
    ) -> fmt::Result {
        let tty = IS_TTY.load(Ordering::Relaxed);
        let ansi = ANSI_ENABLED.load(Ordering::Relaxed);
        let meta = event.metadata();
        let Some(fields) = prepare_event_fields(meta.target(), meta.file(), event) else {
            return Ok(());
        };

        // ── Timestamp (with milliseconds) ─────────────────────────────────────
        let now = chrono::Local::now();
        if tty {
            write!(writer, "{} ", now.format("%H:%M:%S%.3f"))?;
        } else {
            write!(writer, "{} ", now.format("%Y-%m-%d %H:%M:%S%.3f"))?;
        }

        // ── Level + component columns ─────────────────────────────────────────
        let level = *meta.level();
        write_level_tag(&mut writer, level, ansi)?;
        write_component_tag(&mut writer, meta.target(), ansi)?;

        // ── Message and structured fields ─────────────────────────────────────
        // tracing_subscriber writes the `message` field first, then all other
        // key=value fields separated by spaces — e.g.:
        //   "Request received  method=GET path=/b/ latency_ms=4"
        let _ = ctx;
        write_event_fields(&mut writer, &fields)?;
        writeln!(writer)
    }
}

// ─── File formatter ───────────────────────────────────────────────────────────

/// Writes one human-readable line per log event to the log file.
///
/// Format:
///   `2026-03-18 14:22:01.123 [INFO ] [server  ] HTTP server listening  addr=0.0.0.0:8080`
///   `2026-03-18 14:22:01.456 [ERROR] [error   ] DB query failed  err=no such table  (src/db/posts.rs:79)`
///
/// Differences from the terminal format:
///   • Always UTC with the full date — the file is an archive, not a live view.
///   • No ANSI colour codes — clean for `grep`, `less`, and text editors.
///   • WARN and ERROR lines append `(file:line)` at the end so you can jump
///     directly to the call site without having to search the source tree.
struct FileFormatter;

impl<S, N> FormatEvent<S, N> for FileFormatter
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    N: for<'a> FormatFields<'a> + 'static,
{
    fn format_event(
        &self,
        ctx: &FmtContext<'_, S, N>,
        mut writer: Writer<'_>,
        event: &Event<'_>,
    ) -> fmt::Result {
        let meta = event.metadata();
        let _ = ctx;
        let Some(fields) = prepare_event_fields(meta.target(), meta.file(), event) else {
            return Ok(());
        };

        // ── Timestamp — server-local, full date, millisecond precision ───────
        let now = chrono::Local::now();
        write!(writer, "{} ", now.format("%Y-%m-%d %H:%M:%S%.3f"))?;

        // ── Level + component columns (no colour) ─────────────────────────────
        let level = *event.metadata().level();
        write_level_tag(&mut writer, level, false)?;
        write_component_tag(&mut writer, meta.target(), false)?;

        // ── Message and structured key=value fields ───────────────────────────
        write_event_fields(&mut writer, &fields)?;

        // ── Source location suffix for WARN and ERROR ─────────────────────────
        // Only attached at these levels because:
        //   • INFO/DEBUG events fire thousands of times per minute on a busy
        //     board; the call site is rarely the interesting part.
        //   • WARN/ERROR events are rare and almost always need follow-up —
        //     having the exact file:line avoids a grep → blame cycle.
        if matches!(level, Level::ERROR | Level::WARN) {
            if let (Some(file), Some(line)) = (meta.file(), meta.line()) {
                if is_external_source(file) {
                    writeln!(writer)?;
                    return Ok(());
                }
                // Trim the leading "src/" that Rust adds to all file paths so
                // the suffix stays compact: "(db/posts.rs:79)" not
                // "(src/db/posts.rs:79)".
                let trimmed = file.strip_prefix("src/").unwrap_or(file);
                write!(writer, "  ({trimmed}:{line})")?;
            }
        }

        writeln!(writer)
    }
}

// ─── MakeWriter: routes terminal writes through CONSOLE_MUTEX ─────────────────

/// Holds the console lock guard for the duration of one log event write.
///
/// The `_guard` field is held solely for its `Drop` side-effect (releasing
/// `CONSOLE_MUTEX`). It is never accessed directly.
struct LockedWriter {
    _guard: parking_lot::MutexGuard<'static, ()>,
    suppress: bool,
}

impl io::Write for LockedWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.suppress {
            return Ok(buf.len());
        }
        io::stdout().write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        if self.suppress {
            return Ok(());
        }
        io::stdout().flush()
    }
}

/// `MakeWriter` that returns a [`LockedWriter`] for each log event.
///
/// The tracing layer calls `make_writer()` once per event, writes through
/// the returned writer, then drops it — atomically releasing the stdout lock.
struct ConsoleLock;

impl<'a> MakeWriter<'a> for ConsoleLock {
    type Writer = LockedWriter;

    fn make_writer(&'a self) -> LockedWriter {
        LockedWriter {
            _guard: CONSOLE_MUTEX.lock(),
            suppress: is_tui_active(),
        }
    }
}

// ─── Initialisation ───────────────────────────────────────────────────────────

/// Initialise the global tracing subscriber. Call exactly once at startup,
/// before any `tracing::info!` or `tracing::warn!` calls are made.
///
/// Detects whether stdout is an interactive terminal and stores the result
/// in `IS_TTY` for the process lifetime. Installs two layers:
///
/// 1. **Terminal layer** — `TerminalFormatter` writes via `ConsoleLock`
///    (`CONSOLE_MUTEX`). Human-readable, coloured when TTY, plain otherwise.
///    Serialised with `console.rs` output via the shared mutex.
///
/// 2. **File layer** — `FileFormatter`, daily rotation, `debug`+.
///    Written to `{log_dir}/rustchan.YYYY-MM-DD.log`.
///    One human-readable line per event; WARN/ERROR include source location.
///    Independent of the console lock; uses `tracing-appender`'s own lock.
///
/// To switch the file layer to JSON for a log aggregator (Loki, Datadog, …):
/// replace `FileFormatter` with `.json()` and add `.with_file(true)
/// .with_line_number(true)` to the layer builder.
pub fn init_logging(log_dir: &Path) {
    use std::io::IsTerminal as _;

    let tty = io::stdout().is_terminal();
    IS_TTY.store(tty, Ordering::Relaxed);
    ANSI_ENABLED.store(detect_ansi_enabled(tty), Ordering::Relaxed);

    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        EnvFilter::new(
            // F-08: Explicitly suppress Arti's internal crates in the default
            // filter. Without this they emit at DEBUG/TRACE (circuit negotiation,
            // guard selection, consensus downloads) — hundreds of lines/minute.
            // Operators who need Arti internals can set RUST_LOG=tor_proto=debug.
            "rustchan=info,\
             admin=info,\
             board=info,\
             workers=info,\
             server=info,\
             db=info,\
             startup=info,\
             sessions=info,\
             polls=info,\
             chan_net=info,\
             console=info,\
             tls=info,\
             config=info,\
             tower_http=warn,\
             arti_client=warn,\
             tor_proto=warn,\
             tor_circmgr=warn,\
             tor_dirmgr=warn,\
             tor_guardmgr=warn,\
             tor_chanmgr=warn,\
             tor_hsservice=warn,\
             tor_keymgr=warn",
        )
    });

    let terminal_layer = tracing_subscriber::fmt::layer()
        .event_format(TerminalFormatter)
        .with_writer(ConsoleLock);

    // Build the rolling file appender.
    // FIX (filename): tracing_appender::rolling::daily(dir, "rustchan.log")
    // appends the date *after* the full string → "rustchan.log.2024-01-15"
    // (no .log extension on rotated files).  The builder API separates
    // prefix from suffix → "rustchan.2024-01-15.log".
    let rolling = tracing_appender::rolling::RollingFileAppender::builder()
        .rotation(tracing_appender::rolling::Rotation::DAILY)
        .filename_prefix("rustchan")
        .filename_suffix("log")
        .build(log_dir)
        .unwrap_or_else(|e| {
            let _ = writeln!(
                std::io::stderr().lock(),
                "Warning: could not create log file appender: {e}"
            );
            tracing_appender::rolling::never(log_dir, "rustchan.log")
        });

    // FIX (blank files): wrap the appender in a non-blocking writer.
    // RollingFileAppender uses an internal BufWriter that only flushes when
    // the buffer fills to 8 KB or the appender is dropped.  On a quiet server
    // you never hit 8 KB between restarts, so the file stays empty.
    // non_blocking() moves writes to a background thread that flushes after
    // every batch, guaranteeing data reaches disk promptly.
    // The WorkerGuard is stored in FILE_GUARD so it lives for the entire
    // process — see the comment on that static for why this matters.
    let (non_blocking_writer, guard) = tracing_appender::non_blocking(rolling);
    let _ = FILE_GUARD.set(guard);

    let file_layer = tracing_subscriber::fmt::layer()
        .event_format(FileFormatter)
        .with_writer(non_blocking_writer);

    tracing_subscriber::registry()
        .with(env_filter)
        .with(terminal_layer)
        .with(file_layer)
        .init();
}

// ─── Console print helpers ────────────────────────────────────────────────────
//
// All helpers acquire `CONSOLE_MUTEX` before writing so they serialise
// correctly with the tracing terminal layer. Use these instead of
// `println!`/`print!` in `console.rs` and `detect.rs`.

/// Print `msg` followed by a newline to stdout, under the console lock.
pub fn console_println(msg: &str) {
    let _guard = CONSOLE_MUTEX.lock();
    let _ = writeln!(io::stdout(), "{msg}");
}

/// Write a raw pre-formatted block exactly as provided (no trailing newline added).
///
/// Used for banners, install-hint blocks, and stats sections that are already
/// fully formatted including their own newlines.
pub fn console_print_raw(block: &str) {
    let _guard = CONSOLE_MUTEX.lock();
    let _ = write!(io::stdout(), "{block}");
    let _ = io::stdout().flush();
}

/// Write a prompt string (no newline) and flush stdout, under the console lock.
///
/// The lock is released before returning so the subsequent blocking `read_line`
/// call does not prevent log events from being written while waiting for input.
pub fn console_prompt(msg: &str) {
    let _guard = CONSOLE_MUTEX.lock();
    let _ = write!(io::stdout(), "{msg}");
    let _ = io::stdout().flush();
    // _guard dropped here — stdin read happens outside the lock
}

#[cfg(test)]
mod tests {
    use super::{
        display_component, extract_component, humanize_field_name, is_external_source,
        is_tor_descriptor_upload_timeout_event, normalize_duration, normalize_field_value,
        normalize_message_text, parse_formatted_duration, rewrite_message, LogEventFields,
        TorDescriptorTimeoutDecision, TorDescriptorTimeoutLimiter,
    };
    use std::time::{Duration, Instant};

    fn rewrite_for_test(target: &str, message: &str, fields: &[(&str, &str)]) -> LogEventFields {
        let mut event_fields = LogEventFields {
            message: Some(message.to_owned()),
            fields: fields
                .iter()
                .filter_map(|(name, value)| {
                    let clean = normalize_field_value(name, value);
                    (!super::should_hide_field(name, &clean)).then(|| (name.to_string(), clean))
                })
                .collect(),
        };
        rewrite_message(
            target,
            Some("/Users/example/.cargo/registry/src/index.crates.io/tor-test/src/lib.rs"),
            &mut event_fields,
        );
        event_fields
    }

    #[test]
    fn parses_formatted_duration_into_plain_text() {
        assert_eq!(
            parse_formatted_duration("FormattedDuration(29.99997625s)").as_deref(),
            Some("30s")
        );
        assert_eq!(
            parse_formatted_duration("FormattedDuration(125.0s)").as_deref(),
            Some("2m 5s")
        );
        assert_eq!(
            normalize_duration("29s 999ms 988us 413ns").as_deref(),
            Some("30s")
        );
    }

    #[test]
    fn normalizes_common_field_names_and_values() {
        assert_eq!(humanize_field_name("thread_id"), "thread ID");
        assert_eq!(humanize_field_name("latency_ms"), "latency");
        assert_eq!(normalize_field_value("latency_ms", "42"), "42 ms");
        assert_eq!(normalize_field_value("guard", "GuardId(foo)"), "[scrubbed]");
        assert_eq!(
            normalize_field_value(
                "error",
                "Invalid document from directory server [scrubbed] via Circ 8.98"
            ),
            "Invalid document from directory server [scrubbed]"
        );
    }

    #[test]
    fn cleans_up_message_text() {
        assert_eq!(
            normalize_message_text("removing circuit leg"),
            "Removing circuit leg"
        );
        assert_eq!(
            normalize_message_text("image→webp: converted"),
            "Image to webp: converted"
        );
    }

    #[test]
    fn detects_external_source_locations() {
        assert!(is_external_source(
            "/Users/example/.cargo/registry/src/index.crates.io-xxx/tor-proto/src/client/reactor.rs"
        ));
        assert!(!is_external_source("src/server/server.rs"));
        assert_eq!(extract_component("rustchan::server::server"), "server");
        assert_eq!(display_component("tor_dirmgr::bootstrap"), "tor-dir");
        assert_eq!(display_component("tor_guardmgr::guard"), "tor-net");
        assert_eq!(display_component("tor_circmgr::hspool"), "onion");
    }

    #[test]
    fn rewrites_common_tor_guard_messages() {
        let fields = rewrite_for_test(
            "tor_guardmgr::guard",
            "Could not connect to guard $AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA. Retrying in 29s 999ms 988us 413ns.",
            &[],
        );

        assert_eq!(
            fields.message.as_deref(),
            Some("Tor guard connection failed")
        );
        assert_eq!(
            fields.fields,
            vec![("retry_in".to_owned(), "30s".to_owned())]
        );

        let fields = rewrite_for_test(
            "tor_guardmgr::guard",
            "Disabling guard: 72.5% of circuits died under mysterious circumstances, exceeding threshold of 70.0%",
            &[("guard", "GuardId(abc)")],
        );
        assert_eq!(
            fields.message.as_deref(),
            Some("Tor disabled an unstable guard")
        );
        assert_eq!(
            fields.fields,
            vec![("failure_rate".to_owned(), "72.5%".to_owned())]
        );
    }

    #[test]
    fn rewrites_common_tor_directory_messages() {
        let fields = rewrite_for_test(
            "tor_dirmgr::bootstrap",
            "Error while adding directory info",
            &[(
                "error",
                "Invalid document from directory server [scrubbed] via Circ 8.98",
            )],
        );
        assert_eq!(
            fields.message.as_deref(),
            Some("Tor directory update failed")
        );
        assert_eq!(
            fields.fields,
            vec![(
                "reason".to_owned(),
                "directory server sent invalid data".to_owned()
            )]
        );

        let fields = rewrite_for_test(
            "tor_dirmgr::bootstrap",
            "We failed 3 times to bootstrap a directory. We're going to give up.",
            &[],
        );
        assert_eq!(
            fields.message.as_deref(),
            Some("Tor directory bootstrap failed")
        );
        assert_eq!(fields.fields, vec![("attempts".to_owned(), "3".to_owned())]);
    }

    #[test]
    fn rewrites_common_onion_and_circuit_messages() {
        let fields = rewrite_for_test(
            "tor_circmgr::hspool",
            "Too many preemptive onion service circuits failed; waiting a while.",
            &[],
        );
        assert_eq!(
            fields.message.as_deref(),
            Some("Tor onion-service circuits are failing; waiting before retrying")
        );

        let fields = rewrite_for_test(
            "tor_proto::client::reactor",
            "removing circuit leg",
            &[("tunnel_id", "Circ 8.98"), ("reason", "closed")],
        );
        assert_eq!(fields.message.as_deref(), Some("Tor circuit closed"));
        assert_eq!(
            fields.fields,
            vec![("reason".to_owned(), "closed".to_owned())]
        );
    }

    #[test]
    fn suppresses_repetitive_onion_retry_warning() {
        let mut fields = LogEventFields {
            message: Some(
                "Too many preemptive onion service circuits failed; waiting a while.".to_owned(),
            ),
            fields: Vec::new(),
        };
        rewrite_message("tor_circmgr::hspool", Some("src/logging.rs"), &mut fields);
        assert!(super::should_suppress_event("tor_circmgr::hspool", &fields));
    }

    #[test]
    fn detects_tor_descriptor_upload_timeout_warning_only() {
        let fields = rewrite_for_test(
            "tor_hsservice::publish",
            "Failed to upload descriptor for service rustchan (redacted) - error: Timeout exceeded",
            &[],
        );
        assert!(is_tor_descriptor_upload_timeout_event(
            "tor_hsservice::publish",
            &fields
        ));

        let non_timeout = rewrite_for_test(
            "tor_hsservice::publish",
            "Failed to upload descriptor for service rustchan (redacted) - error: permission denied",
            &[],
        );
        assert!(!is_tor_descriptor_upload_timeout_event(
            "tor_hsservice::publish",
            &non_timeout
        ));

        let unrelated_tor_warning = rewrite_for_test(
            "tor_circmgr::mgr",
            "All tunnel attempts failed due to timeout",
            &[],
        );
        assert!(!is_tor_descriptor_upload_timeout_event(
            "tor_circmgr::mgr",
            &unrelated_tor_warning
        ));
    }

    #[test]
    fn rate_limits_repeated_tor_descriptor_upload_timeout_warnings() {
        let mut limiter = TorDescriptorTimeoutLimiter::new(Duration::from_secs(600));
        let start = Instant::now();

        assert_eq!(
            limiter.decide_once(start),
            TorDescriptorTimeoutDecision::EmitOriginal
        );
        assert_eq!(
            limiter.decide_once(start + Duration::from_secs(1)),
            TorDescriptorTimeoutDecision::Suppress
        );
        assert_eq!(
            limiter.decide_once(start + Duration::from_secs(2)),
            TorDescriptorTimeoutDecision::Suppress
        );
        assert_eq!(
            limiter.decide_once(start + Duration::from_secs(601)),
            TorDescriptorTimeoutDecision::EmitSummary {
                suppressed: 2,
                window: Duration::from_secs(600),
            }
        );
        assert_eq!(
            limiter.decide_once(start + Duration::from_secs(602)),
            TorDescriptorTimeoutDecision::Suppress
        );
    }

    #[test]
    fn rate_limiter_reuses_decision_for_second_formatter() {
        let mut limiter = TorDescriptorTimeoutLimiter::new(Duration::from_secs(600));
        let start = Instant::now();

        assert_eq!(
            limiter.decide(start),
            TorDescriptorTimeoutDecision::EmitOriginal
        );
        assert_eq!(
            limiter.decide(start),
            TorDescriptorTimeoutDecision::EmitOriginal
        );
        assert_eq!(
            limiter.decide(start + Duration::from_secs(1)),
            TorDescriptorTimeoutDecision::Suppress
        );
        assert_eq!(
            limiter.decide(start + Duration::from_secs(1)),
            TorDescriptorTimeoutDecision::Suppress
        );
        assert_eq!(limiter.suppressed, 1);
    }
}
