//! Opt-in HTTP thumbnail workload using the production handler and file service.

use anyhow::{ensure, Context as _, Result};
use axum::{routing::get, Router};
use futures::{stream, StreamExt as _, TryStreamExt as _};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};
use std::time::Instant;

/// Counts pooled checkouts during measured requests, excluding fixture creation.
#[derive(Clone, Debug, Default)]
struct CheckoutProbe(Arc<AtomicU64>);

impl r2d2::HandleEvent for CheckoutProbe {
    fn handle_checkout(&self, _event: r2d2::event::CheckoutEvent) {
        let _previous = self.0.fetch_add(1, Ordering::Relaxed);
    }
}

/// Summarizes complete HTTP response times without imposing noisy time limits.
fn latencies(mut values: Vec<u128>) -> serde_json::Value {
    values.sort_unstable();
    let pick = |percent: usize| {
        values
            .get(
                values
                    .len()
                    .saturating_mul(percent)
                    .div_ceil(100)
                    .saturating_sub(1),
            )
            .copied()
            .unwrap_or(0)
    };
    serde_json::json!({"p50_us":pick(50), "p95_us":pick(95), "max_us":values.last().copied().unwrap_or(0)})
}

/// Samples process observations; CPU is cumulative and RSS is an end-of-batch sample.
fn process_observation() -> Result<String> {
    let output = std::process::Command::new("ps")
        .args(["-o", "time=,rss=", "-p", &std::process::id().to_string()])
        .output()?;
    ensure!(output.status.success(), "ps failed");
    Ok(String::from_utf8(output.stdout)?.trim().to_owned())
}

/// Sends a bounded concurrent batch through actual loopback HTTP connections.
async fn batch(
    client: &reqwest::Client,
    base: &str,
    cookie: bool,
    modified: Option<&str>,
    probe: &CheckoutProbe,
) -> Result<serde_json::Value> {
    probe.0.store(0, Ordering::Relaxed);
    let started = Instant::now();
    let results: Vec<_> = stream::iter(0_u32..128)
        .map(|number| async move {
            let request_started = Instant::now();
            let mut request = client.get(format!("{base}/thumbs/{number}.webp"));
            if cookie {
                request = request.header(
                    reqwest::header::COOKIE,
                    format!("{}=benchmark-invalid", super::ADMIN_SESSION_COOKIE),
                );
            }
            if let Some(modified) = modified {
                request = request.header(reqwest::header::IF_MODIFIED_SINCE, modified);
            }
            let response = request.send().await?;
            let expected = if modified.is_some() {
                reqwest::StatusCode::NOT_MODIFIED
            } else {
                reqwest::StatusCode::OK
            };
            ensure!(
                response.status() == expected,
                "unexpected thumbnail status {}",
                response.status()
            );
            ensure!(
                response
                    .headers()
                    .get(reqwest::header::CACHE_CONTROL)
                    .and_then(|value| value.to_str().ok())
                    == Some(crate::cache::CACHE_CONTROL_IMMUTABLE_MEDIA),
                "immutable cache policy changed"
            );
            let bytes = response.bytes().await?.len();
            Ok::<_, anyhow::Error>((request_started.elapsed().as_micros(), bytes))
        })
        .buffer_unordered(16)
        .try_collect()
        .await?;
    Ok(serde_json::json!({
        "requests":results.len(), "concurrency":16_u32, "elapsed_us":started.elapsed().as_micros(),
        "body_bytes":results.iter().map(|entry| entry.1).sum::<usize>(),
        "latency":latencies(results.iter().map(|entry| entry.0).collect()),
        "database_checkouts":probe.0.load(Ordering::Relaxed),
        "process_cpu_time_and_rss_kib":process_observation()?,
    }))
}

#[tokio::test]
#[ignore = "explicit 128-thumbnail HTTP benchmark; set RUSTCHAN_THUMB_EVIDENCE"]
async fn thumbnail_http_workload() -> Result<()> {
    // Normal server startup installs this; isolated handler tests bypass startup.
    drop(rustls::crypto::ring::default_provider().install_default());
    let output = std::env::var("RUSTCHAN_THUMB_EVIDENCE")?;
    let mut state = crate::test_support::app_state();
    let database = state
        .db
        .get()?
        .path()
        .context("test database path")?
        .to_owned();
    let probe = CheckoutProbe::default();
    state.db = r2d2::Pool::builder()
        .max_size(8)
        .event_handler(Box::new(probe.clone()))
        .build(r2d2_sqlite::SqliteConnectionManager::file(database))?;
    let directory = tempfile::Builder::new()
        .prefix("t")
        .tempdir_in(&crate::config::CONFIG.upload_dir)?;
    let board = directory
        .path()
        .file_name()
        .and_then(|name| name.to_str())
        .context("board directory name")?;
    crate::db::create_board(&*state.db.get()?, board, "Thumbnails", "", false).map(|_id| ())?;
    let thumbs = directory.path().join("thumbs");
    std::fs::create_dir(&thumbs)?;
    let original = directory.path().join("source.png");
    image::RgbImage::from_fn(1024, 768, |x, y| {
        image::Rgb([
            u8::try_from(x % 256).unwrap_or(0),
            u8::try_from(y % 256).unwrap_or(0),
            80,
        ])
    })
    .save(&original)?;
    let generated = crate::media::thumbnail::generate_thumbnail(
        &original,
        "image/png",
        &thumbs.join("0.webp"),
        250,
        false,
    )?;
    for number in 1_u32..128 {
        std::fs::copy(&generated, thumbs.join(format!("{number}.webp"))).map(|_bytes| ())?;
    }
    // Delivery must succeed after the original disappears: no request-time resize.
    std::fs::remove_file(original)?;
    let router = Router::new()
        .route("/boards/{*media_path}", get(super::serve_board_media))
        .with_state(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let base = format!("http://{}/boards/{board}", listener.local_addr()?);
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        axum::serve(listener, router)
            .with_graceful_shutdown(async {
                drop(stopped.await);
            })
            .await
    });
    let result = async {
        let client = reqwest::Client::new();
        let response = client.get(format!("{base}/thumbs/127.webp")).send().await?;
        let modified = response.headers().get(reqwest::header::LAST_MODIFIED).context("Last-Modified missing")?.to_str()?.to_owned();
        drop(response.bytes().await?);
        let mut samples = Vec::new();
        for _sample in 0_u32..5 {
            for cookie in [false, true] {
                for conditional in [false, true] {
                    let measured = batch(&client, &base, cookie, conditional.then_some(modified.as_str()), &probe).await?;
                    samples.push(serde_json::json!({"admin_cookie":cookie, "conditional":conditional, "measurement":measured}));
                }
            }
        }
        std::fs::write(output, serde_json::to_vec_pretty(&serde_json::json!({
            "thumbnail_bytes":std::fs::metadata(generated)?.len(), "samples":samples,
            "limitations":"Debug build, loopback HTTP/1.1, warm OS cache, 16 concurrent requests, current-thread runtime; middleware/browser cache excluded. DB checkouts measured; SQL count and syscall count require separate instrumentation. ps CPU cumulative; RSS sampled, not peak. Original removed before requests."
        }))?)?;
        Ok::<_, anyhow::Error>(())
    }.await;
    stop.send(())
        .map_err(|()| anyhow::anyhow!("benchmark server stopped early"))?;
    server.await.context("join benchmark server")??;
    result
}
