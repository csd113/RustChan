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
    /// Active thread count.
    pub threads: i64,
    /// Post count.
    pub posts: i64,
    /// Database file size in bytes.
    pub db_bytes: i64,
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
            boards: 0,
            threads: 0,
            posts: 0,
            db_bytes: 0,
            upload_bytes: 0,
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
    /// Most recent upload-tree byte count; -1 means unavailable.
    upload_bytes: i64,
    /// Time of the slow storage scan.
    storage_sampled_at: Option<Instant>,
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
            upload_bytes: -1,
            storage_sampled_at: None,
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
        let pool = &app.db;
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

        let database = match read_database_stats(pool) {
            Ok(snapshot) => snapshot,
            Err(error) => {
                tracing::warn!(target: "console", error = %error, "Console database metrics unavailable");
                ChanStats {
                    boards: -1,
                    threads: -1,
                    posts: -1,
                    db_bytes: -1,
                    collection_error: Some(
                        "Database metrics are temporarily unavailable.".to_owned(),
                    ),
                    ..ChanStats::default()
                }
            }
        };

        let mut operator = super::telemetry::OperatorSnapshot::read(pool).unwrap_or_else(|error| {
            tracing::warn!(target: "console", %error, "Operator snapshot unavailable");
            super::telemetry::OperatorSnapshot {
                error: Some(
                    "Task and administrative data unavailable; press R to retry and inspect Logs."
                        .to_owned(),
                ),
                ..super::telemetry::OperatorSnapshot::default()
            }
        });

        operator.attach_runtime(app);
        self.history.observe(now, req_count);
        if self
            .storage_sampled_at
            .is_none_or(|last| now.saturating_duration_since(last) >= Duration::from_secs(30))
        {
            self.upload_bytes = dir_size_bytes(&crate::config::CONFIG.upload_dir).unwrap_or(-1);
            self.storage_sampled_at = Some(now);
        }

        ChanStats {
            is_ready: true,
            sampled_at: Some(now),
            uptime_secs,
            http_port,
            req_count,
            rps,
            in_flight,
            online,
            upload_bytes: self.upload_bytes,
            storage_sampled_at: self.storage_sampled_at,
            history: self.history.clone(),
            mem_bytes: process_rss_kb().cast_signed().saturating_mul(1_024),
            active_uploads,
            active_ffmpeg_videos,
            operator,
            spinner_tick: 0,
            onion_address,
            ..database
        }
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
fn dir_size_bytes(path: &str) -> std::io::Result<i64> {
    walkdir_size(std::path::Path::new(path)).map(|size| i64::try_from(size).unwrap_or(i64::MAX))
}

/// Recursively sum regular-file sizes beneath a path.
fn walkdir_size(path: &std::path::Path) -> std::io::Result<u64> {
    let mut total = 0_u64;
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let size = if file_type.is_dir() {
            walkdir_size(&entry.path())?
        } else if file_type.is_file() {
            entry.metadata()?.len()
        } else {
            0
        }; // Do not follow symlinks outside the configured upload tree.
        total = total.saturating_add(size);
    }
    Ok(total)
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
        let malformed_connection = pool.get()?;
        let _changed_rows =
            malformed_connection.execute("UPDATE boards SET short_name = NULL", [])?;
        drop(malformed_connection);
        assert!(
            read_database_stats(&pool).is_err(),
            "a malformed board row must fail the whole snapshot"
        );
        let incomplete_connection = pool.get()?;
        let _dropped_table_rows = incomplete_connection.execute("DROP TABLE posts", [])?;
        drop(incomplete_connection);
        assert!(
            read_database_stats(&pool).is_err(),
            "SQL errors must not turn into healthy zero counts"
        );
        Ok(())
    }
}
