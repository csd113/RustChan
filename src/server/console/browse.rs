//! Shared bounded search, sorting and identity-preserving list projections.

use super::input::KeyEvent;
use super::telemetry::{TaskRow, TaskState};

/// Search and secondary filter state local to a screen.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ViewFilter {
    /// Case-insensitive substring query.
    pub query: String,
    /// Whether keystrokes belong to the search editor.
    pub editing: bool,
    /// Index of the screen-specific sort or severity/state choice.
    pub mode: usize,
    /// Most recently projected row count, used by input paging.
    pub row_count: usize,
}

impl ViewFilter {
    /// Route search editing before global shortcuts; cap paste and controls.
    pub fn edit(&mut self, key: &KeyEvent) {
        match key {
            KeyEvent::Character(character) | KeyEvent::RepeatCharacter(character)
                if !character.is_control() =>
            {
                if self.query.chars().count() < 120 {
                    self.query.push(*character);
                }
            }
            KeyEvent::Paste(value) => {
                let remaining = 120_usize.saturating_sub(self.query.chars().count());
                self.query.extend(
                    value
                        .chars()
                        .filter(|character| !character.is_control())
                        .take(remaining),
                );
            }
            KeyEvent::Backspace => {
                self.query.pop();
            }
            KeyEvent::ClearLine => self.query.clear(),
            KeyEvent::Enter => self.editing = false,
            KeyEvent::Escape => {
                self.query.clear();
                self.editing = false;
            }
            _ => {}
        }
    }

    /// Match a label against the current literal query.
    #[must_use]
    pub fn matches(&self, value: &str) -> bool {
        self.query.is_empty() || value.to_lowercase().contains(&self.query.to_lowercase())
    }

    /// Return the explicit task-state filter label.
    #[must_use]
    pub const fn task_label(&self) -> &'static str {
        match self.mode % 4 {
            1 => "Active",
            2 => "Failed",
            3 => "Completed",
            _ => "All",
        }
    }

    /// Test task state and nonsecret fields against this filter.
    #[must_use]
    pub fn task_matches(&self, task: &TaskRow) -> bool {
        let state_matches = match self.mode % 4 {
            1 => matches!(task.state, TaskState::Queued | TaskState::Running),
            2 => task.state == TaskState::Failed,
            3 => task.state == TaskState::Completed,
            _ => true,
        };
        state_matches
            && (self.query.is_empty()
                || self.matches(&task.id)
                || self.matches(&task.kind)
                || self.matches(&task.item))
    }

    /// Return the explicit log-severity filter label.
    #[must_use]
    pub const fn log_label(&self) -> &'static str {
        match self.mode % 4 {
            1 => "Warning + error",
            2 => "Error",
            3 => "Debug + trace",
            _ => "All levels",
        }
    }

    /// Match retained log records by level and text.
    #[must_use]
    pub fn log_matches(&self, line: &str) -> bool {
        let level = log_level(line);
        let severity = match self.mode % 4 {
            1 => matches!(level, "WARN" | "ERROR"),
            2 => level == "ERROR",
            3 => matches!(level, "DEBUG" | "TRACE"),
            _ => true,
        };
        severity && self.matches(line)
    }

    /// Project and deterministically sort the existing board-count records.
    #[must_use]
    pub fn boards(&self, rows: &[(String, i64, i64)]) -> Vec<(String, i64, i64)> {
        let mut visible: Vec<_> = rows
            .iter()
            .filter(|row| self.matches(&row.0))
            .cloned()
            .collect();
        visible.sort_by(|left, right| match self.mode % 3 {
            1 => right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)),
            2 => right.2.cmp(&left.2).then_with(|| left.0.cmp(&right.0)),
            _ => left.0.cmp(&right.0),
        });
        visible
    }
}

/// Extract the tracing severity field, ignoring severity words in the message.
pub(super) fn log_level(line: &str) -> &str {
    if line.starts_with('{') {
        for level in ["ERROR", "WARN", "INFO", "DEBUG", "TRACE"] {
            if line.contains(&format!("\"level\":\"{level}\""))
                || line.contains(&format!("\"level\": \"{level}\""))
            {
                return level;
            }
        }
        return "UNKNOWN";
    }
    line.split_whitespace()
        .take(4)
        .find_map(|word| {
            let level = word.trim_matches(['[', ']']);
            matches!(level, "ERROR" | "WARN" | "INFO" | "DEBUG" | "TRACE").then_some(level)
        })
        .unwrap_or("UNKNOWN")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_consumes_shortcuts_and_limits_untrusted_paste() {
        let mut filter = ViewFilter {
            editing: true,
            ..ViewFilter::default()
        };
        filter.edit(&KeyEvent::Paste(format!("q\n{}", "界".repeat(1000))));
        assert_eq!(filter.query.chars().count(), 120, "paste must stay bounded");
        assert!(
            !filter.query.contains('\n'),
            "search cannot inject terminal controls"
        );
        filter.edit(&KeyEvent::Escape);
        assert!(
            !filter.editing && filter.query.is_empty(),
            "escape must clear and close search"
        );
    }

    #[test]
    fn filtering_and_sorting_have_deterministic_ties() {
        let filter = ViewFilter {
            query: "A".to_owned(),
            mode: 2,
            ..ViewFilter::default()
        };
        assert_eq!(
            filter.boards(&[
                ("z".to_owned(), 0, 99),
                ("ab".to_owned(), 0, 5),
                ("aa".to_owned(), 0, 5)
            ]),
            vec![("aa".to_owned(), 0, 5), ("ab".to_owned(), 0, 5)],
            "search and sorting must compose"
        );
        let filter = ViewFilter {
            mode: 2,
            ..ViewFilter::default()
        };
        assert!(
            filter.log_matches("2026 ERROR workers failure"),
            "error filter must include errors"
        );
        assert!(
            !filter.log_matches("2026 INFO workers ERROR count=0"),
            "message content is not the log level"
        );
    }
    #[test]
    fn padded_rustchan_levels_are_recognized_before_message_content() {
        let filter = ViewFilter {
            mode: 1,
            ..ViewFilter::default()
        };
        assert!(
            filter.log_matches("2026-09-29 19:18:40.054 [WARN ] [workers ] failed"),
            "padded log levels must match severity filters"
        );
        assert!(
            !filter.log_matches("2026-09-29 19:18:40.054 [INFO ] [workers ] WARN count=0"),
            "message content cannot masquerade as severity"
        );
    }
}
