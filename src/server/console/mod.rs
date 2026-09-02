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
    if RAW_MODE_ACTIVE
        .compare_exchange(true, false, Ordering::SeqCst, Ordering::Relaxed)
        .is_ok()
    {
        crate::logging::set_tui_active(false);
        drop(terminal::disable_raw_mode());
        drop(execute!(
            stdout(),
            event::DisableBracketedPaste,
            terminal::LeaveAlternateScreen,
            cursor::Show
        ));
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
) -> anyhow::Result<(mpsc::UnboundedReceiver<input::KeyEvent>, RedrawSignal)> {
    terminal::enable_raw_mode()?;
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
    let (key_tx, key_rx) = mpsc::unbounded_channel::<input::KeyEvent>();
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

            let active_screen = app_writer.read().await.screen;
            if active_screen == state::Screen::Logs
                && last_log_refresh
                    .is_none_or(|last: Instant| last.elapsed() >= Duration::from_secs(1))
            {
                logs = tokio::task::block_in_place(dashboard::load_log_snapshot);
                last_log_refresh = Some(Instant::now());
            }

            let mut app = app_writer.write().await;
            app.expire_notice(Instant::now());
            app.boards.reconcile(snapshot.board_rows.len());
            animated = matches!(app.dialog, Some(state::Dialog::Progress { .. }))
                || snapshot.active_uploads > 0;
            let draw_result = terminal.draw(|frame| {
                dashboard::render(frame, &app, &snapshot, &logs);
            });
            drop(app);
            if let Err(error) = draw_result {
                tracing::error!(target: "console", error = %error, "Failed to render console frame");
                cleanup();
                return;
            }
        }
    });

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

    let (boards, threads, posts, db_bytes, board_rows, collection_error) = match pool.get() {
        Ok(connection) => {
            let boards = connection
                .query_row("SELECT COUNT(*) FROM boards", [], |row| row.get(0))
                .unwrap_or(0);
            let threads = connection
                .query_row(
                    "SELECT COUNT(*) FROM threads WHERE archived = 0",
                    [],
                    |row| row.get(0),
                )
                .unwrap_or(0);
            let posts = connection
                .query_row("SELECT COUNT(*) FROM posts", [], |row| row.get(0))
                .unwrap_or(0);
            let page_count: i64 = connection
                .query_row("PRAGMA page_count", [], |row| row.get(0))
                .unwrap_or(0);
            let page_size: i64 = connection
                .query_row("PRAGMA page_size", [], |row| row.get(0))
                .unwrap_or(4_096);
            (
                boards,
                threads,
                posts,
                page_count.saturating_mul(page_size),
                crate::db::get_per_board_stats(&connection),
                None,
            )
        }
        Err(error) => {
            tracing::warn!(target: "console", error = %error, "Console database metrics unavailable");
            (
                -1,
                -1,
                -1,
                -1,
                Vec::new(),
                Some("Database metrics are temporarily unavailable.".to_owned()),
            )
        }
    };

    *prev_threads = threads;
    *prev_posts = posts;

    ChanStats {
        is_ready: true,
        sampled_at: Some(now),
        collection_error,
        uptime_secs,
        req_count,
        rps,
        in_flight,
        online,
        boards,
        threads,
        posts,
        db_bytes,
        upload_bytes: dir_size_bytes(&crate::config::CONFIG.upload_dir),
        mem_bytes: process_rss_kb().cast_signed().saturating_mul(1_024),
        board_rows,
        active_uploads,
        active_ffmpeg_videos,
        spinner_tick: 0,
        onion_address,
    }
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
