//! Read-only background database/process sampling with bounded history.
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

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
    /// Total retained thread count, including archived threads.
    pub threads: i64,
    /// Post count.
    pub posts: i64,
    /// Logical SQLite bytes (`page_count` times `page_size`), independent of WAL files.
    pub db_bytes: i64,
    /// Last observed main database, WAL and SHM file sizes; -1 is unavailable.
    pub db_files: [i64; 3],
    /// Time of the most recent complete database file-size sample.
    pub db_files_sampled_at: Option<Instant>,
    /// Database footprint collection error, independent of query health.
    pub db_files_error: Option<String>,
    /// Upload-tree collection error; the last successful value is retained.
    pub storage_error: Option<String>,
    /// Whether final listener and database startup readiness was observed.
    pub runtime_ready: bool,
    /// Whether admission is blocked pending recovery.
    pub recovery_blocked: bool,
    /// Observed Tor lifecycle, publication and forwarding state.
    pub tor: crate::detect::TorStatus,
    /// Upload-tree size in bytes.
    pub upload_bytes: i64,
    /// Last slow upload-tree sample, refreshed at most every thirty seconds.
    pub storage_sampled_at: Option<Instant>,
    /// Resident memory in bytes.
    pub mem_bytes: i64,
    /// Per-board short name, thread count, and post count.
    pub board_rows: Vec<(String, i64, i64)>,
    /// Uploads currently being written.
    pub active_uploads: u64,
    /// Video-processing jobs currently running.
    pub active_ffmpeg_videos: u64,
    /// Bounded task, account and moderation data.
    pub operator: super::telemetry::OperatorSnapshot,
    /// Bounded measured request history.
    pub history: super::history::TrafficHistory,
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
            boards: -1,
            threads: -1,
            posts: -1,
            db_bytes: -1,
            db_files: [-1; 3],
            db_files_sampled_at: None,
            db_files_error: None,
            storage_error: None,
            runtime_ready: false,
            recovery_blocked: false,
            tor: crate::detect::TorStatus::default(),
            upload_bytes: -1,
            storage_sampled_at: None,
            mem_bytes: 0,
            board_rows: Vec::new(),
            active_uploads: 0,
            active_ffmpeg_videos: 0,
            operator: super::telemetry::OperatorSnapshot::default(),
            history: super::history::TrafficHistory::default(),
            spinner_tick: 0,
            onion_address: None,
        }
    }
}

/// Collection baselines and slow-storage cache owned by the sampling task.
#[derive(Debug)]
pub struct Sampler {
    /// Server start time, independent of console startup.
    started: Instant,
    /// Previous cumulative request counter.
    previous_requests: u64,
    /// Time of the previous request sample.
    previous_tick: Instant,
    /// Most recent successful query values, retained across collection failures.
    database: ChanStats,
    /// Last successful operator snapshot, independent of database counters.
    operator: super::telemetry::OperatorSnapshot,
    /// Bounded ten-second request intervals.
    history: super::history::TrafficHistory,
}

impl Sampler {
    /// Initialize the sampler from the actual process request counter.
    #[must_use]
    pub fn new(started: Instant) -> Self {
        Self {
            started,
            previous_requests: crate::server::REQUEST_COUNT.load(Ordering::Relaxed),
            previous_tick: Instant::now(),
            database: ChanStats::default(),
            operator: super::telemetry::OperatorSnapshot::default(),
            history: super::history::TrafficHistory::default(),
        }
    }

    /// Collect read-only diagnostics off the render/input tasks.
    #[expect(
        clippy::as_conversions,
        clippy::cast_precision_loss,
        reason = "request-rate display intentionally converts a monotonic counter delta to floating point"
    )]
    pub fn collect(
        &mut self,
        app: &crate::middleware::AppState,
        onion_address: Option<String>,
        http_port: u16,
    ) -> ChanStats {
        let job_queue = &app.job_queue;
        let uptime_secs = self.started.elapsed().as_secs();
        let now = Instant::now();
        let elapsed_secs = now
            .duration_since(self.previous_tick)
            .as_secs_f64()
            .max(0.001);
        let req_count = crate::server::REQUEST_COUNT.load(Ordering::Relaxed);
        let request_delta = req_count.saturating_sub(self.previous_requests);
        let rps = request_delta as f64 / elapsed_secs;
        self.previous_requests = req_count;
        self.previous_tick = now;

        let in_flight = crate::server::IN_FLIGHT.load(Ordering::Relaxed);
        let active_uploads = crate::server::ACTIVE_UPLOADS.load(Ordering::Relaxed);
        let active_ffmpeg_videos = job_queue.active_video_count();
        let online = crate::server::ACTIVE_IPS.len();

        // An isolated, read-only connection never occupies the application pool.
        // SQLite VM work has a 200 ms budget shared by all queries in this sample.
        let access = app.data_access.try_read();
        let connection = access
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Data access suspended"))
            .and_then(|_| telemetry_connection());
        let database = match &connection {
            Ok(connection) => read_database_stats(connection),
            Err(error) => Err(anyhow::anyhow!("{error}")),
        };
        let collection_error = match database {
            Ok(mut snapshot) => {
                snapshot.sampled_at = Some(Instant::now());
                self.database = snapshot;
                None
            }
            Err(_) => Some("Database collection unavailable, suspended or over its 200ms budget; retaining the last successful values.".to_owned()),
        };
        match &connection {
            Ok(connection) => {
                match super::telemetry::OperatorSnapshot::read(connection) {
                    Ok(mut operator) => {
                        operator.sampled_at = Some(Instant::now());
                        self.operator = operator;
                    }
                    Err(_) => self.operator.error = Some(
                        "Task collection unavailable or over budget; retained values may be stale."
                            .to_owned(),
                    ),
                }
            }
            Err(_) => {
                self.operator.error =
                    Some("Task collection suspended; retained values may be stale.".to_owned());
            }
        }
        let mut operator = self.operator.clone();
        operator.attach_runtime(app);
        self.history.observe(now, req_count);

        ChanStats {
            is_ready: true,
            sampled_at: self.database.sampled_at,
            collection_error,
            uptime_secs,
            http_port,
            req_count,
            rps,
            in_flight,
            online,
            runtime_ready: app.runtime_ready.load(Ordering::Acquire),
            recovery_blocked: app.data_access.is_blocked(),
            tor: crate::detect::tor_status(),
            history: self.history.clone(),
            mem_bytes: process_rss_kb().cast_signed().saturating_mul(1_024),
            active_uploads,
            active_ffmpeg_videos,
            operator,
            spinner_tick: 0,
            onion_address,
            ..self.database.clone()
        }
    }
}

/// Read all database metrics from one consistent snapshot, preserving failures.
fn read_database_stats(connection: &rusqlite::Connection) -> anyhow::Result<ChanStats> {
    let transaction = connection.unchecked_transaction()?;
    let boards = transaction.query_row("SELECT COUNT(*) FROM boards", [], |row| row.get(0))?;
    let threads = transaction.query_row("SELECT COUNT(*) FROM threads", [], |row| row.get(0))?;
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

/// Open a bounded, read-only telemetry connection, never creating a database.
fn telemetry_connection() -> anyhow::Result<rusqlite::Connection> {
    let connection = rusqlite::Connection::open_with_flags(
        &crate::config::CONFIG.database_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    connection.busy_timeout(Duration::from_millis(50))?;
    let started = Instant::now();
    connection.progress_handler(
        1000,
        Some(move || started.elapsed() >= Duration::from_millis(200)),
    )?;
    Ok(connection)
}

/// Return a directory tree's total size as a signed display value.
pub(in crate::server) fn dir_size_bytes(path: &str) -> std::io::Result<i64> {
    walkdir_size(std::path::Path::new(path)).map(|size| i64::try_from(size).unwrap_or(i64::MAX))
}

/// Sum a bounded upload-tree observation without following symlinks.
fn walkdir_size(path: &std::path::Path) -> std::io::Result<u64> {
    let started = Instant::now();
    let mut pending = vec![path.to_path_buf()];
    let mut visited = 0_u32;
    let mut total = 0_u64;
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(directory)? {
            visited = visited.saturating_add(1);
            if visited > 100_000 || started.elapsed() >= Duration::from_secs(2) {
                return Err(std::io::Error::other(
                    "Upload-tree scan exceeded its entry/time budget",
                ));
            }
            let entry = entry?;
            let kind = entry.file_type()?;
            if kind.is_dir() {
                pending.push(entry.path());
            } else if kind.is_file() {
                total = total.saturating_add(entry.metadata()?.len());
            }
        }
    }
    Ok(total)
}

/// Observe main/WAL/SHM bytes. Missing sidecars mean zero; a missing main file is unavailable.
pub(in crate::server) fn database_file_sizes() -> std::io::Result<[i64; 3]> {
    let path = &crate::config::CONFIG.database_path;
    let mut sizes = [-1; 3];
    for (index, suffix) in ["", "-wal", "-shm"].into_iter().enumerate() {
        let bytes = match std::fs::metadata(format!("{path}{suffix}")) {
            Ok(metadata) => i64::try_from(metadata.len()).unwrap_or(i64::MAX),
            Err(error) if index != 0 && error.kind() == std::io::ErrorKind::NotFound => 0,
            Err(error) => return Err(error),
        };
        if let Some(size) = sizes.get_mut(index) {
            *size = bytes;
        }
    }
    Ok(sizes)
}

/// Read the process resident-set size in kibibytes when supported.
#[cfg(any(target_os = "linux", target_os = "macos"))]
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

/// Report an unavailable resident-set sample on other platforms.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
const fn process_rss_kb() -> u64 {
    0
}
