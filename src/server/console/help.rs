//! Scrollable keyboard and operator workflow reference.
use super::widgets::{panel, render_scrollable, ACCENT};
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Text};
use ratatui::widgets::{Cell, Row, Table, Widget};
use ratatui::Frame;

/// Render a responsive keyboard and workflow reference.
pub(super) fn render_help(frame: &mut Frame<'_>, area: Rect, offset: &mut u16) {
    let navigation = [
        ("1 / G", "Operational overview"),
        ("2 / T", "Tasks and activity"),
        ("3 / B", "Content / board list"),
        ("4 / L", "Live logs"),
        ("5 / 6", "System / Configuration"),
        ("? / H", "Contextual help; Esc returns"),
        ("Tab / Shift-Tab", "Next / previous destination"),
        ("R", "Refresh metrics now"),
        ("C", "Create board"),
        ("A", "Create administrator"),
        ("D / X", "Delete thread"),
        ("Q", "Graceful shutdown prompt"),
        ("Esc", "Close / return to overview"),
    ];
    let editing = [
        ("↑ ↓ / J K", "Move selection or scroll"),
        ("PgUp PgDn", "Move by one page"),
        ("Home End", "First/last row or newest log"),
        ("← →", "Pan long log lines"),
        ("F / P", "Follow / pause logs"),
        ("/", "Edit text filter on a list"),
        ("S", "Cycle sort / state / log level"),
        ("Enter", "Inspect selected task or board"),
        ("Tab / Shift-Tab", "Move form focus"),
        ("Space", "Toggle a setting"),
        ("Enter", "Advance or submit final field"),
        ("Ctrl-Enter / F2", "Submit the full form"),
        ("Ctrl-U", "Clear focused text"),
        ("Ctrl-C", "Immediate server stop"),
    ];
    let wide = area.width >= 84;
    let panel_width = if wide {
        area.width.saturating_sub(1) / 2
    } else {
        area.width
    };
    let navigation_height = help_panel_height(&navigation, panel_width);
    let editing_height = help_panel_height(&editing, panel_width);
    let minimum_height = if wide {
        navigation_height.max(editing_height)
    } else {
        navigation_height + editing_height + 1
    };
    render_scrollable(frame, area, minimum_height, offset, |buffer, area| {
        let sections = if wide {
            Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)])
                .spacing(1)
                .split(area)
        } else {
            Layout::vertical([
                Constraint::Length(navigation_height),
                Constraint::Min(editing_height),
            ])
            .spacing(1)
            .split(area)
        };
        render_help_panel(
            buffer,
            sections.first().copied().unwrap_or(area),
            "Navigation & actions",
            &navigation,
        );
        render_help_panel(
            buffer,
            sections.get(1).copied().unwrap_or(area),
            "Lists, logs & forms",
            &editing,
        );
    });
}

/// Wrap the static help descriptions to the available description column.
fn help_description(description: &str, panel_width: u16) -> Text<'static> {
    let width = usize::from(panel_width.saturating_sub(23)).max(1);
    let mut lines = Vec::new();
    let mut line = String::new();
    for word in description.split_whitespace() {
        if !line.is_empty() && line.len() + 1 + word.len() > width {
            lines.push(Line::from(std::mem::take(&mut line)));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    lines.push(Line::from(line));
    Text::from(lines)
}

/// Full panel height needed to keep every wrapped help row reachable.
fn help_panel_height(items: &[(&str, &str)], width: u16) -> u16 {
    let rows = items
        .iter()
        .map(|(_, description)| help_description(description, width).height())
        .sum::<usize>();
    u16::try_from(rows).unwrap_or(u16::MAX).saturating_add(2)
}

/// Render one help category as aligned key/description rows.
fn render_help_panel(buffer: &mut Buffer, area: Rect, title: &str, items: &[(&str, &str)]) {
    let block = panel(title, ACCENT);
    let inner = block.inner(area);
    block.render(area, buffer);
    let rows = items.iter().map(|(key, description)| {
        let text = help_description(description, area.width);
        let height = u16::try_from(text.height()).unwrap_or(u16::MAX);
        Row::new(vec![
            Cell::from((*key).to_owned())
                .style(Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)),
            Cell::from(text),
        ])
        .height(height)
    });
    Widget::render(
        Table::new(rows, [Constraint::Length(18), Constraint::Min(1)]),
        inner,
        buffer,
    );
}
