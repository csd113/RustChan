//! Small synthetic regressions for confirmed console presentation failures.
use super::{
    dashboard,
    input::KeyEvent,
    state::{ConsoleState, Screen},
    stats::ChanStats,
    telemetry::OperatorSnapshot,
    widgets,
};
use ratatui::{backend::TestBackend, Terminal};
use std::time::{Duration, Instant};

#[test]
fn media_failure_row_is_reachable_at_standard_sizes() -> anyhow::Result<()> {
    let stats = ChanStats {
        is_ready: true,
        sampled_at: Some(Instant::now()),
        operator: OperatorSnapshot {
            jobs: Some(crate::db::BackgroundJobSummary {
                running: 0,
                queued: 0,
                failed: 0,
                recent_completed: 0,
            }),
            media_failed: 731,
            ..OperatorSnapshot::default()
        },
        ..ChanStats::default()
    };
    for (width, height) in [(44, 14), (80, 24), (99, 24), (100, 24)] {
        let mut terminal = Terminal::new(TestBackend::new(width, height))?;
        let mut found = false;
        for offset in 0..150 {
            let mut app = ConsoleState {
                overview_scroll: offset,
                ..ConsoleState::default()
            };
            let _frame = terminal.draw(|frame| {
                dashboard::render(frame, &mut app, &stats, &dashboard::LogSnapshot::default());
            })?;
            let text: String = terminal
                .backend()
                .buffer()
                .content
                .iter()
                .map(ratatui::buffer::Cell::symbol)
                .collect();
            found |= text.contains("0 pending") && text.contains("731 failed");
            if found {
                break;
            }
        }
        anyhow::ensure!(
            found,
            "media failure row must be reachable at {width}x{height}"
        );
    }
    Ok(())
}

#[test]
fn help_origin_and_stale_failures_remain_explicit() {
    let mut app = ConsoleState {
        screen: Screen::Tasks,
        ..ConsoleState::default()
    };
    for key in [
        KeyEvent::Character('H'),
        KeyEvent::Character('H'),
        KeyEvent::Character('H'),
        KeyEvent::Escape,
    ] {
        let _action = app.handle_key(&key, 0, (80, 24));
    }
    assert_eq!(
        app.screen,
        Screen::Tasks,
        "repeated uppercase help must preserve its origin"
    );
    let stats = ChanStats {
        is_ready: true,
        sampled_at: Instant::now().checked_sub(Duration::from_secs(20)),
        collection_error: Some("failed".to_owned()),
        ..ChanStats::default()
    };
    assert_eq!(
        widgets::stats_freshness(&stats).label,
        "[STALE]",
        "health failure must not conceal stale data"
    );
}
