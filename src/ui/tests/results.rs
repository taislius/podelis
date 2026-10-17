use super::*;

#[test]
fn query_screen_with_results_sort_and_filter() {
    let (mut app, _dir) = query_app();
    app.focus = Focus::Results;
    for c in ['s', 's', '/', 'c', 'l', 'e', 'o'] {
        app.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
    }
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    app.tab.syntax_hint = Some(SyntaxHint {
        message: "relation \"users\" does not exist".into(),
        offset: Some(28),
        len: Some(5),
        source: HintSource::Server,
        nth: None,
    });
    assert_snapshot!(render(&mut app, 90, 20));
}

#[test]
fn results_one_column_per_line() {
    let (mut app, _dir) = query_app();
    app.focus = Focus::Results;
    let press = |app: &mut App, code| app.handle_key(KeyEvent::new(code, KeyModifiers::NONE));
    press(&mut app, KeyCode::Char('X'));
    assert!(app.tab.grid.vertical);
    let screen = render(&mut app, 100, 30);
    assert!(screen.contains("1. row") && screen.contains("email: ana@example.com"), "{screen}");
    // ↓ goes field by field, then on to the next row.
    for _ in 0..3 {
        press(&mut app, KeyCode::Down);
    }
    assert_eq!((app.tab.grid.table_state.selected(), app.tab.grid.selected_col), (Some(1), 0));
    press(&mut app, KeyCode::Up);
    assert_eq!((app.tab.grid.table_state.selected(), app.tab.grid.selected_col), (Some(0), 2));
    press(&mut app, KeyCode::PageDown);
    assert_eq!(app.tab.grid.table_state.selected(), Some(1));
    assert_snapshot!(render(&mut app, 70, 24));
    press(&mut app, KeyCode::Char('X'));
    assert!(!app.tab.grid.vertical);
}

#[test]
fn the_row_view_steps_to_the_next_row_with_arrows() {
    let (mut app, _dir) = query_app();
    app.focus = Focus::Results;
    let press = |app: &mut App, code| app.handle_key(KeyEvent::new(code, KeyModifiers::NONE));
    press(&mut app, KeyCode::Char('x'));
    let screen = render(&mut app, 100, 30);
    assert_eq!(app.cell_view.as_ref().unwrap().title, "ROW 1 of 3");
    // Its keys are on a line of its own inside it, not the status bar.
    let lines: Vec<&str> = screen.lines().collect();
    let keys = lines.iter().position(|l| l.contains("↑↓ scroll, next row")).unwrap_or_else(|| panic!("{screen}"));
    assert!(lines[keys].contains("Esc close") && lines[keys + 1].contains('└'), "{screen}");
    assert!(!lines.last().unwrap().contains("next row"), "{screen}");
    // The row fits: ↓ is the next row, ↑ the one before.
    press(&mut app, KeyCode::Down);
    let screen = render(&mut app, 100, 30);
    assert!(screen.contains("ROW 2 of 3") && screen.contains("ben@example.com"), "{screen}");
    press(&mut app, KeyCode::Right);
    let _ = render(&mut app, 100, 30);
    press(&mut app, KeyCode::Down);
    assert_eq!(app.cell_view.as_ref().unwrap().title, "ROW 3 of 3", "stays on the last row");
    press(&mut app, KeyCode::Up);
    assert_eq!(app.cell_view.as_ref().unwrap().title, "ROW 2 of 3");
    // A row taller than the popup scrolls first.
    let _ = render(&mut app, 100, 9);
    assert!(app.cell_view.as_ref().unwrap().max_scroll > 0);
    press(&mut app, KeyCode::Down);
    assert_eq!(app.cell_view.as_ref().unwrap().title, "ROW 2 of 3");
    assert_eq!(app.cell_view.as_ref().unwrap().scroll, 1);
}

#[test]
fn the_filter_shows_only_with_its_grid() {
    let (mut app, _dir) = query_app();
    app.focus = Focus::Results;
    let press = |app: &mut App, code| app.handle_key(KeyEvent::new(code, KeyModifiers::NONE));
    for code in [KeyCode::Char('/'), KeyCode::Char('a'), KeyCode::Char('n'), KeyCode::Enter] {
        press(&mut app, code);
    }
    assert!(render(&mut app, 100, 30).contains("filter: an"));
    press(&mut app, KeyCode::Char('2'));
    assert!(!render(&mut app, 100, 30).contains("filter:"), "not over MESSAGES");
    press(&mut app, KeyCode::Char('1'));
    assert!(render(&mut app, 100, 30).contains("filter: an"), "back with the rows");
}

#[test]
fn hidden_columns_show_in_the_footer() {
    let (mut app, _dir) = query_app();
    app.panes.schema_visible = false;
    let columns: Vec<String> = (0..30).map(|i| format!("column_{i}")).collect();
    let row: Vec<Option<String>> = (0..30).map(|i| Some(format!("value {i}"))).collect();
    let outcome = QueryOutcome {
        column_types: vec!["TEXT".into(); 30],
        columns,
        rows: vec![row],
        total_rows: 1,
        statements: 1,
        ..QueryOutcome::default()
    };
    app.show_results_for_tests(outcome);
    app.focus = Focus::Results;
    let screen = render(&mut app, 100, 20);
    assert!(screen.contains("cols 1–") && screen.contains("of 30 ▶"), "{screen}");
    app.handle_key(KeyEvent::new(KeyCode::Char('$'), KeyModifiers::NONE));
    let screen = render(&mut app, 100, 20);
    assert!(screen.contains("◀ cols") && screen.contains("–30 of 30") && screen.contains("column_29"), "{screen}");
}

#[test]
fn a_scripts_result_sets() {
    let (mut app, _dir) = query_app();
    let set = |statement: usize, column: &str, rows: usize| QueryOutcome {
        columns: vec![column.into()],
        column_types: vec!["INT4".into()],
        rows: (0..rows).map(|i| vec![Some(i.to_string())]).collect(),
        total_rows: rows as u64,
        statements: 3,
        statement,
        ..QueryOutcome::default()
    };
    let outcome = QueryOutcome { earlier: vec![set(1, "a", 1), set(2, "b", 2)], ..set(3, "c", 3) };
    app.show_results_for_tests(outcome);
    app.focus = Focus::Results;
    app.handle_key(KeyEvent::new(KeyCode::Char('>'), KeyModifiers::NONE));
    assert_snapshot!(render(&mut app, 90, 16));
}

#[test]
fn cell_view_popup() {
    let (mut app, _dir) = query_app();
    app.cell_view = Some(CellView::new("note".into(), "{\n  \"vip\": true\n}".into()));
    assert_snapshot!(render(&mut app, 70, 16));
}

#[test]
fn a_followed_rows_popover_says_it_opens_in_a_tab() {
    let (mut app, _dir) = query_app();
    app.cell_view = Some(crate::app::CellView {
        follow: Some(("→ customers".into(), "SELECT * FROM customers WHERE id = 1;".into())),
        ..crate::app::CellView::new("→ customers · id = 1".into(), "id    1\nname  Ona\n".into())
    });
    let screen = render(&mut app, 110, 24);
    assert!(screen.contains("→ customers · id = 1") && screen.contains("name  Ona"), "{screen}");
    assert!(screen.contains("e open in a tab"), "{screen}");
}

#[test]
fn kept_results_and_the_statement_above_a_result() {
    use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    let (mut app, _dir) = query_app();
    let one = || QueryOutcome {
        columns: vec!["id".into()],
        column_types: vec!["INT4".into()],
        rows: vec![vec![Some("1".into())]],
        total_rows: 1,
        statements: 1,
        ..QueryOutcome::default()
    };
    app.tab.editor.set_text("select 1;\nselect id, email, note\nfrom users\nwhere id < 4;");
    app.open_run_for_tests(0..9);
    app.show_results_for_tests(one());
    app.open_run_for_tests(10..app.tab.editor.text().len());
    app.show_results_for_tests(one());
    app.tab.runs[1].source.as_mut().unwrap().at = chrono::Local::now();
    app.focus = Focus::Results;
    let screen = render(&mut app, 140, 22);
    assert!(screen.contains(" #1  #2  select id, email, note from users where id < 4;  lines 2–4"), "{screen}");
    // A click on #1 shows it; on the lines, its statement in the editor.
    let (x0, _, i) = app.hits.run_tabs[0];
    let click = |x, y| MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: x,
        row: y,
        modifiers: KeyModifiers::NONE,
    };
    app.handle_mouse(click(x0 + 1, app.hits.run_source.y));
    assert_eq!((i, app.tab.run_at), (0, 0));
    let screen = render(&mut app, 140, 22);
    assert!(screen.contains("select 1;  line 1"), "{screen}");
    let source = app.hits.run_source;
    app.handle_mouse(click(source.x + source.width - 30, source.y));
    assert_eq!((app.focus, app.tab.editor.selected_text().as_deref()), (Focus::Editor, Some("select 1;")));
    // A run that failed says so where its result would be.
    app.open_run_for_tests(10..app.tab.editor.text().len());
    app.tab.run_error = Some("relation \"users\" does not exist".into());
    let screen = render(&mut app, 140, 22);
    assert!(screen.contains("The query failed:") && screen.contains("relation \"users\" does not exist"), "{screen}");
}

#[test]
fn a_runs_outcome_is_shown_in_the_results_pane() {
    let (mut app, _dir) = query_app();
    app.notes.status = "relation \"nope\" does not exist (line 1, col 15)".into();
    app.notes.status_is_error = true;
    app.notes.status_in_results = true;
    let screen = render(&mut app, 100, 24);
    let lines: Vec<&str> = screen.lines().collect();
    let last = lines.last().unwrap();
    assert!(!last.contains("does not exist") && last.contains("keys"), "not in the status bar: {last}");
    // The results pane's footer, just above its bottom border.
    assert!(lines[lines.len() - 3].contains("does not exist"), "{screen}");
    assert!(
        !screen.contains("ran in") && !screen.contains("EXEC"),
        "the time is in the footer, not the title: {screen}"
    );
}
