//! Ratatui rendering for every full-screen console surface.

use super::content::render_boards;
use super::dialogs::render_dialog;
#[cfg(test)]
use super::dialogs::render_form_field;
use super::help::render_help;
#[cfg(test)]
use super::logs::latest_log_file;
use super::logs::render_logs;
pub use super::logs::{load_log_snapshot, LogSnapshot};
use super::state::{terminal_is_usable, ConsoleState, NoticeSeverity, Screen};
#[cfg(test)]
use super::state::{Dialog, FieldValue, FormState};
use super::widgets::{
    fmt_bytes, fmt_uptime, fmt_uptime_compact, format_number_compact, format_number_signed_compact,
    panel, render_centered_state, render_label_rows, render_scrollable, stats_freshness,
    status_style, status_value, StatusKind, ACCENT, DANGER, MUTED, SUCCESS, WARNING,
};
use super::ChanStats;
use crate::config::CONFIG;
use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Paragraph, Tabs, Widget, Wrap};
use ratatui::Frame;

/// Smallest size that can retain useful hierarchy and interaction hints.
const MIN_USEFUL_WIDTH: u16 = 44;
/// Smallest height that can retain useful hierarchy and interaction hints.
const MIN_USEFUL_HEIGHT: u16 = 14;
/// Render the complete console frame.
pub fn render(
    frame: &mut Frame<'_>,
    app: &mut ConsoleState,
    metrics: &ChanStats,
    logs: &LogSnapshot,
) {
    let area = frame.area();
    if !terminal_is_usable(area.width, area.height) {
        render_small_terminal(frame, area);
        return;
    }

    app.reconcile_data(metrics);

    let notice_height = u16::from(app.notice.is_some());
    let layout = Layout::vertical([
        Constraint::Length(2),
        Constraint::Length(2),
        Constraint::Length(notice_height),
        Constraint::Min(5),
        Constraint::Length(2),
    ])
    .split(area);

    let header_area = layout.first().copied().unwrap_or(area);
    let navigation_area = layout.get(1).copied().unwrap_or(area);
    let notice_area = layout.get(2).copied().unwrap_or(area);
    let body_area = layout.get(3).copied().unwrap_or(area);
    let footer_area = layout.get(4).copied().unwrap_or(area);

    render_header(frame, header_area, metrics);
    let shown_screen = if app.screen == Screen::Help {
        app.help_return.unwrap_or_default()
    } else {
        app.screen
    };
    render_navigation(frame, navigation_area, shown_screen);
    if let Some(notice) = &app.notice {
        render_notice(frame, notice_area, notice.severity, &notice.message);
    }

    match shown_screen {
        Screen::Dashboard => render_dashboard(frame, body_area, metrics, &mut app.overview_scroll),
        Screen::Boards => render_boards(frame, body_area, app, metrics),
        Screen::Logs => render_logs(frame, body_area, app, logs),
        Screen::Tasks => super::operator_views::render_tasks(frame, body_area, app, metrics),
        Screen::System => super::operator_views::render_system(frame, body_area, app, metrics),
        Screen::Configuration => {
            super::operator_views::render_configuration(frame, body_area, app, metrics);
        }
        Screen::Help => render_help(frame, body_area, &mut app.help_scroll),
    }
    render_footer(frame, footer_area, app);
    if app.screen == Screen::Help {
        super::dialogs::render_help_overlay(frame, area, shown_screen, &mut app.help_scroll);
    }

    if let Some(dialog) = &mut app.dialog {
        render_dialog(frame, area, dialog, metrics.spinner_tick);
    }
}

/// Render the terminal-size fallback without relying on color.
fn render_small_terminal(frame: &mut Frame<'_>, area: Rect) {
    let block = panel("RustChan console", ACCENT);
    let inner = block.inner(area);
    block.render(area, frame.buffer_mut());
    let message = Text::from(vec![
        Line::from(Span::styled(
            "Terminal too small",
            Style::default().fg(WARNING).add_modifier(Modifier::BOLD),
        )),
        Line::default(),
        Line::from(format!("Current: {} × {}", area.width, area.height)),
        Line::from(format!("Minimum: {MIN_USEFUL_WIDTH} × {MIN_USEFUL_HEIGHT}")),
        Line::default(),
        Line::from("Resize to continue · Ctrl-C stops the server"),
    ]);
    Paragraph::new(message)
        .alignment(Alignment::Center)
        .wrap(Wrap { trim: true })
        .render(inner, frame.buffer_mut());
}

/// Render product identity, sampling freshness, and uptime.
fn render_header(frame: &mut Frame<'_>, area: Rect, stats: &ChanStats) {
    let columns = Layout::horizontal([Constraint::Percentage(65), Constraint::Percentage(35)])
        .horizontal_margin(1)
        .split(area);
    let left = columns.first().copied().unwrap_or(area);
    let right = columns.get(1).copied().unwrap_or(area);
    let title = Line::from(vec![
        Span::styled(
            "RUSTCHAN",
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(" v{}", env!("CARGO_PKG_VERSION")),
            Style::default().fg(MUTED),
        ),
        Span::styled(" / ", Style::default().fg(MUTED)),
        Span::styled(
            super::telemetry::display_text(&CONFIG.forum_name),
            Style::default().add_modifier(Modifier::BOLD),
        ),
    ]);
    Paragraph::new(title).render(left, frame.buffer_mut());

    let freshness = stats_freshness(stats);
    let mut right_spans = vec![Span::styled(freshness.label, status_style(freshness.kind))];
    if right.width >= 25 {
        right_spans.push(Span::styled(" · ", Style::default().fg(MUTED)));
        let (label, uptime) = if right.width >= 38 {
            ("Uptime", fmt_uptime(stats.uptime_secs))
        } else {
            ("Up", fmt_uptime_compact(stats.uptime_secs))
        };
        right_spans.push(Span::raw(format!("{label} {uptime}")));
    }
    let right_line = Line::from(right_spans);
    Paragraph::new(right_line)
        .alignment(Alignment::Right)
        .render(right, frame.buffer_mut());
}

/// Render persistent screen navigation as a tab strip.
fn render_navigation(frame: &mut Frame<'_>, area: Rect, screen: Screen) {
    let selected = match screen {
        Screen::Dashboard => 0,
        Screen::Tasks => 1,
        Screen::Boards => 2,
        Screen::Logs => 3,
        Screen::System => 4,
        Screen::Configuration => 5,
        Screen::Help => 6,
    };
    let mut titles: Vec<_> = if area.width < 58 {
        ["1Home", "2Jobs", "3Data", "4Logs", "5Sys", "6Cfg"]
            .into_iter()
            .map(Line::from)
            .collect()
    } else if area.width < 76 {
        ["1 Home", "2 Jobs", "3 Data", "4 Logs", "5 Sys", "6 Cfg"]
            .into_iter()
            .map(Line::from)
            .collect()
    } else {
        [
            "1 Overview",
            "2 Tasks",
            "3 Content",
            "4 Logs",
            "5 System",
            "6 Configuration",
        ]
        .into_iter()
        .map(Line::from)
        .collect()
    };
    titles.push(Line::from("?Help"));
    Tabs::new(titles)
        .padding("", "")
        .block(
            Block::default()
                .borders(Borders::BOTTOM)
                .border_style(Style::default().fg(MUTED)),
        )
        .select(selected)
        .style(Style::default().fg(MUTED))
        .highlight_style(
            Style::default()
                .fg(ACCENT)
                .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
        )
        .divider(Span::styled(" ", Style::default().fg(MUTED)))
        .render(area, frame.buffer_mut());
}

/// Render one explicit feedback line.
fn render_notice(frame: &mut Frame<'_>, area: Rect, severity: NoticeSeverity, message: &str) {
    let (label, color) = match severity {
        NoticeSeverity::Success => ("[OK]", SUCCESS),
        NoticeSeverity::Info => ("[INFO]", ACCENT),
        NoticeSeverity::Error => ("[ERROR]", DANGER),
    };
    Paragraph::new(Line::from(vec![
        Span::styled(
            format!(" {label} "),
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ),
        Span::raw(message.to_owned()),
    ]))
    .render(area, frame.buffer_mut());
}

/// Render the overview screen with adaptive one- or two-column panels.
fn render_dashboard(frame: &mut Frame<'_>, area: Rect, stats: &ChanStats, offset: &mut u16) {
    let wide = area.width >= 100;
    let panel_width = if wide {
        area.width.saturating_sub(1) / 2
    } else {
        area.width
    };
    let service_height =
        super::widgets::label_rows_height(&service_rows(stats), 15, panel_width.saturating_sub(4));
    let operations_height = super::widgets::label_rows_height(
        &operations_rows(stats),
        12,
        panel_width.saturating_sub(4),
    )
    .max(8);
    let panels_height = if wide {
        service_height.max(operations_height)
    } else {
        service_height + operations_height + 1
    };
    let content_height = panels_height + 8;
    render_scrollable(frame, area, content_height, offset, |buffer, content| {
        let rows = Layout::vertical([
            Constraint::Length(2),
            Constraint::Length(panels_height),
            Constraint::Length(5),
        ])
        .spacing(1)
        .split(content);
        let attention = if !stats.is_ready {
            "[LOADING] Collecting server state…".to_owned()
        } else if stats.collection_error.is_some() || stats.operator.error.is_some() {
            "[ERROR] Some data unavailable · inspect Logs / R retry".to_owned()
        } else if stats.operator.tasks.iter().any(|task| {
            task.state == super::telemetry::TaskState::Failed && task.id.starts_with("database:")
        }) {
            "[ATTENTION] Database maintenance failed · inspect Tasks / System".to_owned()
        } else if let Some(jobs) = stats.operator.jobs {
            format!(
                "{} {} failed jobs · {} failed media · {} reports · {} appeals",
                if jobs.failed > 0 || stats.operator.media_failed > 0 {
                    "[ATTENTION]"
                } else {
                    "[LIVE]"
                },
                jobs.failed,
                stats.operator.media_failed,
                stats.operator.reports,
                stats.operator.appeals
            )
        } else {
            "[UNKNOWN] Task summary awaiting sample".to_owned()
        };
        Paragraph::new(attention)
            .style(Style::default().fg(
                if stats.operator.jobs.is_some_and(|jobs| jobs.failed > 0)
                    || stats.operator.media_failed > 0
                    || stats.collection_error.is_some()
                    || stats.operator.error.is_some()
                {
                    WARNING
                } else {
                    MUTED
                },
            ))
            .wrap(Wrap { trim: true })
            .render(rows.first().copied().unwrap_or(content), buffer);
        let body = rows.get(1).copied().unwrap_or(content);
        if wide {
            let columns =
                Layout::horizontal([Constraint::Percentage(51), Constraint::Percentage(49)])
                    .spacing(1)
                    .split(body);
            render_operations_panel(buffer, columns.first().copied().unwrap_or(body), stats);
            render_service_panel(buffer, columns.get(1).copied().unwrap_or(body), stats);
        } else {
            let sections = Layout::vertical([
                Constraint::Length(operations_height),
                Constraint::Length(service_height),
            ])
            .spacing(1)
            .split(body);
            render_operations_panel(buffer, sections.first().copied().unwrap_or(body), stats);
            render_service_panel(buffer, sections.get(1).copied().unwrap_or(body), stats);
        }
        super::operator_views::render_history(
            buffer,
            rows.get(2).copied().unwrap_or(content),
            stats,
        );
    });
}

/// Render server transports and direct operator endpoints.
fn render_service_panel(buffer: &mut Buffer, area: Rect, stats: &ChanStats) {
    let block = panel("Service & access", ACCENT);
    let inner = block.inner(area);
    block.render(area, buffer);
    let rows = service_rows(stats);
    render_label_rows(buffer, inner, rows, 15);
}

/// Build service-status and endpoint rows.
fn service_rows(stats: &ChanStats) -> Vec<(Line<'static>, Line<'static>)> {
    let mut rows = Vec::with_capacity(7);
    if CONFIG.tls.enabled {
        if CONFIG.enable_tor_support {
            rows.push((
                Line::from("HTTP backend"),
                status_value(
                    StatusKind::Healthy,
                    "RUNNING",
                    backend_address(stats.http_port),
                ),
            ));
        } else {
            rows.push((
                Line::from("HTTP app"),
                status_value(StatusKind::Neutral, "HTTPS ONLY", ""),
            ));
        }
        rows.push((
            Line::from("HTTPS"),
            status_value(
                StatusKind::Healthy,
                "RUNNING",
                format!("port {} · {}", CONFIG.tls.port, https_cert_label()),
            ),
        ));
        rows.push((
            Line::from("Public URL"),
            Line::from(Span::styled(
                format!("https://localhost:{}", CONFIG.tls.port),
                Style::default().fg(ACCENT),
            )),
        ));
        if CONFIG.tls.redirect_http {
            rows.push((
                Line::from("HTTP redirect"),
                status_value(
                    StatusKind::Healthy,
                    "RUNNING",
                    format!("port {}", CONFIG.tls.http_port),
                ),
            ));
        }
    } else {
        rows.push((
            Line::from("Local server"),
            status_value(
                StatusKind::Healthy,
                "RUNNING",
                format!("port {}", stats.http_port),
            ),
        ));
        rows.push((
            Line::from("Local URL"),
            Line::from(Span::styled(
                local_url(stats.http_port),
                Style::default().fg(ACCENT),
            )),
        ));
        rows.push((
            Line::from("HTTPS"),
            status_value(StatusKind::Neutral, "NOT CONFIGURED", ""),
        ));
    }

    match (&stats.onion_address, CONFIG.enable_tor_support) {
        (Some(address), true) => {
            let detail = if CONFIG.tor_only {
                "READY · tor-only".to_owned()
            } else {
                "READY".to_owned()
            };
            rows.push((
                Line::from("Tor"),
                status_value(StatusKind::Healthy, &detail, ""),
            ));
            rows.push((
                Line::from("Onion URL"),
                Line::from(Span::styled(
                    format!("http://{address}"),
                    Style::default().fg(ACCENT),
                )),
            ));
        }
        (None, true) => rows.push((
            Line::from("Tor"),
            status_value(
                StatusKind::Pending,
                "WAIT",
                if CONFIG.tor_only {
                    "bootstrapping · tor-only".to_owned()
                } else {
                    "bootstrapping".to_owned()
                },
            ),
        )),
        (_, false) => rows.push((
            Line::from("Tor"),
            status_value(StatusKind::Neutral, "DISABLED", ""),
        )),
    }
    rows
}

/// Render activity, content, storage, and background-work metrics.
fn render_operations_panel(buffer: &mut Buffer, area: Rect, stats: &ChanStats) {
    let block = panel("Operations", ACCENT);
    let inner = block.inner(area);
    block.render(area, buffer);

    if !stats.is_ready {
        render_centered_state(
            buffer,
            inner,
            "LOADING",
            "Collecting the first operational snapshot…",
            ACCENT,
        );
        return;
    }

    render_label_rows(buffer, inner, operations_rows(stats), 12);
}

/// Build authoritative overview labels independently of panel geometry.
fn operations_rows(stats: &ChanStats) -> Vec<(Line<'static>, Line<'static>)> {
    let rate_style = if stats.rps >= 1.0 {
        Style::default().fg(SUCCESS)
    } else {
        Style::default().fg(MUTED)
    };
    let in_flight_style = if stats.in_flight > 5 {
        Style::default().fg(WARNING)
    } else {
        Style::default()
    };
    let work_label = if stats.active_uploads == 0 && stats.active_ffmpeg_videos == 0 {
        status_value(StatusKind::Idle, "", "no active media work")
    } else {
        status_value(
            StatusKind::Active,
            "MEDIA",
            format!(
                "{} upload(s) · {} video(s)",
                stats.active_uploads, stats.active_ffmpeg_videos
            ),
        )
    };
    let mut rows = Vec::with_capacity(7);
    if let Some(error) = &stats.collection_error {
        rows.push((
            Line::from("Snapshot"),
            status_value(StatusKind::Error, "DEGRADED", error),
        ));
    }
    rows.extend([
        (
            Line::from("Traffic"),
            Line::from(vec![
                Span::raw(format!(
                    "{} total · ",
                    format_number_compact(stats.req_count)
                )),
                Span::styled(format!("{:.2}/s", stats.rps), rate_style),
                Span::raw(" · "),
                Span::styled(
                    format!("{} active", format_number_compact(stats.in_flight)),
                    in_flight_style,
                ),
            ]),
        ),
        (
            Line::from("Active IPs"),
            Line::from(stats.online.to_string()),
        ),
        (
            Line::from("Content"),
            Line::from(format!(
                "{} boards · {} threads · {} posts",
                format_number_signed_compact(stats.boards),
                format_number_signed_compact(stats.threads),
                format_number_signed_compact(stats.posts)
            )),
        ),
        (
            Line::from("Storage"),
            Line::from(format!(
                "{} DB · {} uploads",
                fmt_bytes(stats.db_bytes),
                fmt_bytes(stats.upload_bytes)
            )),
        ),
        (
            Line::from("Memory"),
            Line::from(if stats.mem_bytes <= 0 {
                "Unavailable".to_owned()
            } else {
                fmt_bytes(stats.mem_bytes)
            }),
        ),
        (Line::from("Media work"), work_label),
    ]);
    if let Some(summary) = stats.operator.jobs {
        rows.push((
            Line::from("Tasks"),
            Line::from(format!(
                "{} running · {} queued · {} failed",
                summary.running, summary.queued, summary.failed
            )),
        ));
        rows.push((
            Line::from("Media"),
            Line::from(format!(
                "{} pending · {} failed",
                stats.operator.media_pending, stats.operator.media_failed
            )),
        ));
    }
    rows
}

/// Render context-specific shortcuts at the bottom edge.
fn render_footer(frame: &mut Frame<'_>, area: Rect, app: &ConsoleState) {
    let hints = footer_hints(app, area.width);
    let mut lines = Vec::new();
    let mut spans = Vec::new();
    let mut columns = 0;
    for (key, label) in hints {
        let pair = [
            Span::styled(
                format!(" {key} "),
                Style::default()
                    .fg(Color::Black)
                    .bg(ACCENT)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!(" {label}"), Style::default().fg(MUTED)),
        ];
        let width: usize = pair.iter().map(Span::width).sum();
        if columns > 0 && columns + 3 + width > usize::from(area.width) {
            lines.push(Line::from(std::mem::take(&mut spans)));
            columns = 0;
        }
        if columns > 0 {
            spans.push(Span::raw("   "));
            columns += 3;
        }
        spans.extend(pair);
        columns += width;
    }
    lines.push(Line::from(spans));
    Paragraph::new(lines)
        .alignment(Alignment::Center)
        .render(area, frame.buffer_mut());
}

/// Return the active HTTPS certificate-source label.
fn https_cert_label() -> &'static str {
    if CONFIG.tls.acme.enabled {
        "Let's Encrypt"
    } else if CONFIG.tls.manual_cert.is_some() {
        "manual cert"
    } else {
        "self-signed"
    }
}

/// Return the effective local HTTP URL.
fn local_url(http_port: u16) -> String {
    format!("http://localhost:{http_port}")
}

/// Return the Tor-internal HTTP backend address using the bound port.
fn backend_address(http_port: u16) -> String {
    CONFIG.loopback_addr_with_port(http_port)
}

/// Return concise context-specific controls without mixing them with rendering.
const fn footer_hints(app: &ConsoleState, width: u16) -> &'static [(&'static str, &'static str)] {
    let editing = match app.screen {
        Screen::Boards => app.board_filter.editing,
        Screen::Tasks => app.task_filter.editing,
        Screen::Logs => app.log_filter.editing,
        _ => false,
    };
    if editing {
        &[
            ("Enter", "Apply"),
            ("Esc", "Clear / close"),
            ("Ctrl-U", "Clear"),
        ]
    } else if width < 76 {
        match app.screen {
            Screen::Dashboard => &[
                ("↑↓", "Scroll"),
                ("C", "Board"),
                ("A", "Admin"),
                ("?", "Help"),
                ("Q", "Quit"),
            ],
            Screen::Boards => &[
                ("Enter", "Inspect"),
                ("/", "Search"),
                ("S", "Sort"),
                ("C", "Create"),
            ],
            Screen::Logs => &[("/", "Search"), ("S", "Level"), ("P/F", "Pause/follow")],
            Screen::Tasks => &[
                ("Enter", "Inspect"),
                ("/", "Search"),
                ("S", "State"),
                ("?", "Help"),
            ],
            Screen::System | Screen::Configuration => {
                &[("↑↓", "Scroll"), ("?", "Help"), ("Esc", "Back")]
            }
            Screen::Help => &[("↑↓", "Scroll"), ("Esc", "Back")],
        }
    } else {
        match app.screen {
            Screen::Dashboard => &[
                ("↑↓", "Scroll"),
                ("C", "Board"),
                ("A", "New admin"),
                ("D", "Delete thread"),
                ("R", "Refresh"),
                ("Q", "Quit"),
            ],
            Screen::Boards => &[
                ("↑↓", "Select"),
                ("C", "New board"),
                ("D", "Delete thread"),
                ("/", "Search"),
                ("S", "Filter/sort"),
                ("Esc", "Back"),
            ],
            Screen::Logs => &[
                ("↑↓", "Scroll"),
                ("←→", "Pan"),
                ("F", "Follow"),
                ("/", "Search"),
                ("S", "Filter/sort"),
                ("Esc", "Back"),
            ],
            Screen::Tasks => &[
                ("↑↓", "Select"),
                ("Enter", "Inspect"),
                ("/", "Search"),
                ("S", "State filter"),
                ("?", "Help"),
            ],
            Screen::System | Screen::Configuration => &[
                ("↑↓", "Scroll"),
                ("A", "Create admin"),
                ("?", "Help"),
                ("Esc", "Back"),
            ],
            Screen::Help => &[("↑↓", "Scroll"), ("Esc", "Return to screen")],
        }
    }
}

/// Render directly into a buffer for deterministic layout tests.
#[cfg(test)]
fn render_to_buffer(
    area: Rect,
    app: &ConsoleState,
    metrics: &ChanStats,
    logs: &LogSnapshot,
) -> anyhow::Result<Buffer> {
    let mut terminal =
        ratatui::Terminal::new(ratatui::backend::TestBackend::new(area.width, area.height))?;
    let mut app = app.clone();
    terminal.draw(|frame| render(frame, &mut app, metrics, logs))?;
    Ok(terminal.backend().buffer().clone())
}

/// Convert a test buffer into trimmed plain text for semantic assertions.
#[cfg(test)]
fn buffer_text(buffer: &Buffer) -> String {
    use std::fmt::Write as _;

    let area = *buffer.area();
    let mut output = String::new();
    for y in area.top()..area.bottom() {
        let mut line = String::new();
        for x in area.left()..area.right() {
            if let Some(cell) = buffer.cell((x, y)) {
                line.push_str(cell.symbol());
            }
        }
        if writeln!(output, "{}", line.trim_end()).is_err() {
            break;
        }
    }
    output
}

#[cfg(test)]
#[path = "dashboard_tests.rs"]
mod tests;
