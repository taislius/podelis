use super::*;

/// Every screen and popup must draw without panicking at any size,
/// including absurdly small terminals.
#[test]
fn renders_at_any_size() {
    let sizes = [(1, 1), (2, 2), (10, 4), (20, 6), (40, 10), (120, 40)];
    for (w, h) in sizes {
        for screen in 0..4 {
            let (mut app, _dir) = query_app();
            app.screen = match screen {
                0 => Screen::Projects,
                1 => Screen::Connections,
                2 => Screen::Connect,
                _ => Screen::Query,
            };
            app.panes.schema_visible = true;
            app.help_visible = screen == 1;
            app.cell_view = Some(CellView { scroll: 3, ..CellView::new("x".into(), "y\n".repeat(50)) });
            app.confirm = Some(Confirm {
                message: "Sure?".into(),
                details: vec!["a long detail line ".repeat(10)],
                danger: false,
                action: ConfirmAction::Quit,
                offer_transaction: false,
            });
            let _ = render(&mut app, w, h);
        }
    }
}

#[test]
fn large_results_render_only_the_visible_window() {
    let (mut app, _dir) = query_app();
    let rows: Vec<Vec<Option<String>>> =
        (1..=5000).map(|n| vec![Some(n.to_string()), Some(format!("user{n}@example.com")), None]).collect();
    let mut outcome = app.tab.grid.results.take().unwrap();
    outcome.total_rows = rows.len() as u64;
    outcome.rows = rows;
    app.show_results_for_tests(outcome);
    app.focus = Focus::Results;
    app.tab.grid.table_state.select(Some(3999));
    let screen = render(&mut app, 90, 24);
    assert!(screen.contains("user4000@example.com"), "{screen}");
    assert!(!screen.contains("user1@example.com"));
    // Moving back up scrolls the window back.
    app.tab.grid.table_state.select(Some(0));
    let screen = render(&mut app, 90, 24);
    assert!(screen.contains("user1@example.com") && !screen.contains("user4000@"));
}

#[test]
fn mouse_clicks_follow_the_layout() {
    use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    let (mut app, _dir) = query_app();
    let _ = render(&mut app, 100, 30);
    let click = |app: &mut App, x: u16, y: u16| {
        for kind in [MouseEventKind::Down(MouseButton::Left), MouseEventKind::Up(MouseButton::Left)] {
            app.handle_mouse(MouseEvent { kind, column: x, row: y, modifiers: KeyModifiers::NONE });
        }
    };
    // A result row, then the second column of it.
    let rows = app.hits.result_rows;
    let (x0, _, col) = app.hits.result_cols[1];
    click(&mut app, x0, rows.y + 2);
    assert_eq!(
        (app.focus, app.tab.grid.table_state.selected(), app.tab.grid.selected_col),
        (Focus::Results, Some(2), col)
    );
    // The editor: line 2, column 3.
    let text = app.hits.editor_text;
    click(&mut app, text.x + 3, text.y + 1);
    assert_eq!((app.focus, app.tab.editor.cursor_row, app.tab.editor.cursor_col), (Focus::Editor, 1, 3));
    // Drag selects.
    let drag = MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: text.x,
        row: text.y,
        modifiers: KeyModifiers::NONE,
    };
    app.handle_mouse(drag);
    app.handle_mouse(MouseEvent { kind: MouseEventKind::Drag(MouseButton::Left), column: text.x + 6, ..drag });
    assert_eq!(app.tab.editor.selected_text().as_deref(), Some("select"));
    // The `+` opens a tab; clicking the first tab goes back.
    let (plus, _) = app.hits.new_tab.unwrap();
    let _ = render(&mut app, 100, 30);
    let y = app.hits.tabs_y;
    click(&mut app, plus, y);
    assert_eq!(app.tabs.len(), 2);
    let _ = render(&mut app, 100, 30);
    let (tab0, _, _) = app.hits.tabs[0];
    let y = app.hits.tabs_y;
    click(&mut app, tab0, y);
    assert_eq!(app.active_tab, 0);
    // The wheel over the schema moves its selection.
    let schema = app.hits.schema.unwrap();
    let before = app.connection.schema.as_ref().unwrap().selected;
    app.handle_mouse(MouseEvent {
        kind: MouseEventKind::ScrollDown,
        column: schema.x + 2,
        row: schema.y + 4,
        modifiers: KeyModifiers::NONE,
    });
    assert_ne!(app.connection.schema.as_ref().unwrap().selected, before);
    // Clicking the filter line starts typing in it.
    let filter = app.hits.schema_filter;
    click(&mut app, filter.x + 3, filter.y);
    assert_eq!(app.focus, Focus::Schema);
    assert!(app.connection.schema.as_ref().unwrap().filter_editing);
    app.handle_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::NONE));
    assert_eq!(app.connection.schema.as_ref().unwrap().filter.text(), "u");
}

#[test]
fn ctrl_arrows_resize_the_panes() {
    let (mut app, _dir) = query_app();
    let _ = render(&mut app, 120, 30);
    let (editor, schema) = (app.hits.editor.height, app.hits.schema.unwrap().width);
    let ctrl = |app: &mut App, code| app.handle_key(KeyEvent::new(code, KeyModifiers::CONTROL));
    ctrl(&mut app, KeyCode::Down);
    ctrl(&mut app, KeyCode::Down);
    let _ = render(&mut app, 120, 30);
    assert!(app.hits.editor.height > editor, "the editor grows");
    // In the editor Ctrl+→ moves by word; from the navigator it resizes.
    ctrl(&mut app, KeyCode::Right);
    let _ = render(&mut app, 120, 30);
    assert_eq!(app.hits.schema.unwrap().width, schema);
    app.focus = Focus::Schema;
    ctrl(&mut app, KeyCode::Right);
    let _ = render(&mut app, 120, 30);
    assert_eq!(app.hits.schema.unwrap().width, schema + 2);
    for _ in 0..40 {
        ctrl(&mut app, KeyCode::Left);
        let _ = render(&mut app, 120, 30);
    }
    assert_eq!(app.hits.schema.unwrap().width, 15, "down to a minimum");
    for _ in 0..40 {
        ctrl(&mut app, KeyCode::Up);
    }
    assert_eq!(app.panes.editor_percent, 10);
}

#[test]
fn right_click_opens_a_menu_at_the_pointer() {
    use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    let (mut app, _dir) = query_app();
    let _ = render(&mut app, 100, 30);
    let rows = app.hits.result_rows;
    let click = |kind, x, y| MouseEvent { kind, column: x, row: y, modifiers: KeyModifiers::NONE };
    app.handle_mouse(click(MouseEventKind::Down(MouseButton::Right), rows.x + 10, rows.y + 1));
    assert_eq!((app.focus, app.tab.grid.table_state.selected()), (Focus::Results, Some(1)));
    assert_eq!(app.menu.as_ref().unwrap().title, "RESULTS");
    assert_snapshot!(render(&mut app, 100, 30));
    // Clicking an item does it: "Copy the row" (the 9th).
    let menu = app.hits.menu;
    app.handle_mouse(click(MouseEventKind::Down(MouseButton::Left), menu.x + 3, menu.y + 9));
    assert!(app.menu.is_none());
    assert_eq!(app.take_clipboard().as_deref(), Some("2\tben@example.com\tline one line two"));
}

#[test]
fn zoom_a_pane_and_click_result_tabs() {
    use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    let (mut app, _dir) = query_app();
    app.focus = Focus::Results;
    app.handle_key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::NONE));
    assert!(app.panes.zoomed);
    let screen = render(&mut app, 90, 16);
    assert!(screen.contains("ZOOMED") && !screen.contains("┌ SQL ─"), "only the results pane:\n{screen}");
    assert_snapshot!(screen);
    // Clicking the QUERY PLAN tab, then RESULT.
    let click = |x, y| MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: x,
        row: y,
        modifiers: KeyModifiers::NONE,
    };
    let (plan_x, _, _) = app.hits.result_tabs[2];
    app.handle_mouse(click(plan_x + 1, app.hits.result_tabs_y));
    assert_eq!(app.tab.result_tab, ResultTab::Plan);
    let _ = render(&mut app, 90, 16);
    let (rows_x, _, _) = app.hits.result_tabs[0];
    app.handle_mouse(click(rows_x + 1, app.hits.result_tabs_y));
    assert_eq!(app.tab.result_tab, ResultTab::Rows);
    // Esc returns to the normal layout; F12 zooms the editor.
    app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert!(!app.panes.zoomed);
    app.focus = Focus::Editor;
    app.handle_key(KeyEvent::new(KeyCode::F(12), KeyModifiers::NONE));
    assert!(app.panes.zoomed);
    assert!(!render(&mut app, 90, 16).contains("QUERY RESULT"));
    // Double-clicking a pane title toggles the zoom.
    app.handle_key(KeyEvent::new(KeyCode::F(12), KeyModifiers::NONE));
    let _ = render(&mut app, 90, 16);
    let title = app.hits.results;
    let press = click(title.x + 3, title.y);
    app.handle_mouse(press);
    app.handle_mouse(press);
    assert!(app.panes.zoomed && app.focus == Focus::Results);
}

#[test]
fn clicking_the_project_in_the_top_bar_switches_project() {
    use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    let (mut app, _dir) = query_app();
    app.focus = Focus::Results;
    let screen = render(&mut app, 100, 20);
    assert!(screen.lines().next().unwrap().contains("shop ▼"), "{screen}");
    // The editor's menu is about the editor: no project switching there.
    app.focus = Focus::Editor;
    app.handle_key(KeyEvent::new(KeyCode::F(10), KeyModifiers::NONE));
    assert!(app.menu.as_ref().is_some_and(|m| m.items.iter().all(|i| !i.label.contains("project"))));
    app.menu = None;
    let r = app.hits.project;
    app.handle_mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: r.x + 1,
        row: r.y,
        modifiers: KeyModifiers::NONE,
    });
    // The projects pane, beside the project's connections and settings.
    assert!(matches!(app.screen, Screen::Projects));
    assert_snapshot!(render(&mut app, 120, 20));
    // A click on a connection selects it and moves to that pane.
    let (list, _) = app.hits.manager_connections;
    app.handle_mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: list.x + 2,
        row: list.y + 1,
        modifiers: KeyModifiers::NONE,
    });
    assert!(matches!(app.screen, Screen::Connections));
    assert_eq!(app.manager.connection_selected, 1);
    assert!(render(&mut app, 120, 20).contains("Host  db.internal"));
}

#[test]
fn clicks_wait_for_a_connection_attempt_and_miss_the_borders() {
    use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    let click = |app: &mut App, column, row| {
        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column,
            row,
            modifiers: KeyModifiers::NONE,
        });
    };
    let (mut app, _dir) = app();
    app.screen = Screen::Connect;
    app.connection.form.focus = ConnectField::Host;
    let _ = render(&mut app, 120, 24);
    // The settings pane's top border isn't a field.
    let settings = app.hits.manager_settings;
    click(&mut app, settings.x + 4, settings.y);
    assert_eq!(app.connection.form.focus, ConnectField::Host);
    click(&mut app, settings.x + 4, settings.y + 1);
    assert_eq!(app.connection.form.focus, ConnectField::Name);
    // While connecting, the project's name in the top bar waits.
    app.connection.form.connecting = true;
    let project = app.hits.project;
    click(&mut app, project.x + 1, project.y);
    assert!(matches!(app.screen, Screen::Connect));
    app.connection.form.connecting = false;
    click(&mut app, project.x + 1, project.y);
    assert!(matches!(app.screen, Screen::Projects));
}

#[test]
fn a_long_script_draws_about_as_fast_as_a_short_one() {
    // Only the visible rows are styled, and the highlighting is reused
    // while the text doesn't change, so frames stay cheap anywhere in a
    // long script. Compared with a short script rather than a fixed time,
    // so a slow machine doesn't fail it.
    let frame = |text: &str, row_from_end: usize| {
        let (mut app, _dir) = query_app();
        app.tab.editor.set_text(text);
        app.focus = Focus::Editor;
        app.tab.editor.set_cursor((app.tab.editor.lines.len().saturating_sub(1 + row_from_end), 0));
        let _ = render(&mut app, 120, 40);
        let started = std::time::Instant::now();
        for _ in 0..5 {
            let _ = render(&mut app, 120, 40);
        }
        started.elapsed()
    };
    let row = "insert into t (id, name) values (1, 'name'), (2, 'other'); -- a comment\n";
    let short = frame(&row.repeat(20), 0);
    let long_script = row.repeat(20_000);
    for (place, from_end) in [("top", 19_999), ("bottom", 0)] {
        let long = frame(&long_script, from_end);
        assert!(long < short * 20 + std::time::Duration::from_millis(50), "{place}: {long:?} vs {short:?}");
    }
}

/// Random keys on every pane, popup and screen they lead to, drawing after
/// each at a random size: nothing may panic. The sandbox keeps it inside the
/// app (no connecting, `ssh` or shell commands), and results mix numbers,
/// text, NULLs and JSON so sorting and filtering get exercised. (It's a
/// general net: specific crashes keep their own tests, since a bad sort
/// comparison, for one, only shows on some inputs.)
/// `PODELIS_MONKEY_ROUNDS=500` runs longer.
#[tokio::test(flavor = "multi_thread")]
async fn random_keys_never_crash_the_app() {
    const CHARS: &[char] = &[
        'a', 'z', 'Q', '1', '9', ' ', '/', 's', 'd', 'f', 'e', 'c', 'm', 'k', 'j', 'h', 'l', 'n', 'p', 'r', 't', '.',
        ',', '\'', '"', '(', ')', '*', ';', '=', '<', '>', '-', '?', ':', '$', 'ž', '日', '[', ']', '{', '}', 'M', 'J',
        'C', 'i', 'g', 'G', 'x', 'w', 'u', 'v', 'y', 'b', 'q', '!', '#', '%', '&', '+', '@', '^', '~', '\\', '|',
    ];
    const KEYS: &[KeyCode] = &[
        KeyCode::Enter,
        KeyCode::Esc,
        KeyCode::Tab,
        KeyCode::BackTab,
        KeyCode::Up,
        KeyCode::Down,
        KeyCode::Left,
        KeyCode::Right,
        KeyCode::Home,
        KeyCode::End,
        KeyCode::PageUp,
        KeyCode::PageDown,
        KeyCode::Backspace,
        KeyCode::Delete,
        KeyCode::F(1),
        KeyCode::F(2),
        KeyCode::F(3),
        KeyCode::F(4),
        KeyCode::F(5),
        KeyCode::F(6),
        KeyCode::F(7),
        KeyCode::F(8),
        KeyCode::F(9),
        KeyCode::F(10),
        KeyCode::F(11),
        KeyCode::F(12),
    ];
    const MODS: &[KeyModifiers] = &[
        KeyModifiers::NONE,
        KeyModifiers::NONE,
        KeyModifiers::NONE,
        KeyModifiers::CONTROL,
        KeyModifiers::SHIFT,
        KeyModifiers::ALT,
    ];
    let rounds: u64 = std::env::var("PODELIS_MONKEY_ROUNDS").ok().and_then(|s| s.parse().ok()).unwrap_or(8);
    for round in 0..rounds {
        let mut seed = round.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
        let mut next = move |n: usize| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed % n as u64) as usize
        };
        let (mut app, dir) = query_app();
        app.sandbox = true;
        app.settings.export_dir = Some(dir.path().to_string_lossy().into_owned());
        app.show_results_for_tests(monkey_results(round));
        let (width, height) = (20 + next(140) as u16, 6 + next(50) as u16);
        let mut keys = Vec::new();
        for _ in 0..250 {
            let code = if next(2) == 0 { KeyCode::Char(CHARS[next(CHARS.len())]) } else { KEYS[next(KEYS.len())] };
            let key = KeyEvent::new(code, MODS[next(MODS.len())]);
            keys.push(key);
            let step = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                app.handle_key(key);
                if next(5) == 0 {
                    app.tick();
                }
                let _ = render(&mut app, width, height);
            }));
            assert!(
                step.is_ok(),
                "round {round} at {width}x{height} panicked after {:?}",
                &keys[keys.len().saturating_sub(15)..]
            );
        }
    }
}

#[test]
fn dragging_past_the_editors_top_scrolls_it_and_selects_on() {
    use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    let (mut app, _dir) = query_app();
    let text: Vec<String> = (1..=100).map(|i| format!("select {i};")).collect();
    app.tab.editor.set_text(&text.join("\n"));
    app.tab.editor.restore_cursor((60, 0), 50);
    let _ = render(&mut app, 100, 30);
    let area = app.hits.editor_text;
    let at = |kind, row| MouseEvent { kind, column: area.x + 2, row, modifiers: KeyModifiers::NONE };
    app.handle_mouse(at(MouseEventKind::Down(MouseButton::Left), area.y + 5));
    let (from, scroll) = (app.tab.editor.cursor_row, app.tab.editor.scroll_row);
    // Above the text, three times: it scrolls up and the selection follows.
    for _ in 0..3 {
        app.handle_mouse(at(MouseEventKind::Drag(MouseButton::Left), area.y - 1));
        let _ = render(&mut app, 100, 30);
    }
    assert_eq!(app.tab.editor.scroll_row, scroll - 3, "scrolled up a row each time");
    assert_eq!(app.tab.editor.cursor_row, app.tab.editor.scroll_row, "the cursor at the top of what's shown");
    let (start, end) = app.tab.editor.selection().unwrap();
    assert_eq!((start.0, end.0), (app.tab.editor.scroll_row, from), "selected from the top up to where it began");
    // Held there, the pointer still: it scrolls on by itself, selecting.
    assert!(app.drag_scroll_due().is_some());
    app.drag_scroll();
    assert_eq!(app.tab.editor.scroll_row, scroll - 4);
    assert_eq!(app.tab.editor.selection().unwrap().0.0, scroll - 4);
    // A dialog over the editor stops it.
    // (Opened by something else than a key, which would stop it anyway.)
    app.help_visible = true;
    app.drag_scroll();
    assert_eq!(app.tab.editor.scroll_row, scroll - 4, "not under a dialog");
    assert!(app.drag_scroll_due().is_none());
    app.help_visible = false;
    app.handle_mouse(at(MouseEventKind::Drag(MouseButton::Left), area.y - 1));
    // Let go: no more.
    app.handle_mouse(at(MouseEventKind::Up(MouseButton::Left), area.y - 1));
    assert!(app.drag_scroll_due().is_none());
}
