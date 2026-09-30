//! Searchable content list and selected board context.
use super::state::ConsoleState;
use super::widgets::{
    format_number_signed, panel, render_centered_state, ACCENT, DANGER, MUTED, SELECTION_BG,
};
use super::ChanStats;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Cell, Paragraph, Row, StatefulWidget, Table, TableState, Widget, Wrap};
use ratatui::Frame;

/// Render the board table, detail panel, and empty state.
pub(super) fn render_boards(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &mut ConsoleState,
    metrics: &ChanStats,
) {
    let regions = Layout::vertical([Constraint::Length(1), Constraint::Min(3)]).split(area);
    let mode = match app.board_filter.mode % 3 {
        1 => "Threads descending",
        2 => "Posts descending",
        _ => "Name ascending",
    };
    Paragraph::new(super::operator_views::filter_line(
        &app.board_filter.query,
        app.board_filter.editing,
        mode,
        app.board_filter.row_count,
    ))
    .render(regions.first().copied().unwrap_or(area), frame.buffer_mut());
    let area = regions.get(1).copied().unwrap_or(area);
    let rows = app.board_filter.boards(&metrics.board_rows);
    if !metrics.is_ready {
        let block = panel("Boards", ACCENT);
        let inner = block.inner(area);
        block.render(area, frame.buffer_mut());
        render_centered_state(
            frame.buffer_mut(),
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
        render_centered_state(frame.buffer_mut(), inner, "DATA UNAVAILABLE", error, DANGER);
        return;
    }
    if rows.is_empty() {
        let block = panel("Boards", ACCENT);
        let inner = block.inner(area);
        block.render(area, frame.buffer_mut());
        render_centered_state(
            frame.buffer_mut(),
            inner,
            "NO BOARDS",
            if app.board_filter.query.is_empty() {
                "Create the first board with C."
            } else {
                "No boards match. Esc clears the text filter."
            },
            MUTED,
        );
        return;
    }

    if area.width >= 78 {
        let columns = Layout::horizontal([Constraint::Percentage(64), Constraint::Percentage(36)])
            .spacing(1)
            .split(area);
        render_board_table(frame, columns.first().copied().unwrap_or(area), app, &rows);
        render_board_detail(frame, columns.get(1).copied().unwrap_or(area), app, &rows);
    } else {
        render_board_table(frame, area, app, &rows);
    }
}

/// Render the selectable board statistics table.
fn render_board_table(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &mut ConsoleState,
    rows: &[(String, i64, i64)],
) {
    let block = panel("Boards", ACCENT).title_bottom(Line::from(Span::styled(
        format!(" {} total ", rows.len()),
        Style::default().fg(MUTED),
    )));
    let rows = rows.iter().map(|(short, threads, posts)| {
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
    app.boards.visible_rows = usize::from(area.height.saturating_sub(4)).max(1);
    let mut table_state = TableState::default().with_selected(app.boards.selected);
    if let Some(selected) = app.boards.selected {
        let visible = usize::from(area.height.saturating_sub(4)).max(1);
        *table_state.offset_mut() = selected.saturating_sub(visible.saturating_sub(1));
    }
    StatefulWidget::render(table, area, frame.buffer_mut(), &mut table_state);
}

/// Render context for the selected board row.
fn render_board_detail(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &ConsoleState,
    rows: &[(String, i64, i64)],
) {
    let block = panel("Selected board", ACCENT);
    let inner = block.inner(area);
    block.render(area, frame.buffer_mut());
    let selected = app.boards.selected.and_then(|selected| rows.get(selected));
    let Some((short, threads, posts)) = selected else {
        render_centered_state(
            frame.buffer_mut(),
            inner,
            "NO SELECTION",
            "Choose a board row.",
            MUTED,
        );
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
