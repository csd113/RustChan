//! Operator task tables, measured activity, diagnostics and explicit settings.

use super::state::ConsoleState;
use super::telemetry::{display_text, timestamp, TaskRow, TaskState};
use super::widgets::{
    fmt_bytes, panel, render_centered_state, render_scrollable, ACCENT, DANGER, MUTED,
    SELECTION_BG, SUCCESS, WARNING,
};
use super::ChanStats;
use crate::config::CONFIG;
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Cell, Paragraph, Row, Sparkline, StatefulWidget, Table, TableState, Widget, Wrap,
};
use ratatui::Frame;

/// Render measured traffic history with explicit units and warmup state.
pub(super) fn render_history(buffer: &mut Buffer, area: Rect, stats: &ChanStats) {
    let block = panel("Recent traffic · measured request intervals", ACCENT);
    let inner = block.inner(area);
    block.render(area, buffer);
    if stats.history.samples.is_empty() {
        Paragraph::new(
            "Warming up · first interval after 10 seconds; history is retained in memory.",
        )
        .style(Style::default().fg(MUTED))
        .wrap(Wrap { trim: true })
        .render(inner, buffer);
        return;
    }
    let (average, peak) = stats.history.rates();
    let rows = Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).split(inner);
    let seconds: f64 = stats
        .history
        .samples
        .iter()
        .map(|sample| sample.seconds)
        .sum();
    Paragraph::new(format!(
        "Now {:.2}/s · avg {average:.2}/s · peak {peak:.2}/s · {seconds:.0}s observed",
        stats.rps
    ))
    .render(rows.first().copied().unwrap_or(inner), buffer);
    let width = usize::from(inner.width);
    let start = stats.history.samples.len().saturating_sub(width);
    let values: Vec<_> = stats
        .history
        .samples
        .iter()
        .skip(start)
        .map(super::history::TrafficSample::spark_value)
        .collect();
    Sparkline::default()
        .data(&values)
        .style(Style::default().fg(ACCENT))
        .render(rows.get(1).copied().unwrap_or(inner), buffer);
}

/// Semantic styling shared by task rows and detail state labels.
fn task_style(state: TaskState) -> Style {
    Style::default().fg(match state {
        TaskState::Running => ACCENT,
        TaskState::Queued => WARNING,
        TaskState::Completed => SUCCESS,
        TaskState::Failed => DANGER,
        TaskState::Unknown => MUTED,
    })
}

/// Render a search/status bar shared by operator lists.
pub(super) fn filter_line(query: &str, editing: bool, mode: &str, count: usize) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            if editing { "Search> " } else { "Filter: " },
            Style::default().fg(ACCENT),
        ),
        Span::raw(if query.is_empty() {
            "All text".to_owned()
        } else {
            query.to_owned()
        }),
        Span::styled(
            format!(" · {mode} · {count} rows"),
            Style::default().fg(MUTED),
        ),
    ])
}

/// Render durable jobs, tracked runtime work, and deliberate detail inspection.
pub(super) fn render_tasks(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &mut ConsoleState,
    stats: &ChanStats,
) {
    let snapshot = &stats.operator;
    if !stats.is_ready || snapshot.error.is_some() {
        let block = panel("Tasks", ACCENT);
        let inner = block.inner(area);
        block.render(area, frame.buffer_mut());
        render_centered_state(
            frame.buffer_mut(),
            inner,
            if stats.is_ready {
                "UNAVAILABLE"
            } else {
                "LOADING"
            },
            snapshot
                .error
                .as_deref()
                .unwrap_or("Reading durable jobs and runtime trackers…"),
            if stats.is_ready { DANGER } else { ACCENT },
        );
        return;
    }
    let tasks: Vec<_> = snapshot
        .tasks
        .iter()
        .filter(|task| app.task_filter.task_matches(task))
        .collect();
    let regions = Layout::vertical([
        Constraint::Length(if area.width < 90 { 2 } else { 1 }),
        Constraint::Length(1),
        Constraint::Min(3),
    ])
    .split(area);
    if let Some(summary) = snapshot.jobs {
        Paragraph::new(format!(
            "Workers: {} failed (unacknowledged) · {} running · {} queued · {} completed/24h",
            summary.failed, summary.running, summary.queued, summary.recent_completed
        ))
        .wrap(Wrap { trim: true })
        .render(regions.first().copied().unwrap_or(area), frame.buffer_mut());
    }
    Paragraph::new(filter_line(
        &app.task_filter.query,
        app.task_filter.editing,
        app.task_filter.task_label(),
        tasks.len(),
    ))
    .render(regions.get(1).copied().unwrap_or(area), frame.buffer_mut());
    let body = regions.get(2).copied().unwrap_or(area);
    let columns = Layout::horizontal([Constraint::Percentage(64), Constraint::Percentage(36)])
        .spacing(1)
        .split(body);
    let table_area = if body.width >= 110 {
        columns.first().copied().unwrap_or(body)
    } else {
        body
    };
    render_task_table(frame, table_area, app, &tasks, snapshot.tasks_truncated);
    if body.width >= 110 {
        render_task_detail(
            frame.buffer_mut(),
            columns.get(1).copied().unwrap_or(body),
            app.selected_task.as_ref(),
        );
    }
}

/// Render bounded task rows with compact columns on narrow terminals.
fn render_task_table(
    frame: &mut Frame<'_>,
    table_area: Rect,
    app: &mut ConsoleState,
    tasks: &[&TaskRow],
    truncated: bool,
) {
    let block = panel("Tasks · S changes state filter", ACCENT).title_bottom(if truncated {
        " Bounded rows · some records omitted "
    } else {
        " Enter inspect · / search "
    });
    if tasks.is_empty() {
        let inner = block.inner(table_area);
        block.render(table_area, frame.buffer_mut());
        render_centered_state(
            frame.buffer_mut(),
            inner,
            "NO TASKS",
            if app.task_filter.query.is_empty() && app.task_filter.mode.is_multiple_of(4) {
                "No retained jobs. Worker activity will appear automatically."
            } else {
                "No tasks match this filter. Esc clears text; S changes state."
            },
            MUTED,
        );
    } else {
        let compact = table_area.width < 68;
        let now = chrono::Utc::now().timestamp();
        let rows = tasks.iter().map(|task| {
            let age = task.changed_at.map_or_else(
                || "—".to_owned(),
                |time| format!("{}s", now.saturating_sub(time).max(0)),
            );
            let cells = if compact {
                vec![
                    Cell::from(task.state.label()).style(task_style(task.state)),
                    Cell::from(format!("{} · {}", task.kind, task.item)),
                ]
            } else {
                vec![
                    Cell::from(task.state.label()).style(task_style(task.state)),
                    Cell::from(task.kind.as_str()),
                    Cell::from(task.item.as_str()),
                    Cell::from(age),
                ]
            };
            Row::new(cells)
        });
        let widths = if compact {
            vec![Constraint::Length(10), Constraint::Min(1)]
        } else {
            vec![
                Constraint::Length(10),
                Constraint::Length(17),
                Constraint::Min(1),
                Constraint::Length(8),
            ]
        };
        let header = if compact {
            vec!["STATE", "OPERATION / ITEM"]
        } else {
            vec!["STATE", "OPERATION", "ITEM", "AGE*"]
        };
        let table = Table::new(rows, widths)
            .header(Row::new(header).style(Style::default().fg(MUTED)))
            .block(block)
            .column_spacing(1)
            .row_highlight_style(Style::default().bg(SELECTION_BG).bold())
            .highlight_symbol("› ");
        app.tasks.visible_rows = usize::from(table_area.height.saturating_sub(3)).max(1);
        let mut selection = TableState::default().with_selected(app.tasks.selected);
        *selection.offset_mut() = app
            .tasks
            .selected
            .unwrap_or(0)
            .saturating_sub(app.tasks.visible_rows.saturating_sub(1));
        StatefulWidget::render(table, table_area, frame.buffer_mut(), &mut selection);
    }
}

/// Describe selected work without inventing an ETA or worker identity.
fn render_task_detail(buffer: &mut Buffer, area: Rect, task: Option<&TaskRow>) {
    let block = panel("Selected task", ACCENT);
    let inner = block.inner(area);
    block.render(area, buffer);
    let Some(task) = task else {
        render_centered_state(buffer, inner, "NO SELECTION", "Choose a task row.", MUTED);
        return;
    };
    let lines = vec![
        Line::from(Span::styled(task.state.label(), task_style(task.state).bold())),
        Line::from(task.kind.as_str()), Line::from(task.item.as_str()), Line::default(),
        Line::from(format!("ID {}", task.id)),
        Line::from(format!("Attempts {}", task.attempts.map_or_else(|| "Not published".to_owned(), |count| count.to_string()))),
        Line::from(format!("Changed {}", task.changed_at.map_or_else(|| "Not published".to_owned(), timestamp))),
        Line::default(), Line::from(task.detail.as_str()), Line::default(),
        Line::from("* Age = time since state transition; queued age is time waiting since last transition."),
        Line::from("Enter opens all details."),
    ];
    Paragraph::new(lines)
        .wrap(Wrap { trim: true })
        .render(inner, buffer);
}

/// Render diagnostics and real account/moderation summaries without private fields.
pub(super) fn render_system(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &mut ConsoleState,
    stats: &ChanStats,
) {
    let snapshot = &stats.operator;
    let mut lines = vec![
        format!(
            "RustChan {} · uptime {}s",
            env!("CARGO_PKG_VERSION"),
            stats.uptime_secs
        ),
        format!(
            "Database: {}",
            if !stats.is_ready {
                "Loading"
            } else if stats.collection_error.is_some() {
                "Unavailable · inspect Logs"
            } else {
                "Queries available (not an integrity check)"
            }
        ),
        format!("Schema baseline: {}", crate::db::baseline_schema_version()),
        format!("Database file: {}", display_text(&CONFIG.database_path)),
        format!(
            "Uploads: {} · {}",
            display_text(&CONFIG.upload_dir),
            fmt_bytes(stats.upload_bytes)
        ),
        format!(
            "Upload size sample: {} · cache refreshes every 30s",
            stats.storage_sampled_at.map_or_else(
                || "Unknown".to_owned(),
                |time| format!("{}s ago", time.elapsed().as_secs())
            )
        ),
        format!(
            "Data directory: {}",
            display_text(&crate::config::data_dir().display().to_string())
        ),
        format!(
            "Resident memory: {}",
            if stats.mem_bytes == 0 {
                "Unavailable".to_owned()
            } else {
                fmt_bytes(stats.mem_bytes)
            }
        ),
        "WebP/images: built in (Rust), including animation".to_owned(),
        format!(
            "FFmpeg video: {}",
            snapshot.ffmpeg.map_or_else(
                || "Unknown · awaiting snapshot".to_owned(),
                |(available, vp9)| format!(
                    "{} · WebM VP9/Opus {}",
                    availability(available),
                    availability(vp9)
                )
            )
        ),
        format!(
            "Maintenance gate: {}",
            snapshot.maintenance.as_deref().unwrap_or("Idle")
        ),
        format!("Queue rejections since start: {}", snapshot.dropped),
        String::new(),
    ];
    append_administration_lines(&mut lines, snapshot, stats.is_ready);
    let reconcile = snapshot.reconcile;
    lines.push(String::new());
    lines.push("Managed-media observations since process start".to_owned());
    lines.push(format!(
        "{} files scanned · {} references · {} missing references",
        reconcile.files_scanned_total,
        reconcile.references_scanned_total,
        reconcile.missing_references_total
    ));
    lines.push(format!(
        "{} repairs · {} conflicts · {} incomplete scans",
        reconcile.repairs_total, reconcile.repair_conflicts_total, reconcile.incomplete_scans_total
    ));
    render_document(
        frame,
        area,
        "System · diagnostics, accounts & moderation",
        &lines,
        &mut app.system_scroll,
    );
}

/// Append bounded account and moderation data with deliberate failure states.
fn append_administration_lines(
    lines: &mut Vec<String>,
    snapshot: &super::telemetry::OperatorSnapshot,
    is_ready: bool,
) {
    if let Some(error) = &snapshot.error {
        lines.push(format!("[UNAVAILABLE] {error}"));
    } else if !is_ready {
        lines.push("[LOADING] Account and moderation data".to_owned());
    } else {
        lines.push(format!(
            "Accounts: {} administrators · A creates an administrator",
            snapshot.admin_count
        ));
        lines.extend(snapshot.admins.iter().map(|(id, name, created)| {
            format!(
                "  #{id} {} · created {}",
                display_text(name),
                timestamp(*created)
            )
        }));
        if snapshot.admin_count > i64::try_from(snapshot.admins.len()).unwrap_or(i64::MAX) {
            lines.push(
                "Showing the newest 25 accounts; the web admin lists all accounts.".to_owned(),
            );
        }
        lines.push(String::new());
        lines.push(format!(
            "Moderation: {} open reports · {} appeals · {} active bans",
            snapshot.reports, snapshot.appeals, snapshot.bans
        ));
        lines.push(
            "Use authenticated /admin for reports, bans, board policies and database diagnostics."
                .to_owned(),
        );
        lines.push(String::new());
        lines.push("Recent moderation events (UTC; private details omitted)".to_owned());
        if snapshot.moderation.is_empty() {
            lines.push("No moderation events recorded.".to_owned());
        } else {
            lines.extend(snapshot.moderation.iter().cloned());
        }
    }
}

/// Return explicit capability wording without relying on color.
const fn availability(available: bool) -> &'static str {
    if available {
        "Available"
    } else {
        "Unavailable"
    }
}

/// Render a deliberately read-only allowlist of settings and their scope.
pub(super) fn render_configuration(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &mut ConsoleState,
    stats: &ChanStats,
) {
    let lines = vec![
        "READ ONLY · This screen never modifies configuration.".to_owned(),
        format!("Source: {} (environment/CLI overrides may apply)", display_text(&crate::config::data_dir().join("settings.toml").display().to_string())),
        "Sensitive settings are omitted. Use authenticated /admin for supported changes.".to_owned(),
        String::new(), "Runtime values · currently effective".to_owned(),
        format!("FFmpeg timeout: {} seconds", crate::config::ffmpeg_timeout_secs()),
        format!("Automatic full backups: {}", stats.operator.backup_schedule.map_or_else(|| "Unknown · awaiting snapshot".to_owned(), |(hours, copies)| format!("{} · retain {copies} copies", interval(hours, "hours")))),
        String::new(), "Startup values · changes require restart".to_owned(),
        format!("HTTP bind: {} · effective port {}", display_text(&CONFIG.bind_addr), stats.http_port),
        format!("HTTPS: {} · port {}", if CONFIG.tls.enabled { "Enabled" } else { "Disabled" }, CONFIG.tls.port),
        format!("Tor: {} · Tor-only {}", if CONFIG.enable_tor_support { "Enabled" } else { "Disabled" }, CONFIG.tor_only),
        format!("Queue capacity: {}", if CONFIG.job_queue_capacity == 0 { "Unlimited (configured)".to_owned() } else { CONFIG.job_queue_capacity.to_string() }),
        format!("WAL checkpoint: {}", interval(CONFIG.wal_checkpoint_interval, "seconds")),
        format!("Scheduled VACUUM: {}", interval(CONFIG.auto_vacuum_interval_hours, "hours")),
        format!("Poll cleanup: {}", interval(CONFIG.poll_cleanup_interval_hours, "hours")),
        format!("Managed-media reconciliation: {}", interval(CONFIG.media_reconcile_interval_hours, "hours")),
        format!("Repair scheduling: {}", if CONFIG.media_reconcile_repair_enabled { "Enabled" } else { "Disabled" }),
        "Database repairs, backup/restore and destructive changes use the authenticated web admin confirmations.".to_owned(),
    ];
    render_document(
        frame,
        area,
        "Configuration · source & effective settings",
        &lines,
        &mut app.system_scroll,
    );
}

/// Render an enabled schedule or explicit disabled state.
fn interval(value: u64, unit: &str) -> String {
    if value == 0 {
        "Disabled".to_owned()
    } else {
        format!("Every {value} {unit}")
    }
}

/// Wrap and scroll diagnostic documents so narrow terminals preserve all fields.
fn render_document(
    frame: &mut Frame<'_>,
    area: Rect,
    title: &str,
    lines: &[String],
    offset: &mut u16,
) {
    let text = super::widgets::wrap_lines(lines, area.width.saturating_sub(4));
    let height = u16::try_from(text.len())
        .unwrap_or(u16::MAX)
        .saturating_add(2);
    let paragraph = Paragraph::new(text.into_iter().map(Line::from).collect::<Vec<_>>());
    render_scrollable(frame, area, height, offset, |buffer, content| {
        let block = panel(title, ACCENT);
        let inner = block.inner(content);
        block.render(content, buffer);
        paragraph.render(inner, buffer);
    });
}
