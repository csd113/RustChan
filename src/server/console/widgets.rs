//! Shared suite theme, status tags, formatting and clipped viewports.
use super::ChanStats;
use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Constraint, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{
    Block, BorderType, Borders, Cell, Padding, Paragraph, Row, Table, Widget, Wrap,
};
use ratatui::Frame;

/// Primary brand and focus color.
pub(super) const ACCENT: Color = Color::Cyan;
/// Healthy-state color.
pub(super) const SUCCESS: Color = Color::Green;
/// Pending and caution color.
pub(super) const WARNING: Color = Color::Yellow;
/// Error and destructive-action color.
pub(super) const DANGER: Color = Color::Red;
/// Secondary text and inactive borders.
pub(super) const MUTED: Color = Color::DarkGray;
/// Selected-row background.
pub(super) const SELECTION_BG: Color = Color::Rgb(35, 48, 55);

/// Clip a vertically scrollable content buffer to the visible terminal body.
pub(super) fn render_scrollable(
    frame: &mut Frame<'_>,
    area: Rect,
    minimum_height: u16,
    offset: &mut u16,
    draw: impl FnOnce(&mut Buffer, Rect),
) {
    let content = Rect::new(0, 0, area.width, minimum_height.max(area.height));
    *offset = (*offset).min(content.height.saturating_sub(area.height));
    let mut buffer = Buffer::empty(content);
    draw(&mut buffer, content);
    for y in 0..area.height {
        for x in 0..area.width {
            if let (Some(source), Some(target)) = (
                buffer.cell((x, y.saturating_add(*offset))),
                frame
                    .buffer_mut()
                    .cell_mut((area.x.saturating_add(x), area.y.saturating_add(y))),
            ) {
                *target = source.clone();
            }
        }
    }
}

/// Render label/value rows without allowing long values to escape their panel.
pub(super) fn render_label_rows(
    buffer: &mut Buffer,
    area: Rect,
    rows: Vec<(Line<'static>, Line<'static>)>,
    label_width: u16,
) {
    let table_rows = rows.into_iter().map(|(label, value)| {
        let wrapped = wrap_styled_line(&value, area.width.saturating_sub(label_width + 1));
        let height = u16::try_from(wrapped.len()).unwrap_or(u16::MAX).max(1);
        Row::new(vec![
            Cell::from(label).style(Style::default().fg(MUTED)),
            Cell::from(Text::from(wrapped)),
        ])
        .height(height)
    });
    Widget::render(
        Table::new(
            table_rows,
            [Constraint::Length(label_width), Constraint::Min(1)],
        )
        .column_spacing(1),
        area,
        buffer,
    );
}

/// Render a vertically centered state label and explanation.
pub(super) fn render_centered_state(
    buffer: &mut Buffer,
    area: Rect,
    label: &str,
    message: &str,
    color: Color,
) {
    let height = area.height.min(4);
    let centered = centered_rect(area, area.width, height);
    Paragraph::new(vec![
        Line::from(Span::styled(
            format!("[{label}]"),
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        )),
        Line::default(),
        Line::from(message.to_owned()),
    ])
    .alignment(Alignment::Center)
    .wrap(Wrap { trim: true })
    .render(centered, buffer);
}

/// Build a consistently padded and titled panel.
pub(super) fn panel(title: &str, color: Color) -> Block<'_> {
    Block::default()
        .title(Line::from(Span::styled(
            format!(" {title} "),
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        )))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(MUTED))
        .padding(Padding::horizontal(1))
}

/// Center a popup while clamping it to the available terminal area.
pub(super) fn centered_rect(area: Rect, requested_width: u16, requested_height: u16) -> Rect {
    let width = requested_width.min(area.width.saturating_sub(2)).max(1);
    let height = requested_height.min(area.height.saturating_sub(2)).max(1);
    let horizontal_margin = area.width.saturating_sub(width) / 2;
    let vertical_margin = area.height.saturating_sub(height) / 2;
    Rect::new(
        area.x.saturating_add(horizontal_margin),
        area.y.saturating_add(vertical_margin),
        width,
        height,
    )
}

/// Status semantic used by labeled tags.
#[derive(Clone, Copy)]
pub(super) enum StatusKind {
    /// Healthy and available.
    Healthy,
    /// Work currently executing.
    Active,
    /// No work currently executing.
    Idle,
    /// Waiting on expected startup work.
    Pending,
    /// Disabled or not applicable.
    Neutral,
    /// Failed or unavailable.
    Error,
}

/// Short status summary used in the header.
pub(super) struct StatusSummary {
    /// Status tag text.
    pub(super) label: &'static str,
    /// Status semantic.
    pub(super) kind: StatusKind,
}

/// Determine snapshot readiness and staleness.
pub(super) fn stats_freshness(stats: &ChanStats) -> StatusSummary {
    if !stats.is_ready {
        StatusSummary {
            label: "[LOADING]",
            kind: StatusKind::Pending,
        }
    } else if stats.collection_error.is_some() || stats.operator.error.is_some() {
        StatusSummary {
            label: "[DEGRADED]",
            kind: StatusKind::Error,
        }
    } else if stats.operator.jobs.is_some_and(|jobs| jobs.failed > 0)
        || stats.operator.media_failed > 0
        || stats.operator.tasks.iter().any(|task| {
            task.state == super::telemetry::TaskState::Failed && task.id.starts_with("database:")
        })
    {
        StatusSummary {
            label: "[ATTENTION]",
            kind: StatusKind::Pending,
        }
    } else if stats
        .sampled_at
        .is_some_and(|sampled_at| sampled_at.elapsed().as_secs() > 10)
    {
        StatusSummary {
            label: "[STALE]",
            kind: StatusKind::Pending,
        }
    } else {
        StatusSummary {
            label: "[LIVE]",
            kind: StatusKind::Healthy,
        }
    }
}

/// Convert a semantic status into terminal styling.
pub(super) fn status_style(kind: StatusKind) -> Style {
    let color = match kind {
        StatusKind::Healthy => SUCCESS,
        StatusKind::Active => ACCENT,
        StatusKind::Idle | StatusKind::Neutral => MUTED,
        StatusKind::Pending => WARNING,
        StatusKind::Error => DANGER,
    };
    Style::default().fg(color).add_modifier(Modifier::BOLD)
}

/// Build a status line that remains explicit without color.
pub(super) fn status_value(
    kind: StatusKind,
    label: &str,
    detail: impl Into<String>,
) -> Line<'static> {
    let detail = detail.into();
    let tag = match kind {
        StatusKind::Healthy => format!("[OK] {label}"),
        StatusKind::Active => format!("[ACTIVE] {label}"),
        StatusKind::Idle => "[IDLE]".to_owned(),
        StatusKind::Pending => format!("[WAIT] {label}"),
        StatusKind::Neutral => format!("[OFF] {label}"),
        StatusKind::Error => format!("[ERROR] {label}"),
    };
    let mut spans = vec![Span::styled(tag, status_style(kind))];
    if !detail.is_empty() {
        spans.push(Span::styled(
            format!("  {detail}"),
            Style::default().fg(MUTED),
        ));
    }
    Line::from(spans)
}

/// Format uptime compactly while retaining days for long-running servers.
pub(super) fn fmt_uptime(seconds: u64) -> String {
    let days = seconds / 86_400;
    let hours = (seconds % 86_400) / 3_600;
    let minutes = (seconds % 3_600) / 60;
    if days > 0 {
        format!("{days}d {hours:02}h {minutes:02}m")
    } else {
        let remaining_seconds = seconds % 60;
        format!("{hours}h {minutes:02}m {remaining_seconds:02}s")
    }
}

/// Format uptime without seconds for medium-width headers.
pub(super) fn fmt_uptime_compact(seconds: u64) -> String {
    let days = seconds / 86_400;
    let hours = (seconds % 86_400) / 3_600;
    let minutes = (seconds % 3_600) / 60;
    if days > 0 {
        format!("{days}d {hours:02}h")
    } else {
        format!("{hours}h {minutes:02}m")
    }
}

/// Format a byte count using binary units and fail-closed negative handling.
#[expect(
    clippy::as_conversions,
    clippy::cast_precision_loss,
    reason = "human-readable byte sizes intentionally trade integer precision for compact display"
)]
pub(super) fn fmt_bytes(bytes: i64) -> String {
    const KIB: i64 = 1_024;
    const MIB: i64 = KIB * 1_024;
    const GIB: i64 = MIB * 1_024;
    if bytes < 0 {
        return "unavailable".to_owned();
    }
    if bytes < KIB {
        format!("{bytes} B")
    } else if bytes < MIB {
        format!("{:.1} KiB", bytes as f64 / KIB as f64)
    } else if bytes < GIB {
        format!("{:.1} MiB", bytes as f64 / MIB as f64)
    } else {
        format!("{:.2} GiB", bytes as f64 / GIB as f64)
    }
}

/// Add grouping separators to an unsigned count.
pub(super) fn format_number(number: u64) -> String {
    let digits = number.to_string();
    let mut output = String::with_capacity(digits.len().saturating_add(digits.len() / 3));
    for (index, character) in digits.chars().enumerate() {
        if index > 0 && (digits.len().saturating_sub(index)) % 3 == 0 {
            output.push(',');
        }
        output.push(character);
    }
    output
}

/// Add grouping separators to a signed count.
pub(super) fn format_number_signed(number: i64) -> String {
    if number < 0 {
        return "unavailable".to_owned();
    }
    u64::try_from(number).map_or_else(|_| "unavailable".to_owned(), format_number)
}

/// Format a dashboard count with compact suffixes above four digits.
#[expect(
    clippy::as_conversions,
    clippy::cast_precision_loss,
    reason = "compact dashboard counts intentionally trade precision for stable terminal width"
)]
pub(super) fn format_number_compact(number: u64) -> String {
    const THOUSAND: u64 = 1_000;
    const MILLION: u64 = THOUSAND * 1_000;
    const BILLION: u64 = MILLION * 1_000;
    if number < 10_000 {
        format_number(number)
    } else if number < MILLION {
        format!("{:.1}k", number as f64 / THOUSAND as f64)
    } else if number < BILLION {
        format!("{:.1}m", number as f64 / MILLION as f64)
    } else {
        format!("{:.1}b", number as f64 / BILLION as f64)
    }
}

/// Format a signed dashboard count with compact suffixes.
pub(super) fn format_number_signed_compact(number: i64) -> String {
    if number < 0 {
        return "unavailable".to_owned();
    }
    u64::try_from(number).map_or_else(|_| "unavailable".to_owned(), format_number_compact)
}

/// Return one platform-appropriate spinner frame.
#[cfg(windows)]
pub(super) fn spinner_frame(tick: u8) -> &'static str {
    const FRAMES: [&str; 4] = ["|", "/", "-", "\\"];
    FRAMES
        .get(usize::from(tick) % FRAMES.len())
        .copied()
        .unwrap_or("|")
}

/// Return one platform-appropriate spinner frame.
#[cfg(not(windows))]
pub(super) fn spinner_frame(tick: u8) -> &'static str {
    const FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
    FRAMES
        .get(usize::from(tick) % FRAMES.len())
        .copied()
        .unwrap_or("⠋")
}

/// Prewrap plain detail lines at grapheme boundaries for exact scroll bounds.
pub(super) fn wrap_lines(lines: &[String], width: u16) -> Vec<String> {
    lines
        .iter()
        .flat_map(|line| {
            wrap_styled_line(&Line::from(line.as_str()), width)
                .into_iter()
                .map(|line| line.to_string())
        })
        .collect()
}

/// Wrap styled values at complete graphemes without losing severity colors.
fn wrap_styled_line(line: &Line<'_>, width: u16) -> Vec<Line<'static>> {
    let width = usize::from(width).max(1);
    let mut words = Vec::new();
    let mut word = Vec::new();
    for grapheme in line.styled_graphemes(Style::default()) {
        if grapheme.symbol.chars().all(char::is_whitespace) {
            if !word.is_empty() {
                words.push(std::mem::take(&mut word));
            }
        } else {
            word.push(Span::styled(grapheme.symbol.to_owned(), grapheme.style));
        }
    }
    if !word.is_empty() {
        words.push(word);
    }
    let mut lines = Vec::new();
    let mut spans = Vec::new();
    let mut columns = 0;
    for word in words {
        let word_width: usize = word.iter().map(Span::width).sum();
        if columns > 0 && columns + 1 + word_width > width {
            lines.push(Line::from(std::mem::take(&mut spans)));
            columns = 0;
        }
        if columns > 0 {
            spans.push(Span::raw(" "));
            columns += 1;
        }
        for span in word {
            let size = span.width();
            if columns + size > width && !spans.is_empty() {
                lines.push(Line::from(std::mem::take(&mut spans)));
                columns = 0;
            }
            spans.push(span);
            columns += size;
        }
    }
    lines.push(Line::from(spans));
    lines
}

/// Height needed to preserve all wrapped values in a label/value panel.
pub(super) fn label_rows_height(
    rows: &[(Line<'_>, Line<'_>)],
    label_width: u16,
    width: u16,
) -> u16 {
    let height: usize = rows
        .iter()
        .map(|(_, value)| wrap_styled_line(value, width.saturating_sub(label_width + 1)).len())
        .sum();
    u16::try_from(height).unwrap_or(u16::MAX).saturating_add(2)
}
