use super::*;

#[test]
fn ctrl_y_copies_the_focused_field_in_the_settings() {
    let (mut app, _dir) = test_app();
    app.screen = Screen::Connect;
    app.connection.form.host = "db.example.com".into();
    app.connection.form.password = "secret".into();
    let ctrl = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL);
    app.connection.form.focus = ConnectField::Host;
    app.handle_key(ctrl('y'));
    assert_eq!(app.take_clipboard().as_deref(), Some("db.example.com"));
    app.connection.form.focus = ConnectField::Password;
    app.handle_key(ctrl('y'));
    assert_eq!(app.take_clipboard(), None);
    app.connection.form.focus = ConnectField::Mode;
    app.handle_key(ctrl('y'));
    assert_eq!(app.take_clipboard().as_deref(), Some(app.connection.form.mode.label()));
    // Ctrl+C and Ctrl+Q quit.
    app.handle_key(ctrl('c'));
    assert!(app.should_quit);
    app.should_quit = false;
    app.handle_key(ctrl('q'));
    assert!(app.should_quit);
}

#[test]
fn truncated_status() {
    let (mut app, _dir) = test_app();
    let mut o = outcome(&["a"], &[&[Some("1")]]);
    o.total_rows = 5000;
    o.statements = 2;
    app.show_results(o);
    assert!(app.notes.status.starts_with("Showing first 1 of 5000 rows"));
    assert!(app.notes.status.ends_with("· 2 statements"));
}

#[test]
fn ctrl_c_copies_on_the_query_screen_and_ctrl_q_quits() {
    let (mut app, _dir) = test_app();
    app.screen = Screen::Query;
    app.tab.editor.insert_str("select 1\nfrom t\n");
    let ctrl = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL);
    app.handle_key(ctrl('a'));
    app.handle_key(ctrl('c'));
    assert_eq!(app.take_clipboard().as_deref(), Some("select 1\nfrom t\n"));
    // Nothing selected: the cursor's line.
    app.handle_key(key(KeyCode::Esc));
    app.tab.editor.set_cursor((1, 2));
    app.handle_key(ctrl('c'));
    assert_eq!((app.take_clipboard().as_deref(), said(&app)), (Some("from t"), "Copied line 2"));
    // An empty line has nothing to copy; Ctrl+C never quits here.
    app.tab.editor.set_cursor((2, 0));
    app.handle_key(ctrl('c'));
    assert!(app.take_clipboard().is_none() && !app.should_quit);
    assert_eq!(said(&app), "Nothing to copy here · Ctrl+Q quits");
    app.handle_key(ctrl('q'));
    assert!(app.should_quit);
}

#[test]
fn undo_redo_and_shift_selection_keys() {
    let (mut app, _dir) = test_app();
    app.screen = Screen::Query;
    type_text(&mut app, "select abc");
    app.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::SHIFT | KeyModifiers::CONTROL));
    assert_eq!(app.tab.editor.selected_text().as_deref(), Some("abc"));
    app.handle_key(key(KeyCode::Backspace));
    assert_eq!(app.tab.editor.text(), "select ");
    app.handle_key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::CONTROL));
    assert_eq!(app.tab.editor.text(), "select abc");
    app.handle_key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::CONTROL));
    assert_eq!(app.tab.editor.text(), "select ");
}

#[test]
fn ctrl_arrows_move_by_word_without_selecting() {
    let (mut app, _dir) = test_app();
    app.screen = Screen::Query;
    type_text(&mut app, "select abc from t");
    // After a click, which leaves an empty selection to drag from.
    app.tab.editor.set_anchor(Some(app.tab.editor.cursor()));
    for (code, at) in [(KeyCode::Left, (0, 16)), (KeyCode::Left, (0, 11)), (KeyCode::Right, (0, 15))] {
        app.handle_key(KeyEvent::new(code, KeyModifiers::CONTROL));
        assert_eq!(app.tab.editor.cursor(), at);
        assert_eq!(app.tab.editor.selected_text(), None);
    }
}

#[test]
fn messages_can_be_selected_copied_and_opened() {
    let (mut app, _dir) = test_app();
    app.screen = Screen::Query;
    app.log("Connected".into(), false);
    app.log("relation \"nowhere\" does not exist\nLINE 1: select * from nowhere".into(), true);
    app.focus = Focus::Results;
    app.tab.result_tab = ResultTab::Messages;
    // From the newest; Ctrl+C or `y` copies it whole.
    app.handle_key(key(KeyCode::Up));
    assert_eq!(app.notes.selected, Some(1));
    app.handle_key(key(KeyCode::Up));
    app.handle_key(key(KeyCode::Up));
    assert_eq!(app.notes.selected, Some(0));
    app.handle_key(key(KeyCode::Down));
    app.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
    assert!(app.take_clipboard().is_some_and(|c| c.contains("LINE 1:")));
    app.handle_key(KeyEvent::new(KeyCode::Char('Y'), KeyModifiers::SHIFT));
    assert!(
        app.take_clipboard()
            .is_some_and(|c| c.starts_with(&format!("{}  Connected\n", app.notes.log[0].time)) && c.contains("LINE 1:"))
    );
    app.handle_key(key(KeyCode::Enter));
    let view = app.cell_view.as_ref().unwrap();
    assert!(view.title.contains("error") && view.text.contains("LINE 1:"));
    app.handle_key(key(KeyCode::Char('y')));
    assert!(app.take_clipboard().is_some_and(|c| c.starts_with("relation")));
}

#[tokio::test]
async fn the_statement_at_the_cursor_is_selected_or_zoomed_alone() {
    let (mut app, _dir) = connected_app(ConnectionMode::Normal).await;
    app.tab.editor.set_text("select 'one';\n\nselect 'two'\n  as n;\n\nselect 'three';");
    app.tab.editor.set_cursor((3, 2));
    app.run_action(Action::SelectStatement);
    assert_eq!(app.tab.editor.selected_text().as_deref(), Some("select 'two'\n  as n"));
    assert_eq!(said(&app), "Selected the statement (2 of 3)");
    // Zoomed, F5 runs the statement in sight, not the whole script.
    app.tab.editor.clear_selection();
    app.run_action(Action::ZoomStatement);
    assert!(app.panes.statement_zoom && !app.panes.zoomed);
    app.handle_key(key(KeyCode::F(5)));
    finish(&mut app).await;
    let shown = app.tab.grid.results.as_ref().map(|r| r.rows.clone());
    assert_eq!(shown, Some(vec![vec![Some("two".to_string())]]));
    // Esc drops a selection first, then shows the whole text again.
    app.focus = Focus::Editor;
    app.tab.editor.select_offsets(0, 3);
    app.handle_key(key(KeyCode::Esc));
    assert!(app.panes.statement_zoom && app.tab.editor.selection().is_none());
    app.handle_key(key(KeyCode::Esc));
    assert!(!app.panes.statement_zoom);
    // Another pane taking the keys (an Explain's plan) leaves it open.
    app.run_action(Action::ZoomStatement);
    app.focus = Focus::Results;
    app.handle_key(key(KeyCode::Char(']')));
    assert!(app.panes.statement_zoom);
    app.run_action(Action::ZoomStatement);
    assert!(!app.panes.statement_zoom);
    // The pane zoom and this one don't go together.
    app.run_action(Action::ZoomStatement);
    app.run_action(Action::Zoom);
    assert!(app.panes.zoomed && !app.panes.statement_zoom);
}

#[tokio::test]
async fn autocomplete_from_catalog() {
    let (mut app, _dir) = connected_app(ConnectionMode::Normal).await;
    run_sql(&mut app, "create table users (id int, email text)").await;
    // DDL triggers a catalog reload; wait for it.
    while app.connection.schema.as_ref().is_none_or(|s| s.loading) {
        let event = app.poll_db_event().await.unwrap();
        app.handle_db_event(event);
    }

    app.tab.editor.set_text("select em");
    app.handle_key(key(KeyCode::Tab));
    let c = app.completion.as_ref().expect("Tab after a word opens completion");
    assert_eq!(c.items[0].label, "email");
    app.handle_key(key(KeyCode::Tab));
    assert_eq!(app.tab.editor.text(), "select email");
    assert!(app.completion.is_none());

    type_text(&mut app, " from users u where u.");
    let labels: Vec<_> = app.completion.as_ref().unwrap().items.iter().map(|i| i.label.as_str()).collect();
    assert_eq!(labels, ["email", "id"]);
    type_text(&mut app, "i");
    assert_eq!(app.completion.as_ref().unwrap().items.len(), 1);
    app.handle_key(key(KeyCode::Enter));
    assert_eq!(app.tab.editor.text(), "select email from users u where u.id");

    // Tab with no word before the cursor still switches focus.
    type_text(&mut app, " ");
    app.handle_key(key(KeyCode::Tab));
    assert_eq!(app.focus, Focus::Results);
}

#[tokio::test]
async fn rebound_keys_take_effect() {
    let (mut app, _dir) = connected_app(ConnectionMode::Normal).await;
    let overrides =
        std::collections::HashMap::from([("run".to_string(), crate::config::keys::KeySpec::One("Ctrl+G".into()))]);
    app.keymap = crate::config::keys::KeyMap::new(&overrides).0;
    app.tab.editor.set_text("select 7");
    app.handle_key(key(KeyCode::F(5)));
    assert!(!app.run.executing, "F5 is no longer bound to run");
    app.handle_key(ctrl('g'));
    finish(&mut app).await;
    assert_eq!(app.tab.grid.results.as_ref().unwrap().rows, vec![vec![Some("7".to_string())]]);
}

#[test]
fn find_replace_comment_and_format() {
    let (mut app, _dir) = test_app();
    app.screen = Screen::Query;
    app.tab.editor.set_text("select a from t where a = 1;\nselect A from u");
    app.handle_key(ctrl('f'));
    type_text(&mut app, "a");
    let (matches, current) = app.find_matches();
    assert_eq!((matches.len(), current), (3, Some(0)));
    app.handle_key(key(KeyCode::Enter));
    assert_eq!(app.find_matches().1, Some(1));
    app.handle_key(key(KeyCode::Up));
    assert_eq!(app.find_matches().1, Some(0));
    // Replace one, then all of the rest.
    app.handle_key(key(KeyCode::Tab));
    type_text(&mut app, "id");
    app.handle_key(key(KeyCode::Enter));
    assert_eq!(app.tab.editor.text(), "select id from t where a = 1;\nselect A from u");
    app.handle_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::ALT));
    assert_eq!(app.tab.editor.text(), "select id from t where id = 1;\nselect id from u");
    assert!(app.tab.editor.undo());
    app.handle_key(key(KeyCode::Esc));
    assert!(app.find.is_none());

    app.tab.editor.set_text("select 1");
    app.handle_key(KeyEvent::new(KeyCode::Char('7'), KeyModifiers::CONTROL));
    assert_eq!(app.tab.editor.text(), "-- select 1");
    app.handle_key(KeyEvent::new(KeyCode::Char('7'), KeyModifiers::CONTROL));
    app.tab.editor.set_text("select a, b from t join u on t.id = u.id where a > 1 order by b");
    app.handle_key(key(KeyCode::F(9)));
    assert_eq!(app.tab.editor.text(), "select a, b\nfrom t join u on t.id = u.id\nwhere a > 1\norder by b");
    // In a script, only the statement at the cursor.
    app.tab.editor.set_text("select a,b from t where x=1;\nselect c,d from u where y=2;\nselect e from v;");
    app.tab.editor.set_cursor((1, 3));
    app.handle_key(key(KeyCode::F(9)));
    let text = app.tab.editor.text();
    assert!(text.starts_with("select a,b from t where x=1;\n"), "{text}");
    assert!(text.contains("select c, d\nfrom u\nwhere y = 2;") && text.ends_with("select e from v;"), "{text}");
}

#[test]
fn command_palette_finds_and_runs_commands() {
    let (mut app, _dir) = test_app();
    app.screen = Screen::Query;
    app.tab.editor.set_text("select a,b from t");
    // Ctrl+K waits for a letter; its menu shows only after a pause.
    app.handle_key(ctrl('k'));
    assert!(app.palette.as_ref().is_some_and(|p| !p.menu) && app.notes.status == "^K…");
    assert!(app.palette_menu_due().is_some_and(|due| due > Instant::now()));
    app.show_palette_menu();
    assert!(app.palette.as_ref().is_some_and(|p| p.menu) && app.palette_menu_due().is_none());
    // `/` searches (a letter alone would run its Ctrl+K action).
    type_text(&mut app, "/form sql");
    let items = app.palette_items();
    assert_eq!(items.len(), 1);
    app.handle_key(key(KeyCode::Enter));
    assert!(app.palette.is_none());
    assert_eq!(app.tab.editor.text(), "select a, b\nfrom t");
    app.handle_key(ctrl('k'));
    type_text(&mut app, "/zzz");
    assert!(app.palette_items().is_empty());
    app.handle_key(key(KeyCode::Esc));
    assert!(app.palette.is_none());
}

#[tokio::test]
async fn following_a_foreign_key_shows_the_referenced_row() {
    let (mut app, _dir) = connected_app(ConnectionMode::Normal).await;
    run_sql(
        &mut app,
        "create table parent (id integer primary key, name text);
         create table child (id integer primary key, parent_id integer references parent (id));
         insert into parent values (1, 'one'), (2, 'two'); insert into child values (10, 2);",
    )
    .await;
    finish_catalog(&mut app).await;
    run_sql(&mut app, "select * from child").await;
    app.focus = Focus::Results;
    app.tab.grid.selected_col = 1;
    app.handle_key(key(KeyCode::Char('f')));
    // The foreign keys load first, then the row, in a pop-over: no new tab.
    while app.cell_view.is_none() {
        let event = app.poll_db_event().await.unwrap();
        app.handle_db_event(event);
    }
    let view = app.cell_view.as_ref().unwrap();
    assert_eq!(view.title, "→ parent · id = 2");
    assert!(view.text.contains("id    2") && view.text.contains("name  two"), "{}", view.text);
    assert_eq!(app.tabs.len(), 1);
    // `e` opens it in a tab of its own, and runs it.
    app.handle_key(key(KeyCode::Char('e')));
    finish(&mut app).await;
    assert_eq!(app.tabs[app.active_tab].name, "→ parent");
    assert_eq!(app.tab.editor.text(), "SELECT * FROM parent WHERE id = 2;");
    assert_eq!(app.tab.grid.results.as_ref().unwrap().rows, vec![vec![Some("2".to_string()), Some("two".to_string())]]);

    // While rows are still streaming (they hold the session): the stream
    // stops, and the row comes.
    run_sql(
        &mut app,
        "insert into child with recursive n(i) as (select 1 union all select i + 1 from n where i < 5000) \
         select i + 100, 1 from n",
    )
    .await;
    run_sql(&mut app, "select * from child").await;
    assert!(app.run.stream.is_some(), "still streaming");
    app.focus = Focus::Results;
    app.tab.grid.selected_col = 1;
    app.cell_view = None;
    app.handle_key(key(KeyCode::Char('f')));
    let shown = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while app.cell_view.is_none() {
            let event = app.poll_db_event().await.unwrap();
            app.handle_db_event(event);
        }
    })
    .await;
    assert!(shown.is_ok(), "the lookup waited for the stream");
    assert!(app.run.stream.is_none());

    // From a join too: the column is a foreign key in one of its tables.
    app.cell_view = None;
    run_sql(&mut app, "select c.id, c.parent_id, p.name from child c join parent p on p.id = c.parent_id").await;
    app.focus = Focus::Results;
    app.tab.grid.selected_col = 1;
    app.cell_view = None;
    app.handle_key(key(KeyCode::Char('f')));
    let got = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while app.cell_view.is_none() {
            let event = app.poll_db_event().await.unwrap();
            app.handle_db_event(event);
        }
    })
    .await;
    assert!(got.is_ok(), "{:?}", said(&app));
    assert_eq!(app.cell_view.as_ref().unwrap().title, "→ parent · id = 2");

    // Not a foreign key: says so, and which ones are.
    app.cell_view = None;
    app.focus = Focus::Results;
    app.tab.grid.selected_col = 2;
    app.handle_key(key(KeyCode::Char('f')));
    assert_eq!(said(&app), "Can't follow: name isn't a foreign key; these are: parent_id → parent");

    // A table written two ways (a self-join) is one table, not two.
    app.cell_view = None;
    run_sql(&mut app, "select c.id, c.parent_id from main.child c join child d on d.id = c.id").await;
    app.focus = Focus::Results;
    app.tab.grid.selected_col = 1;
    app.handle_key(key(KeyCode::Char('f')));
    while app.cell_view.is_none() {
        let event = app.poll_db_event().await.unwrap();
        app.handle_db_event(event);
    }
    assert_eq!(app.cell_view.as_ref().unwrap().title, "→ parent · id = 2");
    // An answer for another tab (or connection) than the one now shown:
    // dropped, so `e` can't run it on the wrong database.
    app.cell_view = None;
    let found = crate::app::follow::Followed {
        title: "parent · id = 2".into(),
        tab_name: "→ parent".into(),
        sql: "SELECT 1".into(),
        asked_from: (app.active_tab + 1, None),
    };
    app.followed(found, Ok(outcome(&["id"], &[&[Some("2")]])));
    assert!(app.cell_view.is_none());
}

#[tokio::test]
async fn enter_after_a_fully_typed_name_is_a_new_line() {
    let (mut app, _dir) = connected_app(ConnectionMode::Normal).await;
    run_sql(&mut app, "create table users (id int, email text)").await;
    finish_catalog(&mut app).await;
    app.tab.editor.set_text("select * from users u where ");
    app.focus = Focus::Editor;
    type_text(&mut app, "u.id");
    assert!(app.completion.is_some());
    app.handle_key(key(KeyCode::Enter));
    assert_eq!(app.tab.editor.text(), "select * from users u where u.id\n");
    assert!(app.completion.is_none());
}

#[tokio::test]
async fn menus_offer_what_each_pane_can_do() {
    let (mut app, _dir) = connected_app(ConnectionMode::Normal).await;
    run_sql(&mut app, "select 1 as a, 'x' as b union all select 2, 'y'").await;
    app.focus = Focus::Results;
    app.handle_key(key(KeyCode::Char('m')));
    let menu = app.menu.as_ref().expect("results menu");
    assert_eq!(menu.title, "RESULTS");
    assert!(menu.items.iter().any(|i| i.label == "Copy rows as a Markdown table" && i.keys == "C"));
    // An item's key works inside the menu, doing what the key does.
    app.handle_key(key(KeyCode::Char('Y')));
    assert!(app.menu.is_none());
    assert_eq!(app.take_clipboard().as_deref(), Some("1\tx"));
    // Esc closes; the editor menu lists actions with their keys.
    app.focus = Focus::Editor;
    app.handle_key(KeyEvent::new(KeyCode::F(10), KeyModifiers::SHIFT));
    let menu = app.menu.as_ref().unwrap();
    let labels: Vec<_> = menu.items.iter().map(|i| (i.label, i.keys.as_str())).collect();
    assert_eq!(menu.title, "EDITOR");
    assert_eq!(labels[..2], [("Paste", "Ctrl+V"), ("Run (the selection, or everything)", "F5 · Ctrl+R · Ctrl+Enter")]);
    app.handle_key(key(KeyCode::Esc));
    assert!(app.menu.is_none());
    // With a selection, Copy and Cut come first; Copy copies it.
    app.handle_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL));
    app.handle_key(KeyEvent::new(KeyCode::F(10), KeyModifiers::SHIFT));
    let labels: Vec<_> = app.menu.as_ref().unwrap().items.iter().map(|i| i.label).collect();
    assert_eq!(labels[..3], ["Copy", "Cut", "Paste"]);
    app.handle_key(key(KeyCode::Enter));
    assert_eq!(app.take_clipboard(), Some(app.tab.editor.text()));
    // The results pane without rows on show has its own menu, not the editor's.
    app.focus = Focus::Results;
    app.handle_key(key(KeyCode::Char('2')));
    app.handle_key(key(KeyCode::Char('m')));
    let menu = app.menu.as_ref().unwrap();
    assert_eq!((menu.title, menu.items[0].label), ("RESULTS", "Show the rows"));
}

#[tokio::test]
async fn tab_and_shift_tab_go_round_the_panes() {
    let (mut app, dir) = test_app();
    let project = project_id(&app);
    app.store.add_project("other").unwrap();
    app.store.upsert(&project, None, file_profile("alpha", &dir.path().join("a.db"))).unwrap();
    app.screen = Screen::Query;
    show_projects(&mut app);
    // Another project selected: Tab opens it, into its connections; then
    // back to the first one the same way.
    let other = app.store.projects[1].id.clone();
    app.manager.project_selected = 1;
    app.handle_key(key(KeyCode::Tab));
    assert!(matches!(app.screen, Screen::Connections));
    assert_eq!(app.current_project.as_deref(), Some(other.as_str()));
    app.handle_key(key(KeyCode::BackTab));
    app.manager.project_selected = 0;
    app.handle_key(key(KeyCode::Right));
    assert!(matches!(app.screen, Screen::Connections));
    assert_eq!(app.current_project.as_deref(), Some(project.as_str()));
    app.manager.connection_selected = 0;
    app.handle_key(key(KeyCode::Tab));
    read_password(&mut app).await;
    assert!(matches!(app.screen, Screen::Connect));
    // In the settings ↑↓ move between fields, Tab goes round to the projects.
    app.handle_key(key(KeyCode::Down));
    assert_eq!(app.connection.form.focus, ConnectField::Engine);
    app.handle_key(key(KeyCode::Tab));
    assert!(matches!(app.screen, Screen::Projects));
    // And back the other way.
    app.handle_key(key(KeyCode::BackTab));
    read_password(&mut app).await;
    assert!(matches!(app.screen, Screen::Connect));
    app.handle_key(key(KeyCode::BackTab));
    assert!(matches!(app.screen, Screen::Connections));
    app.handle_key(key(KeyCode::BackTab));
    assert!(matches!(app.screen, Screen::Projects));
}

#[test]
fn no_completions_is_said_in_the_editor_footer() {
    let (mut app, _dir) = test_app();
    app.screen = Screen::Query;
    type_text(&mut app, "select zzqx");
    app.handle_key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::CONTROL));
    assert!(app.completion.is_none());
    assert_eq!(app.notes.editor_note.as_deref(), Some("No completions"));
    assert!(app.notes.status.is_empty(), "not in the status bar: {}", app.notes.status);
    // Gone with the next key.
    app.handle_key(key(KeyCode::Char('y')));
    assert!(app.notes.editor_note.is_none());
}

#[test]
fn a_key_hides_the_reopening_card_and_only_then_counts() {
    let (mut app, _dir) = test_app();
    app.screen = Screen::Query;
    app.splash = Some(session::Splash::for_tests(true));
    app.handle_key(key(KeyCode::Char('x')));
    assert!(app.splash.is_none() && app.tab.editor.text().is_empty(), "the key only hid the card");
    // Before the card shows, typing goes through.
    app.splash = Some(session::Splash::for_tests(false));
    app.handle_key(key(KeyCode::Char('x')));
    assert!(app.splash.is_none() && app.tab.editor.text() == "x");
}

#[test]
fn space_twice_opens_the_command_menu_when_enabled() {
    let (mut app, _dir) = test_app();
    app.screen = Screen::Query;
    let space = |app: &mut App| app.handle_key(key(KeyCode::Char(' ')));

    // Off by default: two spaces are two spaces.
    type_text(&mut app, "select");
    space(&mut app);
    space(&mut app);
    assert_eq!(app.tab.editor.text(), "select  ");
    assert!(app.palette.is_none());

    // On: the second takes the first back and opens the menu.
    app.settings.leader_double_space = true;
    app.tab.editor.set_text("");
    type_text(&mut app, "select");
    space(&mut app);
    space(&mut app);
    assert_eq!(app.tab.editor.text(), "select");
    assert!(app.palette.is_some());
    app.handle_key(key(KeyCode::Esc));

    // Not when something was typed in between, or after a pause.
    type_text(&mut app, " a");
    space(&mut app);
    assert_eq!(app.tab.editor.text(), "select a ");
    app.last_space =
        app.last_space.map(|(at, pos)| (at.checked_sub(std::time::Duration::from_millis(500)).unwrap(), pos));
    space(&mut app);
    assert_eq!(app.tab.editor.text(), "select a  ");
    assert!(app.palette.is_none());
}

#[test]
fn esc_then_tab_leaves_the_editor_after_a_word() {
    let (mut app, _dir) = test_app();
    app.screen = Screen::Query;
    type_text(&mut app, "select");
    assert!(app.tab_completes());
    app.handle_key(key(KeyCode::Tab));
    assert_eq!(app.focus, Focus::Editor, "Tab after a word completes");
    app.completion = None;

    app.handle_key(key(KeyCode::Esc));
    assert!(!app.tab_completes(), "the hint now says Tab moves on");
    app.handle_key(key(KeyCode::Tab));
    assert_eq!(app.focus, Focus::Results);

    // Any other key in between: Tab completes again.
    app.focus = Focus::Editor;
    app.handle_key(key(KeyCode::Esc));
    type_text(&mut app, " x");
    app.handle_key(key(KeyCode::Tab));
    assert_eq!(app.focus, Focus::Editor);
}

#[test]
fn the_popup_in_front_takes_the_keys() {
    let (mut app, _dir) = test_app();
    app.screen = Screen::Query;
    assert_eq!(app.top_overlay(), None);
    // A session's full query opens over the activity monitor.
    app.activity = Some(activity::ActivityView::default());
    app.cell_view = Some(CellView::new("Query".into(), "select 1".into()));
    assert_eq!(app.overlays(), [Overlay::CellView, Overlay::Activity]);
    app.handle_key(key(KeyCode::Esc));
    assert!(app.cell_view.is_none() && app.activity.is_some());
    // A pipe prompt over the activity monitor gets the typing.
    app.pipe_prompt = Some(LineInput::default());
    app.handle_key(key(KeyCode::Char('?')));
    assert_eq!(app.pipe_prompt.as_ref().map(LineInput::text), Some("?"));
    assert!(app.palette.is_none() && app.popup_open());
}

#[test]
fn pastes_go_to_the_input_in_front() {
    let (mut app, _dir) = test_app();
    app.screen = Screen::Query;
    app.tab_prompt = Some(TabPrompt { kind: TabPromptKind::SaveAs, text: LineInput::new("reports/") });
    app.handle_paste("churn\n");
    assert_eq!(app.tab_prompt.as_ref().unwrap().text.text(), "reports/churn");
    assert_eq!(app.tab.editor.text(), "", "nothing reached the editor behind it");
    app.tab_prompt = None;
    app.pipe_prompt = Some(LineInput::default());
    app.handle_paste("grep -i\tvilnius");
    assert_eq!(app.pipe_prompt.as_ref().map(LineInput::text), Some("grep -i vilnius"));
}

#[test]
fn appended_sql_never_joins_the_statement_before_it() {
    let (mut app, _dir) = test_app();
    app.screen = Screen::Query;
    let after = |app: &mut App, before: &str| {
        app.tab.editor.set_text(before);
        app.append_statement("SELECT 2;");
        app.tab.editor.text()
    };
    assert_eq!(after(&mut app, "select 1"), "select 1;\n\nSELECT 2;");
    // What's there stays as it is.
    assert_eq!(after(&mut app, "select 1;\n\n\n"), "select 1;\n\n\nSELECT 2;");
    let script = "DELIMITER //\nCREATE PROCEDURE p() BEGIN SELECT 1; END //";
    assert_eq!(after(&mut app, script), format!("{script}\n\nSELECT 2;"));
    assert_eq!(after(&mut app, "select 1;\n-- later"), "select 1;\n-- later\n\nSELECT 2;");
    // Before a comment after it, not inside it.
    assert_eq!(after(&mut app, "select 1 -- note"), "select 1; -- note\n\nSELECT 2;");
    assert_eq!(after(&mut app, "select 1 /* x */\n-- y"), "select 1; /* x */\n-- y\n\nSELECT 2;");
    assert_eq!(after(&mut app, "-- only a note"), "-- only a note\n\nSELECT 2;");
    assert_eq!(after(&mut app, ""), "SELECT 2;");
    // One undo takes it back.
    app.tab.editor.set_text("select 1");
    app.append_statement("SELECT 2;");
    app.undo();
    assert_eq!(app.tab.editor.text(), "select 1");
}
