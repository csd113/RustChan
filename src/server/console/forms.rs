//! Validated administration forms and bounded Unicode text editing.
use super::input::KeyEvent;
use super::state::OperationRequest;
use std::fmt;

/// Maximum accepted password character count.
const MAX_PASSWORD_CHARS: usize = 256;

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
#[expect(
    clippy::partial_pub_fields,
    reason = "preserve the existing editable field API while keeping the validated text limit private so callers cannot relax it"
)]
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
    pub(super) const fn text(
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
    pub(super) fn text(&self, id: FormFieldId) -> Result<&str, String> {
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
    pub(super) fn move_focus(&mut self, backwards: bool) {
        let count = self.fields.len();
        if count == 0 {
            return;
        }
        self.focused = if backwards {
            self.focused
                .checked_sub(1)
                .unwrap_or_else(|| count.saturating_sub(1))
        } else {
            self.focused
                .saturating_add(1)
                .checked_rem(count)
                .unwrap_or(0)
        };
        self.cursor = self
            .focused_field()
            .and_then(FormField::text_value)
            .map_or(0, |value| value.chars().count());
        self.error = None;
    }

    /// Insert one character into the focused text field.
    pub(super) fn insert_char(&mut self, character: char) {
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
    pub(super) fn insert_paste(&mut self, content: &str) {
        let Some(field) = self.focused_field() else {
            return;
        };
        let Some(value) = field.text_value() else {
            return;
        };
        let remaining = field.max_chars.saturating_sub(value.chars().count());
        // One excess character supplies the existing length error without
        // repeatedly allocating it for the rest of a large clipboard.
        for character in content
            .chars()
            .filter(|character| !character.is_control())
            .take(remaining.saturating_add(1))
        {
            self.insert_char(character);
        }
    }

    /// Remove the character immediately before the cursor.
    pub(super) fn backspace(&mut self) {
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
        let _removed_character = value.remove(byte_index);
        self.cursor = target;
        self.error = None;
    }

    /// Remove the character at the cursor.
    pub(super) fn delete(&mut self) {
        let Some(field) = self.fields.get_mut(self.focused) else {
            return;
        };
        let FieldValue::Text(value) = &mut field.value else {
            return;
        };
        let Some((byte_index, _)) = value.char_indices().nth(self.cursor) else {
            return;
        };
        let _removed_character = value.remove(byte_index);
        self.error = None;
    }

    /// Move the cursor inside the focused text field.
    pub(super) fn move_cursor(&mut self, right: bool) {
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
    pub(super) fn move_cursor_to_edge(&mut self, end: bool) {
        self.cursor = if end {
            self.focused_field()
                .and_then(FormField::text_value)
                .map_or(0, |value| value.chars().count())
        } else {
            0
        };
    }

    /// Clear the focused text field.
    pub(super) fn clear_text(&mut self) {
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
    pub(super) fn request(&self) -> Result<OperationRequest, String> {
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
                let thread_id = raw.parse::<i64>().map_err(|error| {
                    tracing::debug!(%error, "Console thread ID was not a valid integer");
                    "Thread ID must be a positive whole number.".to_owned()
                })?;
                if thread_id <= 0 {
                    return Err("Thread ID must be a positive whole number.".to_owned());
                }
                Ok(OperationRequest::DeleteThread { thread_id })
            }
        }
    }
}

/// Intermediate outcome of a key press inside a form.
pub(super) enum FormAction {
    /// Preserve the edited form.
    KeepOpen,
    /// Proceed with a validated operation.
    Submit(OperationRequest),
}

/// Apply one editing or focus event to a form.
pub(super) fn handle_form_key(form: &mut FormState, key: &KeyEvent) -> Option<FormAction> {
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
        KeyEvent::Character(character) | KeyEvent::RepeatCharacter(character) => {
            form.insert_char(*character);
        }
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
