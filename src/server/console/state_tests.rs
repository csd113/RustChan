//! Regression tests for state behavior.

use super::*;

/// Type text into the currently focused form field.
fn type_text(form: &mut FormState, value: &str) {
    for character in value.chars() {
        form.insert_char(character);
    }
}

#[test]
fn escape_returns_to_dashboard_without_opening_quit_confirmation() {
    let mut state = ConsoleState {
        screen: Screen::Logs,
        ..ConsoleState::default()
    };

    let action = state.handle_key(&KeyEvent::Escape, 0, (80, 24));

    assert_eq!(
        action,
        ConsoleAction::None,
        "escape should not stop the server"
    );
    assert_eq!(
        state.screen,
        Screen::Dashboard,
        "escape should navigate back"
    );
    assert!(state.dialog.is_none(), "escape should not open a dialog");
}

#[test]
fn board_selection_stays_valid_when_rows_change() {
    let mut selection = BoardListState {
        selected: Some(8),
        ..BoardListState::default()
    };

    selection.reconcile(3);
    assert_eq!(
        selection.selected,
        Some(2),
        "selection should clamp to the final row"
    );

    selection.reconcile(0);
    assert_eq!(selection.selected, None, "an empty table has no selection");
}

#[test]
fn log_scrolling_disables_follow_and_end_restores_it() {
    let mut state = ConsoleState {
        screen: Screen::Logs,
        ..ConsoleState::default()
    };

    assert_eq!(
        state.handle_key(&KeyEvent::PageUp, 0, (80, 24)),
        ConsoleAction::None,
        "navigation and editing must not request server-side work"
    );
    assert!(
        !state.logs.follow,
        "manual scrolling should pause follow mode"
    );
    assert_eq!(
        state.logs.rows_from_bottom, 10,
        "page-up should move ten rows"
    );

    assert_eq!(
        state.handle_key(&KeyEvent::End, 0, (80, 24)),
        ConsoleAction::None,
        "navigation and editing must not request server-side work"
    );
    assert!(state.logs.follow, "end should resume follow mode");
    assert_eq!(
        state.logs.rows_from_bottom, 0,
        "end should jump to the newest line"
    );
}

#[test]
fn passwords_are_redacted_from_debug_output() {
    let request = OperationRequest::CreateAdmin {
        username: "operator".to_owned(),
        password: "correct-horse-battery-staple".to_owned(),
    };

    let debug = format!("{request:?}");

    assert!(
        debug.contains("<redacted>"),
        "debug output should mark redaction"
    );
    assert!(
        !debug.contains("correct-horse-battery-staple"),
        "debug output must not contain plaintext passwords"
    );
}

#[test]
fn delete_thread_uses_a_separate_destructive_confirmation() {
    let mut state = ConsoleState::default();
    assert_eq!(
        state.handle_key(&KeyEvent::Character('d'), 0, (80, 24)),
        ConsoleAction::None,
        "navigation and editing must not request server-side work"
    );
    let form_dialog = state.dialog.take();
    assert!(
        matches!(&form_dialog, Some(Dialog::Form(_))),
        "delete shortcut should open a form"
    );
    let Some(Dialog::Form(mut form)) = form_dialog else {
        return;
    };
    type_text(&mut form, "42");
    state.dialog = Some(Dialog::Form(form));

    let action = state.handle_key(&KeyEvent::Submit, 0, (80, 24));

    assert_eq!(
        action,
        ConsoleAction::None,
        "confirmation should precede deletion"
    );
    assert_eq!(
        state.dialog,
        Some(Dialog::ConfirmDelete { thread_id: 42 }),
        "validated deletion should open the destructive confirmation"
    );
}

#[test]
fn create_admin_rejects_mismatched_passwords() {
    let mut form = FormState::new(FormKind::CreateAdmin);
    type_text(&mut form, "operator");
    form.move_focus(false);
    type_text(&mut form, "password-one");
    form.move_focus(false);
    type_text(&mut form, "password-two");

    let result = form.request();

    assert_eq!(result, Err("Passwords do not match.".to_owned()));
}
#[test]
fn hidden_dialogs_reject_input_but_allow_ctrl_c() {
    let mut state = ConsoleState {
        dialog: Some(Dialog::ConfirmDelete { thread_id: 42 }),
        ..ConsoleState::default()
    };
    for size in [(40, 10), (120, 2), (1, 100)] {
        assert_eq!(
            state.handle_key(&KeyEvent::Character('y'), 0, size),
            ConsoleAction::None,
            "a hidden confirmation must not delete"
        );
        assert_eq!(
            state.dialog,
            Some(Dialog::ConfirmDelete { thread_id: 42 }),
            "resize must preserve the pending identity"
        );
    }
    assert_eq!(
        state.handle_key(&KeyEvent::ForceQuit, 0, (0, 0)),
        ConsoleAction::Shutdown { forced: true },
        "Ctrl-C must work at every size"
    );
}

#[test]
fn repeated_enter_cannot_accept_destructive_confirmation() {
    let mut state = ConsoleState::default();
    for key in [
        KeyEvent::Character('d'),
        KeyEvent::Paste("42".to_owned()),
        KeyEvent::Enter,
        KeyEvent::Enter,
        KeyEvent::RepeatCharacter('y'),
    ] {
        assert_eq!(
            state.handle_key(&key, 0, (80, 24)),
            ConsoleAction::None,
            "submission must wait for an explicit confirmation key"
        );
    }
    let request = OperationRequest::DeleteThread { thread_id: 42 };
    assert_eq!(
        state.handle_key(&KeyEvent::Character('y'), 0, (80, 24)),
        ConsoleAction::Submit(request),
        "only the confirmed thread should be submitted"
    );
    for key in [
        KeyEvent::Enter,
        KeyEvent::Character('y'),
        KeyEvent::Character('d'),
        KeyEvent::Escape,
    ] {
        assert_eq!(
            state.handle_key(&key, 0, (80, 24)),
            ConsoleAction::None,
            "progress must prevent duplicate operations"
        );
        assert!(
            matches!(state.dialog, Some(Dialog::Progress { label: _ })),
            "in-flight operation must retain its progress state"
        );
    }
}

#[test]
fn board_selection_tracks_identity_and_page_height() {
    let mut boards = BoardListState {
        selected: Some(1),
        visible_rows: 3,
        ..BoardListState::default()
    };
    boards.reconcile_rows(&[("b".to_owned(), 0, 0), ("c".to_owned(), 0, 0)]);
    boards.reconcile_rows(&[
        ("a".to_owned(), 0, 0),
        ("b".to_owned(), 0, 0),
        ("c".to_owned(), 0, 0),
    ]);
    assert_eq!(
        boards.selected,
        Some(2),
        "inserting a board must not change the selected board"
    );
    boards.page_by(-1, 3);
    assert_eq!(
        boards.selected,
        Some(0),
        "page navigation must use the visible height"
    );
    boards.reconcile_rows(&[]);
    assert!(
        boards.selected.is_none() && boards.selected_short.is_none(),
        "empty snapshots must clear selection identity"
    );
}

#[test]
fn form_editing_preserves_unicode_and_limits_large_paste() {
    let mut form = FormState::new(FormKind::CreateBoard);
    form.move_focus(false);
    form.insert_paste("a界e\u{301}🦀");
    form.move_cursor(false);
    form.backspace();
    form.delete();
    assert_eq!(
        form.text(FormFieldId::BoardName),
        Ok("a界e"),
        "editing must remove complete Unicode scalar values"
    );
    form.move_cursor_to_edge(false);
    form.insert_paste("Z\r\n");
    assert_eq!(
        form.text(FormFieldId::BoardName),
        Ok("Za界e"),
        "paste must not inject terminal controls"
    );
    form.clear_text();
    form.insert_paste(&"界".repeat(100_000));
    assert_eq!(
        form.text(FormFieldId::BoardName)
            .map(|value| value.chars().count()),
        Ok(80),
        "large paste must respect the field limit"
    );
    assert!(
        form.error.is_some(),
        "overflow must report the length limit"
    );
}

#[test]
fn form_focus_and_repeat_keys_do_not_trigger_global_actions() {
    let mut state = ConsoleState::default();
    assert_eq!(
        state.handle_key(&KeyEvent::RepeatCharacter('a'), 0, (80, 24)),
        ConsoleAction::None,
        "navigation and editing must not request server-side work"
    );
    assert!(
        state.dialog.is_none(),
        "a held action key must not reopen forms"
    );
    assert_eq!(
        state.handle_key(&KeyEvent::Character('a'), 0, (80, 24)),
        ConsoleAction::None,
        "navigation and editing must not request server-side work"
    );
    for key in [
        KeyEvent::BackTab,
        KeyEvent::RepeatCharacter('q'),
        KeyEvent::Character('1'),
    ] {
        assert_eq!(
            state.handle_key(&key, 0, (80, 24)),
            ConsoleAction::None,
            "navigation and editing must not request server-side work"
        );
    }
    assert!(
        matches!(state.dialog, Some(Dialog::Form(_))),
        "form must retain focus"
    );
    let Some(Dialog::Form(form)) = &state.dialog else {
        return;
    };
    assert_eq!(form.focused, 2, "back-tab must wrap to the final field");
    assert_eq!(
        form.text(FormFieldId::AdminPasswordConfirm),
        Ok("q1"),
        "global shortcuts and repeated text belong to the focused field"
    );
    assert_eq!(
        state.handle_key(&KeyEvent::Escape, 0, (80, 24)),
        ConsoleAction::None,
        "navigation and editing must not request server-side work"
    );
    assert!(
        state.dialog.is_none(),
        "escape must cancel without submission"
    );
}
#[test]
fn suite_navigation_and_contextual_help_preserve_the_origin() {
    let mut app = ConsoleState::default();
    for expected in [
        Screen::Tasks,
        Screen::Boards,
        Screen::Logs,
        Screen::System,
        Screen::Configuration,
        Screen::Dashboard,
    ] {
        assert_eq!(
            app.handle_key(&KeyEvent::Tab, 0, (80, 24)),
            ConsoleAction::None,
            "navigation and editing must not request server-side work"
        );
        assert_eq!(
            app.screen, expected,
            "Tab must traverse the shared suite hierarchy"
        );
    }
    assert_eq!(
        app.handle_key(&KeyEvent::Character('2'), 0, (80, 24)),
        ConsoleAction::None,
        "navigation and editing must not request server-side work"
    );
    assert_eq!(
        app.handle_key(&KeyEvent::Character('?'), 0, (80, 24)),
        ConsoleAction::None,
        "navigation and editing must not request server-side work"
    );
    assert_eq!(
        app.help_return,
        Some(Screen::Tasks),
        "help must remember its origin"
    );
    assert_eq!(
        app.handle_key(&KeyEvent::Escape, 0, (80, 24)),
        ConsoleAction::None,
        "navigation and editing must not request server-side work"
    );
    assert_eq!(
        app.screen,
        Screen::Tasks,
        "closing help must return to the task table"
    );
    assert_eq!(
        app.handle_key(&KeyEvent::BackTab, 0, (80, 24)),
        ConsoleAction::None,
        "navigation and editing must not request server-side work"
    );
    assert_eq!(
        app.screen,
        Screen::Dashboard,
        "reverse navigation must mirror Tab"
    );
}

#[test]
fn search_is_modal_and_escape_clears_before_navigating() {
    let mut app = ConsoleState {
        screen: Screen::Tasks,
        ..ConsoleState::default()
    };
    for key in [
        KeyEvent::Character('/'),
        KeyEvent::Paste("qad123".to_owned()),
        KeyEvent::Character('q'),
        KeyEvent::Enter,
    ] {
        assert_eq!(
            app.handle_key(&key, 0, (80, 24)),
            ConsoleAction::None,
            "search must consume action shortcuts"
        );
    }
    assert!(
        app.dialog.is_none(),
        "search must not open an administrative form or stop prompt"
    );
    assert_eq!(
        app.task_filter.query, "qad123q",
        "search must accept literal shortcut characters"
    );
    assert_eq!(
        app.handle_key(&KeyEvent::Escape, 0, (80, 24)),
        ConsoleAction::None,
        "navigation and editing must not request server-side work"
    );
    assert_eq!(
        app.screen,
        Screen::Tasks,
        "first Escape must clear search in place"
    );
    assert_eq!(
        app.handle_key(&KeyEvent::Escape, 0, (80, 24)),
        ConsoleAction::None,
        "navigation and editing must not request server-side work"
    );
    assert_eq!(
        app.screen,
        Screen::Dashboard,
        "second Escape must navigate back"
    );
}

#[test]
fn task_identity_survives_refresh_and_inspection_has_no_side_effects() {
    let task = TaskRow {
        id: "job:42".to_owned(),
        kind: "Audio waveform".to_owned(),
        item: "Post #3".to_owned(),
        state: super::super::telemetry::TaskState::Running,
        attempts: Some(1),
        created_at: Some(10),
        changed_at: Some(20),
        detail: "No percentage available".to_owned(),
    };
    let mut stats = super::super::ChanStats::default();
    stats.operator.tasks.push(task.clone());
    let mut app = ConsoleState {
        screen: Screen::Tasks,
        ..ConsoleState::default()
    };
    app.reconcile_data(&stats);
    let mut earlier = task;
    earlier.id = "job:41".to_owned();
    stats.operator.tasks.insert(0, earlier);
    if let Some(row) = stats.operator.tasks.get_mut(1) {
        row.state = super::super::telemetry::TaskState::Completed;
    }
    app.reconcile_data(&stats);
    assert_eq!(
        app.tasks.selected,
        Some(1),
        "refresh must retain task identity across insertion and completion"
    );
    assert_eq!(
        app.handle_key(&KeyEvent::Enter, 0, (80, 24)),
        ConsoleAction::None,
        "inspection must be read only"
    );
    assert!(
        matches!(
            app.dialog,
            Some(Dialog::Inspect {
                title: _,
                lines: _,
                scroll: _
            })
        ),
        "selected task must open details"
    );
    assert_eq!(
        app.handle_key(&KeyEvent::Character('d'), 0, (80, 24)),
        ConsoleAction::None,
        "navigation and editing must not request server-side work"
    );
    assert!(
        matches!(
            app.dialog,
            Some(Dialog::Inspect {
                title: _,
                lines: _,
                scroll: _
            })
        ),
        "detail must capture administrative shortcuts"
    );
    assert_eq!(
        app.handle_key(&KeyEvent::Escape, 0, (80, 24)),
        ConsoleAction::None,
        "navigation and editing must not request server-side work"
    );
    stats.operator.tasks.clear();
    app.reconcile_data(&stats);
    assert!(
        app.tasks.selected.is_none() && app.selected_task.is_none(),
        "empty task snapshots must clear stale selection"
    );
}
