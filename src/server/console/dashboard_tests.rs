//! Regression tests for dashboard behavior.

use super::*;

#[test]
#[expect(
    clippy::panic_in_result_fn,
    reason = "assertion failures are the intended failure mechanism for this test"
)]
fn every_representative_terminal_size_renders_without_overflow() -> anyhow::Result<()> {
    let app = ConsoleState::default();
    let metrics = ChanStats {
        is_ready: true,
        boards: 12,
        threads: 3_456,
        posts: 987_654,
        ..ChanStats::default()
    };
    let logs = LogSnapshot::default();

    for (width, height) in [(40, 10), (44, 14), (60, 18), (80, 24), (120, 40)] {
        let buffer = render_to_buffer(Rect::new(0, 0, width, height), &app, &metrics, &logs)?;
        assert_eq!(
            buffer.area.width, width,
            "rendered width should match backend"
        );
        assert_eq!(
            buffer.area.height, height,
            "rendered height should match backend"
        );
    }
    Ok(())
}

#[test]
#[expect(
    clippy::panic_in_result_fn,
    reason = "assertion failures are the intended failure mechanism for this test"
)]
fn overview_preserves_primary_information_hierarchy() -> anyhow::Result<()> {
    let app = ConsoleState::default();
    let metrics = ChanStats {
        is_ready: true,
        boards: 4,
        threads: 25,
        posts: 1_200,
        ..ChanStats::default()
    };

    let buffer = render_to_buffer(
        Rect::new(0, 0, 120, 32),
        &app,
        &metrics,
        &LogSnapshot::default(),
    )?;
    let text = buffer_text(&buffer);

    assert!(
        text.contains("RUSTCHAN"),
        "header should retain product identity"
    );
    assert!(
        text.contains("Service & access"),
        "transport health should have a clear panel"
    );
    assert!(
        text.contains("Operations"),
        "operator metrics should have a clear panel"
    );
    assert!(
        text.contains("Content"),
        "content metrics should remain grouped"
    );
    assert!(
        text.contains("1,200 posts"),
        "large counts should remain scannable"
    );
    Ok(())
}

#[test]
#[expect(
    clippy::panic_in_result_fn,
    reason = "assertion failures are the intended failure mechanism for this test"
)]
fn form_dialog_exposes_masking_help_and_submission_controls() -> anyhow::Result<()> {
    let app = ConsoleState {
        dialog: Some(Dialog::Form(FormState::new(
            super::super::state::FormKind::CreateAdmin,
        ))),
        ..ConsoleState::default()
    };

    let buffer = render_to_buffer(
        Rect::new(0, 0, 90, 28),
        &app,
        &ChanStats::default(),
        &LogSnapshot::default(),
    )?;
    let text = buffer_text(&buffer);

    assert!(
        text.contains("Create administrator"),
        "form should name its action"
    );
    assert!(
        text.contains("Password"),
        "password fields should be discoverable"
    );
    assert!(
        text.contains("Credentials are masked"),
        "the form should explain password masking"
    );
    assert!(
        text.contains("Ctrl-Enter / F2"),
        "submission shortcut should be visible"
    );
    Ok(())
}

#[test]
#[expect(
    clippy::panic_in_result_fn,
    reason = "assertion failures are the intended failure mechanism for this test"
)]
fn narrow_terminal_uses_explicit_resize_state() -> anyhow::Result<()> {
    let buffer = render_to_buffer(
        Rect::new(0, 0, 40, 10),
        &ConsoleState::default(),
        &ChanStats::default(),
        &LogSnapshot::default(),
    )?;
    let text = buffer_text(&buffer);

    assert!(
        text.contains("Terminal too small"),
        "undersized terminals should receive an actionable state"
    );
    assert!(
        text.contains("40 × 10"),
        "current dimensions should be visible"
    );
    Ok(())
}

#[test]
#[expect(
    clippy::panic_in_result_fn,
    reason = "assertion failures are the intended failure mechanism for this test"
)]
fn log_selection_ignores_dependency_log() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    std::fs::write(directory.path().join("rustchan.2026-04-01.log"), "main")?;
    std::fs::write(
        directory
            .path()
            .join(crate::logging::DEPENDENCY_LOG_FILE_NAME),
        "dependency",
    )?;

    let latest =
        latest_log_file(directory.path()).ok_or_else(|| anyhow::anyhow!("main log not found"))?;

    assert_eq!(
        latest.file_name().and_then(|name| name.to_str()),
        Some("rustchan.2026-04-01.log"),
        "main-process log should win"
    );
    Ok(())
}
#[test]
#[expect(
    clippy::panic_in_result_fn,
    reason = "assertions verify rendered controls and cursor bounds"
)]
fn every_screen_and_dialog_survives_resizing_and_masks_secrets() -> anyhow::Result<()> {
    use super::super::state::FormKind;
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24))?;
    let mut password = FormState::new(FormKind::CreateAdmin);
    if let Some(field) = password.fields.get_mut(1) {
        field.value = FieldValue::Text("Secret界🦀123".to_owned());
    }
    password.focused = 1;
    password.cursor = 11;
    let dialogs = [
        None,
        Some(Dialog::Form(password)),
        Some(Dialog::Form(FormState::new(FormKind::CreateBoard))),
        Some(Dialog::ConfirmDelete {
            thread_id: i64::MAX,
        }),
        Some(Dialog::ConfirmQuit),
        Some(Dialog::Progress {
            label: "Creating board…",
        }),
    ];
    for screen in [
        Screen::Dashboard,
        Screen::Tasks,
        Screen::Boards,
        Screen::System,
        Screen::Configuration,
        Screen::Logs,
        Screen::Help,
    ] {
        for dialog in &dialogs {
            let mut app = ConsoleState {
                screen,
                dialog: dialog.clone(),
                ..ConsoleState::default()
            };
            for (width, height) in [
                (120, 40),
                (40, 10),
                (44, 14),
                (60, 18),
                (80, 24),
                (0, 0),
                (1, 100),
                (200, 1),
                (120, 40),
            ] {
                terminal.backend_mut().resize(width, height);
                terminal.draw(|frame| {
                    render(
                        frame,
                        &mut app,
                        &ChanStats::default(),
                        &LogSnapshot::default(),
                    );
                })?;
                let text = buffer_text(terminal.backend().buffer());
                assert!(
                    !text.contains("Secret"),
                    "password values must never reach the terminal buffer"
                );
                if terminal_is_usable(width, height) && matches!(dialog, Some(Dialog::Form(_))) {
                    assert!(
                        text.contains("Submit") && text.contains("Cancel"),
                        "form actions must be reachable at {width}x{height}: {text}"
                    );
                    let cursor = terminal.get_cursor_position()?;
                    assert!(
                        cursor.x < width && cursor.y < height,
                        "cursor must stay inside the terminal"
                    );
                }
            }
        }
    }
    Ok(())
}

#[test]
#[expect(
    clippy::panic_in_result_fn,
    reason = "assertions verify scrollable panel content"
)]
fn short_viewports_can_reach_the_last_overview_and_help_rows() -> anyhow::Result<()> {
    for (width, height) in [(44, 14), (60, 18), (80, 24), (120, 40)] {
        for (screen, expected) in [
            (Screen::Dashboard, "Recent traffic"),
            (Screen::Help, "Ctrl-C"),
        ] {
            let app = ConsoleState {
                screen,
                overview_scroll: u16::MAX,
                help_scroll: u16::MAX,
                ..ConsoleState::default()
            };
            let buffer = render_to_buffer(
                Rect::new(0, 0, width, height),
                &app,
                &ChanStats {
                    is_ready: true,
                    ..ChanStats::default()
                },
                &LogSnapshot::default(),
            )?;
            assert!(
                buffer_text(&buffer).contains(expected),
                "End must expose {expected} at {width}x{height}"
            );
        }
    }
    Ok(())
}

#[test]
#[expect(
    clippy::panic_in_result_fn,
    reason = "assertions verify field scroll and glyph/cursor alignment"
)]
fn form_scroll_uses_only_the_focused_field_and_whole_wide_glyphs() -> anyhow::Result<()> {
    let mut form = FormState::new(super::super::state::FormKind::CreateBoard);
    let field = form
        .fields
        .get_mut(1)
        .ok_or_else(|| anyhow::anyhow!("missing display-name field"))?;
    field.value = FieldValue::Text("界界界界x".to_owned());
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(28, 3))?;
    let mut cursor = None;
    terminal.draw(|frame| {
        cursor = render_form_field(frame, Rect::new(0, 0, 28, 1), field, true, 5);
    })?;
    let cursor = cursor.ok_or_else(|| anyhow::anyhow!("missing focused cursor"))?;
    assert_eq!(
        terminal
            .backend()
            .buffer()
            .cell((cursor.x - 1, cursor.y))
            .map(ratatui::buffer::Cell::symbol),
        Some("x"),
        "cursor must follow the last glyph after horizontal scrolling"
    );
    field.value = FieldValue::Text("abcdef".to_owned());
    terminal.draw(|frame| {
        render_form_field(frame, Rect::new(0, 0, 25, 1), field, false, 80);
    })?;
    assert!(
        buffer_text(terminal.backend().buffer()).contains("abcd"),
        "an unfocused field must show its prefix independently of the active cursor"
    );
    Ok(())
}

#[test]
#[expect(
    clippy::panic_in_result_fn,
    reason = "assertions verify log scroll bounds after resize and rotation"
)]
fn log_scroll_clamps_to_retained_content() -> anyhow::Result<()> {
    let mut app = ConsoleState {
        screen: Screen::Logs,
        ..ConsoleState::default()
    };
    app.logs.follow = false;
    app.logs.rows_from_bottom = usize::MAX;
    app.logs.horizontal_offset = u16::MAX;
    let logs = LogSnapshot {
        lines: (0..20).map(|index| format!("entry-{index:02}")).collect(),
        ..LogSnapshot::default()
    };
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24))?;
    terminal.draw(|frame| render(frame, &mut app, &ChanStats::default(), &logs))?;
    assert_eq!(
        app.logs.rows_from_bottom,
        logs.lines.len() - app.logs.visible_rows,
        "scroll state must clamp, not only its rendered projection"
    );
    assert_eq!(
        app.logs.horizontal_offset, 0,
        "short lines must not leave a blank panned viewport"
    );
    assert!(
        buffer_text(terminal.backend().buffer()).contains("entry-00"),
        "oldest retained content must remain visible"
    );
    app.handle_key(&super::super::input::KeyEvent::Down, 0, (80, 24));
    terminal.draw(|frame| render(frame, &mut app, &ChanStats::default(), &logs))?;
    assert!(
        buffer_text(terminal.backend().buffer()).contains("entry-01"),
        "one Down press must move after excessive upward scrolling"
    );
    Ok(())
}
#[test]
#[expect(
    clippy::panic_in_result_fn,
    reason = "assertions verify active log selection across fallback/rotation"
)]
fn newer_rotated_log_wins_over_stale_fallback_file() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let fallback = directory.path().join("rustchan.log");
    let current = directory.path().join("rustchan.2026-09-02.log");
    std::fs::write(&fallback, "stale")?;
    std::fs::File::open(&fallback)?.set_modified(std::time::SystemTime::UNIX_EPOCH)?;
    std::fs::write(&current, "live")?;
    std::fs::create_dir(directory.path().join("rustchan.zzz.log"))?;
    assert_eq!(
        latest_log_file(directory.path()),
        Some(current),
        "file timestamps must select the live log even when fallback sorts last"
    );
    Ok(())
}
#[test]
fn service_endpoints_use_the_runtime_port_override() {
    let metrics = ChanStats {
        http_port: 43210,
        ..ChanStats::default()
    };
    if !CONFIG.tls.enabled {
        let displayed = service_rows(&metrics)
            .into_iter()
            .map(|(_, value)| value.to_string())
            .collect::<String>();
        assert!(
            displayed.contains("http://localhost:43210"),
            "service rendering must advertise the bound listener port"
        );
    }
    assert!(
        backend_address(metrics.http_port).ends_with(":43210"),
        "Tor backend details must use the same effective port"
    );
}
#[test]
#[expect(
    clippy::panic_in_result_fn,
    reason = "assertions verify complete validation feedback on narrow terminals"
)]
fn narrow_form_preserves_the_full_validation_rule_and_actions() -> anyhow::Result<()> {
    let mut app = ConsoleState {
        dialog: Some(Dialog::Form(FormState::new(
            super::super::state::FormKind::CreateAdmin,
        ))),
        ..ConsoleState::default()
    };
    app.handle_key(&super::super::input::KeyEvent::Submit, 0, (44, 14));
    let buffer = render_to_buffer(
        Rect::new(0, 0, 44, 14),
        &app,
        &ChanStats::default(),
        &LogSnapshot::default(),
    )?;
    let text = buffer_text(&buffer);
    assert!(
        text.contains("dashes."),
        "validation rules must not lose their final line: {text}"
    );
    assert!(
        text.contains("Submit") && text.contains("Cancel"),
        "validation must not displace form actions"
    );
    Ok(())
}
#[test]
fn populated_tasks_empty_filters_errors_and_details_render_at_all_sizes() -> anyhow::Result<()> {
    let mut stats = ChanStats {
        is_ready: true,
        ..ChanStats::default()
    };
    stats.operator.jobs = Some(crate::db::BackgroundJobSummary {
        running: 1,
        queued: 2,
        failed: 1,
        recent_completed: 3,
    });
    for index in 0..100 {
        stats.operator.tasks.push(super::super::telemetry::TaskRow {
            id: format!("job:{index}"),
            kind: "Video transcode".to_owned(),
            item: format!("Post #{index} · {}", "界-long-name".repeat(20)),
            state: super::super::telemetry::TaskState::Running,
            attempts: Some(2),
            created_at: Some(10),
            changed_at: Some(20),
            detail: "No percentage or ETA published".to_owned(),
        });
    }
    for (width, height) in [(44, 14), (60, 18), (80, 24), (120, 40)] {
        let mut app = ConsoleState {
            screen: Screen::Tasks,
            ..ConsoleState::default()
        };
        app.reconcile_data(&stats);
        let buffer = render_to_buffer(
            Rect::new(0, 0, width, height),
            &app,
            &stats,
            &LogSnapshot::default(),
        )?;
        anyhow::ensure!(
            buffer_text(&buffer).contains("RUNNING"),
            "populated task state disappeared at {width}x{height}"
        );
        app.handle_key(&super::super::input::KeyEvent::Enter, 0, (width, height));
        let buffer = render_to_buffer(
            Rect::new(0, 0, width, height),
            &app,
            &stats,
            &LogSnapshot::default(),
        )?;
        anyhow::ensure!(
            buffer_text(&buffer).contains("State: RUNNING"),
            "task detail did not render state"
        );
        app.dialog = None;
        app.task_filter.query = "no-match".to_owned();
        let buffer = render_to_buffer(
            Rect::new(0, 0, width, height),
            &app,
            &stats,
            &LogSnapshot::default(),
        )?;
        anyhow::ensure!(
            buffer_text(&buffer).contains("NO TASKS"),
            "empty filter must be explained"
        );
        stats.operator.error = Some("Reading tasks failed; R retries".to_owned());
        let buffer = render_to_buffer(
            Rect::new(0, 0, width, height),
            &app,
            &stats,
            &LogSnapshot::default(),
        )?;
        anyhow::ensure!(
            buffer_text(&buffer).contains("UNAVAILABLE"),
            "failed data must be explicit"
        );
        stats.operator.error = None;
    }
    Ok(())
}

#[test]
fn narrow_header_retains_product_version_and_help() -> anyhow::Result<()> {
    let buffer = render_to_buffer(
        Rect::new(0, 0, 44, 14),
        &ConsoleState::default(),
        &ChanStats::default(),
        &LogSnapshot::default(),
    )?;
    let text = buffer_text(&buffer);
    anyhow::ensure!(
        text.contains(env!("CARGO_PKG_VERSION")) && text.contains("?Help"),
        "narrow header must retain full version and discoverable help"
    );
    Ok(())
}

#[test]
fn medium_header_and_footer_keep_complete_uptime_and_key_pairs() -> anyhow::Result<()> {
    let buffer = render_to_buffer(
        Rect::new(0, 0, 80, 24),
        &ConsoleState::default(),
        &ChanStats::default(),
        &LogSnapshot::default(),
    )?;
    let text = buffer_text(&buffer);
    anyhow::ensure!(
        text.contains("Up 0h 00m"),
        "uptime must fit without clipping at normal terminal widths"
    );
    anyhow::ensure!(
        text.contains("Q  Quit"),
        "footer must wrap whole key/label pairs"
    );
    Ok(())
}
