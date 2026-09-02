//! Ratatui rendering for every full-screen console surface.

use super::state::{
    ConsoleState, Dialog, FieldValue, FormField, FormState, NoticeSeverity, Screen,
};
use super::ChanStats;
use crate::config::CONFIG;
use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Constraint, Layout, Margin, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{
    Block, BorderType, Borders, Cell, Clear, Padding, Paragraph, Row, StatefulWidget, Table,
    TableState, Tabs, Widget, Wrap,
};
use ratatui::Frame;
use std::io::{Read as _, Seek as _, SeekFrom};
use std::path::{Path, PathBuf};

/// Smallest size that can retain useful hierarchy and interaction hints.
const MIN_USEFUL_WIDTH: u16 = 44;
/// Smallest height that can retain useful hierarchy and interaction hints.
const MIN_USEFUL_HEIGHT: u16 = 14;
/// Maximum number of bytes retained from the active log file.
const LOG_TAIL_BYTES: usize = 256 * 1024;
/// Primary brand and focus color.
const ACCENT: Color = Color::Cyan;
/// Healthy-state color.
const SUCCESS: Color = Color::Green;
/// Pending and caution color.
const WARNING: Color = Color::Yellow;
/// Error and destructive-action color.
const DANGER: Color = Color::Red;
/// Secondary text and inactive borders.
const MUTED: Color = Color::DarkGray;
/// Selected-row background.
const SELECTION_BG: Color = Color::Rgb(35, 48, 55);

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

/// Render the complete console frame.
pub fn render(frame: &mut Frame<'_>, app: &ConsoleState, metrics: &ChanStats, logs: &LogSnapshot) {
    let area = frame.area();
    if area.width < MIN_USEFUL_WIDTH || area.height < MIN_USEFUL_HEIGHT {
        render_small_terminal(frame, area);
        return;
    }

    let notice_height = u16::from(app.notice.is_some());
    let layout = Layout::vertical([
        Constraint::Length(3),
        Constraint::Length(3),
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
    render_navigation(frame, navigation_area, app.screen);
    if let Some(notice) = &app.notice {
        render_notice(frame, notice_area, notice.severity, &notice.message);
    }

    match app.screen {
        Screen::Dashboard => render_dashboard(frame, body_area, metrics),
        Screen::Boards => render_boards(frame, body_area, app, metrics),
        Screen::Logs => render_logs(frame, body_area, app, logs),
        Screen::Help => render_help(frame, body_area),
    }
    render_footer(frame, footer_area, app);

    if let Some(dialog) = &app.dialog {
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
        .margin(1)
        .split(area);
    let left = columns.first().copied().unwrap_or(area);
    let right = columns.get(1).copied().unwrap_or(area);
    let title = Line::from(vec![
        Span::styled(
            "RUSTCHAN",
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        ),
        Span::styled("  /  ", Style::default().fg(MUTED)),
        Span::styled(
            CONFIG.forum_name.clone(),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("  v{}", env!("CARGO_PKG_VERSION")),
            Style::default().fg(MUTED),
        ),
    ]);
    Paragraph::new(title).render(left, frame.buffer_mut());

    let freshness = stats_freshness(stats);
    let mut right_spans = vec![Span::styled(freshness.label, status_style(freshness.kind))];
    if area.width >= 64 {
        right_spans.push(Span::styled("  •  ", Style::default().fg(MUTED)));
        let uptime = if area.width >= 100 {
            fmt_uptime(stats.uptime_secs)
        } else {
            fmt_uptime_compact(stats.uptime_secs)
        };
        right_spans.push(Span::raw(format!("Uptime {uptime}")));
    }
    let right_line = Line::from(right_spans);
    Paragraph::new(right_line)
        .alignment(Alignment::Right)
        .render(right, frame.buffer_mut());
}

/// Render persistent screen navigation as a tab strip.
fn render_navigation(frame: &mut Frame<'_>, area: Rect, screen: Screen) {
    let titles = if area.width < 58 {
        [
            Line::from(" 1 Home "),
            Line::from(" 2 Boards "),
            Line::from(" 3 Logs "),
            Line::from(" 4 Help "),
        ]
    } else {
        [
            Line::from(" 1  Overview "),
            Line::from(" 2  Boards "),
            Line::from(" 3  Logs "),
            Line::from(" 4  Help "),
        ]
    };
    let selected = match screen {
        Screen::Dashboard => 0,
        Screen::Boards => 1,
        Screen::Logs => 2,
        Screen::Help => 3,
    };
    Tabs::new(titles)
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
        .divider(Span::styled("│", Style::default().fg(MUTED)))
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
fn render_dashboard(frame: &mut Frame<'_>, area: Rect, stats: &ChanStats) {
    if area.width >= 110 {
        let columns = Layout::horizontal([Constraint::Percentage(51), Constraint::Percentage(49)])
            .spacing(1)
            .split(area);
        render_service_panel(frame, columns.first().copied().unwrap_or(area), stats);
        render_operations_panel(frame, columns.get(1).copied().unwrap_or(area), stats);
    } else {
        let service_height = area.height.saturating_mul(45) / 100;
        let rows = Layout::vertical([
            Constraint::Length(service_height.max(6)),
            Constraint::Min(5),
        ])
        .spacing(1)
        .split(area);
        render_service_panel(frame, rows.first().copied().unwrap_or(area), stats);
        render_operations_panel(frame, rows.get(1).copied().unwrap_or(area), stats);
    }
}

/// Render server transports and direct operator endpoints.
fn render_service_panel(frame: &mut Frame<'_>, area: Rect, stats: &ChanStats) {
    let block = panel("Service & access", ACCENT);
    let inner = block.inner(area);
    block.render(area, frame.buffer_mut());
    let rows = service_rows(stats);
    render_label_rows(frame, inner, rows, 15);
}

/// Build service-status and endpoint rows.
fn service_rows(stats: &ChanStats) -> Vec<(Line<'static>, Line<'static>)> {
    let mut rows = Vec::with_capacity(7);
    if CONFIG.tls.enabled {
        if CONFIG.enable_tor_support {
            rows.push((
                Line::from("HTTP backend"),
                status_value(StatusKind::Healthy, "RUNNING", backend_address()),
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
                format!("port {}", configured_http_port()),
            ),
        ));
        rows.push((
            Line::from("Local URL"),
            Line::from(Span::styled(local_url(), Style::default().fg(ACCENT))),
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
fn render_operations_panel(frame: &mut Frame<'_>, area: Rect, stats: &ChanStats) {
    let block = panel("Operations", ACCENT);
    let inner = block.inner(area);
    block.render(area, frame.buffer_mut());

    if !stats.is_ready {
        render_centered_state(
            frame,
            inner,
            "LOADING",
            "Collecting the first operational snapshot…",
            ACCENT,
        );
        return;
    }

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
        status_value(StatusKind::Healthy, "IDLE", "no active media work")
    } else {
        status_value(
            StatusKind::Pending,
            "BUSY",
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
        (Line::from("Online"), Line::from(stats.online.to_string())),
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
        (Line::from("Memory"), Line::from(fmt_bytes(stats.mem_bytes))),
        (Line::from("Media work"), work_label),
    ]);
    render_label_rows(frame, inner, rows, 12);
}

/// Render label/value rows without allowing long values to escape their panel.
fn render_label_rows(
    frame: &mut Frame<'_>,
    area: Rect,
    rows: Vec<(Line<'static>, Line<'static>)>,
    label_width: u16,
) {
    let table_rows = rows.into_iter().map(|(label, value)| {
        Row::new(vec![
            Cell::from(label).style(Style::default().fg(MUTED)),
            Cell::from(value),
        ])
    });
    Widget::render(
        Table::new(
            table_rows,
            [Constraint::Length(label_width), Constraint::Min(1)],
        )
        .column_spacing(1),
        area,
        frame.buffer_mut(),
    );
}

/// Render the board table, detail panel, and empty state.
fn render_boards(frame: &mut Frame<'_>, area: Rect, app: &ConsoleState, metrics: &ChanStats) {
    if !metrics.is_ready {
        let block = panel("Boards", ACCENT);
        let inner = block.inner(area);
        block.render(area, frame.buffer_mut());
        render_centered_state(
            frame,
            inner,
            "LOADING",
            "Collecting board statistics…",
            ACCENT,
        );
        return;
    }
    if let Some(error) = &metrics.collection_error {
        let block = panel("Boards", DANGER);
        let inner = block.inner(area);
        block.render(area, frame.buffer_mut());
        render_centered_state(frame, inner, "DATA UNAVAILABLE", error, DANGER);
        return;
    }
    if metrics.board_rows.is_empty() {
        let block = panel("Boards", ACCENT);
        let inner = block.inner(area);
        block.render(area, frame.buffer_mut());
        render_centered_state(
            frame,
            inner,
            "NO BOARDS",
            "Create the first board with C.",
            MUTED,
        );
        return;
    }

    if area.width >= 78 {
        let columns = Layout::horizontal([Constraint::Percentage(64), Constraint::Percentage(36)])
            .spacing(1)
            .split(area);
        render_board_table(
            frame,
            columns.first().copied().unwrap_or(area),
            app,
            metrics,
        );
        render_board_detail(frame, columns.get(1).copied().unwrap_or(area), app, metrics);
    } else {
        render_board_table(frame, area, app, metrics);
    }
}

/// Render the selectable board statistics table.
fn render_board_table(frame: &mut Frame<'_>, area: Rect, app: &ConsoleState, metrics: &ChanStats) {
    let block = panel("Boards", ACCENT).title_bottom(Line::from(Span::styled(
        format!(" {} total ", metrics.board_rows.len()),
        Style::default().fg(MUTED),
    )));
    let rows = metrics.board_rows.iter().map(|(short, threads, posts)| {
        Row::new(vec![
            Cell::from(format!("/{short}/")),
            Cell::from(format_number_signed(*threads)),
            Cell::from(format_number_signed(*posts)),
        ])
    });
    let header = Row::new(["BOARD", "THREADS", "POSTS"])
        .style(Style::default().fg(MUTED).add_modifier(Modifier::BOLD))
        .bottom_margin(1);
    let table = Table::new(
        rows,
        [
            Constraint::Percentage(55),
            Constraint::Percentage(22),
            Constraint::Percentage(23),
        ],
    )
    .block(block)
    .header(header)
    .column_spacing(1)
    .row_highlight_style(
        Style::default()
            .fg(Color::White)
            .bg(SELECTION_BG)
            .add_modifier(Modifier::BOLD),
    )
    .highlight_symbol("› ");
    let mut table_state = TableState::default().with_selected(app.boards.selected);
    if let Some(selected) = app.boards.selected {
        let visible = usize::from(area.height.saturating_sub(4)).max(1);
        *table_state.offset_mut() = selected.saturating_sub(visible.saturating_sub(1));
    }
    StatefulWidget::render(table, area, frame.buffer_mut(), &mut table_state);
}

/// Render context for the selected board row.
fn render_board_detail(frame: &mut Frame<'_>, area: Rect, app: &ConsoleState, metrics: &ChanStats) {
    let block = panel("Selected board", ACCENT);
    let inner = block.inner(area);
    block.render(area, frame.buffer_mut());
    let selected = app
        .boards
        .selected
        .and_then(|selected| metrics.board_rows.get(selected));
    let Some((short, threads, posts)) = selected else {
        render_centered_state(frame, inner, "NO SELECTION", "Choose a board row.", MUTED);
        return;
    };
    let lines = vec![
        Line::from(Span::styled(
            format!("/{short}/"),
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        )),
        Line::default(),
        Line::from(vec![
            Span::styled("Threads  ", Style::default().fg(MUTED)),
            Span::raw(format_number_signed(*threads)),
        ]),
        Line::from(vec![
            Span::styled("Posts    ", Style::default().fg(MUTED)),
            Span::raw(format_number_signed(*posts)),
        ]),
        Line::default(),
        Line::from(Span::styled(
            "C  Create another board",
            Style::default().fg(MUTED),
        )),
        Line::from(Span::styled(
            "D  Delete a thread",
            Style::default().fg(MUTED),
        )),
    ];
    Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .render(inner, frame.buffer_mut());
}

/// Render the scrollable, follow-capable log viewer.
fn render_logs(frame: &mut Frame<'_>, area: Rect, app: &ConsoleState, logs: &LogSnapshot) {
    let rows = Layout::vertical([Constraint::Length(2), Constraint::Min(3)]).split(area);
    let info_area = rows.first().copied().unwrap_or(area);
    let body_area = rows.get(1).copied().unwrap_or(area);

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
        Span::styled("Source  ", Style::default().fg(MUTED)),
        Span::raw(source.to_owned()),
        Span::styled("   ", Style::default()),
        follow,
        Span::styled(
            format!(
                "   {} retained lines",
                format_number(u64::try_from(logs.lines.len()).unwrap_or(u64::MAX))
            ),
            Style::default().fg(MUTED),
        ),
    ]))
    .render(info_area, frame.buffer_mut());

    let block = panel("Live log", ACCENT).title_bottom(Line::from(Span::styled(
        " ↑↓/PgUp/PgDn scroll · ←→ pan · End/F follow ",
        Style::default().fg(MUTED),
    )));
    let inner = block.inner(body_area);
    block.render(body_area, frame.buffer_mut());

    if let Some(error) = &logs.error {
        render_centered_state(frame, inner, "LOG ERROR", error, DANGER);
        return;
    }
    if logs.lines.is_empty() {
        render_centered_state(
            frame,
            inner,
            "NO LOG ENTRIES",
            "New application events will appear here automatically.",
            MUTED,
        );
        return;
    }

    let visible_rows = usize::from(inner.height).max(1);
    let max_rows_from_bottom = logs.lines.len().saturating_sub(visible_rows);
    let rows_from_bottom = if app.logs.follow {
        0
    } else {
        app.logs.rows_from_bottom.min(max_rows_from_bottom)
    };
    let end = logs.lines.len().saturating_sub(rows_from_bottom);
    let start = end.saturating_sub(visible_rows);
    let visible = logs
        .lines
        .get(start..end)
        .unwrap_or_default()
        .iter()
        .map(|line| Line::from(Span::styled(line.clone(), log_line_style(line))))
        .collect::<Vec<_>>();
    Paragraph::new(visible)
        .scroll((0, app.logs.horizontal_offset))
        .render(inner, frame.buffer_mut());
}

/// Choose log emphasis while preserving the original severity text.
fn log_line_style(line: &str) -> Style {
    let uppercase = line.to_ascii_uppercase();
    if uppercase.contains("ERROR") {
        Style::default().fg(DANGER)
    } else if uppercase.contains("WARN") {
        Style::default().fg(WARNING)
    } else if uppercase.contains("DEBUG") || uppercase.contains("TRACE") {
        Style::default().fg(MUTED)
    } else {
        Style::default()
    }
}

/// Render a responsive keyboard and workflow reference.
fn render_help(frame: &mut Frame<'_>, area: Rect) {
    let sections = if area.width >= 84 {
        Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)])
            .spacing(1)
            .split(area)
    } else {
        Layout::vertical([Constraint::Percentage(50), Constraint::Percentage(50)])
            .spacing(1)
            .split(area)
    };
    render_help_panel(
        frame,
        sections.first().copied().unwrap_or(area),
        "Navigation & actions",
        &[
            ("1 / G", "Operational overview"),
            ("2 / B", "Board list"),
            ("3 / L", "Live logs"),
            ("4 / ? / H", "Keyboard reference"),
            ("R", "Refresh metrics now"),
            ("C", "Create board"),
            ("A", "Create administrator"),
            ("D / X", "Delete thread"),
            ("Q", "Graceful shutdown prompt"),
            ("Esc", "Close / return to overview"),
        ],
    );
    render_help_panel(
        frame,
        sections.get(1).copied().unwrap_or(area),
        "Lists, logs & forms",
        &[
            ("↑ ↓ / J K", "Move selection or scroll"),
            ("PgUp PgDn", "Move by one page"),
            ("Home End", "First/last row or newest log"),
            ("← →", "Pan long log lines"),
            ("F", "Resume log follow mode"),
            ("Tab / Shift-Tab", "Move form focus"),
            ("Space", "Toggle a setting"),
            ("Enter", "Advance or submit final field"),
            ("Ctrl-Enter / F2", "Submit the full form"),
            ("Ctrl-U", "Clear focused text"),
            ("Ctrl-C", "Immediate server stop"),
        ],
    );
}

/// Render one help category as aligned key/description rows.
fn render_help_panel(frame: &mut Frame<'_>, area: Rect, title: &str, items: &[(&str, &str)]) {
    let block = panel(title, ACCENT);
    let inner = block.inner(area);
    block.render(area, frame.buffer_mut());
    let rows = items.iter().map(|(key, description)| {
        Row::new(vec![
            Cell::from((*key).to_owned())
                .style(Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)),
            Cell::from((*description).to_owned()),
        ])
    });
    Widget::render(
        Table::new(rows, [Constraint::Length(18), Constraint::Min(1)]),
        inner,
        frame.buffer_mut(),
    );
}

/// Render context-specific shortcuts at the bottom edge.
fn render_footer(frame: &mut Frame<'_>, area: Rect, app: &ConsoleState) {
    let hints: &[(&str, &str)] = if area.width < 76 {
        match app.screen {
            Screen::Dashboard => &[("C", "Board"), ("A", "Admin"), ("?", "Help"), ("Q", "Quit")],
            Screen::Boards => &[("↑↓", "Select"), ("C", "Create"), ("Esc", "Back")],
            Screen::Logs => &[("↑↓", "Scroll"), ("F", "Follow"), ("Esc", "Back")],
            Screen::Help => &[("Esc", "Back"), ("Q", "Quit")],
        }
    } else {
        match app.screen {
            Screen::Dashboard => &[
                ("C", "New board"),
                ("A", "New admin"),
                ("D", "Delete thread"),
                ("R", "Refresh"),
                ("Q", "Quit"),
            ],
            Screen::Boards => &[
                ("↑↓", "Select"),
                ("C", "New board"),
                ("D", "Delete thread"),
                ("R", "Refresh"),
                ("Esc", "Overview"),
            ],
            Screen::Logs => &[
                ("↑↓", "Scroll"),
                ("←→", "Pan"),
                ("F", "Follow"),
                ("R", "Refresh"),
                ("Esc", "Overview"),
            ],
            Screen::Help => &[("Esc", "Overview"), ("Q", "Quit")],
        }
    };
    let mut spans = Vec::with_capacity(hints.len().saturating_mul(3));
    for (index, (key, label)) in hints.iter().enumerate() {
        if index > 0 {
            spans.push(Span::styled("   ", Style::default().fg(MUTED)));
        }
        spans.push(Span::styled(
            format!(" {key} "),
            Style::default()
                .fg(Color::Black)
                .bg(ACCENT)
                .add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(
            format!(" {label}"),
            Style::default().fg(MUTED),
        ));
    }
    Paragraph::new(Line::from(spans))
        .alignment(Alignment::Center)
        .render(area, frame.buffer_mut());
}

/// Render the active modal dialog above the current screen.
fn render_dialog(frame: &mut Frame<'_>, area: Rect, dialog: &Dialog, spinner_tick: u8) {
    dim_area(frame.buffer_mut(), area);
    match dialog {
        Dialog::ConfirmQuit => render_confirm_dialog(
            frame,
            area,
            "Stop RustChan?",
            "New requests will stop and in-flight requests will drain gracefully.",
            "Y / Enter  Stop server",
            WARNING,
        ),
        Dialog::ConfirmDelete { thread_id } => render_confirm_dialog(
            frame,
            area,
            "Permanently delete thread?",
            &format!("Thread {thread_id} and all of its posts and attached files will be removed."),
            "Y / Enter  Delete permanently",
            DANGER,
        ),
        Dialog::Progress { label } => render_progress_dialog(frame, area, label, spinner_tick),
        Dialog::Form(form) => render_form_dialog(frame, area, form),
    }
}

/// Dim the underlying screen so modal focus is visually unambiguous.
fn dim_area(buffer: &mut Buffer, area: Rect) {
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            if let Some(cell) = buffer.cell_mut((x, y)) {
                cell.set_style(cell.style().add_modifier(Modifier::DIM));
            }
        }
    }
}

/// Render a consistent confirmation dialog.
fn render_confirm_dialog(
    frame: &mut Frame<'_>,
    area: Rect,
    title: &str,
    message: &str,
    confirm: &str,
    accent: Color,
) {
    let popup = centered_rect(area, 72, 9);
    Clear.render(popup, frame.buffer_mut());
    let block = panel(title, accent).border_style(Style::default().fg(accent));
    let inner = block.inner(popup);
    block.render(popup, frame.buffer_mut());
    let lines = vec![
        Line::from(message.to_owned()),
        Line::default(),
        Line::from(vec![
            Span::styled(
                format!(" {confirm} "),
                Style::default()
                    .fg(Color::Black)
                    .bg(accent)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("    N / Esc  Cancel", Style::default().fg(MUTED)),
        ]),
    ];
    Paragraph::new(lines)
        .wrap(Wrap { trim: true })
        .render(inner, frame.buffer_mut());
}

/// Render a non-interactive progress overlay.
fn render_progress_dialog(frame: &mut Frame<'_>, area: Rect, label: &str, spinner_tick: u8) {
    let popup = centered_rect(area, 62, 7);
    Clear.render(popup, frame.buffer_mut());
    let block = panel("Working", ACCENT);
    let inner = block.inner(popup);
    block.render(popup, frame.buffer_mut());
    Paragraph::new(Line::from(vec![
        Span::styled(
            format!("{}  ", spinner_frame(spinner_tick)),
            Style::default().fg(ACCENT),
        ),
        Span::raw(label.to_owned()),
    ]))
    .alignment(Alignment::Center)
    .render(inner, frame.buffer_mut());
}

/// Render a keyboard-efficient form with inline help and validation.
fn render_form_dialog(frame: &mut Frame<'_>, area: Rect, form: &FormState) {
    let desired_height = u16::try_from(form.fields.len())
        .unwrap_or(u16::MAX)
        .saturating_add(9)
        .min(area.height.saturating_sub(2));
    let popup = centered_rect(area, 82, desired_height.max(11));
    Clear.render(popup, frame.buffer_mut());
    let block = panel(form.kind.title(), ACCENT).border_style(Style::default().fg(ACCENT));
    let inner = block.inner(popup);
    block.render(popup, frame.buffer_mut());

    let rows = Layout::vertical([
        Constraint::Length(2),
        Constraint::Min(3),
        Constraint::Length(2),
        Constraint::Length(2),
    ])
    .split(inner);
    let description_area = rows.first().copied().unwrap_or(inner);
    let fields_area = rows.get(1).copied().unwrap_or(inner);
    let help_area = rows.get(2).copied().unwrap_or(inner);
    let actions_area = rows.get(3).copied().unwrap_or(inner);

    Paragraph::new(form.kind.description())
        .style(Style::default().fg(MUTED))
        .wrap(Wrap { trim: true })
        .render(description_area, frame.buffer_mut());

    let visible_rows = usize::from(fields_area.height).max(1);
    let start = form.focused.saturating_sub(visible_rows.saturating_sub(1));
    let end = start.saturating_add(visible_rows).min(form.fields.len());
    let field_areas = Layout::vertical(std::iter::repeat_n(
        Constraint::Length(1),
        end.saturating_sub(start),
    ))
    .split(fields_area);
    let mut cursor_position = None;
    if let Some(fields) = form.fields.get(start..end) {
        for (visible_index, field) in fields.iter().enumerate() {
            let Some(row_area) = field_areas.get(visible_index).copied() else {
                continue;
            };
            let absolute_index = start.saturating_add(visible_index);
            let focused = absolute_index == form.focused;
            cursor_position =
                render_form_field(frame, row_area, field, focused, form.cursor).or(cursor_position);
        }
    }

    if let Some(error) = &form.error {
        Paragraph::new(Line::from(vec![
            Span::styled("[ERROR] ", Style::default().fg(DANGER).bold()),
            Span::styled(error.clone(), Style::default().fg(DANGER)),
        ]))
        .wrap(Wrap { trim: true })
        .render(help_area, frame.buffer_mut());
    } else if let Some(field) = form.focused_field() {
        Paragraph::new(field.help)
            .style(Style::default().fg(MUTED))
            .wrap(Wrap { trim: true })
            .render(help_area, frame.buffer_mut());
    }

    Paragraph::new(Line::from(vec![
        Span::styled(" Tab ", key_style()),
        Span::styled(" Next   ", Style::default().fg(MUTED)),
        Span::styled(" Space ", key_style()),
        Span::styled(" Toggle   ", Style::default().fg(MUTED)),
        Span::styled(" Ctrl-Enter / F2 ", key_style()),
        Span::styled(" Submit   ", Style::default().fg(MUTED)),
        Span::styled(" Esc ", key_style()),
        Span::styled(" Cancel", Style::default().fg(MUTED)),
    ]))
    .render(actions_area, frame.buffer_mut());

    if let Some(position) = cursor_position {
        frame.set_cursor_position(position);
    }
}

/// Render one form row and return the text cursor position when focused.
fn render_form_field(
    frame: &mut Frame<'_>,
    area: Rect,
    field: &FormField,
    focused: bool,
    cursor: usize,
) -> Option<Position> {
    let columns = Layout::horizontal([
        Constraint::Length(2),
        Constraint::Length(19),
        Constraint::Min(4),
    ])
    .split(area);
    let marker_area = columns.first().copied().unwrap_or(area);
    let label_area = columns.get(1).copied().unwrap_or(area);
    let value_area = columns.get(2).copied().unwrap_or(area);
    Paragraph::new(if focused { "›" } else { " " })
        .style(Style::default().fg(ACCENT).bold())
        .render(marker_area, frame.buffer_mut());
    Paragraph::new(field.label)
        .style(if focused {
            Style::default().add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(MUTED)
        })
        .render(label_area, frame.buffer_mut());

    match &field.value {
        FieldValue::Toggle(enabled) => {
            let (symbol, text) = if *enabled {
                ("[x]", "Enabled")
            } else {
                ("[ ]", "Disabled")
            };
            Paragraph::new(Line::from(vec![
                Span::styled(
                    symbol,
                    Style::default()
                        .fg(if *enabled { SUCCESS } else { MUTED })
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(format!(" {text}")),
            ]))
            .style(input_style(focused))
            .render(value_area, frame.buffer_mut());
            None
        }
        FieldValue::Text(value) => {
            let displayed = if field.secret {
                "•".repeat(value.chars().count())
            } else {
                value.clone()
            };
            let cursor_prefix = displayed.chars().take(cursor).collect::<String>();
            let cursor_width = Line::from(cursor_prefix).width();
            let viewport_width = usize::from(value_area.width).max(1);
            let horizontal_scroll = cursor_width.saturating_sub(viewport_width.saturating_sub(1));
            Paragraph::new(displayed)
                .style(input_style(focused))
                .scroll((0, u16::try_from(horizontal_scroll).unwrap_or(u16::MAX)))
                .render(value_area, frame.buffer_mut());
            if focused {
                let cursor_column = cursor_width.saturating_sub(horizontal_scroll);
                Some(Position::new(
                    value_area
                        .x
                        .saturating_add(u16::try_from(cursor_column).unwrap_or(u16::MAX)),
                    value_area.y,
                ))
            } else {
                None
            }
        }
    }
}

/// Return the visual style for a form value.
fn input_style(focused: bool) -> Style {
    if focused {
        Style::default().fg(Color::White).bg(SELECTION_BG)
    } else {
        Style::default()
    }
}

/// Return the compact visual treatment for a key cap.
fn key_style() -> Style {
    Style::default()
        .fg(Color::Black)
        .bg(ACCENT)
        .add_modifier(Modifier::BOLD)
}

/// Render a vertically centered state label and explanation.
fn render_centered_state(
    frame: &mut Frame<'_>,
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
    .render(centered, frame.buffer_mut());
}

/// Build a consistently padded and titled panel.
fn panel(title: &str, color: Color) -> Block<'_> {
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
fn centered_rect(area: Rect, requested_width: u16, requested_height: u16) -> Rect {
    let width = requested_width.min(area.width.saturating_sub(2)).max(1);
    let height = requested_height.min(area.height.saturating_sub(2)).max(1);
    let horizontal_margin = area.width.saturating_sub(width) / 2;
    let vertical_margin = area.height.saturating_sub(height) / 2;
    area.inner(Margin {
        horizontal: horizontal_margin,
        vertical: vertical_margin,
    })
}

/// Status semantic used by labeled tags.
#[derive(Clone, Copy)]
enum StatusKind {
    /// Healthy and available.
    Healthy,
    /// Waiting on expected startup work.
    Pending,
    /// Disabled or not applicable.
    Neutral,
    /// Failed or unavailable.
    Error,
}

/// Short status summary used in the header.
struct StatusSummary {
    /// Status tag text.
    label: &'static str,
    /// Status semantic.
    kind: StatusKind,
}

/// Determine snapshot readiness and staleness.
fn stats_freshness(stats: &ChanStats) -> StatusSummary {
    if !stats.is_ready {
        StatusSummary {
            label: "[LOADING]",
            kind: StatusKind::Pending,
        }
    } else if stats.collection_error.is_some() {
        StatusSummary {
            label: "[DEGRADED]",
            kind: StatusKind::Error,
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
fn status_style(kind: StatusKind) -> Style {
    let color = match kind {
        StatusKind::Healthy => SUCCESS,
        StatusKind::Pending => WARNING,
        StatusKind::Neutral => MUTED,
        StatusKind::Error => DANGER,
    };
    Style::default().fg(color).add_modifier(Modifier::BOLD)
}

/// Build a status line that remains explicit without color.
fn status_value(kind: StatusKind, label: &str, detail: impl Into<String>) -> Line<'static> {
    let detail = detail.into();
    let tag = match kind {
        StatusKind::Healthy => format!("[OK] {label}"),
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
fn local_url() -> String {
    format!("http://localhost:{}", configured_http_port())
}

/// Return the Tor-internal HTTP backend address.
fn backend_address() -> String {
    CONFIG.loopback_addr_with_port(configured_http_port())
}

/// Parse the configured application port with the established fallback.
fn configured_http_port() -> u16 {
    CONFIG
        .bind_addr
        .rsplit_once(':')
        .and_then(|(_, port)| port.parse::<u16>().ok())
        .unwrap_or(8080)
}

/// Format uptime compactly while retaining days for long-running servers.
fn fmt_uptime(seconds: u64) -> String {
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
fn fmt_uptime_compact(seconds: u64) -> String {
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
fn fmt_bytes(bytes: i64) -> String {
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
fn format_number(number: u64) -> String {
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
fn format_number_signed(number: i64) -> String {
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
fn format_number_compact(number: u64) -> String {
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
fn format_number_signed_compact(number: i64) -> String {
    if number < 0 {
        return "unavailable".to_owned();
    }
    u64::try_from(number).map_or_else(|_| "unavailable".to_owned(), format_number_compact)
}

/// Return one platform-appropriate spinner frame.
#[cfg(windows)]
fn spinner_frame(tick: u8) -> &'static str {
    const FRAMES: [&str; 4] = ["|", "/", "-", "\\"];
    FRAMES
        .get(usize::from(tick) % FRAMES.len())
        .copied()
        .unwrap_or("|")
}

/// Return one platform-appropriate spinner frame.
#[cfg(not(windows))]
fn spinner_frame(tick: u8) -> &'static str {
    const FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
    FRAMES
        .get(usize::from(tick) % FRAMES.len())
        .copied()
        .unwrap_or("⠋")
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
    match read_log_tail(&path, LOG_TAIL_BYTES) {
        Ok((content, truncated)) => LogSnapshot {
            source: Some(source),
            lines: content.lines().map(str::to_owned).collect(),
            truncated,
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

/// Find the newest main-process log file.
fn latest_log_file(logs_dir: &Path) -> Option<PathBuf> {
    let mut files = std::fs::read_dir(logs_dir)
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| crate::logging::is_main_log_file(path))
        .collect::<Vec<_>>();
    files.sort();
    files.pop()
}

/// Read at most the newest `max_bytes` of a log file without loading its prefix.
fn read_log_tail(path: &Path, max_bytes: usize) -> Result<(String, bool), String> {
    let mut file = std::fs::File::open(path).map_err(|error| format!("Open log: {error}"))?;
    let length = file
        .metadata()
        .map_err(|error| format!("Log metadata: {error}"))?
        .len();
    let start = length.saturating_sub(u64::try_from(max_bytes).unwrap_or(u64::MAX));
    file.seek(SeekFrom::Start(start))
        .map_err(|error| format!("Seek log: {error}"))?;
    let mut bytes =
        Vec::with_capacity(usize::try_from(length.saturating_sub(start)).unwrap_or(max_bytes));
    std::io::copy(&mut file.take(length.saturating_sub(start)), &mut bytes)
        .map_err(|error| format!("Read log: {error}"))?;
    let text = String::from_utf8_lossy(&bytes).into_owned();
    let truncated = start > 0;
    let content = if truncated {
        text.split_once('\n')
            .map_or_else(|| text.clone(), |(_, remainder)| remainder.to_owned())
    } else {
        text
    };
    Ok((content, truncated))
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
    terminal.draw(|frame| render(frame, app, metrics, logs))?;
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
mod tests {
    use super::*;

    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "assertion failures are the intended failure mechanism for this test"
    )]
    fn every_representative_terminal_size_renders_without_overflow() -> anyhow::Result<()> {
        let app = ConsoleState::default();
        let metrics = ChanStats {
            is_ready: true,
            boards: 12,
            threads: 3_456,
            posts: 987_654,
            ..ChanStats::default()
        };
        let logs = LogSnapshot::default();

        for (width, height) in [(40, 10), (44, 14), (60, 18), (80, 24), (120, 40)] {
            let buffer = render_to_buffer(Rect::new(0, 0, width, height), &app, &metrics, &logs)?;
            assert_eq!(
                buffer.area.width, width,
                "rendered width should match backend"
            );
            assert_eq!(
                buffer.area.height, height,
                "rendered height should match backend"
            );
        }
        Ok(())
    }

    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "assertion failures are the intended failure mechanism for this test"
    )]
    fn overview_preserves_primary_information_hierarchy() -> anyhow::Result<()> {
        let app = ConsoleState::default();
        let metrics = ChanStats {
            is_ready: true,
            boards: 4,
            threads: 25,
            posts: 1_200,
            ..ChanStats::default()
        };

        let buffer = render_to_buffer(
            Rect::new(0, 0, 120, 32),
            &app,
            &metrics,
            &LogSnapshot::default(),
        )?;
        let text = buffer_text(&buffer);

        assert!(
            text.contains("RUSTCHAN"),
            "header should retain product identity"
        );
        assert!(
            text.contains("Service & access"),
            "transport health should have a clear panel"
        );
        assert!(
            text.contains("Operations"),
            "operator metrics should have a clear panel"
        );
        assert!(
            text.contains("Content"),
            "content metrics should remain grouped"
        );
        assert!(
            text.contains("1,200 posts"),
            "large counts should remain scannable"
        );
        Ok(())
    }

    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "assertion failures are the intended failure mechanism for this test"
    )]
    fn form_dialog_exposes_masking_help_and_submission_controls() -> anyhow::Result<()> {
        let app = ConsoleState {
            dialog: Some(Dialog::Form(FormState::new(
                super::super::state::FormKind::CreateAdmin,
            ))),
            ..ConsoleState::default()
        };

        let buffer = render_to_buffer(
            Rect::new(0, 0, 90, 28),
            &app,
            &ChanStats::default(),
            &LogSnapshot::default(),
        )?;
        let text = buffer_text(&buffer);

        assert!(
            text.contains("Create administrator"),
            "form should name its action"
        );
        assert!(
            text.contains("Password"),
            "password fields should be discoverable"
        );
        assert!(
            text.contains("Credentials are masked"),
            "the form should explain password masking"
        );
        assert!(
            text.contains("Ctrl-Enter / F2"),
            "submission shortcut should be visible"
        );
        Ok(())
    }

    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "assertion failures are the intended failure mechanism for this test"
    )]
    fn narrow_terminal_uses_explicit_resize_state() -> anyhow::Result<()> {
        let buffer = render_to_buffer(
            Rect::new(0, 0, 40, 10),
            &ConsoleState::default(),
            &ChanStats::default(),
            &LogSnapshot::default(),
        )?;
        let text = buffer_text(&buffer);

        assert!(
            text.contains("Terminal too small"),
            "undersized terminals should receive an actionable state"
        );
        assert!(
            text.contains("40 × 10"),
            "current dimensions should be visible"
        );
        Ok(())
    }

    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "assertion failures are the intended failure mechanism for this test"
    )]
    fn log_tail_reads_only_the_configured_suffix() -> anyhow::Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("rustchan.log");
        std::fs::write(&path, "discard-this-line\nkeep-one\nkeep-two\n")?;

        let (tail, truncated) = read_log_tail(&path, 20).map_err(anyhow::Error::msg)?;

        assert!(truncated, "a short byte limit should report truncation");
        assert!(
            !tail.contains("discard"),
            "discarded prefix should not be loaded"
        );
        assert!(
            tail.contains("keep-two"),
            "newest complete line should remain"
        );
        Ok(())
    }

    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "assertion failures are the intended failure mechanism for this test"
    )]
    fn log_selection_ignores_dependency_log() -> anyhow::Result<()> {
        let directory = tempfile::tempdir()?;
        std::fs::write(directory.path().join("rustchan.2026-04-01.log"), "main")?;
        std::fs::write(
            directory
                .path()
                .join(crate::logging::DEPENDENCY_LOG_FILE_NAME),
            "dependency",
        )?;

        let latest = latest_log_file(directory.path())
            .ok_or_else(|| anyhow::anyhow!("main log not found"))?;

        assert_eq!(
            latest.file_name().and_then(|name| name.to_str()),
            Some("rustchan.2026-04-01.log"),
            "main-process log should win"
        );
        Ok(())
    }
}
