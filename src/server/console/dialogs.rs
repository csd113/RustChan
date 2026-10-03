//! Shared modal detail, confirmation, progress and validated form presentation.
use super::state::{Dialog, FieldValue, FormField, FormState};
use super::widgets::{
    centered_rect, panel, spinner_frame, ACCENT, DANGER, MUTED, SELECTION_BG, SUCCESS, WARNING,
};
use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph, Widget, Wrap};
use ratatui::Frame;

/// Render the active modal dialog above the current screen.
pub(super) fn render_dialog(
    frame: &mut Frame<'_>,
    area: Rect,
    dialog: &mut Dialog,
    spinner_tick: u8,
) {
    dim_area(frame.buffer_mut(), area);
    match dialog {
        Dialog::Inspect {
            title,
            lines,
            scroll,
        } => {
            let popup = centered_rect(area, 92, area.height.saturating_sub(4));
            Clear.render(popup, frame.buffer_mut());
            let block = panel(title, ACCENT).title_bottom(" ↑↓/PgUp/PgDn scroll · Esc close ");
            let inner = block.inner(popup);
            block.render(popup, frame.buffer_mut());
            let wrapped = super::widgets::wrap_lines(lines, inner.width);
            let maximum = wrapped.len().saturating_sub(usize::from(inner.height));
            *scroll = (*scroll).min(u16::try_from(maximum).unwrap_or(u16::MAX));
            Paragraph::new(wrapped.into_iter().map(Line::from).collect::<Vec<_>>())
                .scroll((*scroll, 0))
                .render(inner, frame.buffer_mut());
        }
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
            "Y  Delete permanently",
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
                let _styled_cell = cell.set_style(cell.style().add_modifier(Modifier::DIM));
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
    let expanded_error = form.error.is_some() && area.width < 64;
    let desired_height = u16::try_from(form.fields.len())
        .unwrap_or(u16::MAX)
        .saturating_add(9_u16.saturating_add(u16::from(expanded_error)))
        .min(area.height.saturating_sub(2));
    let popup = centered_rect(
        area,
        82,
        desired_height.max(if expanded_error { 12 } else { 11 }),
    );
    Clear.render(popup, frame.buffer_mut());
    let block = panel(form.kind.title(), ACCENT).border_style(Style::default().fg(ACCENT));
    let inner = block.inner(popup);
    block.render(popup, frame.buffer_mut());

    let rows = Layout::vertical([
        Constraint::Length(2),
        Constraint::Min(3),
        Constraint::Length(if expanded_error { 3 } else { 2 }),
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

    Paragraph::new(vec![
        Line::from(vec![
            Span::styled(" Tab ", key_style()),
            Span::styled(" Next   ", Style::default().fg(MUTED)),
            Span::styled(" Space ", key_style()),
            Span::styled(" Toggle", Style::default().fg(MUTED)),
        ]),
        Line::from(vec![
            Span::styled(
                if inner.width < 60 {
                    " F2 "
                } else {
                    " Ctrl-Enter / F2 "
                },
                key_style(),
            ),
            Span::styled(" Submit   ", Style::default().fg(MUTED)),
            Span::styled(" Esc ", key_style()),
            Span::styled(" Cancel", Style::default().fg(MUTED)),
        ]),
    ])
    .render(actions_area, frame.buffer_mut());

    if let Some(position) = cursor_position {
        frame.set_cursor_position(position);
    }
}

/// Render one form row and return the text cursor position when focused.
pub(super) fn render_form_field(
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
            let desired_scroll = if focused {
                cursor_width.saturating_sub(viewport_width.saturating_sub(1))
            } else {
                0
            };
            // Scroll only at complete grapheme boundaries. Cutting through a
            // double-width glyph shifts the visible text away from the cursor.
            let line = Line::from(displayed.as_str());
            let mut horizontal_scroll = 0usize;
            let mut visible = String::new();
            for grapheme in line.styled_graphemes(Style::default()) {
                if horizontal_scroll < desired_scroll {
                    horizontal_scroll =
                        horizontal_scroll.saturating_add(Span::raw(grapheme.symbol).width());
                } else {
                    visible.push_str(grapheme.symbol);
                }
            }
            Paragraph::new(visible)
                .style(input_style(focused))
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

/// Contextual help overlays its originating screen and retains visible navigation.
pub(super) fn render_help_overlay(
    frame: &mut Frame<'_>,
    area: Rect,
    screen: super::state::Screen,
    scroll: &mut u16,
) {
    dim_area(frame.buffer_mut(), area);
    let popup = centered_rect(area, 104, area.height.saturating_sub(2));
    Clear.render(popup, frame.buffer_mut());
    let title = format!("Help · {screen:?} · Esc returns");
    let block = panel(&title, ACCENT);
    let inner = block.inner(popup);
    block.render(popup, frame.buffer_mut());
    super::help::render_help(frame, inner, scroll);
}
