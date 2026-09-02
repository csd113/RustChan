//! Crossterm input adapter for the full-screen console.
//!
//! Polling runs on a dedicated thread so terminal reads never block Tokio.

use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use std::sync::Arc;
use tokio::sync::{mpsc, Notify};

/// Terminal input vocabulary consumed by the console state machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyEvent {
    /// Printable character.
    Character(char),
    /// Bracketed-paste content.
    Paste(String),
    /// Confirm or advance.
    Enter,
    /// Move focus forward.
    Tab,
    /// Move focus backward.
    BackTab,
    /// Remove the preceding character.
    Backspace,
    /// Remove the current character.
    Delete,
    /// Move upward.
    Up,
    /// Move downward.
    Down,
    /// Move left.
    Left,
    /// Move right.
    Right,
    /// Move one page upward.
    PageUp,
    /// Move one page downward.
    PageDown,
    /// Move to the beginning.
    Home,
    /// Move to the end.
    End,
    /// Dismiss the current context or return to the dashboard.
    Escape,
    /// Clear the focused line after Ctrl-U.
    ClearLine,
    /// Submit the entire form after Ctrl-Enter or F2.
    Submit,
    /// Request immediate shutdown after Ctrl-C.
    ForceQuit,
    /// Redraw after resize or terminal focus changes.
    Resize,
}

/// Map one Crossterm key event into the console vocabulary.
const fn map_key(code: KeyCode, modifiers: KeyModifiers) -> Option<KeyEvent> {
    if modifiers.contains(KeyModifiers::CONTROL) {
        return match code {
            KeyCode::Char('c' | 'C') => Some(KeyEvent::ForceQuit),
            KeyCode::Char('u' | 'U') => Some(KeyEvent::ClearLine),
            KeyCode::Enter => Some(KeyEvent::Submit),
            _ => None,
        };
    }
    if modifiers.contains(KeyModifiers::ALT) {
        return None;
    }
    match code {
        KeyCode::Char(character) => Some(KeyEvent::Character(character)),
        KeyCode::Enter => Some(KeyEvent::Enter),
        KeyCode::Tab => Some(KeyEvent::Tab),
        KeyCode::BackTab => Some(KeyEvent::BackTab),
        KeyCode::Backspace => Some(KeyEvent::Backspace),
        KeyCode::Delete => Some(KeyEvent::Delete),
        KeyCode::Up => Some(KeyEvent::Up),
        KeyCode::Down => Some(KeyEvent::Down),
        KeyCode::Left => Some(KeyEvent::Left),
        KeyCode::Right => Some(KeyEvent::Right),
        KeyCode::PageUp => Some(KeyEvent::PageUp),
        KeyCode::PageDown => Some(KeyEvent::PageDown),
        KeyCode::Home => Some(KeyEvent::Home),
        KeyCode::End => Some(KeyEvent::End),
        KeyCode::Esc => Some(KeyEvent::Escape),
        KeyCode::F(2) => Some(KeyEvent::Submit),
        _ => None,
    }
}

/// Convert a raw terminal event into a state-machine event.
fn map_event(event: Event) -> Option<KeyEvent> {
    match event {
        Event::Key(key) if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) => {
            map_key(key.code, key.modifiers)
        }
        Event::Paste(content) => Some(KeyEvent::Paste(content)),
        Event::Resize(_, _) | Event::FocusGained | Event::FocusLost => Some(KeyEvent::Resize),
        Event::Key(_) | Event::Mouse(_) => None,
    }
}

/// Spawn a blocking terminal-input thread.
///
/// The thread exits when the receiver closes or the terminal detaches. Every
/// delivered input also wakes the render task for low-latency feedback.
///
/// # Errors
///
/// Returns an error if the operating system refuses to create the thread.
pub fn spawn(tx: mpsc::UnboundedSender<KeyEvent>, redraw: Arc<Notify>) -> std::io::Result<()> {
    std::thread::Builder::new()
        .name("console-input".into())
        .spawn(move || loop {
            match event::poll(std::time::Duration::from_millis(50)) {
                Ok(true) => {
                    let Ok(event) = event::read() else {
                        break;
                    };
                    let Some(mapped) = map_event(event) else {
                        continue;
                    };
                    if tx.send(mapped).is_err() {
                        break;
                    }
                    redraw.notify_one();
                }
                Ok(false) => {
                    if tx.is_closed() {
                        break;
                    }
                }
                Err(_) => break,
            }
        })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escape_maps_to_back_instead_of_quit() {
        assert_eq!(
            map_key(KeyCode::Esc, KeyModifiers::NONE),
            Some(KeyEvent::Escape),
            "escape should use the navigation-back action"
        );
    }

    #[test]
    fn control_shortcuts_do_not_insert_text() {
        assert_eq!(
            map_key(KeyCode::Char('u'), KeyModifiers::CONTROL),
            Some(KeyEvent::ClearLine),
            "Ctrl-U should clear the active input"
        );
        assert_eq!(
            map_key(KeyCode::Char('c'), KeyModifiers::CONTROL),
            Some(KeyEvent::ForceQuit),
            "Ctrl-C should retain immediate shutdown"
        );
    }

    #[test]
    fn alternate_modified_keys_are_ignored() {
        assert_eq!(
            map_key(KeyCode::Char('q'), KeyModifiers::ALT),
            None,
            "Alt-modified input should not trigger global actions"
        );
    }
}
