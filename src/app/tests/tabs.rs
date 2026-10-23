use super::*;

#[test]
fn drafts_survive_project_switches_and_restarts() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("c.toml");
    let (mut store, _) = ConnectionStore::load_from(path.clone());
    store.add_project("a").unwrap();
    store.add_project("b").unwrap();
    let mut app = App::new(store, None, Some(dir.path().to_path_buf()));
    app.open_project_by_name("a").unwrap();
    app.tab.editor.insert_str("select 'draft a'");
    app.open_project_by_name("b").unwrap();
    assert!(app.tab.editor.is_empty());
    app.open_project_by_name("a").unwrap();
    assert_eq!(app.tab.editor.text(), "select 'draft a'");
    app.save_draft();

    // A new app (restart) restores it too.
    let (store, _) = ConnectionStore::load_from(path);
    let mut app = App::new(store, None, Some(dir.path().to_path_buf()));
    app.open_project_by_name("a").unwrap();
    assert_eq!(app.tab.editor.text(), "select 'draft a'");

    // An emptied editor comes back empty.
    app.tab.editor.set_text("");
    app.save_draft();
    let (store, _) = ConnectionStore::load_from(dir.path().join("c.toml"));
    let mut app = App::new(store, None, Some(dir.path().to_path_buf()));
    app.open_project_by_name("a").unwrap();
    assert!(app.tab.editor.is_empty());
}

#[tokio::test]
async fn esc_stops_only_its_own_tabs_query_and_others_say_whose_runs() {
    let (mut app, dir) = test_app();
    app.data_dir = Some(dir.path().to_path_buf());
    let project = project_id(&app);
    let (a, b) = (file_profile("alpha", &dir.path().join("a.db")), file_profile("beta", &dir.path().join("b.db")));
    app.store.upsert(&project, None, a.clone()).unwrap();
    app.store.upsert(&project, None, b.clone()).unwrap();
    app.load_tabs(&project);
    connect_profile(&mut app, &a).await;
    app.handle_key(ctrl('o'));
    connect_profile(&mut app, &b).await;
    // A read that never ends, in alpha's tab: the other tabs stay free.
    app.handle_key(alt('1'));
    app.tab.editor.set_text("WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n) SELECT count(*) FROM n");
    app.handle_key(key(KeyCode::F(5)));
    assert!(app.run.executing);
    app.handle_key(alt('2'));
    assert_eq!((app.active_tab, app.connection.form.name.as_str()), (1, "beta"));
    // Esc here doesn't stop it: it says whose it is, and how to get there.
    app.focus = Focus::Editor;
    app.handle_key(key(KeyCode::Esc));
    assert!(!app.run.cancelling && app.run.executing);
    assert_eq!(said(&app), "Tab “Query 1” on alpha is running a query: Alt+1 goes to it, Esc there stops it");
    // A connection without a saved name (from a URL) is named by its address.
    app.handle_key(alt('1'));
    app.connection.form.name.clear();
    app.handle_key(alt('2'));
    app.focus = Focus::Editor;
    app.handle_key(key(KeyCode::Esc));
    assert!(said(&app).starts_with("Tab “Query 1” on ") && said(&app).contains("a.db"), "{}", said(&app));
    // (Once it's under way: a cancel before the run starts is cleared by it.)
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    // In its own tab, Esc stops it on its own connection.
    app.handle_key(alt('1'));
    app.handle_key(key(KeyCode::Esc));
    assert!(app.run.cancelling);
    tokio::time::timeout(std::time::Duration::from_secs(5), finish(&mut app)).await.expect("stopped");
    assert!(!app.run.executing && !app.run.cancel_failed);
    assert!(!app.notes.status.contains("Couldn't cancel"), "{}", app.notes.status);
}

#[tokio::test]
async fn runs_show_their_tab_at_once_and_f7_waits_for_a_running_query() {
    let (mut app, _dir) = connected_app(ConnectionMode::Normal).await;
    run_sql(&mut app, "create table t (id integer primary key)").await;
    // F7 shows QUERY PLAN as it starts, F5 the RESULT tab.
    app.tab.editor.set_text("select * from t");
    app.handle_key(key(KeyCode::F(7)));
    assert!(app.run.executing && app.tab.result_tab == ResultTab::Plan);
    finish(&mut app).await;
    // (Showing the plan moved to the results; back to typing.)
    app.focus = Focus::Editor;
    app.handle_key(key(KeyCode::F(5)));
    assert!(app.run.executing && app.tab.result_tab == ResultTab::Rows);
    // While it runs, F5 says why it does nothing, and F7 waits for it.
    app.handle_key(key(KeyCode::F(5)));
    assert!(app.notes.editor_note_is_error && said(&app).starts_with("A query is running"), "{}", said(&app));
    app.tab.editor.set_text("select id from t where id = 1");
    app.handle_key(key(KeyCode::F(7)));
    assert!(said(&app).starts_with("The plan follows"), "{}", said(&app));
    // In the editor's footer: the results pane still says it's running.
    assert_eq!(app.notes.status, "Running… (Esc to cancel)");
    // Waits for the query, then the plan made after it.
    finish(&mut app).await;
    assert_eq!(app.tab.result_tab, ResultTab::Plan);
    let plan = app.tab.plan.as_ref().map(|p| p.text.as_str()).expect("plan");
    assert!(plan.contains("SEARCH t"), "{plan}");

    // A failed query keeps its error in view: the plan isn't made.
    app.focus = Focus::Editor;
    app.tab.editor.set_text("select nope from t");
    app.handle_key(key(KeyCode::F(5)));
    app.handle_key(key(KeyCode::F(7)));
    finish(&mut app).await;
    assert!(
        app.notes.status.contains("nope") && app.notes.status.ends_with(" · the plan wasn't made"),
        "{}",
        app.notes.status
    );
    assert!(app.queued_plan.is_none() && app.tab.result_tab != ResultTab::Plan);
    // Esc stops both the query and the plan waiting for it.
    app.tab.editor.set_text("select * from t");
    app.handle_key(key(KeyCode::F(5)));
    app.handle_key(key(KeyCode::F(7)));
    assert!(app.queued_plan.is_some());
    app.handle_key(key(KeyCode::Esc));
    assert!(app.queued_plan.is_none());
    finish(&mut app).await;
}

#[tokio::test]
async fn other_tabs_can_be_used_while_a_read_runs() {
    let (mut app, _dir) = connected_app(ConnectionMode::Normal).await;
    run_sql(&mut app, "create table t (id integer primary key); insert into t values (1), (2)").await;
    // A page of one row: the read streams.
    app.settings.row_limit = 1;
    app.tab.editor.set_text("select id from t");
    app.handle_key(key(KeyCode::F(5)));
    assert!(app.run.executing && app.run_owner() == Some(0));
    // A new tab opens meanwhile; there, the run is another tab's.
    app.new_tab(None, "select 2", None);
    assert_eq!(app.active_tab, 1);
    assert!(
        app.notes.status.starts_with("Tab “Query 1” on :memory: is running a query: Alt+1 goes to it"),
        "{}",
        app.notes.status
    );
    assert_eq!(app.running_label(), None, "this tab's results aren't running");
    app.handle_key(key(KeyCode::F(5)));
    assert!(said(&app).ends_with("wait for it, or Esc there stops it"), "{}", said(&app));
    // (Esc here says whose it is: `esc_stops_only_its_own_tabs_query_and_others_say_whose_runs`.)
    // The outcome lands in its own tab, and the status bar says so here.
    finish(&mut app).await;
    assert!(app.tab.grid.results.is_none());
    assert!(app.notes.status.starts_with("Query 1: "), "{}", app.notes.status);
    // Its stream was closed while hidden: M runs it again for the rest.
    assert!(app.run.stream.is_none());
    app.switch_tab(0);
    assert!(app.tab.grid.cut_short && app.tab.grid.results.as_ref().unwrap().rows.len() == 1);
    app.focus = Focus::Results;
    app.handle_key(key(KeyCode::Char('M')));
    finish(&mut app).await;
    assert_eq!(app.tab.grid.results.as_ref().unwrap().rows.len(), 2);
    app.focus = Focus::Editor;
    // A write keeps every tab where it is.
    app.tab.editor.set_text("insert into t values (3)");
    app.handle_key(key(KeyCode::F(5)));
    assert!(app.run.executing && app.run_owner().is_none());
    app.switch_tab(1);
    assert_eq!(app.active_tab, 0);
    finish(&mut app).await;
}

#[tokio::test]
async fn result_tabs_messages_and_help() {
    let (mut app, _dir) = connected_app(ConnectionMode::Normal).await;
    run_sql(&mut app, "select 1").await;
    run_sql(&mut app, "select nope").await;
    assert_eq!(app.notes.log.iter().filter(|m| m.is_error).count(), 1);
    assert!(app.notes.log.iter().any(|m| m.text.starts_with("Connected to")));

    app.focus = Focus::Results;
    app.handle_key(key(KeyCode::Char(']')));
    assert_eq!(app.tab.result_tab, ResultTab::Messages);
    app.handle_key(key(KeyCode::Char('3')));
    assert_eq!(app.tab.result_tab, ResultTab::Plan);
    app.handle_key(key(KeyCode::Char('[')));
    assert_eq!(app.tab.result_tab, ResultTab::Messages);

    // F1 shows every key; any key but Esc/?/F1 is swallowed.
    app.handle_key(key(KeyCode::F(1)));
    assert!(app.help_visible);
    app.handle_key(key(KeyCode::Char('1')));
    assert!(app.help_visible && app.tab.result_tab == ResultTab::Messages);
    app.handle_key(key(KeyCode::Esc));
    assert!(!app.help_visible);
    // `?` outside the editor is Ctrl+K; `?` in its menu shows every key.
    app.handle_key(key(KeyCode::Char('?')));
    assert!(app.palette.as_ref().is_some_and(|p| !p.searching));
    app.handle_key(key(KeyCode::Char('?')));
    assert!(app.palette.is_none() && app.help_visible);
    app.handle_key(key(KeyCode::Esc));
    // In the editor `?` is just a character.
    app.focus = Focus::Editor;
    app.tab.editor.set_text("");
    app.handle_key(key(KeyCode::Char('?')));
    assert!(!app.help_visible && app.palette.is_none() && app.tab.editor.text() == "?");
}

#[tokio::test]
async fn tabs_keep_their_own_sql_and_results() {
    let (mut app, _dir) = connected_app(ConnectionMode::Normal).await;
    run_sql(&mut app, "select 1 as one").await;
    app.handle_key(ctrl('t'));
    assert_eq!((app.tabs.len(), app.active_tab), (2, 1));
    assert!(app.tab.editor.is_empty() && app.tab.grid.results.is_none());
    run_sql(&mut app, "select 2 as two").await;

    app.handle_key(KeyEvent::new(KeyCode::PageUp, KeyModifiers::CONTROL));
    assert_eq!(app.active_tab, 0);
    assert_eq!(app.tab.editor.text(), "select 1 as one");
    assert_eq!(app.tab.grid.results.as_ref().unwrap().columns, ["one"]);
    app.handle_key(KeyEvent::new(KeyCode::Char('2'), KeyModifiers::ALT));
    assert_eq!(app.tab.grid.results.as_ref().unwrap().columns, ["two"]);

    // Closing a tab with unsaved text asks first.
    app.handle_key(ctrl('w'));
    assert!(matches!(app.confirm.as_ref().unwrap().action, ConfirmAction::CloseTab));
    app.handle_key(key(KeyCode::Char('y')));
    assert_eq!((app.tabs.len(), app.active_tab), (1, 0));
    assert_eq!(app.tab.editor.text(), "select 1 as one");
}

#[tokio::test]
async fn save_rename_group_and_search_queries() {
    let (mut app, dir) = connected_app(ConnectionMode::Normal).await;
    // Persist to the temp dir so the restart below has something to load.
    app.data_dir = Some(dir.path().to_path_buf());
    let project = project_id(&app);
    app.load_tabs(&project);
    app.tab.editor.set_text("select * from customers where status = 'paused'");
    app.handle_key(ctrl('s'));
    assert!(app.tab_prompt.is_some());
    type_text(&mut app, "reports/churn");
    app.handle_key(key(KeyCode::Enter));
    assert_eq!(app.tabs[0].name, "churn");
    assert!(!app.tab_dirty(0));
    app.tab.editor.set_text("select 1");
    assert!(app.tab_dirty(0));
    app.handle_key(ctrl('s')); // saves in place, no prompt
    assert!(app.tab_prompt.is_none() && !app.tab_dirty(0));

    // F4 renames and moves it to another group.
    app.handle_key(key(KeyCode::F(4)));
    assert_eq!(app.tab_prompt.as_ref().unwrap().text.text(), "reports/churn");
    for _ in 0.."reports/churn".len() {
        app.handle_key(key(KeyCode::Backspace));
    }
    type_text(&mut app, "finance/churn by month");
    app.handle_key(key(KeyCode::Enter));
    assert_eq!(app.library.queries[0].path(), "finance/churn by month");
    assert_eq!(app.tabs[0].name, "churn by month");

    // History entries are searchable too; F3 opens one in a new tab.
    app.handle_key(ctrl('t'));
    run_sql(&mut app, "select 42 as answer").await;
    app.handle_key(key(KeyCode::F(3)));
    type_text(&mut app, "answer");
    assert!(
        matches!(app.library_items().as_slice(), [LibraryItem::History(sql, Some(_))] if sql == "select 42 as answer")
    );
    app.handle_key(key(KeyCode::Enter));
    assert_eq!((app.tabs.len(), app.tab.editor.text().as_str()), (3, "select 42 as answer"));

    // Searching by group finds the saved query; opening it switches to its tab.
    app.handle_key(key(KeyCode::F(3)));
    type_text(&mut app, "finance");
    assert!(matches!(app.library_items()[0], LibraryItem::Saved(_)));
    app.handle_key(key(KeyCode::Enter));
    assert_eq!(app.active_tab, 0);

    // Everything survives a restart: tabs, names, links, saved queries.
    app.save_draft();
    let (store, _) = ConnectionStore::load_from(dir.path().join("c.toml"));
    let mut again = App::new(store, None, Some(dir.path().to_path_buf()));
    again.open_project_by_name("p").unwrap();
    assert_eq!(
        again.tabs.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(),
        ["churn by month", "Query 1", "Query 2"]
    );
    assert_eq!(again.tab_text(1), "select 42 as answer");
    assert_eq!(again.library.queries.len(), 1);
    assert!(!again.tab_dirty(0));
}

#[tokio::test]
async fn deleting_a_saved_query_unlinks_its_tab() {
    let (mut app, _dir) = connected_app(ConnectionMode::Normal).await;
    app.tab.editor.set_text("select 1");
    app.handle_key(ctrl('s'));
    type_text(&mut app, "one");
    app.handle_key(key(KeyCode::Enter));
    app.handle_key(key(KeyCode::F(3)));
    app.handle_key(ctrl('d'));
    app.handle_key(key(KeyCode::Char('y')));
    assert!(app.library.queries.is_empty());
    assert_eq!(app.tabs[0].saved_id, None);
    app.handle_key(key(KeyCode::Esc));
    assert!(app.library_view.is_none());
}

#[test]
fn old_single_draft_becomes_the_first_tab() {
    let dir = tempfile::tempdir().unwrap();
    let (mut store, _) = ConnectionStore::load_from(dir.path().join("c.toml"));
    let id = store.add_project("p").unwrap();
    std::fs::create_dir_all(dir.path().join("drafts")).unwrap();
    std::fs::write(dir.path().join("drafts").join(format!("{id}.sql")), "select 'old draft'").unwrap();
    let mut app = App::new(store, None, Some(dir.path().to_path_buf()));
    app.open_project_by_name("p").unwrap();
    assert_eq!(app.tab.editor.text(), "select 'old draft'");
    app.save_draft();
    assert!(!dir.path().join("drafts").join(format!("{id}.sql")).exists(), "superseded by the tabs file");
}

#[tokio::test]
async fn the_pause_save_runs_in_the_background_and_the_newest_wins() {
    let dir = tempfile::tempdir().unwrap();
    let (mut store, _) = ConnectionStore::load_from(dir.path().join("c.toml"));
    let id = store.add_project("p").unwrap();
    let mut app = App::new(store, None, Some(dir.path().to_path_buf()));
    app.open_project_by_name("p").unwrap();
    app.screen = Screen::Query;
    let file = dir.path().join("drafts").join(format!("{id}.json"));
    let saved = || std::fs::read_to_string(&file).unwrap_or_default();

    type_text(&mut app, "select 'typed'");
    app.lint_due = Some(Instant::now());
    app.tick();
    let started = Instant::now();
    while !saved().contains("select 'typed'") {
        assert!(started.elapsed() < Duration::from_secs(5), "never saved");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }

    // A write prepared earlier but finishing later doesn't undo a newer one.
    let older = app.draft_job().unwrap();
    app.tab.editor.set_text("select 'newer'");
    app.draft_job().unwrap().write();
    older.write();
    assert!(saved().contains("select 'newer'"), "{}", saved());
}

#[test]
fn tabs_are_autosaved_when_typing_pauses() {
    let dir = tempfile::tempdir().unwrap();
    let (mut store, _) = ConnectionStore::load_from(dir.path().join("c.toml"));
    let id = store.add_project("p").unwrap();
    let mut app = App::new(store, None, Some(dir.path().to_path_buf()));
    app.open_project_by_name("p").unwrap();
    app.screen = Screen::Query;
    type_text(&mut app, "select 'unsaved work'");
    // Simulate the pause: the background syntax check (and autosave) is due.
    app.lint_due = Some(Instant::now());
    app.tick();
    let saved = std::fs::read_to_string(dir.path().join("drafts").join(format!("{id}.json"))).unwrap();
    assert!(saved.contains("select 'unsaved work'"), "{saved}");
}

#[test]
fn the_tab_list_finds_a_tab_by_name_connection_or_sql() {
    let (mut app, _dir) = test_app();
    app.screen = Screen::Query;
    for i in 2..=12 {
        app.handle_key(ctrl('t'));
        app.tab.editor.set_text(&format!("select {i} from orders_{i}"));
        app.tabs[app.active_tab].name = format!("report {i}");
    }
    app.handle_key(ctrl('k'));
    app.handle_key(key(KeyCode::Char('g')));
    let palette = app.palette.as_ref().expect("the tab list");
    assert!(palette.tabs_only && palette.selected == app.active_tab);
    // All of them, the shown one marked.
    assert_eq!(app.palette_items().len(), 12);
    assert!(app.tab_entry(app.active_tab).ends_with("(this one)"), "{}", app.tab_entry(app.active_tab));
    // By the SQL in a tab.
    type_text(&mut app, "orders_7");
    assert_eq!(app.palette_items(), [crate::app::palette::Command::SwitchTab(6)]);
    app.handle_key(key(KeyCode::Enter));
    assert!(app.palette.is_none());
    assert_eq!((app.active_tab, app.tabs[app.active_tab].name.as_str()), (6, "report 7"));
    // By name, then Esc keeps the tab.
    app.handle_key(ctrl('k'));
    app.handle_key(key(KeyCode::Char('g')));
    type_text(&mut app, "report 1");
    assert_eq!(app.palette_items().len(), 3, "reports 10, 11 and 12");
    // Backspace past the start stays in the list (no letters menu there).
    for _ in 0..12 {
        app.handle_key(key(KeyCode::Backspace));
    }
    assert!(app.palette.as_ref().is_some_and(|p| p.tabs_only && p.searching));
    app.handle_key(key(KeyCode::Esc));
    assert_eq!(app.active_tab, 6);
}

#[test]
fn the_statement_at_the_cursor_goes_to_the_library_alone() {
    let (mut app, _dir) = test_app();
    app.screen = Screen::Query;
    app.tab.editor.set_text("select 1;\nselect * from orders where id = 2;\nselect 3;");
    app.tab.editor.set_cursor((1, 5));
    app.run_action(Action::SaveStatement);
    type_text(&mut app, "reports/order two");
    app.handle_key(key(KeyCode::Enter));
    let saved = app.library.queries.iter().find(|q| q.name == "order two").unwrap();
    assert_eq!((saved.group.as_str(), saved.sql.as_str()), ("reports", "select * from orders where id = 2"));
    assert!(saved.changed.is_some());
    // The tab isn't tied to it, and keeps all its text.
    assert!(app.tabs[0].saved_id.is_none());
    assert!(app.tab.editor.text().starts_with("select 1;"));
    // A selection is what's saved, when there is one.
    app.tab.editor.select_offsets(0, 8);
    app.run_action(Action::SaveStatement);
    type_text(&mut app, "one");
    app.handle_key(key(KeyCode::Enter));
    assert_eq!(app.library.queries.iter().find(|q| q.name == "one").unwrap().sql, "select 1");
}

#[test]
fn closing_the_last_tab_forgets_its_text_for_good() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("c.toml");
    let (mut store, _) = ConnectionStore::load_from(path.clone());
    store.add_project("a").unwrap();
    let mut app = App::new(store, None, Some(dir.path().to_path_buf()));
    app.open_project_by_name("a").unwrap();
    app.tab.editor.insert_str("select 'gone'");
    app.save_draft();
    app.close_tab();
    assert!(app.tab.editor.is_empty());
    // A restart doesn't bring it back.
    let (store, _) = ConnectionStore::load_from(path);
    let mut app = App::new(store, None, Some(dir.path().to_path_buf()));
    app.open_project_by_name("a").unwrap();
    assert!(app.tab.editor.is_empty());
}

#[test]
fn snippets_start_with_everything_when_there_is_no_snippets_group() {
    let (mut app, _dir) = test_app();
    app.screen = Screen::Query;
    app.library.add("adhoc/top", "select * from orders").unwrap();
    app.run_action(Action::InsertSnippet);
    assert_eq!(app.library_view.as_ref().unwrap().query.text(), "");
    assert_eq!(app.library_items().len(), 1);
    app.library_view = None;
    app.library.add("snippets/now", "select now()").unwrap();
    app.run_action(Action::InsertSnippet);
    assert_eq!(app.library_view.as_ref().unwrap().query.text(), "snippets");
}

#[test]
fn the_library_finds_by_date() {
    let (mut app, _dir) = test_app();
    app.screen = Screen::Query;
    app.library.add("today", "select 1").unwrap();
    let old = app.library.add("old", "select 2").unwrap();
    app.library.queries.iter_mut().find(|q| q.id == old).unwrap().changed = Some(1_600_000_000);
    app.history.push("select 3");
    app.open_library();
    type_text(&mut app, "@today");
    let items = app.library_items();
    assert_eq!(items.len(), 2, "{items:?}");
    assert!(items.iter().any(|i| matches!(i, LibraryItem::History(sql, Some(_)) if sql == "select 3")));
    assert!(app.library_item_time(&items[0]).is_some());
    app.library_view = None;
    app.open_library();
    type_text(&mut app, "@2020-09 select");
    let items = app.library_items();
    assert_eq!(items, [LibraryItem::Saved(old)]);
}
