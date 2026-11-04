//! Full-screen, responsive terminal administration console.

/// Terminal presentation and capability policy.
pub mod capabilities;
/// Background sampling and operational values.
pub mod stats;
pub use stats::ChanStats;
/// Shared search and list projections.
pub mod browse;
/// Content list presentation.
mod content;
/// Ratatui rendering and log-tail loading.
pub mod dashboard;
/// Modal dialog presentation.
mod dialogs;
/// Validated forms and text editing.
mod forms;
/// Keyboard reference presentation.
mod help;
/// Bounded measured traffic history.
pub mod history;
/// Blocking terminal-input adapter.
pub mod input;
/// Log loading and viewing.
mod logs;
/// Task, diagnostic and configuration presentation.
mod operator_views;
/// Typed navigation, form, dialog, and feedback state.
pub mod state;
/// Read-only task and runtime snapshots.
pub mod telemetry;
/// Shared rendering primitives and theme.
mod widgets;
/// Administrative operation execution and first-run setup.
pub mod wizard;

use crossterm::{cursor, event, execute, terminal};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use std::io::stdout;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, Notify, RwLock};

pub use state::{ConsoleAction, ConsoleState, OperationRequest};
pub use wizard::prompt_create_first_admin;

/// True while raw mode and the alternate screen are active.
static RAW_MODE_ACTIVE: AtomicBool = AtomicBool::new(false);

/// Whether the first-run line editor owns raw mode (without an alternate screen).
static LINE_INPUT_ACTIVE: AtomicBool = AtomicBool::new(false);

/// Serialize drawing and restoration, including reentry from a draw panic.
static TERMINAL_IO: parking_lot::ReentrantMutex<()> = parking_lot::ReentrantMutex::new(());

/// Exact Windows host input mode before raw-mode entry.
#[cfg(windows)]
static INPUT_MODE: parking_lot::Mutex<Option<(crossterm_winapi::ConsoleMode, u32)>> =
    parking_lot::Mutex::new(None);

/// Save the host mode before Crossterm changes it.
fn enable_raw_mode() -> std::io::Result<()> {
    #[cfg(windows)]
    {
        let mode =
            crossterm_winapi::ConsoleMode::from(crossterm_winapi::Handle::current_in_handle()?);
        let original = mode.mode()?;
        *INPUT_MODE.lock() = Some((mode, original));
    }
    terminal::enable_raw_mode()
}

/// Restore the exact Windows mode or the saved Unix termios state.
fn restore_input_mode() {
    #[cfg(windows)]
    {
        let saved = INPUT_MODE.lock().take();
        if let Some((mode, original)) = saved {
            drop(mode.set_mode(original));
        }
    }
    #[cfg(not(windows))]
    drop(terminal::disable_raw_mode());
}

/// Whether input and drawing still belong to an active console session.
pub(super) fn is_active() -> bool {
    RAW_MODE_ACTIVE.load(Ordering::SeqCst)
}

/// Start raw first-run input under the same panic/exit cleanup as the TUI.
pub(super) fn start_line_input() -> std::io::Result<()> {
    let _terminal_guard = TERMINAL_IO.lock();
    enable_raw_mode()?;
    LINE_INPUT_ACTIVE.store(true, Ordering::SeqCst);
    crate::logging::set_tui_active(true);
    if let Err(error) = execute!(stdout(), event::EnableBracketedPaste) {
        cleanup();
        return Err(error);
    }
    Ok(())
}

/// Whether first-run input can continue after a shutdown signal.
pub(super) fn line_input_active() -> bool {
    LINE_INPUT_ACTIVE.load(Ordering::SeqCst)
}

/// Concurrently shared console interaction state.
pub type SharedConsoleState = Arc<RwLock<ConsoleState>>;
/// Concurrently shared statistics snapshot.
pub type SharedStats = Arc<RwLock<ChanStats>>;
/// Render-task wakeup used for immediate input and state feedback.
pub type RedrawSignal = Arc<Notify>;

/// Restore the terminal after normal shutdown, Ctrl-C, or a panic.
///
/// The compare-and-swap makes repeated cleanup calls safe.
pub fn cleanup() {
    let _terminal_guard = TERMINAL_IO.lock();
    if LINE_INPUT_ACTIVE.swap(false, Ordering::SeqCst) {
        restore_input_mode();
        drop(execute!(stdout(), event::DisableBracketedPaste));
        crate::logging::set_tui_active(false);
    }
    if RAW_MODE_ACTIVE
        .compare_exchange(true, false, Ordering::SeqCst, Ordering::Relaxed)
        .is_ok()
    {
        restore_input_mode();
        drop(execute!(
            stdout(),
            event::DisableBracketedPaste,
            terminal::LeaveAlternateScreen,
            cursor::Show
        ));
        crate::logging::set_tui_active(false);
    }
}

/// Enter the alternate screen and start input and diff-render tasks.
///
/// Unlike the previous console implementation, this never resizes the user's
/// terminal. Layout adaptation happens inside the renderer.
///
/// # Errors
///
/// Returns an error when raw mode, alternate-screen setup, or the Ratatui
/// backend cannot be initialized.
pub fn start(
    shared_metrics: &SharedStats,
    shared_app: &SharedConsoleState,
) -> anyhow::Result<(mpsc::Receiver<input::KeyEvent>, RedrawSignal)> {
    let _terminal_guard = TERMINAL_IO.lock();
    enable_raw_mode()?;
    crate::logging::set_tui_active(true);
    if let Err(error) = execute!(
        stdout(),
        terminal::EnterAlternateScreen,
        event::EnableBracketedPaste,
        cursor::Hide
    ) {
        drop(execute!(
            stdout(),
            event::DisableBracketedPaste,
            terminal::LeaveAlternateScreen,
            cursor::Show
        ));
        restore_input_mode();
        crate::logging::set_tui_active(false);
        return Err(error.into());
    }
    RAW_MODE_ACTIVE.store(true, Ordering::SeqCst);
    crate::logging::set_tui_active(true);

    let backend = CrosstermBackend::new(stdout());
    let mut terminal = match Terminal::new(backend) {
        Ok(terminal) => terminal,
        Err(error) => {
            cleanup();
            return Err(error.into());
        }
    };
    let redraw = Arc::new(Notify::new());
    let (key_tx, key_rx) = mpsc::channel::<input::KeyEvent>(256);
    if let Err(error) = input::spawn(key_tx, Arc::clone(&redraw)) {
        cleanup();
        return Err(error.into());
    }

    let metrics_reader = Arc::clone(shared_metrics);
    let app_writer = Arc::clone(shared_app);
    let redraw_for_render = Arc::clone(&redraw);
    let render_task = tokio::spawn(async move {
        let mut spinner_tick = 0u8;
        let mut logs = dashboard::LogSnapshot::default();
        let mut last_log_refresh = None;
        let mut pending_logs: Option<tokio::task::JoinHandle<dashboard::LogSnapshot>> = None;
        let mut animated = false;
        loop {
            let frame_delay = if animated {
                Duration::from_millis(250)
            } else {
                Duration::from_secs(1)
            };
            tokio::select! {
                () = tokio::time::sleep(frame_delay) => {}
                () = redraw_for_render.notified() => {}
            }
            if !RAW_MODE_ACTIVE.load(Ordering::Relaxed) {
                return;
            }

            let mut snapshot = metrics_reader.read().await.clone();
            spinner_tick = spinner_tick.wrapping_add(1) % 10;
            snapshot.spinner_tick = spinner_tick;

            let (active_screen, follow_logs) = {
                let app = app_writer.read().await;
                (app.screen, app.logs.follow)
            };
            refresh_logs(
                active_screen,
                follow_logs,
                &app_writer,
                &mut logs,
                &mut pending_logs,
                &mut last_log_refresh,
            )
            .await;

            let mut app = app_writer.write().await;
            app.expire_notice(Instant::now());
            animated = matches!(app.dialog, Some(state::Dialog::Progress { label: _ }))
                || snapshot.active_uploads > 0;
            let draw_result = {
                let _render_terminal_guard = TERMINAL_IO.lock();
                if !is_active() {
                    return;
                }
                terminal.draw(|frame| {
                    dashboard::render(frame, &mut app, &snapshot, &logs);
                    capabilities::ascii_frame(frame.buffer_mut());
                })
            };
            drop(app);
            if let Err(error) = draw_result {
                cleanup();
                tracing::error!(target: "console", error = %error, "Console disabled after render failure");
                return;
            }
        }
    });
    // The task owns the terminal until console cleanup stops its loop.
    // Dropping this handle intentionally detaches it from the startup caller.
    drop(render_task);

    redraw.notify_one();
    Ok((key_rx, redraw))
}

/// Write a bounded plain operational report without ANSI, stdin or alternate screen.
pub(super) fn print_plain(stats: &ChanStats) {
    let age = stats.sampled_at.map_or_else(
        || "unknown".to_owned(),
        |time| format!("{}s", time.elapsed().as_secs()),
    );
    let tasks = stats.operator.jobs.map_or_else(
        || "unknown".to_owned(),
        |jobs| {
            format!(
                "{} queued / {} running / {} failed",
                jobs.queued, jobs.running, jobs.failed
            )
        },
    );
    crate::logging::console_print_raw(&format!(
        "[RustChan] HTTP/HTTPS {} (external health unverified); DB {} logical / {} disk; sample age {age}; tasks {tasks}; media {} failed; Tor {}; onion {}{}\n",
        if stats.runtime_ready { "BOUND" } else { "STARTING" },
        widgets::fmt_bytes(stats.db_bytes), widgets::fmt_bytes(dashboard::database_footprint(stats)),
        stats.operator.media_failed, stats.tor.phase.label(),
        stats.onion_address.as_deref().map_or_else(|| "unavailable".to_owned(), telemetry::display_text),
        if stats.collection_error.is_some() || stats.operator.error.is_some() { "; collection DEGRADED" } else { "" }
    ));
}

#[cfg(test)]
#[path = "tests/regressions.rs"]
mod regression_tests;

/// Poll a single read-only worker without putting filesystem IO on the render path.
async fn refresh_logs(
    active_screen: state::Screen,
    follow_logs: bool,
    app_writer: &SharedConsoleState,
    logs: &mut dashboard::LogSnapshot,
    pending_logs: &mut Option<tokio::task::JoinHandle<dashboard::LogSnapshot>>,
    last_log_refresh: &mut Option<Instant>,
) {
    if pending_logs
        .as_ref()
        .is_some_and(tokio::task::JoinHandle::is_finished)
    {
        let completed = pending_logs.take();
        if let Some(task) = completed {
            let mut next = task.await.unwrap_or_else(|_| dashboard::LogSnapshot {
                error: Some("Log reader failed; retry with F".to_owned()),
                ..dashboard::LogSnapshot::default()
            });
            if app_writer.read().await.logs.follow || logs.sampled_at.is_none() {
                if next.error.is_some() && logs.sampled_at.is_some() {
                    next.lines = std::mem::take(&mut logs.lines);
                    next.source.clone_from(&logs.source);
                    next.sampled_at = logs.sampled_at;
                }
                *logs = next;
            }
        }
    }
    if active_screen == state::Screen::Logs
        && pending_logs.is_none()
        && (follow_logs || last_log_refresh.is_none())
        && last_log_refresh.is_none_or(|last: Instant| last.elapsed() >= Duration::from_secs(1))
    {
        *pending_logs = Some(tokio::task::spawn_blocking(dashboard::load_log_snapshot));
        *last_log_refresh = Some(Instant::now());
    }
}
