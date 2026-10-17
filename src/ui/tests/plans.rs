use super::*;

#[test]
fn plan_findings_point_at_their_steps() {
    let (mut app, _dir) = query_app();
    let note = |text: &str, line| crate::db::PlanNote { text: text.into(), line: Some(line) };
    app.tab.plan = Some(crate::db::Plan {
        text: "Sort  (cost=900.00..950.00 rows=10)\n  Sort Key: total\n  ->  Seq Scan on orders  (cost=0.00..800.00 rows=10)\n        Filter: (total > 10)".into(),
        notes: vec![
            note("Costliest step: Seq Scan on orders, 84% of the estimated cost", 2),
            note("Seq Scan on orders with a filter over a big table: an index may help", 2),
        ],
    });
    app.tab.result_tab = crate::app::ResultTab::Plan;
    assert_snapshot!(render(&mut app, 100, 20));
}

#[test]
fn a_wide_plan_scrolls_across() {
    let (mut app, _dir) = query_app();
    let wide = format!("-> Table scan on orders  (cost=1.0 rows=3) {}END", "x".repeat(150));
    app.tab.plan = Some(crate::db::Plan { text: wide, notes: Vec::new() });
    app.tab.result_tab = crate::app::ResultTab::Plan;
    app.focus = Focus::Results;
    let screen = render(&mut app, 100, 20);
    assert!(!screen.contains("END") && screen.contains("cols 1–"), "{screen}");
    for _ in 0..40 {
        app.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
    }
    let screen = render(&mut app, 100, 20);
    assert!(screen.contains("END") && screen.contains("◀ cols"), "stops at the widest line:\n{screen}");
    app.handle_key(KeyEvent::new(KeyCode::Char('0'), KeyModifiers::NONE));
    assert_eq!(app.tab.tab_hscroll, 0);
}

#[test]
fn a_tabular_plan_is_a_grid_with_its_notes() {
    let (mut app, _dir) = query_app();
    let cells = |r: &[&str]| r.iter().map(|c| Some((*c).to_string())).collect::<Vec<_>>();
    app.show_plan_for_tests(QueryOutcome {
        columns: ["id", "select_type", "table", "type", "rows", "Extra"].map(String::from).to_vec(),
        column_types: vec![
            "BIGINT".into(),
            "TEXT".into(),
            "TEXT".into(),
            "TEXT".into(),
            "BIGINT".into(),
            "TEXT".into(),
        ],
        rows: vec![
            cells(&["1", "PRIMARY", "orders", "ALL", "50000", "Using where"]),
            cells(&["2", "DEPENDENT SUBQUERY", "users", "eq_ref", "1", ""]),
        ],
        total_rows: 2,
        statements: 1,
        ..QueryOutcome::default()
    });
    assert_snapshot!(render(&mut app, 100, 30));
}

#[test]
fn a_plan_says_what_it_is_for_and_how_old_it_is() {
    let (mut app, _dir) = query_app();
    app.tab.plan = Some(crate::db::Plan { text: "Seq Scan on users".into(), notes: Vec::new() });
    let mut source = crate::app::PlanSource::new("from users\nwhere id < 4".into(), false, 1);
    source.at = chrono::Local::now() - chrono::TimeDelta::minutes(3);
    app.tab.plan_source = Some(source);
    app.tab.result_tab = crate::app::ResultTab::Plan;
    let screen = render(&mut app, 100, 30);
    assert!(
        screen.contains(" EXPLAIN  from users where id < 4  lines 2–3") && screen.contains("3 min ago"),
        "{screen}"
    );
    assert!(!screen.contains("edited since"), "{screen}");
    // Lines added above it: it's found where it is now, and a click there
    // puts the cursor on it.
    app.tab.editor.set_text("-- one\n-- two\nselect id, email, note\nfrom users\nwhere id < 4;");
    assert!(render(&mut app, 100, 30).contains("lines 4–5"));
    let at = app.hits.plan_source;
    app.handle_mouse(crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
        column: at.x + 2,
        row: at.y,
        modifiers: KeyModifiers::NONE,
    });
    assert_eq!((app.focus, app.tab.editor.cursor_row, app.tab.editor.cursor_col), (Focus::Editor, 3, 0));
    assert_eq!(app.tab.editor.selected_text().as_deref(), Some("from users\nwhere id < 4"), "the statement selected");
    // The statement is no longer in the editor.
    app.tab.editor.set_text("select 1");
    app.tab.result_tab = crate::app::ResultTab::Plan;
    let screen = render(&mut app, 100, 30);
    assert!(screen.contains("was lines 2–3 · edited since"), "{screen}");
}

#[test]
fn the_plans_lines_are_selected_by_mouse_and_kept_when_scrolled() {
    use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    let (mut app, _dir) = query_app();
    let text = (1..=40).map(|i| format!("step {i}")).collect::<Vec<_>>().join("\n");
    app.tab.plan = Some(crate::db::Plan { text, notes: Vec::new() });
    app.tab.result_tab = ResultTab::Plan;
    app.focus = Focus::Results;
    let _ = render(&mut app, 110, 30);
    let area = app.hits.plan;
    let mouse = |kind, y, modifiers| MouseEvent { kind, column: area.x + 4, row: y, modifiers };
    // Click on "step 3", drag past the bottom: the plan scrolls, the
    // selection goes on with it.
    let row_of = |app: &App, line: usize| {
        u16::try_from(usize::from(app.hits.plan.y) + app.hits.plan_notes + line - usize::from(app.tab.tab_scroll))
            .unwrap()
    };
    let at = row_of(&app, 2);
    app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), at, KeyModifiers::NONE));
    for _ in 0..5 {
        app.handle_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), area.bottom() + 1, KeyModifiers::NONE));
        let _ = render(&mut app, 110, 30);
    }
    app.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), area.bottom(), KeyModifiers::NONE));
    assert!(app.tab.tab_scroll >= 5, "scrolled: {}", app.tab.tab_scroll);
    let (from, to) = app.tab.plan_selected.unwrap();
    assert_eq!(from, 2);
    // It ends on the bottom line in sight, not one above it.
    let bottom =
        usize::from(area.bottom()) - 1 - usize::from(area.y) + usize::from(app.tab.tab_scroll) - app.hits.plan_notes;
    assert_eq!(to, bottom);
    // Scrolled back up, it's all still there: copied whole.
    app.tab.tab_scroll = 0;
    let screen = render(&mut app, 110, 30);
    assert!(screen.contains("step 3"));
    app.handle_key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE));
    let copied = app.take_clipboard().unwrap();
    assert!(copied.starts_with("step 3\nstep 4") && copied.ends_with(&format!("step {}", to + 1)), "{copied}");
    // Shift+↓ goes one further; Shift+click on a line extends to it.
    app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::SHIFT));
    assert_eq!(app.tab.plan_selected, Some((2, to + 1)));
    // Right after, before a redraw, a click goes by the scroll it left.
    let top = usize::from(app.tab.tab_scroll) - app.hits.plan_notes;
    assert!(top > 0, "Shift+↓ scrolled: {}", app.tab.tab_scroll);
    app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), area.y, KeyModifiers::SHIFT));
    assert_eq!(app.tab.plan_selected, Some((2, top)));
    app.tab.tab_scroll = 0;
    let _ = render(&mut app, 110, 30);
    let at = row_of(&app, 0);
    app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), at, KeyModifiers::SHIFT));
    assert_eq!(app.tab.plan_selected, Some((2, 0)));
    // Esc lets go; `y` then copies the whole plan.
    app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert!(app.tab.plan_selected.is_none());
}
