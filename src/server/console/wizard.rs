//! Administrative operation execution and first-run line-mode setup.

use super::state::OperationRequest;
use crate::db::DbPool;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use std::io::BufRead;

/// Execute a fully validated console operation.
///
/// Database work and password hashing are intentionally synchronous; callers
/// run this function on Tokio's blocking pool.
///
/// # Errors
///
/// Returns an operator-facing message when validation, password hashing,
/// database mutation, or required filesystem bookkeeping fails.
pub fn execute(request: &OperationRequest, pool: &DbPool) -> Result<String, String> {
    execute_inner(request, pool).map_err(|error| {
        tracing::error!(target: "console", operation = ?request, error = %error, "Console operation failed");
        error.to_string()
    })
}

/// Execute an operation while retaining its structured error chain.
fn execute_inner(request: &OperationRequest, pool: &DbPool) -> anyhow::Result<String> {
    match request {
        OperationRequest::CreateBoard {
            short,
            name,
            description,
            nsfw,
            allow_images,
            allow_video,
            allow_audio,
        } => {
            validate_board(short, name, description)?;
            let connection = pool.get()?;
            let id = crate::db::create_board_with_media_flags(
                &connection,
                short,
                name,
                description,
                *nsfw,
                *allow_images,
                *allow_video,
                *allow_audio,
            )?;
            tracing::info!(
                target: "console",
                board = %short,
                name = %name,
                id,
                "Board created via console"
            );
            Ok(format!("Board /{short}/ — {name} created (ID {id})."))
        }
        OperationRequest::CreateAdmin { username, password } => {
            validate_username(username)?;
            crate::utils::crypto::validate_password(password)?;
            let hash = crate::utils::crypto::hash_password(password)?;
            let connection = pool.get()?;
            let id = crate::db::create_admin(&connection, username, &hash)?;
            tracing::info!(
                target: "console",
                username = %username,
                id,
                "Administrator created via console"
            );
            Ok(format!("Administrator '{username}' created (ID {id})."))
        }
        OperationRequest::DeleteThread { thread_id } => {
            if *thread_id <= 0 {
                anyhow::bail!("Thread ID must be a positive whole number.");
            }
            let connection = pool.get()?;
            let deleted = crate::db::delete_thread(&connection, *thread_id)?;
            let file_count = deleted.paths.len();
            let cleanup_result = crate::pending_fs::finalize_delete_files_payload(
                &connection,
                &crate::config::CONFIG.upload_dir,
                deleted.pending_fs_op_id.as_deref(),
                &deleted.paths,
            );
            if let Err(error) = &cleanup_result {
                tracing::warn!(
                    target: "console",
                    thread_id,
                    error = %error,
                    "Thread deleted but file cleanup remains pending"
                );
            }
            tracing::info!(
                target: "console",
                thread_id,
                files_removed = file_count,
                "Thread deleted via console"
            );
            if cleanup_result.is_ok() {
                Ok(format!(
                    "Thread {thread_id} deleted; {file_count} attached file(s) removed."
                ))
            } else {
                Ok(format!(
                    "Thread {thread_id} deleted; attached-file cleanup remains queued."
                ))
            }
        }
    }
}

/// Revalidate a board request at the mutation boundary.
fn validate_board(short: &str, name: &str, description: &str) -> anyhow::Result<()> {
    if short.is_empty()
        || short.len() > 8
        || !short
            .chars()
            .all(|character| character.is_ascii_alphanumeric())
    {
        anyhow::bail!("Short name must be 1-8 ASCII letters or numbers.");
    }
    if name.trim().is_empty() || name.chars().count() > 80 {
        anyhow::bail!("Display name must be 1-80 characters.");
    }
    if description.chars().count() > 240 {
        anyhow::bail!("Description must be 240 characters or fewer.");
    }
    Ok(())
}

/// Revalidate an administrator username at the mutation boundary.
fn validate_username(username: &str) -> anyhow::Result<()> {
    if !(3..=32).contains(&username.len())
        || !username
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '-'))
    {
        anyhow::bail!("Username must be 3-32 ASCII letters, numbers, underscores, or dashes.");
    }
    Ok(())
}

/// Return an ANSI sequence only when console color is available.
fn color(code: &'static str) -> &'static str {
    if crate::logging::ansi_enabled() {
        code
    } else {
        ""
    }
}

/// Print a consistently styled first-run prompt and read a trimmed response.
fn prompt(reader: &mut dyn BufRead, label: &str) -> Option<String> {
    if crate::logging::is_tty() {
        return prompt_terminal(label, false);
    }
    crate::logging::console_prompt(&format!(
        "  {}{label}{} ",
        color("\x1b[36m"),
        color("\x1b[0m")
    ));
    let mut value = String::new();
    match reader.read_line(&mut value) {
        Ok(0) | Err(_) => None,
        Ok(_) => Some(value.trim().to_owned()),
    }
}

/// Prompt until a valid first administrator username is supplied.
fn prompt_username(reader: &mut dyn BufRead) -> Option<String> {
    loop {
        let username = prompt(reader, "Username:")?;
        match validate_username(&username) {
            Ok(()) => return Some(username),
            Err(error) => print_line(&format!(
                "  {}[ERROR]{} {error}",
                color("\x1b[31m"),
                color("\x1b[0m")
            )),
        }
    }
}

/// Prompt until a valid and matching first administrator password is supplied.
fn prompt_password(reader: &mut dyn BufRead) -> Option<String> {
    loop {
        let password = if crate::logging::is_tty() {
            prompt_terminal("Password (8+ characters):", true)?
        } else {
            prompt(reader, "Password (8+ characters):")?
        };
        if let Err(error) = crate::utils::crypto::validate_password(&password) {
            print_line(&format!(
                "  {}[ERROR]{} {error}",
                color("\x1b[31m"),
                color("\x1b[0m")
            ));
            continue;
        }
        let confirmation = if crate::logging::is_tty() {
            prompt_terminal("Confirm password:", true)?
        } else {
            prompt(reader, "Confirm password:")?
        };
        if password == confirmation {
            return Some(password);
        }
        print_line(&format!(
            "  {}[ERROR]{} Passwords do not match.",
            color("\x1b[31m"),
            color("\x1b[0m")
        ));
    }
}

/// Restore raw mode even when setup returns early or unwinds.
struct SetupInputGuard;

impl Drop for SetupInputGuard {
    fn drop(&mut self) {
        super::cleanup();
    }
}

/// Print line-mode output with carriage returns while raw input is active.
fn print_raw(message: &str) {
    if super::line_input_active() {
        crate::logging::console_print_raw(&message.replace('\n', "\r\n"));
    } else {
        crate::logging::console_print_raw(message);
    }
}

/// Print one first-run setup line.
fn print_line(message: &str) {
    print_raw(&format!("{message}\n"));
}

/// Read all interactive setup fields through one event reader so buffered
/// keystrokes never disappear between cooked stdin and masked raw input.
fn prompt_terminal(label: &str, secret: bool) -> Option<String> {
    let mut value = String::new();
    redraw_prompt(label, &value, secret);
    while super::line_input_active() {
        match event::poll(std::time::Duration::from_millis(50)) {
            Ok(false) => continue,
            Err(_) => return None,
            Ok(true) => {}
        }
        let terminal_event = event::read().ok()?;
        match terminal_event {
            Event::Key(key) if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) => {
                match key.code {
                    KeyCode::Enter if key.kind == KeyEventKind::Press => {
                        print_line("");
                        return Some(if secret {
                            value
                        } else {
                            value.trim().to_owned()
                        });
                    }
                    KeyCode::Esc => {
                        print_line("");
                        return None;
                    }
                    KeyCode::Char('c' | 'C' | 'd' | 'D')
                        if key.modifiers.contains(KeyModifiers::CONTROL) =>
                    {
                        print_line("");
                        return None;
                    }
                    KeyCode::Char('u' | 'U') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        value.clear();
                    }
                    KeyCode::Backspace => {
                        value.pop();
                    }
                    KeyCode::Char(character)
                        if !key
                            .modifiers
                            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                            && !character.is_control()
                            && value.chars().count() < 256 =>
                    {
                        value.push(character);
                    }
                    _ => {}
                }
            }
            Event::Paste(content) => {
                let remaining = 256usize.saturating_sub(value.chars().count());
                value.extend(
                    content
                        .chars()
                        .filter(|character| !character.is_control())
                        .take(remaining),
                );
            }
            _ => {}
        }
        redraw_prompt(label, &value, secret);
    }
    None
}

/// Keep first-run input on one physical row, including narrow terminals.
fn redraw_prompt(label: &str, value: &str, secret: bool) {
    use ratatui::style::Style;
    use ratatui::text::{Line, Span};

    let width = usize::from(
        crossterm::terminal::size()
            .map_or(80, |size| size.0)
            .saturating_sub(1),
    );
    let label = format!("  {label} ");
    let label: String = label.chars().take(width / 2).collect();
    let available = width.saturating_sub(Line::from(label.as_str()).width());
    let displayed = if secret {
        "•".repeat(value.chars().count())
    } else {
        value.to_owned()
    };
    let line = Line::from(displayed.as_str());
    let mut skip = line.width().saturating_sub(available.saturating_sub(1));
    let mut visible = String::new();
    for grapheme in line.styled_graphemes(Style::default()) {
        if skip > 0 {
            skip = skip.saturating_sub(Span::raw(grapheme.symbol).width());
        } else {
            visible.push_str(grapheme.symbol);
        }
    }
    crate::logging::console_prompt(&format!(
        "\r\x1b[2K{}{label}{}{visible}",
        color("\x1b[36m"),
        color("\x1b[0m")
    ));
}

/// First-run account bootstrap shown before the full-screen console starts.
pub fn prompt_create_first_admin(
    pool: &DbPool,
    reader: &mut dyn BufRead,
    cancel: &tokio_util::sync::CancellationToken,
) {
    let _input_guard = if crate::logging::is_tty() {
        if let Err(error) = super::start_line_input() {
            print_line(&format!("Setup input unavailable: {error}"));
            return;
        }
        Some(SetupInputGuard)
    } else {
        None
    };
    if cancel.is_cancelled() {
        return;
    }
    print_raw(&format!(
        "\n  {}┌─ First-run setup ─────────────────────────────────────┐\n\
           │  Create the administrator used at /admin.             │\n\
           │  Ctrl-C or end-of-input skips setup for now.           │\n\
           └────────────────────────────────────────────────────────┘{}\n\n",
        color("\x1b[36m"),
        color("\x1b[0m")
    ));
    if crate::logging::is_tty() {
        print_line(&format!(
            "  {}[SECURE] Password input is masked.{}",
            color("\x1b[32m"),
            color("\x1b[0m")
        ));
    }

    let Some(username) = prompt_username(reader) else {
        print_line("\n  Setup skipped. Use: rustchan-cli admin create-admin <user> <pass>");
        return;
    };
    let Some(password) = prompt_password(reader) else {
        print_line("\n  Setup skipped.");
        return;
    };
    let request = OperationRequest::CreateAdmin { username, password };
    match execute(&request, pool) {
        Ok(message) => print_line(&format!(
            "\n  {}[OK]{} {message}",
            color("\x1b[32m"),
            color("\x1b[0m")
        )),
        Err(error) => {
            print_line(&format!(
                "\n  {}[ERROR]{} {error}",
                color("\x1b[31m"),
                color("\x1b[0m")
            ));
            return;
        }
    }

    let create_board = prompt(reader, "Create the first board now? [y/N]:")
        .is_some_and(|answer| matches!(answer.to_ascii_lowercase().as_str(), "y" | "yes"));
    if create_board {
        prompt_create_first_board(pool, reader);
    }
    print_line("");
}

/// Collect and create an optional first board during line-mode setup.
fn prompt_create_first_board(pool: &DbPool, reader: &mut dyn BufRead) {
    print_line("");
    let Some(short) = prompt(reader, "Short name (for example, tech):") else {
        print_line("  Board setup skipped.");
        return;
    };
    let Some(name) = prompt(reader, "Display name:") else {
        print_line("  Board setup skipped.");
        return;
    };
    // Cancellation at any remaining field must not create a board using
    // defaults for questions the operator never answered.
    let request = (|| {
        Some(OperationRequest::CreateBoard {
            short: short.to_ascii_lowercase(),
            name,
            description: prompt(reader, "Description (optional):")?,
            nsfw: yes(&prompt(reader, "NSFW board? [y/N]:")?),
            allow_images: !yes(&prompt(reader, "Disable image uploads? [y/N]:")?),
            allow_video: !yes(&prompt(reader, "Disable video uploads? [y/N]:")?),
            allow_audio: yes(&prompt(reader, "Enable audio uploads? [y/N]:")?),
        })
    })();
    let Some(request) = request else {
        print_line("  Board setup skipped.");
        return;
    };
    match execute(&request, pool) {
        Ok(message) => print_line(&format!(
            "  {}[OK]{} {message}",
            color("\x1b[32m"),
            color("\x1b[0m")
        )),
        Err(error) => print_line(&format!(
            "  {}[ERROR]{} {error}",
            color("\x1b[31m"),
            color("\x1b[0m")
        )),
    }
}

/// Interpret an optional line-mode yes/no answer.
fn yes(answer: &str) -> bool {
    matches!(answer.to_ascii_lowercase().as_str(), "y" | "yes")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn username_validation_matches_web_setup_policy() {
        assert!(validate_username("admin_user-1").is_ok());
        assert!(validate_username("ab").is_err());
        assert!(validate_username("not allowed").is_err());
        assert!(validate_username("éclair").is_err());
    }

    #[test]
    fn board_validation_caps_operator_supplied_content() {
        assert!(validate_board("tech", "Technology", "Discussion").is_ok());
        assert!(validate_board("too-long-name", "Technology", "").is_err());
        assert!(validate_board("tech", "", "").is_err());
        assert!(validate_board("tech", "Technology", &"x".repeat(241)).is_err());
    }

    #[test]
    fn operation_debug_redacts_first_run_password() {
        let request = OperationRequest::CreateAdmin {
            username: "operator".to_owned(),
            password: "not-for-logs".to_owned(),
        };

        assert!(
            !format!("{request:?}").contains("not-for-logs"),
            "operation debug output must redact passwords"
        );
    }
    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "assertions verify cancellation before filesystem/database mutation"
    )]
    fn first_board_cancellation_does_not_create_from_partial_answers() -> anyhow::Result<()> {
        let pool = crate::db::init_test_pool()?;
        for answers in ["tech\nTechnology\n", "tech\nTechnology\nDiscussion\nn\nn\n"] {
            let mut reader = std::io::Cursor::new(answers);
            prompt_create_first_board(&pool, &mut reader);
            let count: i64 = pool
                .get()?
                .query_row("SELECT COUNT(*) FROM boards", [], |row| row.get(0))?;
            assert_eq!(count, 0, "cancelled prompts must not create a board");
        }
        Ok(())
    }
}
