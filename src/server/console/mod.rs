//! Full-screen, responsive terminal administration console.

/// Ratatui rendering and log-tail loading.
pub mod dashboard;
/// Blocking terminal-input adapter.
pub mod input;
/// Typed navigation, form, dialog, and feedback state.
pub mod state;
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

/// Whether input and drawing still belong to an active console session.
pub(super) fn is_active() -> bool {
    RAW_MODE_ACTIVE.load(Ordering::SeqCst)
}

/// Start raw first-run input under the same panic/exit cleanup as the TUI.
pub(super) fn start_line_input() -> std::io::Result<()> {
    let _terminal_guard = TERMINAL_IO.lock();
    terminal::enable_raw_mode()?;
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

/// Operational values rendered by the console dashboard.
#[derive(Clone, Debug)]
pub struct ChanStats {
    /// Whether the first collection attempt has completed.
    pub is_ready: bool,
    /// Monotonic time of the most recent collection.
    pub sampled_at: Option<Instant>,
    /// Recoverable collection failure, when metrics are degraded.
    pub collection_error: Option<String>,
    /// Process uptime in seconds.
    pub uptime_secs: u64,
    /// Actual bound HTTP port, including a command-line override.
    pub http_port: u16,
    /// Total handled requests.
    pub req_count: u64,
    /// Requests handled per second during the latest sample.
    pub rps: f64,
    /// Requests currently in flight.
    pub in_flight: u64,
    /// Recently active client IP count.
    pub online: usize,
    /// Board count.
    pub boards: i64,
    /// Active thread count.
    pub threads: i64,
    /// Post count.
    pub posts: i64,
    /// Database file size in bytes.
    pub db_bytes: i64,
    /// Upload-tree size in bytes.
    pub upload_bytes: i64,
    /// Resident memory in bytes.
    pub mem_bytes: i64,
    /// Per-board short name, thread count, and post count.
    pub board_rows: Vec<(String, i64, i64)>,
    /// Uploads currently being written.
    pub active_uploads: u64,
    /// Video-processing jobs currently running.
    pub active_ffmpeg_videos: u64,
    /// Current progress-spinner frame index.
    pub spinner_tick: u8,
    /// Live onion address once Tor has bootstrapped.
    pub onion_address: Option<String>,
}

impl Default for ChanStats {
    fn default() -> Self {
        Self {
            is_ready: false,
            sampled_at: None,
            collection_error: None,
            uptime_secs: 0,
            http_port: 8080,
            req_count: 0,
            rps: 0.0,
            in_flight: 0,
            online: 0,
            boards: 0,
            threads: 0,
            posts: 0,
            db_bytes: 0,
            upload_bytes: 0,
            mem_bytes: 0,
            board_rows: Vec::new(),
            active_uploads: 0,
            active_ffmpeg_videos: 0,
            spinner_tick: 0,
            onion_address: None,
        }
    }
}

/// Restore the terminal after normal shutdown, Ctrl-C, or a panic.
///
/// The compare-and-swap makes repeated cleanup calls safe.
pub fn cleanup() {
    let _terminal_guard = TERMINAL_IO.lock();
    if LINE_INPUT_ACTIVE.swap(false, Ordering::SeqCst) {
        drop(terminal::disable_raw_mode());
        drop(execute!(stdout(), event::DisableBracketedPaste));
        crate::logging::set_tui_active(false);
    }
    if RAW_MODE_ACTIVE
        .compare_exchange(true, false, Ordering::SeqCst, Ordering::Relaxed)
        .is_ok()
    {
        drop(terminal::disable_raw_mode());
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
    terminal::enable_raw_mode()?;
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
        drop(terminal::disable_raw_mode());
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
    tokio::spawn(async move {
        let mut spinner_tick = 0u8;
        let mut logs = dashboard::LogSnapshot::default();
        let mut last_log_refresh = None;
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
            if active_screen == state::Screen::Logs
                && (follow_logs || last_log_refresh.is_none())
                && last_log_refresh
                    .is_none_or(|last: Instant| last.elapsed() >= Duration::from_secs(1))
            {
                let next_logs = tokio::task::block_in_place(dashboard::load_log_snapshot);
                // Input can pause while disk IO is in flight. Do not replace
                // the snapshot the operator has just chosen to inspect.
                if app_writer.read().await.logs.follow || last_log_refresh.is_none() {
                    logs = next_logs;
                    last_log_refresh = Some(Instant::now());
                }
            }

            let mut app = app_writer.write().await;
            app.expire_notice(Instant::now());
            animated = matches!(app.dialog, Some(state::Dialog::Progress { .. }))
                || snapshot.active_uploads > 0;
            let draw_result = {
                let _terminal_guard = TERMINAL_IO.lock();
                if !is_active() {
                    return;
                }
                terminal.draw(|frame| {
                    dashboard::render(frame, &mut app, &snapshot, &logs);
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

    redraw.notify_one();
    Ok((key_rx, redraw))
}

/// Collect a fresh statistics snapshot from the database and process atomics.
///
/// Delta-tracking values are mutated in place so request-rate samples remain
/// accurate across collection calls.
#[expect(
    clippy::as_conversions,
    clippy::cast_precision_loss,
    reason = "request-rate display intentionally converts a monotonic counter delta to floating point"
)]
#[expect(
    clippy::too_many_arguments,
    reason = "the collector receives the complete sampling state maintained by its caller"
)]
pub fn collect_stats(
    pool: &crate::db::DbPool,
    job_queue: &crate::workers::JobQueue,
    start: Instant,
    prev_req: &mut u64,
    prev_tick: &mut Instant,
    prev_threads: &mut i64,
    prev_posts: &mut i64,
    onion_address: Option<String>,
    http_port: u16,
) -> ChanStats {
    let uptime_secs = start.elapsed().as_secs();
    let now = Instant::now();
    let elapsed_secs = now.duration_since(*prev_tick).as_secs_f64().max(0.001);
    let req_count = crate::server::REQUEST_COUNT.load(Ordering::Relaxed);
    let request_delta = req_count.saturating_sub(*prev_req);
    let rps = request_delta as f64 / elapsed_secs;
    *prev_req = req_count;
    *prev_tick = now;

    let in_flight = crate::server::IN_FLIGHT.load(Ordering::Relaxed);
    let active_uploads = crate::server::ACTIVE_UPLOADS.load(Ordering::Relaxed);
    let active_ffmpeg_videos = job_queue.active_video_count();
    let online = crate::server::ACTIVE_IPS.len();

    let database = match read_database_stats(pool) {
        Ok(snapshot) => snapshot,
        Err(error) => {
            tracing::warn!(target: "console", error = %error, "Console database metrics unavailable");
            ChanStats {
                boards: -1,
                threads: -1,
                posts: -1,
                db_bytes: -1,
                collection_error: Some("Database metrics are temporarily unavailable.".to_owned()),
                ..ChanStats::default()
            }
        }
    };

    *prev_threads = database.threads;
    *prev_posts = database.posts;

    ChanStats {
        is_ready: true,
        sampled_at: Some(now),
        uptime_secs,
        http_port,
        req_count,
        rps,
        in_flight,
        online,
        upload_bytes: dir_size_bytes(&crate::config::CONFIG.upload_dir),
        mem_bytes: process_rss_kb().cast_signed().saturating_mul(1_024),
        active_uploads,
        active_ffmpeg_videos,
        spinner_tick: 0,
        onion_address,
        ..database
    }
}

/// Read all database metrics from one consistent snapshot, preserving failures.
fn read_database_stats(pool: &crate::db::DbPool) -> anyhow::Result<ChanStats> {
    let connection = pool.get()?;
    let transaction = connection.unchecked_transaction()?;
    let boards = transaction.query_row("SELECT COUNT(*) FROM boards", [], |row| row.get(0))?;
    let threads = transaction.query_row(
        "SELECT COUNT(*) FROM threads WHERE archived = 0",
        [],
        |row| row.get(0),
    )?;
    let posts = transaction.query_row("SELECT COUNT(*) FROM posts", [], |row| row.get(0))?;
    let page_count: i64 = transaction.query_row("PRAGMA page_count", [], |row| row.get(0))?;
    let page_size: i64 = transaction.query_row("PRAGMA page_size", [], |row| row.get(0))?;
    let board_rows = crate::db::get_per_board_stats(&transaction)?;
    transaction.commit()?;
    Ok(ChanStats {
        boards,
        threads,
        posts,
        db_bytes: page_count.saturating_mul(page_size),
        board_rows,
        ..ChanStats::default()
    })
}

/// Return a directory tree's total size as a signed display value.
fn dir_size_bytes(path: &str) -> i64 {
    walkdir_size(std::path::Path::new(path)).cast_signed()
}

/// Recursively sum regular-file sizes beneath a path.
fn walkdir_size(path: &std::path::Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(path) else {
        return 0;
    };
    entries
        .flatten()
        .map(|entry| {
            if entry.file_type().is_ok_and(|file_type| file_type.is_dir()) {
                walkdir_size(&entry.path())
            } else {
                entry.metadata().map_or(0, |metadata| metadata.len())
            }
        })
        .sum()
}

/// Read the process resident-set size in kibibytes when supported.
fn process_rss_kb() -> u64 {
    #[cfg(target_os = "linux")]
    {
        if let Ok(status) = std::fs::read_to_string("/proc/self/status") {
            for line in status.lines() {
                if let Some(value) = line.strip_prefix("VmRSS:") {
                    return value
                        .split_whitespace()
                        .next()
                        .and_then(|number| number.parse().ok())
                        .unwrap_or(0);
                }
            }
        }
    }
    #[cfg(target_os = "macos")]
    {
        let pid = std::process::id().to_string();
        if let Ok(output) = std::process::Command::new("ps")
            .args(["-o", "rss=", "-p", &pid])
            .output()
        {
            let value = String::from_utf8_lossy(&output.stdout);
            if let Ok(kibibytes) = value.trim().parse::<u64>() {
                return kibibytes;
            }
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "assertions verify that query failures cannot be displayed as healthy zero metrics"
    )]
    fn database_snapshot_propagates_schema_and_row_errors() -> anyhow::Result<()> {
        let manager = r2d2_sqlite::SqliteConnectionManager::memory();
        let pool = r2d2::Pool::builder().max_size(1).build(manager)?;
        let connection = pool.get()?;
        connection.execute_batch("CREATE TABLE boards (id INTEGER, short_name TEXT); CREATE TABLE threads (id INTEGER, board_id INTEGER, archived INTEGER); CREATE TABLE posts (id INTEGER, thread_id INTEGER); INSERT INTO boards VALUES (1, 'test');")?;
        drop(connection);
        let snapshot = read_database_stats(&pool)?;
        assert_eq!(snapshot.boards, 1, "healthy board count must be retained");
        let connection = pool.get()?;
        connection.execute("UPDATE boards SET short_name = NULL", [])?;
        drop(connection);
        assert!(
            read_database_stats(&pool).is_err(),
            "a malformed board row must fail the whole snapshot"
        );
        let connection = pool.get()?;
        connection.execute("DROP TABLE posts", [])?;
        drop(connection);
        assert!(
            read_database_stats(&pool).is_err(),
            "SQL errors must not turn into healthy zero counts"
        );
        Ok(())
    }
}
