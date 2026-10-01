//! Bounded disk log tail, filtering and follow/pause rendering.
use super::state::ConsoleState;
use super::widgets::{
    format_number, panel, render_centered_state, ACCENT, DANGER, MUTED, SUCCESS, WARNING,
};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget};
use ratatui::Frame;
use std::path::{Path, PathBuf};

/// Maximum retained log bytes.
const LOG_TAIL_BYTES: usize = 256 * 1024;

/// Cached tail of the current application log.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LogSnapshot {
    /// Source filename, when a main log exists.
    pub source: Option<String>,
    /// Complete lines retained from the tail.
    pub lines: Vec<String>,
    /// Whether older bytes were omitted.
    pub truncated: bool,
    /// Read error suitable for operator display.
    pub error: Option<String>,
}

/// Render the scrollable, follow-capable log viewer.
pub(super) fn render_logs(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &mut ConsoleState,
    logs: &LogSnapshot,
) {
    let regions = Layout::vertical([Constraint::Length(1), Constraint::Min(3)]).split(area);
    let matching = logs
        .lines
        .iter()
        .filter(|line| app.log_filter.log_matches(line))
        .collect::<Vec<_>>();
    Paragraph::new(super::operator_views::filter_line(
        &app.log_filter.query,
        app.log_filter.editing,
        app.log_filter.log_label(),
        matching.len(),
    ))
    .render(regions.first().copied().unwrap_or(area), frame.buffer_mut());
    let area = regions.get(1).copied().unwrap_or(area);
    let lines = &matching;
    let rows = Layout::vertical([Constraint::Length(2), Constraint::Min(3)]).split(area);
    let info_area = rows.first().copied().unwrap_or(area);
    let body_area = rows.get(1).copied().unwrap_or(area);

    app.logs.visible_rows = usize::from(body_area.height.saturating_sub(2)).max(1);
    app.logs.rows_from_bottom = app
        .logs
        .rows_from_bottom
        .min(lines.len().saturating_sub(app.logs.visible_rows));
    let max_width = lines
        .iter()
        .map(|line| Line::from(line.as_str()).width())
        .max()
        .unwrap_or(0);
    app.logs.horizontal_offset = app.logs.horizontal_offset.min(
        u16::try_from(max_width.saturating_sub(usize::from(body_area.width.saturating_sub(4))))
            .unwrap_or(u16::MAX),
    );
    let follow = if app.logs.follow {
        Span::styled("[FOLLOWING]", Style::default().fg(SUCCESS))
    } else {
        Span::styled(
            format!("[PAUSED · {} lines back]", app.logs.rows_from_bottom),
            Style::default().fg(WARNING),
        )
    };
    let source = logs
        .source
        .as_deref()
        .unwrap_or("waiting for first log file");
    Paragraph::new(Line::from(vec![
        follow,
        Span::styled("  Source  ", Style::default().fg(MUTED)),
        Span::raw(super::telemetry::display_text(source)),
        Span::styled(
            format!(
                "   {} retained lines",
                format_number(u64::try_from(lines.len()).unwrap_or(u64::MAX))
            ),
            Style::default().fg(MUTED),
        ),
    ]))
    .render(info_area, frame.buffer_mut());

    let block = panel("Live log", ACCENT).title_bottom(Line::from(Span::styled(
        " ↑↓ scroll · ←→ pan · Enter inspect last visible · P/F pause/follow ",
        Style::default().fg(MUTED),
    )));
    let inner = block.inner(body_area);
    block.render(body_area, frame.buffer_mut());

    app.selected_log = None;
    if let Some(error) = &logs.error {
        render_centered_state(frame.buffer_mut(), inner, "LOG ERROR", error, DANGER);
        return;
    }
    if lines.is_empty() {
        app.selected_log = None;
        render_centered_state(
            frame.buffer_mut(),
            inner,
            "NO LOG ENTRIES",
            "No retained entries match. / searches; S changes level; F follows new events.",
            MUTED,
        );
        return;
    }

    let visible_rows = usize::from(inner.height).max(1);
    let max_rows_from_bottom = lines.len().saturating_sub(visible_rows);
    let rows_from_bottom = if app.logs.follow {
        0
    } else {
        app.logs.rows_from_bottom.min(max_rows_from_bottom)
    };
    let end = lines.len().saturating_sub(rows_from_bottom);
    let start = end.saturating_sub(visible_rows);
    app.selected_log = lines.get(end.saturating_sub(1)).map(|line| (*line).clone());
    let visible = lines
        .get(start..end)
        .unwrap_or_default()
        .iter()
        .map(|line| Line::from(Span::styled(line.as_str(), log_line_style(line))))
        .collect::<Vec<_>>();
    Paragraph::new(visible)
        .scroll((0, app.logs.horizontal_offset))
        .render(inner, frame.buffer_mut());
}

/// Choose log emphasis while preserving the original severity text.
fn log_line_style(line: &str) -> Style {
    let level = super::browse::log_level(line);
    if level == "ERROR" {
        Style::default().fg(DANGER)
    } else if level == "WARN" {
        Style::default().fg(WARNING)
    } else if matches!(level, "DEBUG" | "TRACE") {
        Style::default().fg(MUTED)
    } else {
        Style::default()
    }
}

/// Load the newest main-process log tail for the next frame.
#[must_use]
pub fn load_log_snapshot() -> LogSnapshot {
    let logs_dir = crate::config::logs_dir();
    let Some(path) = latest_log_file(&logs_dir) else {
        return LogSnapshot::default();
    };
    let source = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("current log")
        .to_owned();
    match crate::logging::read_log_tail(&path, LOG_TAIL_BYTES) {
        Ok((content, truncated)) => LogSnapshot {
            source: Some(source),
            lines: content
                .lines()
                .rev()
                .take(4000)
                .map(safe_log_line)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect(),
            truncated: truncated
                || content.lines().count() > 4000
                || content.lines().any(|line| line.chars().count() > 4096),
            error: None,
        },
        Err(error) => LogSnapshot {
            source: Some(source),
            lines: Vec::new(),
            truncated: false,
            error: Some(error),
        },
    }
}

/// Redact credential fields and URL userinfo before buffering or inspecting logs.
fn safe_log_line(line: &str) -> String {
    use std::sync::LazyLock;
    /// Known credential fields; compilation failure disables unsafe presentation.
    static FIELDS: LazyLock<Option<regex::Regex>> = LazyLock::new(|| {
        regex::Regex::new(r#"(?i)(\"?(?:password|new_password|cookie_secret|session_secret|api_key|access_token|refresh_token|token|authorization|cookie|ip_hash)\"?\s*[:=]\s*)(?:\"[^\"]*\"|[^,}\n]+)"#).ok()
    });
    /// URI credentials are private even when no secret field label is present.
    static URLS: LazyLock<Option<regex::Regex>> =
        LazyLock::new(|| regex::Regex::new(r"(?i)(https?://)[^/\s@]+:[^/\s@]+@").ok());
    let (Some(fields), Some(urls)) = (&*FIELDS, &*URLS) else {
        return "[UNAVAILABLE] Log redaction could not initialize".to_owned();
    };
    let cleaned: String = line
        .chars()
        .filter(|character| !character.is_control())
        .take(4096)
        .collect();
    let cleaned = fields.replace_all(&cleaned, "$1[REDACTED]");
    urls.replace_all(&cleaned, "$1[REDACTED]@").into_owned()
}

/// Find the newest main-process log file.
pub(super) fn latest_log_file(logs_dir: &Path) -> Option<PathBuf> {
    std::fs::read_dir(logs_dir)
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| crate::logging::is_main_log_file(path))
        .filter_map(|path| {
            let metadata = path.metadata().ok()?;
            metadata.is_file().then(|| (metadata.modified().ok(), path))
        })
        .max()
        .map(|(_, path)| path)
}
#[cfg(test)]
mod tests {
    use super::safe_log_line;

    #[test]
    fn credential_fields_urls_and_terminal_controls_are_not_buffered() {
        let cleaned = safe_log_line(
            "INFO password=PRIVATE, token=OTHER, url=https://alice:PASS@host/path \u{1b}",
        );
        assert!(
            !cleaned.contains("PRIVATE") && !cleaned.contains("OTHER") && !cleaned.contains("PASS"),
            "credentials must be redacted before search or inspection"
        );
        assert!(
            !cleaned.contains('\u{1b}'),
            "log controls must not reach the terminal"
        );
    }
}
