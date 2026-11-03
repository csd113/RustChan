//! Warning-count regressions through the production routing layer.

use super::events::TorDescriptorTimeoutDecision;
use super::{OutputLayer, TorDescriptorTimeoutLimiter};
use anyhow::{Context as _, Result};
use std::io;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing::Dispatch;
use tracing_subscriber::{fmt::MakeWriter, layer::SubscriberExt as _, EnvFilter};

macro_rules! ensure {
    ($condition:expr_2021, $message:literal) => {
        if !$condition {
            anyhow::bail!($message);
        }
    };
}

#[derive(Clone, Default)]
struct Capture(Arc<parking_lot::Mutex<Vec<u8>>>);

impl Capture {
    fn text(&self) -> Result<String> {
        String::from_utf8(self.0.lock().clone()).context("captured log must be UTF-8")
    }
}

impl io::Write for Capture {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.lock().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for Capture {
    type Writer = Self;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

struct Fixture {
    dispatch: Dispatch,
    terminal: Capture,
    main_file: Capture,
    dependency_file: Capture,
    limiter: Arc<parking_lot::Mutex<TorDescriptorTimeoutLimiter>>,
}

impl Fixture {
    fn new() -> Self {
        let terminal = Capture::default();
        let main_file = Capture::default();
        let dependency_file = Capture::default();
        let layer = OutputLayer::new(terminal.clone(), main_file.clone(), dependency_file.clone());
        let limiter = Arc::clone(&layer.descriptor_timeouts);
        let subscriber = tracing_subscriber::registry()
            .with(layer)
            .with(EnvFilter::new("warn"));
        Self {
            dispatch: Dispatch::new(subscriber),
            terminal,
            main_file,
            dependency_file,
            limiter,
        }
    }
}

#[derive(Clone, Copy)]
enum Route {
    Dependency,
    BothApplicationOutputs,
}

fn warning(route: Route) {
    match route {
        Route::Dependency => tracing::warn!(
            target: "tor_hsservice::publish",
            attempts = 1_u64,
            "failed to upload descriptor: timeout exceeded"
        ),
        // This valid target satisfies the existing Tor and FFmpeg routing
        // predicates, exercising fanout without adding a test-only route.
        Route::BothApplicationOutputs => tracing::warn!(
            target: "tor_hsservice::ffmpeg",
            attempts = 1_u64,
            "failed to upload descriptor: timeout exceeded"
        ),
    }
}

fn check_routing(route: Route) -> Result<()> {
    let fixture = Fixture::new();
    tracing::dispatcher::with_default(&fixture.dispatch, || {
        for _ in 0_u8..3 {
            warning(route);
        }
        tracing::info!(target: "server", "filtered informational event");
    });
    ensure!(
        fixture.limiter.lock().suppressed == 2,
        "three events must count two suppressed warnings regardless of sink count"
    );
    let terminal = fixture.terminal.text()?;
    let main_file = fixture.main_file.text()?;
    let dependency_file = fixture.dependency_file.text()?;
    let emitted = match route {
        Route::Dependency => {
            ensure!(
                terminal.is_empty(),
                "dependency target stays out of the terminal"
            );
            ensure!(
                main_file.is_empty(),
                "dependency target stays out of the main log"
            );
            ensure!(
                dependency_file.contains("[onion   ] Tor: Failed"),
                "Tor rewriting and component remain intact"
            );
            vec![dependency_file]
        }
        Route::BothApplicationOutputs => {
            ensure!(
                dependency_file.is_empty(),
                "main targets stay out of dependency log"
            );
            ensure!(
                !terminal.contains("regressions.rs:"),
                "terminal output omits source suffix"
            );
            ensure!(
                main_file.contains("regressions.rs:"),
                "file warning retains source suffix"
            );
            vec![terminal, main_file]
        }
    };
    for output in emitted {
        ensure!(
            output.lines().count() == 1,
            "each enabled sink emits exactly the first warning"
        );
        ensure!(output.contains("[WARN ]"), "warning level remains visible");
        ensure!(
            output.contains("Failed to upload descriptor: timeout exceeded - attempts: 1"),
            "message and normalized fields remain intact"
        );
    }
    Ok(())
}

#[test]
fn dependency_only_warnings_are_individual_events() -> Result<()> {
    check_routing(Route::Dependency)
}

#[test]
fn two_outputs_share_one_decision_per_event() -> Result<()> {
    check_routing(Route::BothApplicationOutputs)
}

#[test]
fn concurrent_events_are_counted_once_before_output_fanout() -> Result<()> {
    for route in [Route::Dependency, Route::BothApplicationOutputs] {
        let fixture = Fixture::new();
        std::thread::scope(|scope| {
            let handles: Vec<_> = (0_u8..4)
                .map(|_| {
                    let dispatch = fixture.dispatch.clone();
                    scope.spawn(move || {
                        tracing::dispatcher::with_default(&dispatch, || {
                            for _ in 0_u8..16 {
                                warning(route);
                            }
                        });
                    })
                })
                .collect();
            for handle in handles {
                handle
                    .join()
                    .map_err(|_panic_payload| anyhow::anyhow!("logging worker panicked"))?;
            }
            Ok::<(), anyhow::Error>(())
        })?;
        ensure!(
            fixture.limiter.lock().suppressed == 63,
            "64 concurrent events must count exactly 63 suppressions"
        );
        let expected = match route {
            Route::Dependency => [0, 0, 1],
            Route::BothApplicationOutputs => [1, 1, 0],
        };
        let actual = [
            fixture.terminal.text()?.lines().count(),
            fixture.main_file.text()?.lines().count(),
            fixture.dependency_file.text()?.lines().count(),
        ];
        ensure!(
            actual == expected,
            "concurrent fanout must emit one original per enabled sink"
        );
    }
    Ok(())
}

#[test]
fn window_boundary_summarizes_exact_count_and_resets() -> Result<()> {
    let start = Instant::now();
    let window = Duration::from_secs(600);
    let mut limiter = TorDescriptorTimeoutLimiter::new(window);
    let before_boundary = (start + window)
        .checked_sub(Duration::from_nanos(1))
        .context("fixture window must exceed one nanosecond")?;
    ensure!(
        limiter.decide_once(start) == TorDescriptorTimeoutDecision::EmitOriginal,
        "first event is emitted"
    );
    ensure!(
        limiter.decide_once(before_boundary) == TorDescriptorTimeoutDecision::Suppress,
        "event immediately before boundary is suppressed"
    );
    ensure!(
        limiter.decide_once(start + window)
            == TorDescriptorTimeoutDecision::EmitSummary {
                suppressed: 1,
                window
            },
        "boundary emits prior-window count"
    );
    ensure!(
        limiter.decide_once(start + window + Duration::from_secs(1))
            == TorDescriptorTimeoutDecision::Suppress,
        "next event must not reuse summary"
    );
    ensure!(
        limiter.decide_once(start + window + window)
            == TorDescriptorTimeoutDecision::EmitSummary {
                suppressed: 1,
                window
            },
        "next window counts its own event"
    );
    ensure!(
        limiter.decide_once(start + window + window + window)
            == TorDescriptorTimeoutDecision::EmitOriginal,
        "idle window returns to an original warning"
    );
    Ok(())
}
