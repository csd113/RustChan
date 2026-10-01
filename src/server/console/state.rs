//! Typed state and keyboard transitions for the full-screen console.

use super::browse::ViewFilter;
use super::forms::{handle_form_key, FormAction};
pub use super::forms::{FieldValue, FormField, FormFieldId, FormKind, FormState};
use super::input::KeyEvent;
use super::telemetry::{timestamp, TaskRow};
use std::fmt;
use std::time::{Duration, Instant};

/// Duration for transient success and information notices.
const NOTICE_TTL: Duration = Duration::from_secs(8);

/// Whether the terminal can display the console and its dialogs.
#[must_use]
pub const fn terminal_is_usable(width: u16, height: u16) -> bool {
    width >= 44 && height >= 14
}

/// Primary console destination.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Screen {
    /// Live operational overview.
    #[default]
    Dashboard,
    /// Durable and runtime background work.
    Tasks,
    /// Per-board content table.
    Boards,
    /// Runtime diagnostics and account/moderation summaries.
    System,
    /// Explicit nonsecret runtime and restart configuration.
    Configuration,
    /// Application log viewer.
    Logs,
    /// Contextual keyboard reference.
    Help,
}

impl Screen {
    /// Return the screen selected by a numeric navigation shortcut.
    const fn from_number(number: char) -> Option<Self> {
        match number {
            '1' => Some(Self::Dashboard),
            '2' => Some(Self::Tasks),
            '3' => Some(Self::Boards),
            '4' => Some(Self::Logs),
            '5' => Some(Self::System),
            '6' => Some(Self::Configuration),
            _ => None,
        }
    }
}

/// Severity attached to operator feedback.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoticeSeverity {
    /// Successful administrative action.
    Success,
    /// Neutral progress or refresh information.
    Info,
    /// Recoverable failure requiring operator attention.
    Error,
}

/// Feedback shown beneath the main navigation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notice {
    /// Visual and semantic severity.
    pub severity: NoticeSeverity,
    /// Human-readable outcome.
    pub message: String,
    /// Creation time used for transient expiry.
    created_at: Instant,
}

impl Notice {
    /// Construct a new notice at the current time.
    fn new(severity: NoticeSeverity, message: impl Into<String>) -> Self {
        Self {
            severity,
            message: message.into(),
            created_at: Instant::now(),
        }
    }

    /// Return whether this notice should disappear automatically.
    fn is_expired(&self, now: Instant) -> bool {
        self.severity != NoticeSeverity::Error
            && now.saturating_duration_since(self.created_at) >= NOTICE_TTL
    }
}

/// Selection state for the board table.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BoardListState {
    /// Selected row, when at least one board exists.
    pub selected: Option<usize>,
    /// Identity retained across statistics refreshes.
    pub selected_short: Option<String>,
    /// Number of rows visible in the latest frame.
    pub visible_rows: usize,
}

impl BoardListState {
    /// Keep the selected board stable when rows are inserted or removed.
    pub fn reconcile_rows(&mut self, rows: &[(String, i64, i64)]) {
        if let Some(short) = &self.selected_short {
            if let Some(index) = rows.iter().position(|(name, _, _)| name == short) {
                self.selected = Some(index);
            }
        }
        self.reconcile(rows.len());
        self.selected_short = self
            .selected
            .and_then(|index| rows.get(index))
            .map(|row| row.0.clone());
    }

    /// Reconcile selection after the backing row count changes.
    pub fn reconcile(&mut self, row_count: usize) {
        self.selected = match (self.selected, row_count) {
            (_, 0) => None,
            (Some(selected), count) => Some(selected.min(count.saturating_sub(1))),
            (None, _) => Some(0),
        };
    }

    /// Move the selection by one row without wrapping.
    fn move_by(&mut self, delta: i32, row_count: usize) {
        self.selected_short = None;
        self.reconcile(row_count);
        let Some(selected) = self.selected else {
            return;
        };
        self.selected = if delta.is_negative() {
            Some(selected.saturating_sub(1))
        } else {
            Some(selected.saturating_add(1).min(row_count.saturating_sub(1)))
        };
    }

    /// Move the selection by a page-sized distance.
    fn page_by(&mut self, delta: i32, row_count: usize) {
        let page = self.visible_rows.max(1);
        self.selected_short = None;
        self.reconcile(row_count);
        let Some(selected) = self.selected else {
            return;
        };
        self.selected = if delta.is_negative() {
            Some(selected.saturating_sub(page))
        } else {
            Some(
                selected
                    .saturating_add(page)
                    .min(row_count.saturating_sub(1)),
            )
        };
    }
}

/// Scroll and follow state for the live log viewer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogViewState {
    /// Number of rows held above the newest line.
    pub rows_from_bottom: usize,
    /// Horizontal column offset.
    pub horizontal_offset: u16,
    /// Whether newly appended lines keep the view pinned to the end.
    pub follow: bool,
    /// Height of the most recently rendered log viewport.
    pub visible_rows: usize,
}

impl Default for LogViewState {
    fn default() -> Self {
        Self {
            rows_from_bottom: 0,
            horizontal_offset: 0,
            follow: true,
            visible_rows: 10,
        }
    }
}

/// Modal content layered over the active screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Dialog {
    /// Read-only contextual details, never raw payloads.
    Inspect {
        /// Human-readable title.
        title: String,
        /// Safe, labeled detail lines.
        lines: Vec<String>,
        /// Scroll position within the detail.
        scroll: u16,
    },
    /// Graceful server-shutdown confirmation.
    ConfirmQuit,
    /// Administrative data-entry form.
    Form(FormState),
    /// Destructive thread-deletion confirmation.
    ConfirmDelete {
        /// Thread selected for deletion.
        thread_id: i64,
    },
    /// Blocking database or password-hashing operation.
    Progress {
        /// Present-tense operation label.
        label: &'static str,
    },
}

/// Complete interaction state shared by input and render tasks.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ConsoleState {
    /// Active primary screen.
    pub screen: Screen,
    /// Optional modal dialog.
    pub dialog: Option<Dialog>,
    /// Optional operator feedback.
    pub notice: Option<Notice>,
    /// Board-table selection.
    pub boards: BoardListState,
    /// Durable task selection, retaining identity across refreshes.
    pub tasks: BoardListState,
    /// Selected safe task detail from the latest snapshot.
    pub selected_task: Option<TaskRow>,
    /// Content search and sort.
    pub board_filter: ViewFilter,
    /// Task search and lifecycle filter.
    pub task_filter: ViewFilter,
    /// Log search and severity filter.
    pub log_filter: ViewFilter,
    /// Last visible log record for read-only inspection.
    pub selected_log: Option<String>,
    /// Origin screen restored when contextual help closes.
    pub help_return: Option<Screen>,
    /// Scroll position for System and Configuration.
    pub system_scroll: u16,
    /// Log scrolling and follow mode.
    pub logs: LogViewState,
    /// Vertical viewport for overview panels on short terminals.
    pub overview_scroll: u16,
    /// Vertical viewport for the keyboard reference.
    pub help_scroll: u16,
}

impl ConsoleState {
    /// Reconcile filtered selections with the latest authoritative snapshot.
    pub fn reconcile_data(&mut self, stats: &super::ChanStats) {
        let boards = self.board_filter.boards(&stats.board_rows);
        self.board_filter.row_count = boards.len();
        self.boards.reconcile_rows(&boards);
        let tasks: Vec<_> = stats
            .operator
            .tasks
            .iter()
            .filter(|task| self.task_filter.task_matches(task))
            .collect();
        if let Some(id) = &self.tasks.selected_short {
            if let Some(index) = tasks.iter().position(|task| &task.id == id) {
                self.tasks.selected = Some(index);
            }
        }
        self.tasks.reconcile(tasks.len());
        self.task_filter.row_count = tasks.len();
        self.selected_task = self
            .tasks
            .selected
            .and_then(|index| tasks.get(index))
            .map(|task| (*task).clone());
        self.tasks.selected_short = self.selected_task.as_ref().map(|task| task.id.clone());
    }

    /// Return the search state for the current destination.
    pub const fn active_filter(&mut self) -> Option<&mut ViewFilter> {
        match self.screen {
            Screen::Boards => Some(&mut self.board_filter),
            Screen::Tasks => Some(&mut self.task_filter),
            Screen::Logs => Some(&mut self.log_filter),
            _ => None,
        }
    }

    /// Open contextual task or content details without launching side effects.
    fn inspect_selection(&mut self) {
        let detail = match self.screen {
            Screen::Tasks => self.selected_task.as_ref().map(|task| (task.kind.clone(), vec![
                format!("Identity: {}", task.id), format!("State: {}", task.state.label()),
                format!("Item: {}", task.item),
                format!("Attempts: {}", task.attempts.map_or_else(|| "Not published".to_owned(), |value| value.to_string())),
                format!("Created: {}", task.created_at.map_or_else(|| "Not published".to_owned(), timestamp)),
                format!("State changed: {}", task.changed_at.map_or_else(|| "Not published".to_owned(), timestamp)),
                task.detail.clone(), "Per-job cancellation/retry and worker identity are not published by this console.".to_owned(),
            ])),
            Screen::Logs => self.selected_log.as_ref().map(|line| ("Log entry · last visible record".to_owned(), vec![line.clone()])),
            Screen::Boards => self.boards.selected_short.as_ref().map(|short| (format!("Board /{short}/"), vec![
                format!("Route: /{short}/"), "Manage board policies and moderation in the authenticated /admin interface.".to_owned(),
                "C creates a board; D selects a thread for confirmed deletion.".to_owned(),
            ])),
            _ => None,
        };
        if let Some((title, lines)) = detail {
            self.dialog = Some(Dialog::Inspect {
                title,
                lines,
                scroll: 0,
            });
        }
    }

    /// Cycle the suite's primary destinations without entering the help overlay.
    fn cycle_screen(&mut self, backward: bool) {
        let screens = [
            Screen::Dashboard,
            Screen::Tasks,
            Screen::Boards,
            Screen::Logs,
            Screen::System,
            Screen::Configuration,
        ];
        let index = screens
            .iter()
            .position(|screen| *screen == self.screen)
            .unwrap_or(0);
        let next = if backward {
            (index + screens.len() - 1) % screens.len()
        } else {
            (index + 1) % screens.len()
        };
        if let Some(screen) = screens.get(next) {
            self.screen = *screen;
        }
    }

    /// Remove a transient notice after its display period.
    pub fn expire_notice(&mut self, now: Instant) {
        if self
            .notice
            .as_ref()
            .is_some_and(|notice| notice.is_expired(now))
        {
            self.notice = None;
        }
    }

    /// Replace the current notice with a new message.
    pub fn set_notice(&mut self, severity: NoticeSeverity, message: impl Into<String>) {
        self.notice = Some(Notice::new(severity, message));
    }

    /// Complete a background operation and return to its most useful screen.
    pub fn finish_operation(&mut self, request: &OperationRequest, result: Result<String, String>) {
        self.dialog = None;
        match result {
            Ok(message) => {
                if matches!(request, OperationRequest::CreateBoard { .. }) {
                    self.screen = Screen::Boards;
                }
                self.set_notice(NoticeSeverity::Success, message);
            }
            Err(message) => self.set_notice(NoticeSeverity::Error, message),
        }
    }

    /// Route one input event and return any side effect for the server runtime.
    pub fn handle_key(
        &mut self,
        key: &KeyEvent,
        board_count: usize,
        size: (u16, u16),
    ) -> ConsoleAction {
        if matches!(key, KeyEvent::ForceQuit) {
            return ConsoleAction::Shutdown { forced: true };
        }

        // A hidden form or confirmation must never accept input. Retain its
        // state until a usable terminal is restored; Ctrl-C remains available.
        if !terminal_is_usable(size.0, size.1) {
            return ConsoleAction::None;
        }

        if let Some(dialog) = self.dialog.take() {
            return self.handle_dialog(dialog, key);
        }

        if self.screen == Screen::Help
            && matches!(key, KeyEvent::Escape | KeyEvent::Character('?' | 'q' | 'h'))
        {
            self.screen = self.help_return.take().unwrap_or_default();
            return ConsoleAction::None;
        }
        if let Some(filter) = self.active_filter() {
            if filter.editing {
                filter.edit(key);
                self.selected_log = None;
                return ConsoleAction::None;
            }
            if matches!(key, KeyEvent::Character('/')) {
                filter.editing = true;
                return ConsoleAction::None;
            }
            if matches!(key, KeyEvent::Character('s' | 'S')) {
                filter.mode = filter.mode.wrapping_add(1);
                self.selected_log = None;
                return ConsoleAction::None;
            }
            if matches!(key, KeyEvent::Escape) && !filter.query.is_empty() {
                filter.query.clear();
                return ConsoleAction::None;
            }
        }
        if let KeyEvent::Character(character) = key {
            if let Some(screen) = Screen::from_number(*character) {
                self.screen = screen;
                self.notice = None;
                return ConsoleAction::None;
            }
        }

        match key {
            KeyEvent::Character('q' | 'Q') => self.dialog = Some(Dialog::ConfirmQuit),
            KeyEvent::Character('?' | 'h' | 'H') => {
                self.help_return = Some(self.screen);
                self.screen = Screen::Help;
                self.help_scroll = 0;
            }
            KeyEvent::Character('t' | 'T') => self.screen = Screen::Tasks,
            KeyEvent::Tab => self.cycle_screen(false),
            KeyEvent::BackTab => self.cycle_screen(true),
            KeyEvent::Enter => self.inspect_selection(),
            KeyEvent::Character('g' | 'G') => self.screen = Screen::Dashboard,
            KeyEvent::Character('b' | 'B') => self.screen = Screen::Boards,
            KeyEvent::Character('l' | 'L') => self.screen = Screen::Logs,
            KeyEvent::Character('c' | 'C') => {
                self.dialog = Some(Dialog::Form(FormState::new(FormKind::CreateBoard)));
            }
            KeyEvent::Character('a' | 'A') => {
                self.dialog = Some(Dialog::Form(FormState::new(FormKind::CreateAdmin)));
            }
            KeyEvent::Character('d' | 'D' | 'x' | 'X') => {
                self.dialog = Some(Dialog::Form(FormState::new(FormKind::DeleteThread)));
            }
            KeyEvent::Character('r' | 'R') => {
                self.set_notice(NoticeSeverity::Info, "Refreshing operational metrics…");
                return ConsoleAction::Reload;
            }
            KeyEvent::Escape => {
                if self.screen == Screen::Dashboard {
                    self.notice = None;
                } else {
                    self.screen = Screen::Dashboard;
                }
            }
            KeyEvent::RepeatCharacter(character) => {
                self.handle_screen_key(&KeyEvent::Character(*character), board_count);
            }
            _ => self.handle_screen_key(key, board_count),
        }
        ConsoleAction::None
    }

    /// Handle an input event while a modal is active.
    fn handle_dialog(&mut self, mut dialog: Dialog, key: &KeyEvent) -> ConsoleAction {
        match &mut dialog {
            Dialog::Inspect { scroll, .. } => {
                match key {
                    KeyEvent::Escape | KeyEvent::Character('q') => return ConsoleAction::None,
                    KeyEvent::Up | KeyEvent::Character('k') => *scroll = scroll.saturating_sub(1),
                    KeyEvent::Down | KeyEvent::Character('j') => *scroll = scroll.saturating_add(1),
                    KeyEvent::PageUp => *scroll = scroll.saturating_sub(5),
                    KeyEvent::PageDown => *scroll = scroll.saturating_add(5),
                    KeyEvent::Home => *scroll = 0,
                    _ => {}
                }
                self.dialog = Some(dialog);
            }
            Dialog::ConfirmQuit => match key {
                KeyEvent::Enter | KeyEvent::Character('y' | 'Y') => {
                    return ConsoleAction::Shutdown { forced: false };
                }
                KeyEvent::Escape | KeyEvent::Character('n' | 'N' | 'q' | 'Q') => {}
                _ => self.dialog = Some(dialog),
            },
            Dialog::ConfirmDelete { thread_id } => match key {
                KeyEvent::Character('y' | 'Y') => {
                    let request = OperationRequest::DeleteThread {
                        thread_id: *thread_id,
                    };
                    self.dialog = Some(Dialog::Progress {
                        label: request.progress_label(),
                    });
                    return ConsoleAction::Submit(request);
                }
                KeyEvent::Escape | KeyEvent::Character('n' | 'N') => {}
                _ => self.dialog = Some(dialog),
            },
            Dialog::Progress { .. } => self.dialog = Some(dialog),
            Dialog::Form(form) => {
                if matches!(key, KeyEvent::Escape) {
                    return ConsoleAction::None;
                }
                if let Some(action) = handle_form_key(form, key) {
                    match action {
                        FormAction::KeepOpen => self.dialog = Some(dialog),
                        FormAction::Submit(request) => {
                            if let OperationRequest::DeleteThread { thread_id } = request {
                                self.dialog = Some(Dialog::ConfirmDelete { thread_id });
                            } else {
                                self.dialog = Some(Dialog::Progress {
                                    label: request.progress_label(),
                                });
                                return ConsoleAction::Submit(request);
                            }
                        }
                    }
                } else {
                    self.dialog = Some(dialog);
                }
            }
        }
        ConsoleAction::None
    }

    /// Move through the bounded task table without wrapping or losing identity.
    fn handle_tasks_key(&mut self, key: &KeyEvent) {
        let count = self.task_filter.row_count;
        match key {
            KeyEvent::Up | KeyEvent::Character('k' | 'K') => self.tasks.move_by(-1, count),
            KeyEvent::Down | KeyEvent::Character('j' | 'J') => self.tasks.move_by(1, count),
            KeyEvent::PageUp => self.tasks.page_by(-1, count),
            KeyEvent::PageDown => self.tasks.page_by(1, count),
            KeyEvent::Home => {
                self.tasks.selected_short = None;
                self.tasks.selected = (count > 0).then_some(0);
            }
            KeyEvent::End => {
                self.tasks.selected_short = None;
                self.tasks.selected = (count > 0).then_some(count.saturating_sub(1));
            }
            _ => {}
        }
    }

    /// Handle navigation local to the active primary screen.
    fn handle_screen_key(&mut self, key: &KeyEvent, board_count: usize) {
        match self.screen {
            Screen::Tasks => self.handle_tasks_key(key),
            Screen::Boards => match key {
                KeyEvent::Up | KeyEvent::Character('k' | 'K') => {
                    self.boards.move_by(-1, board_count);
                }
                KeyEvent::Down | KeyEvent::Character('j' | 'J') => {
                    self.boards.move_by(1, board_count);
                }
                KeyEvent::PageUp => self.boards.page_by(-1, board_count),
                KeyEvent::PageDown => self.boards.page_by(1, board_count),
                KeyEvent::Home => {
                    self.boards.selected_short = None;
                    self.boards.reconcile(board_count);
                    if board_count > 0 {
                        self.boards.selected = Some(0);
                    }
                }
                KeyEvent::End => {
                    self.boards.selected_short = None;
                    self.boards.reconcile(board_count);
                    if board_count > 0 {
                        self.boards.selected = Some(board_count.saturating_sub(1));
                    }
                }
                _ => {}
            },
            Screen::Logs => match key {
                KeyEvent::Up | KeyEvent::Character('k' | 'K') => {
                    self.logs.rows_from_bottom = self.logs.rows_from_bottom.saturating_add(1);
                    self.logs.follow = false;
                }
                KeyEvent::Down | KeyEvent::Character('j' | 'J') => {
                    self.logs.rows_from_bottom = self.logs.rows_from_bottom.saturating_sub(1);
                }
                KeyEvent::PageUp => {
                    self.logs.rows_from_bottom = self
                        .logs
                        .rows_from_bottom
                        .saturating_add(self.logs.visible_rows.max(1));
                    self.logs.follow = false;
                }
                KeyEvent::PageDown => {
                    self.logs.rows_from_bottom = self
                        .logs
                        .rows_from_bottom
                        .saturating_sub(self.logs.visible_rows.max(1));
                }
                KeyEvent::Left => {
                    self.logs.horizontal_offset = self.logs.horizontal_offset.saturating_sub(4);
                }
                KeyEvent::Right => {
                    self.logs.horizontal_offset = self.logs.horizontal_offset.saturating_add(4);
                }
                KeyEvent::Home => self.logs.horizontal_offset = 0,
                KeyEvent::Character('p' | 'P') => self.logs.follow = !self.logs.follow,
                KeyEvent::End | KeyEvent::Character('f' | 'F') => {
                    self.logs.rows_from_bottom = 0;
                    self.logs.follow = true;
                }
                _ => {}
            },
            Screen::Dashboard | Screen::Help | Screen::System | Screen::Configuration => {
                let offset = if self.screen == Screen::Help {
                    &mut self.help_scroll
                } else if matches!(self.screen, Screen::System | Screen::Configuration) {
                    &mut self.system_scroll
                } else {
                    &mut self.overview_scroll
                };
                match key {
                    KeyEvent::Up | KeyEvent::Character('k' | 'K') => {
                        *offset = offset.saturating_sub(1);
                    }
                    KeyEvent::Down | KeyEvent::Character('j' | 'J') => {
                        *offset = offset.saturating_add(1);
                    }
                    KeyEvent::PageUp => *offset = offset.saturating_sub(10),
                    KeyEvent::PageDown => *offset = offset.saturating_add(10),
                    KeyEvent::Home => *offset = 0,
                    KeyEvent::End => *offset = u16::MAX,
                    _ => {}
                }
            }
        }
    }
}

/// Effect requested by a state transition.
#[derive(Clone, PartialEq, Eq)]
pub enum ConsoleAction {
    /// No server-side work is needed.
    None,
    /// Refresh the statistics snapshot immediately.
    Reload,
    /// Gracefully or immediately stop the server.
    Shutdown {
        /// Whether Ctrl-C bypassed the confirmation.
        forced: bool,
    },
    /// Execute an administrative operation off the async runtime.
    Submit(OperationRequest),
}

impl fmt::Debug for ConsoleAction {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::None => formatter.write_str("None"),
            Self::Reload => formatter.write_str("Reload"),
            Self::Shutdown { forced } => formatter
                .debug_struct("Shutdown")
                .field("forced", forced)
                .finish(),
            Self::Submit(request) => formatter.debug_tuple("Submit").field(request).finish(),
        }
    }
}

/// Fully validated administrative operation.
#[derive(Clone, PartialEq, Eq)]
pub enum OperationRequest {
    /// Create a new board.
    CreateBoard {
        /// Normalized URL segment.
        short: String,
        /// Display name.
        name: String,
        /// Optional description.
        description: String,
        /// Adult-content designation.
        nsfw: bool,
        /// Whether image uploads are allowed.
        allow_images: bool,
        /// Whether video uploads are allowed.
        allow_video: bool,
        /// Whether audio uploads are allowed.
        allow_audio: bool,
    },
    /// Create an administrator.
    CreateAdmin {
        /// Validated username.
        username: String,
        /// Plaintext password retained only for hashing.
        password: String,
    },
    /// Permanently delete a thread.
    DeleteThread {
        /// Positive thread identifier.
        thread_id: i64,
    },
}

impl OperationRequest {
    /// Return the present-tense progress label.
    const fn progress_label(&self) -> &'static str {
        match self {
            Self::CreateBoard { .. } => "Creating board…",
            Self::CreateAdmin { .. } => "Securing administrator credentials…",
            Self::DeleteThread { .. } => "Deleting thread and attached files…",
        }
    }

    /// Return whether completion changes statistics shown in the console.
    #[must_use]
    pub const fn refreshes_stats(&self) -> bool {
        matches!(
            self,
            Self::CreateBoard { .. } | Self::DeleteThread { .. } | Self::CreateAdmin { .. }
        )
    }
}

impl fmt::Debug for OperationRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CreateBoard {
                short,
                name,
                description,
                nsfw,
                allow_images,
                allow_video,
                allow_audio,
            } => formatter
                .debug_struct("CreateBoard")
                .field("short", short)
                .field("name", name)
                .field("description", description)
                .field("nsfw", nsfw)
                .field("allow_images", allow_images)
                .field("allow_video", allow_video)
                .field("allow_audio", allow_audio)
                .finish(),
            Self::CreateAdmin { username, .. } => formatter
                .debug_struct("CreateAdmin")
                .field("username", username)
                .field("password", &"<redacted>")
                .finish(),
            Self::DeleteThread { thread_id } => formatter
                .debug_struct("DeleteThread")
                .field("thread_id", thread_id)
                .finish(),
        }
    }
}

#[cfg(test)]
#[path = "state_tests.rs"]
mod tests;
