//! Typed state and keyboard transitions for the full-screen console.

use super::input::KeyEvent;
use std::fmt;
use std::time::{Duration, Instant};

/// Duration for transient success and information notices.
const NOTICE_TTL: Duration = Duration::from_secs(8);
/// Maximum character count accepted by a password field.
const MAX_PASSWORD_CHARS: usize = 256;

/// Primary console destination.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Screen {
    /// Live operational overview.
    #[default]
    Dashboard,
    /// Per-board content table.
    Boards,
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
            '2' => Some(Self::Boards),
            '3' => Some(Self::Logs),
            '4' => Some(Self::Help),
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
}

impl BoardListState {
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
        const PAGE: usize = 10;
        self.reconcile(row_count);
        let Some(selected) = self.selected else {
            return;
        };
        self.selected = if delta.is_negative() {
            Some(selected.saturating_sub(PAGE))
        } else {
            Some(
                selected
                    .saturating_add(PAGE)
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
}

impl Default for LogViewState {
    fn default() -> Self {
        Self {
            rows_from_bottom: 0,
            horizontal_offset: 0,
            follow: true,
        }
    }
}

/// Administration form purpose.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FormKind {
    /// Create a board and choose media policy defaults.
    CreateBoard,
    /// Create an administrator account.
    CreateAdmin,
    /// Select a thread for permanent deletion.
    DeleteThread,
}

impl FormKind {
    /// Return the concise form title.
    #[must_use]
    pub const fn title(self) -> &'static str {
        match self {
            Self::CreateBoard => "Create board",
            Self::CreateAdmin => "Create administrator",
            Self::DeleteThread => "Delete thread",
        }
    }

    /// Return the form's operator-facing description.
    #[must_use]
    pub const fn description(self) -> &'static str {
        match self {
            Self::CreateBoard => "Set the board identity and initial media policy.",
            Self::CreateAdmin => "Credentials are masked and never written to the console log.",
            Self::DeleteThread => "Enter a thread ID. A separate confirmation follows.",
        }
    }
}

/// Stable identifier for a form field.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FormFieldId {
    /// Board URL segment.
    BoardShort,
    /// Board display name.
    BoardName,
    /// Board description.
    BoardDescription,
    /// Adult-content designation.
    BoardNsfw,
    /// Image-upload policy.
    BoardImages,
    /// Video-upload policy.
    BoardVideo,
    /// Audio-upload policy.
    BoardAudio,
    /// Administrator username.
    AdminUsername,
    /// Administrator password.
    AdminPassword,
    /// Repeated administrator password.
    AdminPasswordConfirm,
    /// Thread database identifier.
    ThreadId,
}

/// Editable form value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FieldValue {
    /// Single-line text.
    Text(String),
    /// Boolean toggle.
    Toggle(bool),
}

/// One reusable form field.
#[derive(Clone, PartialEq, Eq)]
pub struct FormField {
    /// Stable field identifier.
    pub id: FormFieldId,
    /// Visible label.
    pub label: &'static str,
    /// Context shown below the focused field.
    pub help: &'static str,
    /// Mutable value.
    pub value: FieldValue,
    /// Whether text must be masked.
    pub secret: bool,
    /// Maximum accepted character count for text values.
    max_chars: usize,
}

impl fmt::Debug for FormField {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let value = match (&self.value, self.secret) {
            (FieldValue::Text(_), true) => "<redacted>".to_owned(),
            (FieldValue::Text(value), false) => value.clone(),
            (FieldValue::Toggle(value), _) => value.to_string(),
        };
        formatter
            .debug_struct("FormField")
            .field("id", &self.id)
            .field("label", &self.label)
            .field("value", &value)
            .field("secret", &self.secret)
            .finish_non_exhaustive()
    }
}

impl FormField {
    /// Construct a plain text field.
    const fn text(
        id: FormFieldId,
        label: &'static str,
        help: &'static str,
        max_chars: usize,
    ) -> Self {
        Self {
            id,
            label,
            help,
            value: FieldValue::Text(String::new()),
            secret: false,
            max_chars,
        }
    }

    /// Construct a masked text field.
    const fn secret(id: FormFieldId, label: &'static str, help: &'static str) -> Self {
        Self {
            id,
            label,
            help,
            value: FieldValue::Text(String::new()),
            secret: true,
            max_chars: MAX_PASSWORD_CHARS,
        }
    }

    /// Construct a boolean field.
    const fn toggle(
        id: FormFieldId,
        label: &'static str,
        help: &'static str,
        enabled: bool,
    ) -> Self {
        Self {
            id,
            label,
            help,
            value: FieldValue::Toggle(enabled),
            secret: false,
            max_chars: 0,
        }
    }

    /// Return the text value when this is a text field.
    #[must_use]
    pub fn text_value(&self) -> Option<&str> {
        match &self.value {
            FieldValue::Text(value) => Some(value),
            FieldValue::Toggle(_) => None,
        }
    }

    /// Return the toggle value when this is a toggle field.
    const fn toggle_value(&self) -> Option<bool> {
        match &self.value {
            FieldValue::Toggle(value) => Some(*value),
            FieldValue::Text(_) => None,
        }
    }
}

/// Interactive modal form.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FormState {
    /// Administrative operation being collected.
    pub kind: FormKind,
    /// Ordered form fields.
    pub fields: Vec<FormField>,
    /// Focused field index.
    pub focused: usize,
    /// Cursor position in Unicode scalar values for the focused text field.
    pub cursor: usize,
    /// Inline validation error.
    pub error: Option<String>,
}

impl FormState {
    /// Construct a form with safe operator defaults.
    #[must_use]
    pub fn new(kind: FormKind) -> Self {
        let fields = match kind {
            FormKind::CreateBoard => vec![
                FormField::text(
                    FormFieldId::BoardShort,
                    "Short name",
                    "1-8 ASCII letters or numbers; used in /board/ URLs.",
                    8,
                ),
                FormField::text(
                    FormFieldId::BoardName,
                    "Display name",
                    "Human-readable board name.",
                    80,
                ),
                FormField::text(
                    FormFieldId::BoardDescription,
                    "Description",
                    "Optional concise purpose shown to visitors.",
                    240,
                ),
                FormField::toggle(
                    FormFieldId::BoardNsfw,
                    "NSFW board",
                    "Marks the board as adult content.",
                    false,
                ),
                FormField::toggle(
                    FormFieldId::BoardImages,
                    "Image uploads",
                    "Allow image attachments.",
                    true,
                ),
                FormField::toggle(
                    FormFieldId::BoardVideo,
                    "Video uploads",
                    "Allow video attachments.",
                    true,
                ),
                FormField::toggle(
                    FormFieldId::BoardAudio,
                    "Audio uploads",
                    "Allow audio attachments.",
                    false,
                ),
            ],
            FormKind::CreateAdmin => vec![
                FormField::text(
                    FormFieldId::AdminUsername,
                    "Username",
                    "3-32 ASCII letters, numbers, underscores, or dashes.",
                    32,
                ),
                FormField::secret(
                    FormFieldId::AdminPassword,
                    "Password",
                    "At least 8 characters; input is masked.",
                ),
                FormField::secret(
                    FormFieldId::AdminPasswordConfirm,
                    "Confirm password",
                    "Repeat the password exactly.",
                ),
            ],
            FormKind::DeleteThread => vec![FormField::text(
                FormFieldId::ThreadId,
                "Thread ID",
                "Positive numeric database ID; deletion cannot be undone.",
                20,
            )],
        };
        Self {
            kind,
            fields,
            focused: 0,
            cursor: 0,
            error: None,
        }
    }

    /// Return the focused field.
    #[must_use]
    pub fn focused_field(&self) -> Option<&FormField> {
        self.fields.get(self.focused)
    }

    /// Return a field by stable identifier.
    fn field(&self, id: FormFieldId) -> Option<&FormField> {
        self.fields.iter().find(|field| field.id == id)
    }

    /// Return a required text field or an internal form error.
    fn text(&self, id: FormFieldId) -> Result<&str, String> {
        self.field(id)
            .and_then(FormField::text_value)
            .ok_or_else(|| "The form could not read a required field.".to_owned())
    }

    /// Return a required toggle field or an internal form error.
    fn toggle(&self, id: FormFieldId) -> Result<bool, String> {
        self.field(id)
            .and_then(FormField::toggle_value)
            .ok_or_else(|| "The form could not read a required setting.".to_owned())
    }

    /// Move focus by one field, wrapping at either end.
    fn move_focus(&mut self, backwards: bool) {
        let count = self.fields.len();
        if count == 0 {
            return;
        }
        self.focused = if backwards {
            self.focused
                .checked_sub(1)
                .unwrap_or_else(|| count.saturating_sub(1))
        } else {
            self.focused.saturating_add(1) % count
        };
        self.cursor = self
            .focused_field()
            .and_then(FormField::text_value)
            .map_or(0, |value| value.chars().count());
        self.error = None;
    }

    /// Insert one character into the focused text field.
    fn insert_char(&mut self, character: char) {
        if character.is_control() {
            return;
        }
        let cursor = self.cursor;
        let Some(field) = self.fields.get_mut(self.focused) else {
            return;
        };
        let FieldValue::Text(value) = &mut field.value else {
            return;
        };
        if value.chars().count() >= field.max_chars {
            self.error = Some(format!(
                "{} accepts at most {} characters.",
                field.label, field.max_chars
            ));
            return;
        }
        let byte_index = value
            .char_indices()
            .nth(cursor)
            .map_or(value.len(), |(index, _)| index);
        value.insert(byte_index, character);
        self.cursor = cursor.saturating_add(1);
        self.error = None;
    }

    /// Insert sanitized pasted content into the focused text field.
    fn insert_paste(&mut self, content: &str) {
        for character in content.chars().filter(|character| !character.is_control()) {
            self.insert_char(character);
        }
    }

    /// Remove the character immediately before the cursor.
    fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let target = self.cursor.saturating_sub(1);
        let Some(field) = self.fields.get_mut(self.focused) else {
            return;
        };
        let FieldValue::Text(value) = &mut field.value else {
            return;
        };
        let Some((byte_index, _)) = value.char_indices().nth(target) else {
            return;
        };
        value.remove(byte_index);
        self.cursor = target;
        self.error = None;
    }

    /// Remove the character at the cursor.
    fn delete(&mut self) {
        let Some(field) = self.fields.get_mut(self.focused) else {
            return;
        };
        let FieldValue::Text(value) = &mut field.value else {
            return;
        };
        let Some((byte_index, _)) = value.char_indices().nth(self.cursor) else {
            return;
        };
        value.remove(byte_index);
        self.error = None;
    }

    /// Move the cursor inside the focused text field.
    fn move_cursor(&mut self, right: bool) {
        let length = self
            .focused_field()
            .and_then(FormField::text_value)
            .map_or(0, |value| value.chars().count());
        self.cursor = if right {
            self.cursor.saturating_add(1).min(length)
        } else {
            self.cursor.saturating_sub(1)
        };
    }

    /// Move the cursor to the start or end of the focused text field.
    fn move_cursor_to_edge(&mut self, end: bool) {
        self.cursor = if end {
            self.focused_field()
                .and_then(FormField::text_value)
                .map_or(0, |value| value.chars().count())
        } else {
            0
        };
    }

    /// Clear the focused text field.
    fn clear_text(&mut self) {
        let Some(field) = self.fields.get_mut(self.focused) else {
            return;
        };
        if let FieldValue::Text(value) = &mut field.value {
            value.clear();
            self.cursor = 0;
            self.error = None;
        }
    }

    /// Flip the focused boolean field.
    fn toggle_focused(&mut self) {
        let Some(field) = self.fields.get_mut(self.focused) else {
            return;
        };
        if let FieldValue::Toggle(value) = &mut field.value {
            *value = !*value;
            self.error = None;
        }
    }

    /// Validate and convert this form into an operation request.
    fn request(&self) -> Result<OperationRequest, String> {
        match self.kind {
            FormKind::CreateBoard => {
                let short = self
                    .text(FormFieldId::BoardShort)?
                    .trim()
                    .to_ascii_lowercase();
                if short.is_empty()
                    || short.len() > 8
                    || !short
                        .chars()
                        .all(|character| character.is_ascii_alphanumeric())
                {
                    return Err("Short name must be 1-8 ASCII letters or numbers.".to_owned());
                }
                let name = self.text(FormFieldId::BoardName)?.trim().to_owned();
                if name.is_empty() {
                    return Err("Display name is required.".to_owned());
                }
                Ok(OperationRequest::CreateBoard {
                    short,
                    name,
                    description: self.text(FormFieldId::BoardDescription)?.trim().to_owned(),
                    nsfw: self.toggle(FormFieldId::BoardNsfw)?,
                    allow_images: self.toggle(FormFieldId::BoardImages)?,
                    allow_video: self.toggle(FormFieldId::BoardVideo)?,
                    allow_audio: self.toggle(FormFieldId::BoardAudio)?,
                })
            }
            FormKind::CreateAdmin => {
                let username = self.text(FormFieldId::AdminUsername)?.trim().to_owned();
                if !(3..=32).contains(&username.len())
                    || !username.chars().all(|character| {
                        character.is_ascii_alphanumeric() || matches!(character, '_' | '-')
                    })
                {
                    return Err(
                        "Username must be 3-32 ASCII letters, numbers, underscores, or dashes."
                            .to_owned(),
                    );
                }
                let password = self.text(FormFieldId::AdminPassword)?.to_owned();
                crate::utils::crypto::validate_password(&password)
                    .map_err(|error| error.to_string())?;
                if password != self.text(FormFieldId::AdminPasswordConfirm)? {
                    return Err("Passwords do not match.".to_owned());
                }
                Ok(OperationRequest::CreateAdmin { username, password })
            }
            FormKind::DeleteThread => {
                let raw = self.text(FormFieldId::ThreadId)?.trim();
                let thread_id = raw
                    .parse::<i64>()
                    .map_err(|_| "Thread ID must be a positive whole number.".to_owned())?;
                if thread_id <= 0 {
                    return Err("Thread ID must be a positive whole number.".to_owned());
                }
                Ok(OperationRequest::DeleteThread { thread_id })
            }
        }
    }
}

/// Modal content layered over the active screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Dialog {
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
    /// Log scrolling and follow mode.
    pub logs: LogViewState,
}

impl ConsoleState {
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
    pub fn handle_key(&mut self, key: &KeyEvent, board_count: usize) -> ConsoleAction {
        if matches!(key, KeyEvent::ForceQuit) {
            return ConsoleAction::Shutdown { forced: true };
        }

        if let Some(dialog) = self.dialog.take() {
            return self.handle_dialog(dialog, key);
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
            KeyEvent::Character('?' | 'h' | 'H') => self.screen = Screen::Help,
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
            _ => self.handle_screen_key(key, board_count),
        }
        ConsoleAction::None
    }

    /// Handle an input event while a modal is active.
    fn handle_dialog(&mut self, mut dialog: Dialog, key: &KeyEvent) -> ConsoleAction {
        match &mut dialog {
            Dialog::ConfirmQuit => match key {
                KeyEvent::Enter | KeyEvent::Character('y' | 'Y') => {
                    return ConsoleAction::Shutdown { forced: false };
                }
                KeyEvent::Escape | KeyEvent::Character('n' | 'N' | 'q' | 'Q') => {}
                _ => self.dialog = Some(dialog),
            },
            Dialog::ConfirmDelete { thread_id } => match key {
                KeyEvent::Enter | KeyEvent::Character('y' | 'Y') => {
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

    /// Handle navigation local to the active primary screen.
    fn handle_screen_key(&mut self, key: &KeyEvent, board_count: usize) {
        match self.screen {
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
                    self.boards.reconcile(board_count);
                    if board_count > 0 {
                        self.boards.selected = Some(0);
                    }
                }
                KeyEvent::End => {
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
                    self.logs.rows_from_bottom = self.logs.rows_from_bottom.saturating_add(10);
                    self.logs.follow = false;
                }
                KeyEvent::PageDown => {
                    self.logs.rows_from_bottom = self.logs.rows_from_bottom.saturating_sub(10);
                }
                KeyEvent::Left => {
                    self.logs.horizontal_offset = self.logs.horizontal_offset.saturating_sub(4);
                }
                KeyEvent::Right => {
                    self.logs.horizontal_offset = self.logs.horizontal_offset.saturating_add(4);
                }
                KeyEvent::Home => self.logs.horizontal_offset = 0,
                KeyEvent::End | KeyEvent::Character('f' | 'F') => {
                    self.logs.rows_from_bottom = 0;
                    self.logs.follow = true;
                }
                _ => {}
            },
            Screen::Dashboard | Screen::Help => {}
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
        matches!(self, Self::CreateBoard { .. } | Self::DeleteThread { .. })
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

/// Intermediate outcome of a key press inside a form.
enum FormAction {
    /// Preserve the edited form.
    KeepOpen,
    /// Proceed with a validated operation.
    Submit(OperationRequest),
}

/// Apply one editing or focus event to a form.
fn handle_form_key(form: &mut FormState, key: &KeyEvent) -> Option<FormAction> {
    match key {
        KeyEvent::Tab | KeyEvent::Down => form.move_focus(false),
        KeyEvent::BackTab | KeyEvent::Up => form.move_focus(true),
        KeyEvent::Left => form.move_cursor(false),
        KeyEvent::Right => form.move_cursor(true),
        KeyEvent::Home => form.move_cursor_to_edge(false),
        KeyEvent::End => form.move_cursor_to_edge(true),
        KeyEvent::Backspace => form.backspace(),
        KeyEvent::Delete => form.delete(),
        KeyEvent::ClearLine => form.clear_text(),
        KeyEvent::Character(' ')
            if form
                .focused_field()
                .is_some_and(|field| matches!(field.value, FieldValue::Toggle(_))) =>
        {
            form.toggle_focused();
        }
        KeyEvent::Character(character) => form.insert_char(*character),
        KeyEvent::Paste(content) => form.insert_paste(content),
        KeyEvent::Enter => {
            let final_field = form.focused.saturating_add(1) >= form.fields.len();
            if final_field {
                match form.request() {
                    Ok(request) => return Some(FormAction::Submit(request)),
                    Err(error) => form.error = Some(error),
                }
            } else {
                form.move_focus(false);
            }
        }
        KeyEvent::Submit => match form.request() {
            Ok(request) => return Some(FormAction::Submit(request)),
            Err(error) => form.error = Some(error),
        },
        KeyEvent::Escape
        | KeyEvent::PageUp
        | KeyEvent::PageDown
        | KeyEvent::ForceQuit
        | KeyEvent::Resize => return None,
    }
    Some(FormAction::KeepOpen)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Type text into the currently focused form field.
    fn type_text(form: &mut FormState, value: &str) {
        for character in value.chars() {
            form.insert_char(character);
        }
    }

    #[test]
    fn escape_returns_to_dashboard_without_opening_quit_confirmation() {
        let mut state = ConsoleState {
            screen: Screen::Logs,
            ..ConsoleState::default()
        };

        let action = state.handle_key(&KeyEvent::Escape, 0);

        assert_eq!(
            action,
            ConsoleAction::None,
            "escape should not stop the server"
        );
        assert_eq!(
            state.screen,
            Screen::Dashboard,
            "escape should navigate back"
        );
        assert!(state.dialog.is_none(), "escape should not open a dialog");
    }

    #[test]
    fn board_selection_stays_valid_when_rows_change() {
        let mut selection = BoardListState { selected: Some(8) };

        selection.reconcile(3);
        assert_eq!(
            selection.selected,
            Some(2),
            "selection should clamp to the final row"
        );

        selection.reconcile(0);
        assert_eq!(selection.selected, None, "an empty table has no selection");
    }

    #[test]
    fn log_scrolling_disables_follow_and_end_restores_it() {
        let mut state = ConsoleState {
            screen: Screen::Logs,
            ..ConsoleState::default()
        };

        state.handle_key(&KeyEvent::PageUp, 0);
        assert!(
            !state.logs.follow,
            "manual scrolling should pause follow mode"
        );
        assert_eq!(
            state.logs.rows_from_bottom, 10,
            "page-up should move ten rows"
        );

        state.handle_key(&KeyEvent::End, 0);
        assert!(state.logs.follow, "end should resume follow mode");
        assert_eq!(
            state.logs.rows_from_bottom, 0,
            "end should jump to the newest line"
        );
    }

    #[test]
    fn passwords_are_redacted_from_debug_output() {
        let request = OperationRequest::CreateAdmin {
            username: "operator".to_owned(),
            password: "correct-horse-battery-staple".to_owned(),
        };

        let debug = format!("{request:?}");

        assert!(
            debug.contains("<redacted>"),
            "debug output should mark redaction"
        );
        assert!(
            !debug.contains("correct-horse-battery-staple"),
            "debug output must not contain plaintext passwords"
        );
    }

    #[test]
    fn delete_thread_uses_a_separate_destructive_confirmation() {
        let mut state = ConsoleState::default();
        state.handle_key(&KeyEvent::Character('d'), 0);
        let form_dialog = state.dialog.take();
        assert!(
            matches!(&form_dialog, Some(Dialog::Form(_))),
            "delete shortcut should open a form"
        );
        let Some(Dialog::Form(mut form)) = form_dialog else {
            return;
        };
        type_text(&mut form, "42");
        state.dialog = Some(Dialog::Form(form));

        let action = state.handle_key(&KeyEvent::Submit, 0);

        assert_eq!(
            action,
            ConsoleAction::None,
            "confirmation should precede deletion"
        );
        assert_eq!(
            state.dialog,
            Some(Dialog::ConfirmDelete { thread_id: 42 }),
            "validated deletion should open the destructive confirmation"
        );
    }

    #[test]
    fn create_admin_rejects_mismatched_passwords() {
        let mut form = FormState::new(FormKind::CreateAdmin);
        type_text(&mut form, "operator");
        form.move_focus(false);
        type_text(&mut form, "password-one");
        form.move_focus(false);
        type_text(&mut form, "password-two");

        let result = form.request();

        assert_eq!(result, Err("Passwords do not match.".to_owned()));
    }
}
