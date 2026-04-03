// server/console/input.rs — Crossterm keyboard event reader.
//
// Runs in a dedicated std::thread so crossterm::event::poll() never blocks
// the Tokio runtime. Sends KeyEvent values over an unbounded channel.
// 50 ms poll timeout keeps CPU idle with <50 ms key latency.

use crossterm::event::{self, Event, KeyCode, KeyModifiers};
use tokio::sync::mpsc;

// ─── Key event vocabulary ─────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyEvent {
    Help,
    Reload,       // [R] — force immediate stats refresh
    ToggleNgrok,  // [T]
    ToggleLogs,   // [L]
    BoardList,    // [B]
    CreateBoard,  // [C] — enters wizard
    CreateAdmin,  // [A] — enters wizard
    DeleteThread, // [D] — enters wizard
    Quit,         // [Q] or Esc → ConfirmQuit screen
    Confirm,      // [Y]
    Cancel,       // [N]
    ForceQuit,    // Ctrl-C
    Other,
}

// ─── Key mapping ──────────────────────────────────────────────────────────────

fn map_key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
    if modifiers.contains(KeyModifiers::CONTROL) && code == KeyCode::Char('c') {
        return KeyEvent::ForceQuit;
    }
    match code {
        KeyCode::Char('h' | 'H') => KeyEvent::Help,
        KeyCode::Char('r' | 'R') => KeyEvent::Reload,
        KeyCode::Char('t' | 'T') => KeyEvent::ToggleNgrok,
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

// ─── spawn() ─────────────────────────────────────────────────────────────────

/// Spawn a blocking thread that polls crossterm events and sends mapped
/// `KeyEvent` values over `tx`. Exits when the channel is closed.
///
/// Returns an error if the OS refuses to create the thread.
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
