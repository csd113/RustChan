//! Crossterm keyboard-event reader.
//!
//! Polling runs on a dedicated thread so it never blocks the Tokio runtime.

use crossterm::event::{self, Event, KeyCode, KeyModifiers};
use tokio::sync::mpsc;

// Key event vocabulary
#[derive(Debug, Clone, PartialEq, Eq)]
/// Console action produced from a terminal key event.
pub enum KeyEvent {
    /// Open or close help.
    Help,
    /// Force an immediate statistics refresh.
    Reload,
    /// Open or close the live-log view.
    ToggleLogs,
    /// Open or close the board list.
    BoardList,
    /// Enter the create-board wizard.
    CreateBoard,
    /// Enter the create-administrator wizard.
    CreateAdmin,
    /// Enter the delete-thread wizard.
    DeleteThread,
    /// Request a confirmed graceful shutdown.
    Quit,
    /// Confirm the current prompt.
    Confirm,
    /// Cancel the current prompt.
    Cancel,
    /// Request immediate shutdown after Ctrl-C.
    ForceQuit,
    /// Ignore an unmapped key.
    Other,
}

// Key mapping
/// Map one Crossterm key event into the console action vocabulary.
fn map_key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
    if modifiers.contains(KeyModifiers::CONTROL) && code == KeyCode::Char('c') {
        return KeyEvent::ForceQuit;
    }
    match code {
        KeyCode::Char('h' | 'H') => KeyEvent::Help,
        KeyCode::Char('r' | 'R') => KeyEvent::Reload,
        KeyCode::Char('l' | 'L') => KeyEvent::ToggleLogs,
        KeyCode::Char('b' | 'B') => KeyEvent::BoardList,
        KeyCode::Char('c' | 'C') => KeyEvent::CreateBoard,
        KeyCode::Char('a' | 'A') => KeyEvent::CreateAdmin,
        KeyCode::Char('d' | 'D') => KeyEvent::DeleteThread,
        KeyCode::Char('q' | 'Q') | KeyCode::Esc => KeyEvent::Quit,
        KeyCode::Char('y' | 'Y') => KeyEvent::Confirm,
        KeyCode::Char('n' | 'N') => KeyEvent::Cancel,
        _ => KeyEvent::Other,
    }
}

/// Spawn a blocking thread that polls crossterm events and sends mapped
/// `KeyEvent` values over `tx`. Exits when the channel is closed.
///
/// # Errors
///
/// Returns an error if the operating system refuses to create the thread.
pub fn spawn(tx: mpsc::UnboundedSender<KeyEvent>) -> std::io::Result<()> {
    std::thread::Builder::new()
        .name("console-input".into())
        .spawn(move || loop {
            match event::poll(std::time::Duration::from_millis(50)) {
                Ok(true) => {
                    if let Ok(Event::Key(key)) = event::read() {
                        if tx.send(map_key(key.code, key.modifiers)).is_err() {
                            break;
                        }
                    }
                }
                Ok(false) => {
                    if tx.is_closed() {
                        break;
                    }
                }
                Err(_) => break, // terminal detached
            }
        })?;
    Ok(())
}
