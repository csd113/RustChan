//! Event-field normalization and bounded Tor warning coalescing.

use super::extract_component;
use std::fmt;
use std::time::{Duration, Instant};
use tracing::field::{Field, Visit};
use tracing::{Event, Level};
use tracing_subscriber::fmt::format::Writer;

/// Window used to coalesce repeated Tor descriptor-upload timeout warnings.
pub(super) const TOR_DESCRIPTOR_TIMEOUT_SUPPRESSION_WINDOW: Duration = Duration::from_mins(10);

/// Removes ANSI control sequences from a captured field value.
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

/// Converts a structured field name into a human-readable label.
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

/// Returns whether a source path belongs to a dependency or the Rust toolchain.
pub(super) fn is_external_source(file: &str) -> bool {
    file.contains("/.cargo/registry/src/") || file.contains("/rustc/")
}

/// Returns whether a display component belongs to the Tor stack.
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

/// Returns whether a tracing target belongs to Arti or another Tor crate.
pub(super) fn is_tor_target(target: &str) -> bool {
    target == "arti_client"
        || target.starts_with("arti_client::")
        || target.starts_with("tor_")
        || target.starts_with("tor-")
}

/// Uppercases an initial ASCII lowercase letter without changing other text.
fn title_case_message(message: &str) -> String {
    let mut chars = message.chars();
    match chars.next() {
        Some(first) if first.is_ascii_lowercase() => {
            format!("{}{}", first.to_ascii_uppercase(), chars.as_str())
        }
        _ => message.to_owned(),
    }
}

/// Parses a debug-formatted duration into compact human-readable text.
fn parse_formatted_duration(value: &str) -> Option<String> {
    let inner = value
        .strip_prefix("FormattedDuration(")?
        .strip_suffix(')')?
        .strip_suffix('s')?;
    let seconds = inner.parse::<f64>().ok()?;

    if seconds >= 60.0 {
        let total = Duration::from_secs_f64(seconds.round()).as_secs();
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
            Duration::from_secs_f64(seconds.round()).as_secs()
        ))
    } else {
        Some(format!("{seconds:.1}s"))
    }
}

/// Formats a duration represented as nanoseconds using compact units.
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

/// Parses whitespace-separated duration parts such as `29s 999ms`.
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
        let duration_value = number.parse::<u128>().ok()?;
        let nanos = match unit {
            "h" => duration_value.saturating_mul(3_600_000_000_000),
            "m" => duration_value.saturating_mul(60_000_000_000),
            "s" => duration_value.saturating_mul(1_000_000_000),
            "ms" => duration_value.saturating_mul(1_000_000),
            "us" | "µs" => duration_value.saturating_mul(1_000),
            "ns" => duration_value,
            _ => return None,
        };
        total_nanos = total_nanos.saturating_add(nanos);
        saw_unit = true;
    }

    saw_unit.then(|| format_duration_parts(total_nanos))
}

/// Normalizes either supported upstream duration representation.
fn normalize_duration(value: &str) -> Option<String> {
    parse_formatted_duration(value).or_else(|| parse_compound_duration(value))
}

/// Redacts volatile Tor guard and circuit identifiers from a field value.
fn scrub_tor_value(value: &str) -> String {
    let mut out = value.to_owned();

    while let Some(start) = out.find("GuardId(") {
        let Some(remainder) = out.get(start..) else {
            break;
        };
        let Some(relative_end) = remainder.find(')') else {
            break;
        };
        let Some(end) = start.checked_add(relative_end) else {
            break;
        };
        out.replace_range(start..=end, "[scrubbed guard]");
    }

    while let Some(start) = out.find(" via Circ ") {
        let Some(token_value_start) = start.checked_add(" via Circ ".len()) else {
            break;
        };
        let Some(remainder) = out.get(token_value_start..) else {
            break;
        };
        let end = remainder
            .find(char::is_whitespace)
            .and_then(|offset| token_value_start.checked_add(offset))
            .unwrap_or(out.len());
        out.replace_range(start..end, "");
    }

    out
}

/// Normalizes one structured log field for human-readable output.
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

/// Returns whether an empty or redundant structured field should be omitted.
fn should_hide_field(name: &str, value: &str) -> bool {
    (matches!(value, "" | "<missing>")
        && matches!(name, "content_type" | "content_length" | "mime" | "path"))
        || (name == "guard" && value == "[scrubbed]")
}

#[derive(Default)]
/// Captured and normalized fields for one tracing event.
pub(super) struct LogEventFields {
    /// Optional event message.
    message: Option<String>,
    /// Remaining structured name/value pairs.
    fields: Vec<(String, String)>,
}

impl LogEventFields {
    /// Normalizes and stores one recorded tracing field.
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

/// Inserts or replaces a structured field by name.
fn upsert_field(fields: &mut Vec<(String, String)>, name: &str, value: String) {
    if let Some((_, existing)) = fields.iter_mut().find(|(field_name, _)| field_name == name) {
        *existing = value;
    } else {
        fields.push((name.to_owned(), value));
    }
}

/// Removes a structured field by name and returns its value.
fn remove_field(fields: &mut Vec<(String, String)>, name: &str) -> Option<String> {
    fields
        .iter()
        .position(|(field_name, _)| field_name == name)
        .map(|index| fields.remove(index).1)
}

/// Extracts the numeric token immediately preceding the first percent sign.
fn extract_percent(message: &str) -> Option<String> {
    let percent_index = message.find('%')?;
    let before_percent = message.get(..percent_index)?;
    let number = before_percent
        .rsplit_once(' ')
        .map_or(before_percent, |(_, value)| value);
    Some(format!("{number}%"))
}

/// Extracts a Tor bootstrap attempt count from its upstream message.
fn extract_attempt_count(message: &str) -> Option<String> {
    let before = message
        .strip_prefix("We failed ")?
        .split_once(" times to bootstrap")?
        .0;
    before.parse::<u64>().ok().map(|count| count.to_string())
}

/// Rewrites common verbose Tor errors into concise operator-facing text.
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
/// Output action selected for a descriptor-upload timeout event.
pub(super) enum TorDescriptorTimeoutDecision {
    /// Emit the original warning.
    EmitOriginal,
    /// Suppress this repeated warning.
    Suppress,
    /// Emit a summary of warnings suppressed during a completed window.
    EmitSummary {
        /// Number of repeated warnings suppressed.
        suppressed: u64,
        /// Suppression window represented by the summary.
        window: Duration,
    },
}

#[derive(Debug)]
/// Coalesces repeated Tor descriptor-upload timeout warnings.
pub(super) struct TorDescriptorTimeoutLimiter {
    /// Duration of one suppression window.
    window: Duration,
    /// Start time of the active suppression window.
    window_started_at: Option<Instant>,
    /// Number of repeated warnings suppressed in the active window.
    pub(super) suppressed: u64,
}

impl TorDescriptorTimeoutLimiter {
    /// Creates an idle limiter with the supplied window.
    pub(super) const fn new(window: Duration) -> Self {
        Self {
            window,
            window_started_at: None,
            suppressed: 0,
        }
    }

    /// Selects a rate-limit decision for one logical tracing event.
    pub(super) fn decide_once(&mut self, now: Instant) -> TorDescriptorTimeoutDecision {
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

/// Detects Tor descriptor-upload timeout warnings eligible for coalescing.
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

/// Applies descriptor-timeout coalescing and returns whether to emit the event.
fn apply_tor_descriptor_timeout_rate_limit(
    fields: &mut LogEventFields,
    limiter: &parking_lot::Mutex<TorDescriptorTimeoutLimiter>,
) -> bool {
    let decision = limiter.lock().decide_once(Instant::now());
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

/// Normalizes punctuation, whitespace, and capitalization in a log message.
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

#[expect(
    clippy::cognitive_complexity,
    reason = "message rewriting intentionally centralizes the closed set of upstream log patterns"
)]
#[expect(
    clippy::too_many_lines,
    reason = "keeping the closed message-rewrite table together makes overlapping upstream patterns auditable"
)]
/// Rewrites known upstream messages and fields into stable operator-facing text.
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
                let _previous_value = remove_field(&mut fields.fields, "tunnel_id");
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

/// Returns whether a noisy rewritten event should be omitted entirely.
fn should_suppress_event(target: &str, fields: &LogEventFields) -> bool {
    matches!(
        (extract_component(target), fields.message.as_deref()),
        (
            "hspool",
            Some("Tor onion-service circuits are failing; waiting before retrying")
        )
    )
}

/// Records, normalizes, rewrites, and rate-limits fields for one event.
pub(super) fn prepare_event_fields(
    target: &str,
    file: Option<&str>,
    event: &Event<'_>,
    limiter: &parking_lot::Mutex<TorDescriptorTimeoutLimiter>,
) -> Option<LogEventFields> {
    let mut fields = LogEventFields::default();
    event.record(&mut fields);
    rewrite_message(target, file, &mut fields);
    if *event.metadata().level() == Level::WARN
        && is_tor_descriptor_upload_timeout_event(target, &fields)
        && !apply_tor_descriptor_timeout_rate_limit(&mut fields, limiter)
    {
        return None;
    }
    (!should_suppress_event(target, &fields)).then_some(fields)
}

/// Writes normalized event fields in the shared human-readable format.
pub(super) fn write_event_fields(writer: &mut Writer<'_>, fields: &LogEventFields) -> fmt::Result {
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
