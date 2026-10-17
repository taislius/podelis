use super::*;

#[test]
fn params_validation() {
    let mut form = ConnectForm::new();
    assert!(form.params().is_ok());
    form.port = "abc".into();
    assert!(form.params().unwrap_err().contains("Port"));
    form.port = "0".into();
    assert!(form.params().is_err());
    form.port = "5432".into();
    form.host = "  ".into();
    assert!(form.params().unwrap_err().contains("Host"));
    form.host = " ::1 ".into();
    assert_eq!(form.params().unwrap().host, "::1");
}

#[test]
fn stale_events_are_ignored() {
    let (mut app, _dir) = test_app();
    app.screen = Screen::Query;
    app.run.executing = true;
    let stale_generation = app.generation;
    app.disconnect();
    assert!(!app.run.executing);
    app.handle_db_event(DbEvent {
        generation: stale_generation,
        run: None,
        kind: DbEventKind::QueryDone(outcome(&["a"], &[&[Some("1")]]), None, None),
    });
    assert!(app.tab.grid.results.is_none());
}

/// Statements that set a password aren't kept in the history.
#[tokio::test]
async fn history_leaves_out_passwords() {
    let (mut app, _dir) = connected_app(ConnectionMode::Normal).await;
    run_sql(&mut app, "select 1").await;
    run_sql(&mut app, "create user bob identified by 'hunter2'").await;
    run_sql(&mut app, "select 2").await;
    let kept = app.history.entries();
    assert!(kept.iter().any(|q| q == "select 2") && !kept.iter().any(|q| q.contains("hunter2")), "{kept:?}");
}

#[tokio::test]
async fn long_queries_announce_their_end_unless_you_are_watching() {
    let (mut app, _dir) = connected_app(ConnectionMode::Normal).await;
    let long_ago = || Some(Instant::now().checked_sub(Duration::from_secs(30)).unwrap());
    // A quick query: nothing.
    run_sql(&mut app, "select 1").await;
    assert_eq!(app.take_notification(), None);

    app.run.extra.started = long_ago();
    app.notify_if_long(|took| format!("took {}", took.as_secs()));
    assert_eq!(app.take_notification().as_deref(), Some("took 30"));

    // Looking at the terminal: you saw it end.
    app.set_terminal_focus(true);
    app.run.extra.started = long_ago();
    app.notify_if_long(|_| "x".into());
    assert_eq!(app.take_notification(), None);

    app.set_terminal_focus(false);
    app.settings.notify_after = 0;
    app.run.extra.started = long_ago();
    app.notify_if_long(|_| "x".into());
    assert_eq!(app.take_notification(), None);
}

#[tokio::test]
async fn lint_warnings_after_a_syntax_check_finds_nothing() {
    let (mut app, _dir) = connected_app(ConnectionMode::Normal).await;
    run_sql(&mut app, "create table t (id int)").await;
    app.tab.editor.set_text("select * from t where id = null");
    check_syntax_now(&mut app).await;
    let hint = app.tab.syntax_hint.clone().expect("a lint warning");
    assert_eq!(hint.source, HintSource::Lint("null-compare"));

    // A real error comes first.
    app.tab.editor.set_text("select * from nope where id = null");
    check_syntax_now(&mut app).await;
    assert!(app.tab.syntax_hint.as_ref().is_some_and(|h| h.source == HintSource::Server));

    // Without the server, the parser's error still wins over a lint.
    let (mut app, _dir) = test_app();
    app.tab.editor.set_text("selec 1; delete from t");
    check_syntax_now(&mut app).await;
    assert!(app.tab.syntax_hint.as_ref().is_some_and(|h| h.source == HintSource::Parser));
    app.tab.editor.set_text("delete from t");
    check_syntax_now(&mut app).await;
    assert!(app.tab.syntax_hint.as_ref().is_some_and(|h| h.source == HintSource::Lint("no-where")));

    app.settings.lint = false;
    check_syntax_now(&mut app).await;
    assert_eq!(app.tab.syntax_hint, None);
}

#[test]
fn next_warning_steps_through_lints_and_wraps() {
    let (mut app, _dir) = test_app();
    let text = "select * from t where a = null;\ndelete from t;\nselect 1";
    app.tab.editor.set_text(text);
    let at = |app: &App| (app.tab.editor.cursor_offset(), app.tab.syntax_hint.as_ref().and_then(|h| h.nth));
    app.tab.editor.set_cursor(app.tab.editor.pos_of_offset(0));
    app.run_action(Action::NextWarning);
    assert_eq!(at(&app), (text.find("a = null").unwrap(), Some((1, 2))));
    app.run_action(Action::NextWarning);
    assert_eq!(at(&app), (text.find("delete").unwrap(), Some((2, 2))));
    app.run_action(Action::NextWarning);
    assert_eq!(at(&app), (text.find("a = null").unwrap(), Some((1, 2))));

    app.tab.editor.set_text("select 1");
    app.tab.syntax_hint = None;
    app.run_action(Action::NextWarning);
    assert_eq!(app.tab.syntax_hint, None);
    assert!(app.status.contains("No lint warnings"), "{}", app.status);
}

#[tokio::test]
async fn parameters_are_asked_for_and_remembered() {
    let (mut app, _dir) = connected_app(ConnectionMode::Normal).await;
    run_sql(&mut app, "create table t (id integer, name text); insert into t values (1, 'a'), (2, 'b')").await;
    app.tab.editor.set_text("select name from t where id = :id or name = :name");
    app.handle_key(key(KeyCode::F(5)));
    assert!(!app.run.executing);
    let form = app.param_form.as_ref().unwrap();
    assert_eq!(form.params.len(), 2);
    type_text(&mut app, "2");
    app.handle_key(key(KeyCode::Down));
    app.handle_paste("O'Brien");
    app.handle_key(key(KeyCode::Enter));
    finish(&mut app).await;
    assert!(app.param_form.is_none());
    let results = app.tab.grid.results.as_ref().unwrap();
    assert_eq!(results.rows, vec![vec![Some("b".to_string())]]);

    // Values come back next time; Esc cancels without running.
    app.handle_key(key(KeyCode::F(5)));
    assert_eq!(app.param_form.as_ref().unwrap().values, ["2", "O'Brien"]);
    app.handle_key(key(KeyCode::Esc));
    assert!(app.param_form.is_none() && !app.run.executing);

    // An error in the filled SQL is marked at the right place in the editor.
    app.tab.editor.set_text("select :v from nope");
    app.handle_key(key(KeyCode::F(5)));
    type_text(&mut app, "12345678");
    app.handle_key(key(KeyCode::Enter));
    finish(&mut app).await;
    assert!(app.status_is_error);
}

/// Watch mode runs the query again, marks the rows that changed, and stops
/// on Esc or when another query runs.
#[tokio::test]
async fn watch_mode_marks_what_changed() {
    let (mut app, _dir) = connected_app(ConnectionMode::Normal).await;
    run_sql(&mut app, "create table jobs (id integer primary key, state text); insert into jobs values (1, 'queued')")
        .await;
    run_sql(&mut app, "select * from jobs order by id").await;
    app.start_watch(2);
    finish(&mut app).await;
    assert!(app.status.starts_with("Watching every 2 s · run 1"), "{}", app.status);

    let db = app.database.clone().unwrap();
    db.run("update jobs set state = 'done' where id = 1; insert into jobs values (2, 'queued')", 1).await.unwrap();
    app.watch.as_mut().unwrap().next = std::time::Instant::now();
    app.tick_running();
    finish(&mut app).await;
    assert!(app.status.contains("run 2 · 2 changed"), "{}", app.status);
    assert_eq!(app.tab.grid.changed_rows, std::collections::HashSet::from([0, 1]));

    app.focus = Focus::Results;
    app.handle_key(key(KeyCode::Esc));
    assert!(app.watch.is_none() && app.tab.grid.changed_rows.is_empty());
    assert_eq!(said(&app), "Stopped watching");

    // A write can't be watched; another query stops watching.
    app.start_watch(2);
    finish(&mut app).await;
    run_sql(&mut app, "select 1").await;
    assert!(app.watch.is_none());
    run_sql(&mut app, "update jobs set state = 'x' where id = 2").await;
    app.start_watch(2);
    assert!(app.status_is_error && app.watch.is_none(), "{}", app.status);

    // Esc clears a results filter first, and is vim's in the editor.
    run_sql(&mut app, "select * from jobs order by id").await;
    app.start_watch(2);
    finish(&mut app).await;
    app.tab.grid.filter.set("queued");
    app.handle_key(key(KeyCode::Esc));
    assert!(app.watch.is_some(), "the filter went, not the watch");
    // The palette stops it from anywhere.
    app.run_command(crate::app::palette::Command::Watch(0));
    assert!(app.watch.is_none());

    // It stays with its tab: another tab stops it, and a watch that finds
    // another query in its place stops instead of running it.
    app.start_watch(2);
    finish(&mut app).await;
    app.handle_key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL));
    assert!(app.watch.is_none(), "{}", app.status);
    run_sql(&mut app, "select 1").await;
    app.start_watch(2);
    finish(&mut app).await;
    app.tab.last_sql = Some(("delete from jobs where id = 1".into(), 0));
    app.watch.as_mut().unwrap().next = std::time::Instant::now();
    app.tick_running();
    assert!(app.watch.is_none() && !app.run.executing, "{}", app.status);
}
