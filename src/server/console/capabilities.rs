//! Explicit terminal policy; redirected streams never trigger a GUI relaunch.
use std::io::IsTerminal as _;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

/// Operator-selected console presentation.
#[derive(Clone, Copy, Debug, Default, clap::ValueEnum)]
#[repr(u8)]
pub enum Mode {
    /// Full screen on a capable terminal, otherwise plain logs.
    #[default]
    Auto,
    /// Request full screen, with plain fallback on initialization failure.
    Tui,
    /// Periodic bounded operational lines without cursor controls.
    Plain,
    /// Disable interactive console and operational summaries; retain logs.
    Off,
}

/// Process-local presentation policy configured before logging and startup.
static MODE: AtomicU8 = AtomicU8::new(0);
/// Whether to replace unsupported display glyphs with ASCII.
static ASCII: AtomicBool = AtomicBool::new(false);

/// Apply command-line policy before starting either line input or the console.
pub fn configure(mode: Mode, ascii: bool) {
    MODE.store(
        match mode {
            Mode::Auto => 0,
            Mode::Tui => 1,
            Mode::Plain => 2,
            Mode::Off => 3,
        },
        Ordering::Relaxed,
    );
    ASCII.store(ascii, Ordering::Relaxed);
}

/// Whether full-screen input/output is available under the selected policy.
#[must_use]
pub fn interactive() -> bool {
    let available = MODE.load(Ordering::Relaxed) <= 1
        && std::io::stdin().is_terminal()
        && std::io::stdout().is_terminal()
        && std::env::var("TERM").map_or(cfg!(windows), |term| !term.is_empty() && term != "dumb");
    #[cfg(windows)]
    let available = available && crossterm::ansi_support::supports_ansi();
    available
}

/// Whether periodic plain operational summaries were explicitly selected.
#[must_use]
pub fn plain() -> bool {
    MODE.load(Ordering::Relaxed) == 2
}

/// Replace terminal chrome and unsupported glyphs for an explicit ASCII mode.
pub(super) fn ascii_frame(buffer: &mut ratatui::buffer::Buffer) {
    if !ASCII.load(Ordering::Relaxed) {
        return;
    }
    for cell in &mut buffer.content {
        let symbol = cell.symbol();
        if symbol.is_ascii() {
            continue;
        }
        let replacement = match symbol {
            "│" | "┃" | "║" => "|",
            "─" | "━" | "═" | "—" | "–" => "-",
            "╭" | "╮" | "╰" | "╯" | "┌" | "┐" | "└" | "┘" | "┬" | "┴" | "┼" | "✓" => {
                "+"
            }
            "↑" => "^",
            "↓" => "v",
            "←" => "<",
            "→" | "▶" => ">",
            "·" | "•" | "…" => ".",
            "×" => "x",
            _ => "?",
        };
        let _cell = cell.set_symbol(replacement);
    }
}
