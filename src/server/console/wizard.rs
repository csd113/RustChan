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
            Err(error) => crate::logging::console_println(&format!(
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
            prompt_secret("Password (8+ characters):")?
        } else {
            prompt(reader, "Password (8+ characters):")?
        };
        if let Err(error) = crate::utils::crypto::validate_password(&password) {
            crate::logging::console_println(&format!(
                "  {}[ERROR]{} {error}",
                color("\x1b[31m"),
                color("\x1b[0m")
            ));
            continue;
        }
        let confirmation = if crate::logging::is_tty() {
            prompt_secret("Confirm password:")?
        } else {
            prompt(reader, "Confirm password:")?
        };
        if password == confirmation {
            return Some(password);
        }
        crate::logging::console_println(&format!(
            "  {}[ERROR]{} Passwords do not match.",
            color("\x1b[31m"),
            color("\x1b[0m")
        ));
    }
}

/// Raw-mode guard used only during pre-console secret input.
struct SecretInputGuard;

impl Drop for SecretInputGuard {
    fn drop(&mut self) {
        drop(crossterm::terminal::disable_raw_mode());
    }
}

/// Read and mask one first-run secret without adding a password-input crate.
fn prompt_secret(label: &str) -> Option<String> {
    if crossterm::terminal::enable_raw_mode().is_err() {
        return None;
    }
    let guard = SecretInputGuard;
    crate::logging::console_prompt(&format!(
        "  {}{label}{} ",
        color("\x1b[36m"),
        color("\x1b[0m")
    ));
    let mut secret = String::new();
    loop {
        let Ok(terminal_event) = event::read() else {
            return None;
        };
        let Event::Key(key) = terminal_event else {
            continue;
        };
        if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            continue;
        }
        match key.code {
            KeyCode::Enter => break,
            KeyCode::Char('c' | 'C') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                return None;
            }
            KeyCode::Backspace => {
                if secret.pop().is_some() {
                    crate::logging::console_print_raw("\u{8} \u{8}");
                }
            }
            KeyCode::Char(character)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                    && secret.chars().count() < 256 =>
            {
                secret.push(character);
                crate::logging::console_print_raw("•");
            }
            _ => {}
        }
    }
    drop(guard);
    crate::logging::console_println("");
    Some(secret)
}

/// First-run account bootstrap shown before the full-screen console starts.
pub fn prompt_create_first_admin(pool: &DbPool, reader: &mut dyn BufRead) {
    crate::logging::console_print_raw(&format!(
        "\n  {}┌─ First-run setup ─────────────────────────────────────┐\n\
           │  Create the administrator used at /admin.             │\n\
           │  Ctrl-C or end-of-input skips setup for now.           │\n\
           └────────────────────────────────────────────────────────┘{}\n\n",
        color("\x1b[36m"),
        color("\x1b[0m")
    ));
    if crate::logging::is_tty() {
        crate::logging::console_println(&format!(
            "  {}[SECURE] Password input is masked.{}",
            color("\x1b[32m"),
            color("\x1b[0m")
        ));
    }

    let Some(username) = prompt_username(reader) else {
        crate::logging::console_println(
            "\n  Setup skipped. Use: rustchan-cli admin create-admin <user> <pass>",
        );
        return;
    };
    let Some(password) = prompt_password(reader) else {
        crate::logging::console_println("\n  Setup skipped.");
        return;
    };
    let request = OperationRequest::CreateAdmin { username, password };
    match execute(&request, pool) {
        Ok(message) => crate::logging::console_println(&format!(
            "\n  {}[OK]{} {message}",
            color("\x1b[32m"),
            color("\x1b[0m")
        )),
        Err(error) => {
            crate::logging::console_println(&format!(
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
    crate::logging::console_println("");
}

/// Collect and create an optional first board during line-mode setup.
fn prompt_create_first_board(pool: &DbPool, reader: &mut dyn BufRead) {
    crate::logging::console_println("");
    let Some(short) = prompt(reader, "Short name (for example, tech):") else {
        crate::logging::console_println("  Board setup skipped.");
        return;
    };
    let Some(name) = prompt(reader, "Display name:") else {
        crate::logging::console_println("  Board setup skipped.");
        return;
    };
    let description = prompt(reader, "Description (optional):").unwrap_or_default();
    let nsfw = yes(prompt(reader, "NSFW board? [y/N]:"));
    let allow_images = !yes(prompt(reader, "Disable image uploads? [y/N]:"));
    let allow_video = !yes(prompt(reader, "Disable video uploads? [y/N]:"));
    let allow_audio = yes(prompt(reader, "Enable audio uploads? [y/N]:"));
    let request = OperationRequest::CreateBoard {
        short: short.to_ascii_lowercase(),
        name,
        description,
        nsfw,
        allow_images,
        allow_video,
        allow_audio,
    };
    match execute(&request, pool) {
        Ok(message) => crate::logging::console_println(&format!(
            "  {}[OK]{} {message}",
            color("\x1b[32m"),
            color("\x1b[0m")
        )),
        Err(error) => crate::logging::console_println(&format!(
            "  {}[ERROR]{} {error}",
            color("\x1b[31m"),
            color("\x1b[0m")
        )),
    }
}

/// Interpret an optional line-mode yes/no answer.
fn yes(answer: Option<String>) -> bool {
    answer.is_some_and(|value| matches!(value.to_ascii_lowercase().as_str(), "y" | "yes"))
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
}
