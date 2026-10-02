//! Explicit actual-media benchmark for pooled connection ownership.

use super::tests::{temp_upload, thread_command, TEST_BOARD};
use super::{submit_post, SubmitPostCommand, SubmitPostResult};
use anyhow::{Context as _, Result};

/// Event subscriber used only by the explicit posting benchmark.
#[derive(Debug, Clone, Default)]
struct PostingPoolProbe {
    /// Connection hold durations on producer threads.
    holds: std::sync::Arc<parking_lot::Mutex<Vec<u128>>>,
    /// Checkout durations on reader threads.
    waits: std::sync::Arc<parking_lot::Mutex<Vec<u128>>>,
    /// Checkout timeouts across all benchmark threads.
    timeouts: std::sync::Arc<std::sync::atomic::AtomicU64>,
}

impl r2d2::HandleEvent for PostingPoolProbe {
    fn handle_checkin(&self, event: r2d2::event::CheckinEvent) {
        if std::thread::current().name() == Some("db-post-benchmark") {
            self.holds.lock().push(event.duration().as_micros());
        }
    }
    fn handle_checkout(&self, event: r2d2::event::CheckoutEvent) {
        if std::thread::current().name() == Some("db-read-benchmark") {
            self.waits.lock().push(event.duration().as_micros());
        }
    }
    fn handle_timeout(&self, _event: r2d2::event::TimeoutEvent) {
        self.timeouts
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Summarize an elapsed-time series in microseconds.
fn posting_latencies(mut values: Vec<u128>) -> serde_json::Value {
    values.sort_unstable();
    let percentile = |percent: usize| {
        values
            .get((values.len() * percent).div_ceil(100).saturating_sub(1))
            .copied()
            .unwrap_or(0)
    };
    serde_json::json!({"p50_us":percentile(50),"p95_us":percentile(95),"p99_us":percentile(99),
        "max_us":values.last().copied().unwrap_or(0),"samples":values.len()})
}

/// Production entry point shared by the current and archived-baseline harness.
fn benchmark_submit(
    pool: &crate::db::DbPool,
    queue: &crate::workers::JobQueue,
    command: SubmitPostCommand,
) -> crate::error::Result<SubmitPostResult> {
    submit_post(pool, queue, command)
}

/// Run actual PNG submissions alongside pooled reads; preparation is excluded from timing.
fn media_posting_case(size: u32, synchronous: &'static str) -> Result<serde_json::Value> {
    let mut state = crate::test_support::app_state();
    let upload_dir = tempfile::tempdir()?;
    let path = {
        let conn = state.db.get()?;
        conn.path().context("DB path")?.to_owned()
    };
    let probe = PostingPoolProbe::default();
    let manager = r2d2_sqlite::SqliteConnectionManager::file(path).with_init(move |conn| {
        conn.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON; PRAGMA busy_timeout=1000;",
        )?;
        conn.pragma_update(None, "synchronous", synchronous)
    });
    state.db = r2d2::Pool::builder()
        .max_size(size)
        .min_idle(Some(size))
        .connection_timeout(std::time::Duration::from_secs(1))
        .event_handler(Box::new(probe.clone()))
        .build(manager)?;
    state.job_queue = std::sync::Arc::new(crate::workers::JobQueue::new(state.db.clone()));
    crate::db::create_board(&*state.db.get()?, TEST_BOARD, "Test", "", false)?;
    let mut commands = Vec::new();
    for number in 0_u8..8 {
        let pixels = image::RgbImage::from_fn(768, 768, |x, y| {
            image::Rgb([
                u8::try_from(x % 256).unwrap_or(0),
                u8::try_from(y % 256).unwrap_or(0),
                number,
            ])
        });
        let mut bytes = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgb8(pixels).write_to(&mut bytes, image::ImageFormat::Png)?;
        let (upload, name) = temp_upload(&format!("bench-{number}.png"), bytes.get_ref())?;
        let mut command = thread_command(
            TEST_BOARD,
            &format!("media-bench-{number}"),
            "media body",
            upload_dir.path().to_str().context("uploads path")?,
        );
        command.file_data = Some((upload, name));
        commands.push(command);
    }
    let mut result = run_media_posting_case(&state, commands, &probe, size)?;
    result
        .as_object_mut()
        .context("media benchmark object")?
        .insert("synchronous".into(), serde_json::json!(synchronous));
    Ok(result)
}

/// Coordinate measured readers/producers without background network activity.
fn run_media_posting_case(
    state: &crate::middleware::AppState,
    commands: Vec<SubmitPostCommand>,
    probe: &PostingPoolProbe,
    size: u32,
) -> Result<serde_json::Value> {
    let barrier = std::sync::Barrier::new(6);
    let remaining = std::sync::atomic::AtomicU64::new(2);
    let post_times = parking_lot::Mutex::new(Vec::new());
    let reads = std::sync::atomic::AtomicU64::new(0);
    let started = std::time::Instant::now();
    std::thread::scope(|scope| -> Result<()> {
        let mut handles = Vec::new();
        let mut commands = commands.into_iter();
        for _ in 0..2 {
            let batch: Vec<_> = commands.by_ref().take(4).collect();
            let barrier = &barrier;
            let remaining = &remaining;
            let post_times = &post_times;
            handles.push(
                std::thread::Builder::new()
                    .name("db-post-benchmark".into())
                    .spawn_scoped(scope, move || -> Result<()> {
                        barrier.wait();
                        let result = (|| -> Result<()> {
                            for command in batch {
                                let started = std::time::Instant::now();
                                benchmark_submit(&state.db, &state.job_queue, command)?;
                                post_times.lock().push(started.elapsed().as_micros());
                            }
                            Ok(())
                        })();
                        remaining.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
                        result
                    })?,
            );
        }
        for _ in 0..4 {
            let barrier = &barrier;
            let remaining = &remaining;
            let reads = &reads;
            handles.push(
                std::thread::Builder::new()
                    .name("db-read-benchmark".into())
                    .spawn_scoped(scope, move || -> Result<()> {
                        barrier.wait();
                        while remaining.load(std::sync::atomic::Ordering::Relaxed) > 0 {
                            if let Ok(conn) = state.db.get() {
                                crate::db::database_ready_probe(&conn)?;
                                reads.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                            }
                            std::thread::sleep(std::time::Duration::from_millis(2));
                        }
                        Ok(())
                    })?,
            );
        }
        for handle in handles {
            handle
                .join()
                .map_err(|_| anyhow::anyhow!("media benchmark worker panicked"))??;
        }
        Ok(())
    })?;
    let elapsed = started.elapsed();
    let conn = state.db.get()?;
    crate::db::verify_database_schema(&conn)?;
    let pending = crate::db::list_pending_fs_ops(&conn)?.len();
    let posts: i64 = conn.query_row("SELECT COUNT(*) FROM posts", [], |r| r.get(0))?;
    Ok(
        serde_json::json!({"pool_size":size,"posts":posts,"reads":reads.load(std::sync::atomic::Ordering::Relaxed),
        "seconds":elapsed.as_secs_f64(),"posting":posting_latencies(post_times.into_inner()),
        "posting_connection_holds":posting_latencies(probe.holds.lock().clone()),
        "reader_checkout":posting_latencies(probe.waits.lock().clone()),
        "pool_timeouts":probe.timeouts.load(std::sync::atomic::Ordering::Relaxed),"pending_fs_ops":pending}),
    )
}

#[test]
#[ignore = "explicit actual-media/connection-lifetime benchmark; set RUSTCHAN_DB_POST_EVIDENCE"]
fn posting_connection_workload() -> Result<()> {
    let output = std::env::var("RUSTCHAN_DB_POST_EVIDENCE")?;
    let mut measurements = Vec::new();
    for synchronous in ["NORMAL", "FULL"] {
        for size in [2, 4, 8] {
            measurements.push(media_posting_case(size, synchronous)?);
        }
    }
    std::fs::write(output, serde_json::to_vec_pretty(&measurements)?)?;
    Ok(())
}
