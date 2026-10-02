//! Offline, opt-in measurements against the bundled SQLite and real DB helpers.

use anyhow::{ensure, Context as _, Result};
use rusqlite::{params, Connection};
use serde::Serialize;
use std::{path::Path, sync::Arc, time::Instant};

/// Number of concurrent callers used for each measured case.
const WORKERS: u32 = 8;
/// Number of measured operations per caller.
const OPERATIONS: u32 = 400;

/// One operation's elapsed times in microseconds.
#[derive(Debug, Serialize)]
struct Sample {
    /// Full checkout plus operation latency.
    elapsed_us: u128,
    /// Time waiting for a pooled connection.
    checkout_us: u128,
    /// Time acquiring an immediate SQLite write lock (zero for reads).
    write_wait_us: u128,
    /// Time holding a write transaction after lock acquisition.
    transaction_us: u128,
    /// Whether SQLite or the pool rejected the operation.
    failed: bool,
    /// Whether the rejection was SQLite busy/locked.
    busy: bool,
    /// Whether the pool checkout timed out.
    pool_timeout: bool,
}

/// Nearest-rank latency summary, with integer units avoiding precision loss.
#[derive(Debug, Serialize)]
struct Latencies {
    /// Median microseconds.
    #[serde(rename = "p50_us")]
    p50: u128,
    /// 95th percentile microseconds.
    #[serde(rename = "p95_us")]
    p95: u128,
    /// 99th percentile microseconds.
    #[serde(rename = "p99_us")]
    p99: u128,
    /// Maximum microseconds.
    #[serde(rename = "max_us")]
    max: u128,
}

/// Result for one pool/durability/workload combination.
#[derive(Debug, Serialize)]
struct Measurement {
    /// Configured maximum connections.
    pool_size: u32,
    /// Explicit SQLite synchronous mode.
    synchronous: String,
    /// Workload traffic mix.
    workload: String,
    /// Number of attempted operations.
    operations: u32,
    /// Successful operations per wall-clock second.
    operations_per_second: f64,
    /// Total request latency.
    latency: Latencies,
    /// Read-only operation latency including checkout.
    read_latency: Latencies,
    /// Write operation latency including checkout and lock acquisition.
    write_latency: Latencies,
    /// Pool checkout latency.
    checkout: Latencies,
    /// Immediate lock acquisition latency for writes only.
    write_wait: Latencies,
    /// Write lock duration after acquisition, including commit.
    transaction: Latencies,
    /// Requests whose checkout exceeded 100 microseconds.
    checkout_over_100us: usize,
    /// Failed operations of all kinds.
    failures: u32,
    /// SQLite busy or locked errors.
    busy_errors: usize,
    /// Pool timeouts.
    pool_timeouts: usize,
    /// Database bytes, including pages resident in WAL.
    database_bytes: i64,
    /// WAL file bytes before passive checkpoint.
    wal_bytes: u64,
    /// WAL pages and progress reported by a passive checkpoint.
    checkpoint: (i64, i64, i64),
    /// Passive checkpoint elapsed microseconds after workers release snapshots.
    checkpoint_us: u128,
    /// Resident memory in KiB, if the host provides ps.
    rss_kib: Option<u64>,
    /// Process CPU seconds during this case, if the host provides ps.
    cpu_seconds: Option<f64>,
}

/// Explain output for a named production query shape.
#[derive(Debug, Serialize)]
struct Plan {
    /// Stable description of the query.
    name: String,
    /// Planner details from the populated database.
    details: Vec<String>,
}

/// Generate deterministic representative content, including FTS and media rows.
fn seed(conn: &Connection) -> Result<()> {
    super::schema::install_or_migrate_schema(conn)?;
    conn.execute_batch(
        "BEGIN IMMEDIATE;
         WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<8)
         INSERT INTO boards(id,short_name,name) SELECT x,'b'||x,'Board '||x FROM n;
         WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<800)
         INSERT INTO threads(id,board_id,subject,archived,reply_count,bumped_at)
         SELECT x,1+(x-1)%8,'Thread '||x,x%5=0,49,1700000000+x FROM n;
         WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<40000)
         INSERT INTO posts(id,thread_id,board_id,body,body_html,ip_hash,deletion_token,
                           is_op,created_at,file_path,thumb_path,mime_type,media_type,file_size)
         SELECT x,1+(x-1)/50,1+((x-1)/50)%8,
                CASE WHEN x%101=0 THEN 'rare benchmark needle' ELSE 'representative post text' END,
                'representative post text','actor-'||(x%1000),'fixture-token',(x-1)%50=0,
                1700000000+x,CASE WHEN x%4=0 THEN 'b'||(1+((x-1)/50)%8)||'/src/'||x||'.png' END,
                CASE WHEN x%4=0 THEN 'b'||(1+((x-1)/50)%8)||'/thumb/'||x||'.png' END,
                CASE WHEN x%4=0 THEN 'image/png' END,CASE WHEN x%4=0 THEN 'image' END,
                CASE WHEN x%4=0 THEN 1024 END FROM n;
         INSERT INTO file_hashes(sha256,file_path,thumb_path,mime_type)
         SELECT printf('%064x',id),file_path,thumb_path,mime_type FROM posts WHERE file_path IS NOT NULL;
         INSERT INTO bans(ip_hash,reason) SELECT DISTINCT ip_hash,'fixture' FROM posts WHERE id%101=0;
         INSERT INTO admin_users(id,username,password_hash) VALUES(1,'fixture-admin','fixture-hash');
         INSERT INTO reports(post_id,thread_id,board_id,reason,reporter_hash)
         SELECT id,thread_id,board_id,'fixture','reporter-'||id FROM posts WHERE id%83=0;
         INSERT INTO mod_log(admin_id,admin_name,action,target_type,target_id)
         SELECT 1,'fixture-admin','fixture','post',id FROM posts WHERE id%37=0;
         INSERT INTO polls(thread_id,question,expires_at)
         SELECT id,'fixture poll',unixepoch()+86400 FROM threads WHERE id%7=0;
         INSERT INTO poll_options(poll_id,text,position) SELECT id,'yes',0 FROM polls;
         INSERT INTO poll_options(poll_id,text,position) SELECT id,'no',1 FROM polls;
         INSERT INTO poll_votes(poll_id,option_id,ip_hash)
         SELECT poll_id,id,'fixture-voter' FROM poll_options WHERE position=0;
         INSERT INTO background_jobs(job_type,payload,status)
         SELECT 'spam_check','{}',CASE WHEN id%2=0 THEN 'done' ELSE 'pending' END FROM posts WHERE id%31=0;
         COMMIT;",
    )?;
    Ok(())
}

/// Create warmed file-backed production-configured connections for a case.
fn pool(path: &Path, size: u32, synchronous: &'static str) -> Result<super::DbPool> {
    let manager = r2d2_sqlite::SqliteConnectionManager::file(path).with_init(move |conn| {
        conn.execute_batch(super::pool::CONNECTION_PRAGMAS)?;
        conn.pragma_update(None, "synchronous", synchronous)
    });
    Ok(r2d2::Pool::builder()
        .max_size(size)
        .min_idle(Some(size))
        .connection_timeout(std::time::Duration::from_secs(1))
        .build(manager)?)
}

/// Run production read helpers, covering board/catalog/thread/polling/FTS/admin.
fn read(conn: &Connection, operation: u32) -> Result<()> {
    let thread = i64::from(operation % 800 + 1);
    let board = (thread - 1) % 8 + 1;
    match operation % 7 {
        0 => drop(super::get_threads_for_board(conn, board, 10, 0)?),
        1 => drop(super::get_threads_for_board(conn, board, 100, 0)?),
        2 => {
            drop(super::get_thread(conn, thread)?);
            drop(super::get_posts_for_thread(conn, thread)?);
        }
        3 => drop(super::get_new_posts_since(
            conn,
            board,
            thread,
            thread * 50 - 5,
            100,
        )?),
        4 => drop(super::search_posts(conn, board, "needle", 20, 0)?),
        5 => drop(super::get_open_reports(conn)?),
        _ => {
            drop(super::get_poll_for_thread(conn, thread, "fixture-voter")?);
            drop(super::find_file_by_hash(
                conn,
                &format!("{:064x}", operation * 4 + 4),
            )?);
        }
    }
    Ok(())
}

/// Run a short post transaction with FTS, counters and submission-token work.
fn write(conn: &Connection, worker: u32, operation: u32) -> Result<()> {
    let thread = i64::from((operation * WORKERS + worker) % 799 + 1);
    let board = (thread - 1) % 8 + 1;
    conn.execute(
        "INSERT INTO posts(thread_id,board_id,body,body_html,ip_hash,deletion_token)
         VALUES(?1,?2,'workload reply','workload reply',?3,'workload-delete')",
        params![thread, board, format!("workload-{worker}")],
    )?;
    let post_id = conn.last_insert_rowid();
    conn.execute(
        "UPDATE threads SET reply_count=reply_count+1,bumped_at=unixepoch() WHERE id=?1",
        [thread],
    )?;
    super::record_post_submission(
        conn,
        &format!("workload-{worker}-{operation}"),
        &format!("workload-{worker}"),
        board,
        thread,
        post_id,
        false,
    )?;
    if operation.is_multiple_of(17) {
        super::enqueue_job(conn, "spam_check", "{}")?;
        if let Some((job, _)) = super::claim_next_job(conn)? {
            super::complete_job(conn, job)?;
        }
    }
    if operation.is_multiple_of(23) {
        conn.execute(
            "INSERT OR IGNORE INTO poll_votes(poll_id,option_id,ip_hash)
                      SELECT poll_id,id,?1 FROM poll_options WHERE position=0 LIMIT 1",
            [format!("workload-{worker}-{operation}")],
        )?;
    }
    if operation.is_multiple_of(41) {
        conn.execute(
            "DELETE FROM background_jobs WHERE id IN
                      (SELECT id FROM background_jobs WHERE status='done' LIMIT 4)",
            [],
        )?;
    }
    Ok(())
}

/// Record a single operation without hiding failures behind retries.
fn sample(pool: &super::DbPool, worker: u32, operation: u32, write_percent: u32) -> Sample {
    let started = Instant::now();
    let acquired = pool.get();
    let checkout_us = started.elapsed().as_micros();
    let pool_timeout = acquired.is_err();
    let mut write_wait_us = 0;
    let mut transaction_us = 0;
    let result = acquired.map_err(anyhow::Error::from).and_then(|conn| {
        if (operation * 37 + worker * 13) % 100 >= write_percent {
            return read(&conn, operation);
        }
        let wait = Instant::now();
        let begin = conn.execute_batch("BEGIN IMMEDIATE");
        write_wait_us = wait.elapsed().as_micros().max(1);
        begin?;
        let locked = Instant::now();
        let result = write(&conn, worker, operation)
            .and_then(|()| super::commit_transaction(&conn, "commit workload reply"));
        if result.is_err() {
            drop(conn.execute_batch("ROLLBACK"));
        }
        transaction_us = locked.elapsed().as_micros().max(1);
        result
    });
    let busy = result.as_ref().err().is_some_and(|error| {
        error.downcast_ref::<rusqlite::Error>().is_some_and(|error| {
            matches!(error, rusqlite::Error::SqliteFailure(code, _)
                if matches!(code.code, rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked))
        })
    });
    Sample {
        elapsed_us: started.elapsed().as_micros(),
        checkout_us,
        write_wait_us,
        transaction_us,
        failed: result.is_err(),
        busy,
        pool_timeout,
    }
}

/// Compute nearest-rank percentiles over one elapsed-time series.
fn latencies(values: impl Iterator<Item = u128>) -> Latencies {
    let mut values: Vec<_> = values.collect();
    values.sort_unstable();
    let percentile = |percent: usize| {
        values
            .get((values.len() * percent).div_ceil(100).saturating_sub(1))
            .copied()
            .unwrap_or(0)
    };
    Latencies {
        p50: percentile(50),
        p95: percentile(95),
        p99: percentile(99),
        max: values.last().copied().unwrap_or(0),
    }
}

/// Read this process's resident memory using an optional host utility.
fn rss_kib() -> Option<u64> {
    let output = std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &std::process::id().to_string()])
        .output()
        .ok()?;
    String::from_utf8(output.stdout).ok()?.trim().parse().ok()
}

/// Read accumulated process CPU time without adding a platform dependency.
fn cpu_seconds() -> Option<f64> {
    let output = std::process::Command::new("ps")
        .args(["-o", "time=", "-p", &std::process::id().to_string()])
        .output()
        .ok()?;
    let time = String::from_utf8(output.stdout).ok()?;
    time.trim().split(':').try_fold(0.0_f64, |total, part| {
        part.parse::<f64>()
            .ok()
            .map(|part| total.mul_add(60.0, part))
    })
}

/// Verify persisted relationships, FTS and counters after concurrent operations.
fn verify_stress_state(conn: &Connection) -> Result<()> {
    let integrity: String = conn.query_row("PRAGMA integrity_check", [], |r| r.get(0))?;
    ensure!(integrity == "ok", "stress integrity check: {integrity}");
    ensure!(
        !conn.prepare("PRAGMA foreign_key_check")?.exists([])?,
        "stress foreign-key violation"
    );
    conn.execute(
        "INSERT INTO posts_fts(posts_fts,rank) VALUES('integrity-check',1)",
        [],
    )?;
    super::verify_database_schema(conn)?;
    let incorrect_counts: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM threads t WHERE t.reply_count !=
             (SELECT COUNT(*) FROM posts p WHERE p.thread_id=t.id AND p.is_op=0))",
        [],
        |row| row.get(0),
    )?;
    ensure!(
        !incorrect_counts,
        "stress transaction left an incorrect reply count"
    );
    Ok(())
}

/// Stress one mix; verify database and FTS before returning the measurement.
fn measure(path: &Path, size: u32, sync: &'static str, mix: (&str, u32)) -> Result<Measurement> {
    let pool = pool(path, size, sync)?;
    let barrier = Arc::new(std::sync::Barrier::new(usize::try_from(WORKERS)?));
    let cpu_started = cpu_seconds();
    let started = Instant::now();
    let samples = std::thread::scope(|scope| -> Result<Vec<Sample>> {
        let handles: Vec<_> = (0..WORKERS)
            .map(|worker| {
                let pool = &pool;
                let barrier = Arc::clone(&barrier);
                scope.spawn(move || {
                    barrier.wait();
                    (0..OPERATIONS)
                        .map(|operation| sample(pool, worker, operation, mix.1))
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        let mut samples = Vec::new();
        for handle in handles {
            samples.extend(
                handle
                    .join()
                    .map_err(|_| anyhow::anyhow!("workload worker panicked"))?,
            );
        }
        Ok(samples)
    })?;
    let elapsed = started.elapsed();
    let cpu_elapsed = cpu_started
        .zip(cpu_seconds())
        .map(|(before, after)| (after - before).max(0.0));
    let conn = pool.get()?;
    let wal_bytes = std::fs::metadata(path.with_extension("sqlite3-wal")).map_or(0, |m| m.len());
    let checkpoint_started = Instant::now();
    let checkpoint = conn.query_row("PRAGMA wal_checkpoint(PASSIVE)", [], |r| {
        Ok((r.get(0)?, r.get(1)?, r.get(2)?))
    })?;
    let checkpoint_us = checkpoint_started.elapsed().as_micros();
    verify_stress_state(&conn)?;
    let failures = u32::try_from(samples.iter().filter(|s| s.failed).count())?;
    Ok(Measurement {
        pool_size: size,
        synchronous: sync.to_owned(),
        workload: mix.0.to_owned(),
        operations: WORKERS * OPERATIONS,
        operations_per_second: f64::from(WORKERS * OPERATIONS - failures) / elapsed.as_secs_f64(),
        latency: latencies(samples.iter().map(|s| s.elapsed_us)),
        read_latency: latencies(
            samples
                .iter()
                .filter(|s| !s.pool_timeout && s.write_wait_us == 0)
                .map(|s| s.elapsed_us),
        ),
        write_latency: latencies(
            samples
                .iter()
                .filter(|s| s.write_wait_us > 0)
                .map(|s| s.elapsed_us),
        ),
        checkout: latencies(samples.iter().map(|s| s.checkout_us)),
        write_wait: latencies(
            samples
                .iter()
                .filter(|s| s.write_wait_us > 0)
                .map(|s| s.write_wait_us),
        ),
        transaction: latencies(
            samples
                .iter()
                .filter(|s| s.transaction_us > 0)
                .map(|s| s.transaction_us),
        ),
        checkout_over_100us: samples.iter().filter(|s| s.checkout_us > 100).count(),
        failures,
        busy_errors: samples.iter().filter(|s| s.busy).count(),
        pool_timeouts: samples.iter().filter(|s| s.pool_timeout).count(),
        database_bytes: super::get_db_size_bytes(&conn)?,
        wal_bytes,
        checkpoint,
        checkpoint_us,
        rss_kib: rss_kib(),
        cpu_seconds: cpu_elapsed,
    })
}

/// Profile production query shapes on the exact generated schema and data.
fn plans(conn: &Connection) -> Result<Vec<Plan>> {
    let board = super::threads::thread_page_sql(false)
        .replace("?1", "1")
        .replace("?2", "10")
        .replace("?3", "0")
        .replace("?4", "0");
    let catalog = super::threads::thread_page_sql(false)
        .replace("?1", "1")
        .replace("?2", "100")
        .replace("?3", "0")
        .replace("?4", "0");
    let queries = [
        ("board_index", board.as_str()),
        ("catalog", catalog.as_str()),
        ("thread_loading", "SELECT * FROM posts WHERE thread_id=1 ORDER BY created_at ASC,id ASC"),
        ("latest_post", "SELECT created_at,id FROM posts WHERE board_id=1 ORDER BY created_at DESC,id DESC LIMIT 1"),
        ("thread_bump", "UPDATE threads SET reply_count=reply_count+1,bumped_at=CASE WHEN reply_count<(SELECT bump_limit FROM boards WHERE id=1) THEN unixepoch() ELSE bumped_at END WHERE id=1 AND board_id=1 AND locked=0 AND archived=0"),
        ("admin_overview", "SELECT (SELECT COUNT(*) FROM posts),(SELECT COUNT(*) FROM threads),(SELECT COUNT(*) FROM reports WHERE status='open')"),
        ("report_lookup", "SELECT id FROM reports WHERE post_id=83 AND reporter_hash='reporter-83' AND status='open'"),
        ("backup_board_inventory", "SELECT id,file_path,thumb_path,audio_file_path FROM posts WHERE board_id=1 ORDER BY id"),
        (
            "live_poll",
            "SELECT * FROM posts WHERE board_id=1 AND thread_id=1 AND id>45 ORDER BY id LIMIT 100",
        ),
        (
            "cooldown",
            "SELECT MAX(created_at) FROM posts WHERE board_id=1 AND ip_hash='actor-1'",
        ),
        (
            "latest_thread",
            "SELECT created_at,id FROM threads WHERE board_id=1 AND archived=0 ORDER BY created_at DESC,id DESC LIMIT 1",
        ),
        (
            "prune",
            "SELECT id FROM threads WHERE board_id=1 AND archived=0 AND sticky=0 ORDER BY bumped_at DESC LIMIT -1 OFFSET 100",
        ),
        (
            "search_selective",
            "SELECT p.id FROM posts_fts CROSS JOIN posts p ON p.id=posts_fts.rowid WHERE p.board_id=1 AND posts_fts MATCH '\"needle\"*' ORDER BY p.created_at DESC,p.id DESC LIMIT 20",
        ),
        (
            "reports",
            "SELECT * FROM reports WHERE status='open' ORDER BY created_at DESC LIMIT 20",
        ),
        (
            "bans",
            "SELECT reason FROM bans WHERE ip_hash='actor-1' AND (expires_at IS NULL OR expires_at>unixepoch())",
        ),
        (
            "job_claim",
            "SELECT id FROM background_jobs WHERE status='pending' AND attempts<3 ORDER BY priority DESC,created_at LIMIT 1",
        ),
        (
            "file_refs",
            "SELECT 1 FROM posts WHERE file_path='b1/src/4.png' OR thumb_path='b1/src/4.png' OR audio_file_path='b1/src/4.png' LIMIT 1",
        ),
        (
            "dedup",
            "SELECT * FROM file_hashes WHERE sha256=printf('%064x',4)",
        ),
        (
            "mod_log",
            "SELECT * FROM mod_log ORDER BY created_at DESC LIMIT 50",
        ),
    ];
    queries
        .into_iter()
        .map(|(name, sql)| {
            let details = conn
                .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))?
                .query_map([], |r| r.get::<_, String>(3))?
                .collect::<rusqlite::Result<_>>()?;
            Ok(Plan {
                name: name.to_owned(),
                details,
            })
        })
        .collect()
}

/// Explicitly invoked benchmark; temporary databases never enter the repository.
#[test]
#[ignore = "populated offline performance matrix; run explicitly with RUSTCHAN_DB_EVIDENCE set"]
fn benchmark() -> Result<()> {
    let output = std::env::var("RUSTCHAN_DB_EVIDENCE")
        .context("set RUSTCHAN_DB_EVIDENCE to a JSON result path")?;
    let directory = tempfile::tempdir()?;
    let seed_path = directory.path().join("seed.sqlite3");
    let seed_conn = Connection::open(&seed_path)?;
    seed_conn.execute_batch(super::pool::CONNECTION_PRAGMAS)?;
    seed(&seed_conn)?;
    let query_plans = plans(&seed_conn)?;
    seed_conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")?;
    drop(seed_conn);
    let mut measurements = Vec::new();
    for sync in ["NORMAL", "FULL"] {
        for size in [2, 4, 8, 12, 16] {
            for mix in [("read_heavy", 5), ("write_burst", 70), ("mixed", 25)] {
                let path = directory
                    .path()
                    .join(format!("{sync}-{size}-{}.sqlite3", mix.0));
                std::fs::copy(&seed_path, &path)?;
                measurements.push(measure(&path, size, sync, mix)?);
                std::fs::write(
                    &output,
                    serde_json::to_vec_pretty(&serde_json::json!({
                        "sqlite_version":rusqlite::version(),"workers":WORKERS,
                        "fixture":{"boards":8,"threads":800,"posts":40000},
                        "query_plans":query_plans,"measurements":measurements,
                        "limitations":"SQL/database workload; media CPU and HTTP excluded; RSS sampled at case end; ps optional; process-kill is not power loss"
                    }))?,
                )?;
            }
        }
    }
    std::fs::write(
        output,
        serde_json::to_vec_pretty(&serde_json::json!({
            "sqlite_version": rusqlite::version(), "workers":WORKERS,
            "fixture":{"boards":8,"threads":800,"posts":40000},
            "query_plans":query_plans, "measurements":measurements,
            "limitations":"SQL/database workload; media CPU and HTTP excluded; RSS sampled at case end; ps optional; process-kill is not power loss"
        }))?,
    )?;
    Ok(())
}

/// Child process guard prevents a failing parent test from leaving a writer alive.
#[derive(Debug)]
struct CrashChild(std::process::Child);

impl Drop for CrashChild {
    fn drop(&mut self) {
        drop(self.0.kill());
        drop(self.0.wait());
    }
}

/// Child-only workload; parent kills it with an open WAL write transaction.
#[test]
#[ignore = "invoked only by the crash/restart parent regression"]
fn crash_writer_child() -> Result<()> {
    let path = std::path::PathBuf::from(std::env::var("RUSTCHAN_DB_CRASH_PATH")?);
    ensure!(!path.exists(), "crash fixture must be a fresh database");
    let conn = Connection::open(&path)?;
    conn.execute_batch(super::pool::CONNECTION_PRAGMAS)?;
    conn.pragma_update(
        None,
        "synchronous",
        std::env::var("RUSTCHAN_DB_CRASH_SYNC")?,
    )?;
    super::schema::install_or_migrate_schema(&conn)?;
    conn.execute_batch(
        r#"BEGIN IMMEDIATE;
         INSERT INTO boards(id,short_name,name) VALUES(1,'crash','Crash');
         INSERT INTO threads(id,board_id) VALUES(1,1);
         INSERT INTO posts(thread_id,board_id,body,body_html,deletion_token,is_op)
         VALUES(1,1,'op','op','delete',1);
         INSERT INTO background_jobs(job_type,payload,status,attempts)
         VALUES('spam_check','{}','running',1);
         INSERT INTO pending_fs_ops(id,kind,payload_json)
         VALUES('crash-delete','delete_files','{"paths":["crash/src/remove.txt"],"dirs":[]}');
         COMMIT;"#,
    )?;
    let root = path
        .parent()
        .context("crash fixture parent")?
        .join("uploads");
    std::fs::create_dir_all(root.join("crash/src"))?;
    std::fs::write(root.join("crash/src/remove.txt"), b"recoverable cleanup")?;
    // The readiness marker is created only inside a later write transaction,
    // after committed data, FTS content, a job and a filesystem intent exist.
    for number in 0_u64.. {
        conn.execute_batch("BEGIN IMMEDIATE")?;
        conn.execute(
            "INSERT INTO posts(thread_id,board_id,body,body_html,deletion_token)
             VALUES(1,1,'crash reply','crash reply','delete')",
            [],
        )?;
        conn.execute(
            "UPDATE threads SET reply_count=reply_count+1 WHERE id=1",
            [],
        )?;
        if number == 100 {
            std::fs::write(path.with_extension("ready"), b"open transaction")?;
            // Give the parent a deterministic open transaction to interrupt.
            std::thread::sleep(std::time::Duration::from_secs(30));
            anyhow::bail!("crash parent failed to terminate the child within its deadline");
        }
        super::commit_transaction(&conn, "commit crash workload")?;
    }
    Ok(())
}

/// Verify process-crash recovery for NORMAL and FULL without simulating power loss.
#[test]
fn abrupt_writer_termination_recovers_wal_fts_jobs_and_fs_intents() -> Result<()> {
    for synchronous in ["NORMAL", "FULL"] {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("crash.sqlite3");
        let mut child = CrashChild(
            std::process::Command::new(std::env::current_exe()?)
                .args(["--ignored", "--exact", "db::workload::crash_writer_child"])
                .env("RUSTCHAN_DB_CRASH_PATH", &path)
                .env("RUSTCHAN_DB_CRASH_SYNC", synchronous)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()?,
        );
        let deadline = Instant::now() + std::time::Duration::from_secs(20);
        while !path.with_extension("ready").exists() {
            ensure!(
                child.0.try_wait()?.is_none(),
                "crash writer exited before readiness"
            );
            ensure!(
                Instant::now() < deadline,
                "crash writer readiness timed out"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        child.0.kill()?;
        child.0.wait()?;
        let pool = pool(&path, 2, synchronous)?;
        let conn = pool.get()?;
        super::schema::install_or_migrate_schema(&conn)?;
        super::verify_database_schema(&conn)?;
        conn.execute(
            "INSERT INTO posts_fts(posts_fts,rank) VALUES('integrity-check',1)",
            [],
        )?;
        let (replies, count): (i64, i64) = conn.query_row(
            "SELECT (SELECT COUNT(*) FROM posts WHERE is_op=0),reply_count FROM threads WHERE id=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        ensure!(
            replies == 100 && count == replies,
            "crash transaction was partially committed"
        );
        ensure!(
            conn.is_autocommit(),
            "restart inherited an active transaction"
        );
        super::recover_interrupted_background_jobs(&conn)?;
        let status: String =
            conn.query_row("SELECT status FROM background_jobs WHERE id=1", [], |r| {
                r.get(0)
            })?;
        ensure!(status == "pending", "interrupted job was not recovered");
        drop(conn);
        let uploads = directory.path().join("uploads");
        crate::pending_fs::reconcile_pending_fs_ops(
            &pool,
            uploads.to_str().context("uploads path")?,
        )?;
        ensure!(
            !uploads.join("crash/src/remove.txt").exists(),
            "durable filesystem intent did not replay"
        );
        let conn = pool.get()?;
        ensure!(
            super::list_pending_fs_ops(&conn)?.is_empty(),
            "filesystem intent was not completed"
        );
        conn.execute_batch("BEGIN IMMEDIATE; ROLLBACK")?;
        super::verify_database_schema(&conn)?;
    }
    Ok(())
}

/// Time one cached query and retain IDs to prove result equivalence.
fn timed_query(conn: &Connection, sql: &str) -> Result<(Latencies, Vec<i64>)> {
    let mut timings = Vec::new();
    let mut ids = Vec::new();
    let mut statement = conn.prepare_cached(sql)?;
    for _ in 0..25 {
        let started = Instant::now();
        ids = statement
            .query_map([], |r| r.get::<_, i64>(0))?
            .collect::<rusqlite::Result<_>>()?;
        timings.push(started.elapsed().as_micros());
    }
    Ok((latencies(timings.into_iter()), ids))
}

/// Collect the actual access plan for a parameter-free profiling query.
fn explain(conn: &Connection, sql: &str) -> Result<Vec<String>> {
    let mut statement = conn.prepare(&format!("EXPLAIN QUERY PLAN {sql}"))?;
    let details = statement
        .query_map([], |r| r.get(3))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(details)
}

/// Compare measured candidate queries/indexes against their baseline counterparts.
#[test]
#[ignore = "explicit populated query and index evidence; set RUSTCHAN_DB_QUERY_EVIDENCE"]
fn query_profile() -> Result<()> {
    let output = std::env::var("RUSTCHAN_DB_QUERY_EVIDENCE")?;
    let directory = tempfile::tempdir()?;
    let conn = Connection::open(directory.path().join("profile.sqlite3"))?;
    conn.execute_batch(super::pool::CONNECTION_PRAGMAS)?;
    seed(&conn)?;
    let mut evidence = Vec::new();
    for term in ["needle", "representative", "missingword"] {
        for selection in ["p.id", "COUNT(*)"] {
            let tail = if selection == "p.id" {
                "ORDER BY p.created_at DESC,p.id DESC LIMIT 20"
            } else {
                ""
            };
            let before = format!(
                "SELECT {selection} FROM posts p JOIN posts_fts f ON f.rowid=p.id
                                  WHERE p.board_id=1 AND posts_fts MATCH '\"{term}\"*' {tail}"
            );
            let after = format!(
                "SELECT {selection} FROM posts_fts CROSS JOIN posts p ON p.id=posts_fts.rowid
                                 WHERE p.board_id=1 AND posts_fts MATCH '\"{term}\"*' {tail}"
            );
            let (before_time, before_ids) = timed_query(&conn, &before)?;
            let (after_time, after_ids) = timed_query(&conn, &after)?;
            ensure!(before_ids == after_ids, "FTS query changed results");
            evidence.push(serde_json::json!({"query":format!("fts_{selection}_{term}"),
                "before":before_time,"after":after_time,"before_plan":explain(&conn,&before)?,"after_plan":explain(&conn,&after)?}));
        }
    }
    evidence.extend(profile_search(&conn)?);
    let before_board = format!(
        "{} WHERE t.board_id=1 AND t.archived=0 GROUP BY t.id,op.id
                               ORDER BY t.sticky DESC,t.bumped_at DESC LIMIT 10",
        super::threads::THREAD_SELECT
    );
    let after_board = super::threads::thread_page_sql(false)
        .replace("?1", "1")
        .replace("?2", "10")
        .replace("?3", "0")
        .replace("?4", "0");
    let (before_time, before_ids) = timed_query(&conn, &before_board)?;
    let (after_time, after_ids) = timed_query(&conn, &after_board)?;
    ensure!(before_ids == after_ids, "board page changed results");
    evidence.push(
        serde_json::json!({"query":"board_page","before":before_time,"after":after_time,
        "before_plan":explain(&conn,&before_board)?,"after_plan":explain(&conn,&after_board)?}),
    );
    // Reconstruct the pre-index fixture, including reclaiming dropped pages,
    // before measuring index storage and write cost.
    conn.execute_batch(
        "DROP INDEX IF EXISTS idx_posts_thread_live;
                        DROP INDEX IF EXISTS idx_posts_board_ip_created;
                        VACUUM;",
    )?;
    let queries = [
        (
            "live_poll",
            "SELECT id FROM posts WHERE board_id=1 AND thread_id=1 AND id>45 ORDER BY id LIMIT 100",
        ),
        (
            "cooldown",
            "SELECT MAX(created_at) FROM posts WHERE board_id=1 AND ip_hash='actor-1'",
        ),
    ];
    let before = queries
        .iter()
        .map(|(_, sql)| Ok((timed_query(&conn, sql)?, explain(&conn, sql)?)))
        .collect::<Result<Vec<_>>>()?;
    let write_before = profile_writes(&conn, 0)?;
    let size_before = super::get_db_size_bytes(&conn)?;
    conn.execute_batch("CREATE INDEX idx_posts_thread_live ON posts(thread_id,id);
                        CREATE INDEX idx_posts_board_ip_created ON posts(board_id,ip_hash,created_at DESC);")?;
    let size_after = super::get_db_size_bytes(&conn)?;
    for ((name, sql), ((before_time, _), before_plan)) in queries.into_iter().zip(before) {
        let (after_time, _) = timed_query(&conn, sql)?;
        evidence.push(
            serde_json::json!({"query":name,"before":before_time,"after":after_time,
            "before_plan":before_plan,"after_plan":explain(&conn,sql)?}),
        );
    }
    let write_after = profile_writes(&conn, 100)?;
    std::fs::write(
        output,
        serde_json::to_vec_pretty(&serde_json::json!({"sqlite":rusqlite::version(),
        "queries":evidence,"write_before":write_before,"write_after":write_after,
        "database_bytes_before":size_before,"database_bytes_after":size_after}))?,
    )?;
    Ok(())
}

/// Compare full post projections using the actual sanitized prefix syntax.
fn profile_search(conn: &Connection) -> Result<Vec<serde_json::Value>> {
    let mut evidence = Vec::new();
    for term in ["needle", "representative", "missingword"] {
        let before = format!("SELECT p.* FROM posts p JOIN posts_fts f ON f.rowid=p.id
            WHERE p.board_id=1 AND posts_fts MATCH '\"{term}\"*' ORDER BY p.created_at DESC,p.id DESC LIMIT 20");
        let (before_time, ids) = timed_query(conn, &before)?;
        let mut times = Vec::new();
        for _ in 0..25 {
            let started = Instant::now();
            let posts = super::search_posts(conn, 1, term, 20, 0)?;
            times.push(started.elapsed().as_micros());
            ensure!(
                ids == posts.iter().map(|post| post.id).collect::<Vec<_>>(),
                "production FTS results changed"
            );
        }
        evidence.push(
            serde_json::json!({"query":format!("production_fts_{term}"),"before":before_time,
            "after":latencies(times.into_iter())}),
        );
    }
    Ok(evidence)
}

/// Measure representative write cost before and after candidate indexes.
fn profile_writes(conn: &Connection, first: u32) -> Result<Latencies> {
    let mut timings = Vec::new();
    for operation in first..first + 100 {
        let started = Instant::now();
        conn.execute_batch("BEGIN IMMEDIATE")?;
        write(conn, 0, operation)?;
        super::commit_transaction(conn, "profile writes")?;
        timings.push(started.elapsed().as_micros());
    }
    Ok(latencies(timings.into_iter()))
}
