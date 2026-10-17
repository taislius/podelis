use super::*;

#[test]
fn a_hint_says_its_line_and_a_click_goes_there() {
    let (mut app, _dir) = query_app();
    // "where" on line 3, while the cursor is on line 1.
    app.tab.syntax_hint = Some(SyntaxHint {
        message: "Unknown column".into(),
        offset: Some(34),
        len: Some(5),
        source: HintSource::Server,
        nth: None,
    });
    app.tab.editor.set_cursor((0, 0));
    let screen = render(&mut app, 100, 30);
    assert!(screen.contains("SERVER line 3  Unknown column"), "{screen}");
    let at = app.hits.syntax_hint;
    app.focus = Focus::Results;
    app.handle_mouse(crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
        column: at.x + 3,
        row: at.y,
        modifiers: KeyModifiers::NONE,
    });
    assert_eq!((app.focus, app.tab.editor.cursor_row, app.tab.editor.cursor_col), (Focus::Editor, 2, 0));
}

#[test]
fn the_active_tabs_line_joins_the_panes_border() {
    let (mut app, _dir) = query_app();
    app.schema_visible = false;
    app.handle_key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL));
    let screen = render(&mut app, 70, 16);
    let lines: Vec<&str> = screen.lines().collect();
    // Second tab active: the separator before it is thick, joined above.
    let tabs = lines.iter().position(|l| l.contains('┃')).unwrap_or_else(|| panic!("{screen}"));
    let x = lines[tabs].chars().position(|c| c == '┃').unwrap();
    assert!(x > 1, "the second tab's line, not the pane's border: {screen}");
    assert_eq!(lines[tabs - 1].chars().nth(x), Some('┰'), "{screen}");
    // And the line on its right.
    let right = lines[tabs].chars().skip(x + 1).position(|c| c == '┃' || c == '│').unwrap() + x + 1;
    assert_eq!(lines[tabs].chars().nth(right), Some('┃'), "{screen}");
    assert_eq!(lines[tabs - 1].chars().nth(right), Some('┰'), "{screen}");
}

#[test]
fn lint_warning_names_its_rule() {
    let (mut app, _dir) = query_app();
    app.tab.editor.set_text("select * from users\nwhere note = null;");
    app.tab.syntax_hint =
        crate::sql::lint::lints(&crate::sql::parse::parse(&app.tab.editor.text(), app.engine()), "").into_iter().next();
    assert_snapshot!(render(&mut app, 160, 14));
}

#[test]
fn the_footer_says_which_statement_f6_runs() {
    let (mut app, _dir) = query_app();
    app.tab.editor.set_text("select 1;\nselect ';' as semi;\n\nselect 3");
    app.tab.editor.set_cursor((1, 3));
    assert_snapshot!(render(&mut app, 100, 24));
    // A blank line goes with the statement before it, as F6 picks it.
    app.tab.editor.set_cursor((2, 0));
    assert!(render(&mut app, 100, 24).contains("stmt 2/3"));
    // F6 runs a selection instead, and one statement needs no count.
    app.tab.editor.select_all();
    assert!(!render(&mut app, 100, 24).contains("stmt "));
    app.tab.editor.set_text("select 1;");
    assert!(!render(&mut app, 100, 24).contains("stmt "));
}

#[test]
fn completion_popup() {
    let (mut app, _dir) = query_app();
    app.tab.editor.set_text("select em");
    let cand = |label: &str, kind, detail: &str| Candidate {
        label: label.into(),
        insert: label.into(),
        kind,
        detail: detail.into(),
    };
    app.completion = Some(Completion {
        items: vec![
            cand("email", CandKind::Column, "text · users"),
            cand("employee_id", CandKind::Column, "integer · orders"),
            cand("employees", CandKind::Table, "table"),
        ],
        selected: 1,
        prefix_len: 2,
    });
    assert_snapshot!(render(&mut app, 80, 20));
}

#[test]
fn command_palette_and_find_bar() {
    let (mut app, _dir) = query_app();
    app.handle_key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL));
    for c in "id".chars() {
        app.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
    }
    app.handle_key(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::CONTROL));
    for c in "tab".chars() {
        app.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
    }
    assert_snapshot!(render(&mut app, 100, 28));
}

#[test]
fn the_active_tab_shows_its_whole_name_and_stays_in_sight() {
    let (mut app, _dir) = query_app();
    let press = |app: &mut App, code, modifiers| app.handle_key(KeyEvent::new(code, modifiers));
    for _ in 2..=12 {
        press(&mut app, KeyCode::Char('t'), KeyModifiers::CONTROL);
    }
    for (i, tab) in app.tabs.iter_mut().enumerate().skip(1) {
        tab.name = format!("monthly revenue report {}", i + 1);
    }
    for _ in 0..3 {
        press(&mut app, KeyCode::PageUp, KeyModifiers::CONTROL);
    }
    assert_eq!(app.active_tab, 8);
    // Tabs after it that don't fit are said too, and the `+` stays.
    let screen = render(&mut app, 110, 24);
    let row = screen.lines().nth(2).unwrap();
    assert!(row.contains("›") && row.contains(" + "), "{row}");
    assert!(app.hits.new_tab.is_some());
    // There its 24 columns of name don't fit beside "‹8", "›3" and the `+`
    // (40 columns of tabs): cut. With 10 more, whole.
    assert!(row.contains("monthly revenue report…"), "{row}");
    let screen = render(&mut app, 120, 24);
    let row = screen.lines().nth(2).unwrap();
    assert!(row.contains("monthly revenue report 9") && row.contains(" + "), "the active tab's name, whole: {row}");
    // Many tabs: the others squeezed, those before it cut from the start.
    for _ in 0..3 {
        press(&mut app, KeyCode::PageDown, KeyModifiers::CONTROL);
    }
    let screen = render(&mut app, 160, 24);
    let tabs = screen.lines().nth(2).unwrap();
    assert!(tabs.contains("monthly revenue report 12"), "{tabs}");
    assert!(tabs.contains("‹"), "says some are left out: {tabs}");
    // Clicking a tab still picks the one drawn there.
    let hit = app.hits.tabs.iter().find(|(_, _, i)| *i == 10).copied().unwrap();
    app.handle_mouse(crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
        column: hit.0 + 1,
        row: app.hits.tabs_y,
        modifiers: KeyModifiers::NONE,
    });
    assert_eq!(app.active_tab, 10);
    // "‹N": the tab just before those shown.
    let _ = render(&mut app, 110, 24);
    let (start, _, before) = app.hits.tabs[0];
    app.handle_mouse(crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
        column: start,
        row: app.hits.tabs_y,
        modifiers: KeyModifiers::NONE,
    });
    assert_eq!(app.active_tab, before);
    assert!(before < 10);
    // Whatever is beside the name (a saved query's " ●", wide characters,
    // the left line after "‹N"), a narrow row keeps the active tab and `+`.
    let active = app.active_tab;
    app.tabs[active].saved_id = Some("changed".into());
    app.tab.editor.set_text("select 1;");
    for name in ["x".repeat(60), "月次売上レポート".repeat(6)] {
        app.tabs[active].name = name;
        for width in [40, 50, 70, 90] {
            let screen = render(&mut app, width, 24);
            let row = screen.lines().nth(2).unwrap();
            assert!(row.contains(" + ") && app.hits.new_tab.is_some(), "the + in sight at {width}: {row}");
            assert!(row.contains("●"), "the change mark at {width}: {row}");
            assert!(app.hits.tabs.iter().any(|(_, _, i)| *i == active), "the active tab at {width}: {row}");
        }
    }
}

#[test]
fn the_statement_zoom_shows_the_statement_alone() {
    let (mut app, _dir) = query_app();
    app.tab.editor.set_text("select 1;\n\nselect id,\n  name\nfrom t;\n\nselect 3;");
    app.tab.editor.set_cursor((3, 2));
    app.statement_zoom = true;
    let screen = render(&mut app, 110, 30);
    assert!(screen.contains("THE STATEMENT ALONE"), "{screen}");
    assert!(screen.contains("3  select id,") && screen.contains("5  from t;"), "{screen}");
    assert!(!screen.contains("select 1;") && !screen.contains("select 3;"), "only it: {screen}");
    assert!(screen.contains("QUERY RESULT"), "the results under it: {screen}");
}

#[test]
fn ctrl_k_menu_shows_letters_and_the_panes_keys() {
    let (mut app, _dir) = query_app();
    app.schema_visible = true;
    app.focus = Focus::Schema;
    app.status.clear();
    let screen = render(&mut app, 90, 30);
    assert!(screen.contains("^K  keys") && !screen.contains("REFRESH"), "minimal hints, no pane footer:\n{screen}");
    // `?` outside the editor opens the same menu as Ctrl+K.
    app.handle_key(KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE));
    let screen = render(&mut app, 90, 30);
    assert!(screen.contains("IN THE NAVIGATOR") && screen.contains("zoom pane"), "{screen}");
    // The editor's own keys only from the editor.
    assert!(
        !screen.contains("format SQL")
            && !screen.contains("explain analyze")
            && !screen.contains("rename tab")
            && !screen.contains("transaction mode"),
        "{screen}"
    );
    assert_snapshot!(screen);
    // A letter runs its action.
    app.handle_key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::NONE));
    assert!(app.zoomed && app.palette.is_none());
    // Anything else (or `/`) searches every command.
    app.handle_key(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::CONTROL));
    for c in "/export".chars() {
        app.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
    }
    let screen = render(&mut app, 90, 30);
    assert!(screen.contains("COMMANDS") && screen.contains("Export rows as CSV"), "{screen}");
    // From the editor, its own keys are there.
    app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    app.zoomed = false;
    app.focus = Focus::Editor;
    app.handle_key(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::CONTROL));
    if let Some(palette) = &mut app.palette {
        palette.menu = true;
    }
    let screen = render(&mut app, 90, 30);
    assert!(
        screen.contains("format SQL")
            && screen.contains("explain analyze")
            && screen.contains("rename tab")
            && screen.contains("transaction mode"),
        "{screen}"
    );
    // Commit and roll back only with a transaction open.
    assert!(!screen.contains("commit") && !screen.contains("roll back"), "{screen}");
    app.txn = TxnState::Open;
    let screen = render(&mut app, 90, 30);
    assert!(screen.contains("c commit") && screen.contains("r roll back"), "{screen}");
}

#[test]
fn run_buttons_sit_right_of_the_tabs_and_click() {
    use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    let (mut app, _dir) = query_app();
    app.focus = Focus::Results;
    app.status.clear();
    let screen = render(&mut app, 120, 20);
    assert!(screen.contains("▶ F5 Run") && screen.contains("F7 Explain"), "{screen}");
    let &(x, _, action) = app.hits.run_buttons.first().unwrap();
    assert_eq!(action, crate::config::keys::Action::Run);
    app.handle_mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: x + 1,
        row: app.hits.tabs_y,
        modifiers: KeyModifiers::NONE,
    });
    // It ran (here: there's no database behind the test connection).
    assert_eq!(app.focus, Focus::Editor);
    let note = app.editor_note.as_deref().unwrap_or_default();
    assert!(note.contains("connect"), "in the editor's footer: {note}");
}

#[test]
fn ctrl_k_lists_the_tabs_and_finds_them_by_name() {
    let (mut app, _dir) = query_app();
    app.focus = Focus::Results;
    app.handle_key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL));
    assert_eq!(app.active_tab, 1);
    app.focus = Focus::Results;
    let first = app.tabs[0].name.clone();
    app.handle_key(KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE));
    let screen = render(&mut app, 100, 40);
    assert!(screen.contains("TABS") && screen.contains("(this one)") && screen.contains(&first), "{screen}");
    assert_snapshot!(screen);
    // The number switches.
    app.handle_key(KeyEvent::new(KeyCode::Char('1'), KeyModifiers::NONE));
    assert!(app.active_tab == 0 && app.palette.is_none());
    // Searching finds the other tab by name.
    app.handle_key(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::CONTROL));
    app.handle_key(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE));
    for c in format!("switch to tab: {}", app.tabs[1].name).chars() {
        app.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
    }
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert_eq!(app.active_tab, 1);
}

#[test]
fn clicking_the_editor_tabs_focuses_the_editor() {
    use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    let click = |kind, x, y| MouseEvent { kind, column: x, row: y, modifiers: KeyModifiers::NONE };
    let (mut app, _dir) = query_app();
    // Even while a query runs (the tab can't be switched then).
    app.run.executing = true;
    let _ = render(&mut app, 110, 26);
    let (tab, y) = (app.hits.tabs[0].0 + 2, app.hits.tabs_y);
    app.focus = Focus::Results;
    app.handle_mouse(click(MouseEventKind::Down(MouseButton::Left), tab, y));
    assert_eq!(app.focus, Focus::Editor);
    // A right-click there opens the editor's menu, not the results'.
    app.focus = Focus::Results;
    app.handle_mouse(click(MouseEventKind::Down(MouseButton::Right), tab, y));
    assert_eq!(app.menu.as_ref().map(|m| m.title), Some("EDITOR"));
}
