use super::*;
use crate::language::statements;

#[test]
fn delete_requires_confirmation() {
    let (mut app, _dir) = test_app();
    let id = project_id(&app);
    app.store.upsert(&id, None, ConnectionDraft { name: "x".into(), ..ConnectionDraft::new() }.to_profile()).unwrap();
    app.handle_key(key(KeyCode::Char('d')));
    assert!(app.manager.confirm_delete);
    app.handle_key(key(KeyCode::Char('n')));
    assert_eq!(app.profiles().len(), 1);
    assert!(!app.manager.confirm_delete);
}

#[cfg(feature = "sqlite")]
#[tokio::test]
async fn read_only_mode_refuses_writes() {
    let (mut app, _dir) = connected_app(ConnectionMode::ReadOnly).await;
    run_sql(&mut app, "select 1; delete from t where id = 1").await;
    assert!(!app.tab.run.executing && app.popups.confirm.is_none());
    assert!(app.notes.editor_note_is_error && said(&app).contains("Read-only"), "{}", said(&app));
    run_sql(&mut app, "select 42").await;
    assert_eq!(app.tab.grid.results.as_ref().unwrap().rows, vec![vec![Some("42".to_string())]]);
}

#[cfg(feature = "sqlite")]
#[tokio::test]
async fn destructive_statements_need_confirmation() {
    let (mut app, _dir) = connected_app(ConnectionMode::Normal).await;
    run_sql(&mut app, "create table t (id int); insert into t values (1), (2)").await;
    assert!(app.popups.confirm.is_none(), "plain writes run directly in normal mode");

    run_sql(&mut app, "delete from t").await;
    let confirm = app.popups.confirm.as_ref().expect("asks first");
    assert!(confirm.details[0].starts_with("DELETE without WHERE"));
    app.handle_key(key(KeyCode::Char('n')));
    assert!(app.popups.confirm.is_none() && !app.tab.run.executing);

    run_sql(&mut app, "delete from t").await;
    app.handle_key(key(KeyCode::Char('y')));
    finish(&mut app).await;
    assert_eq!(app.tab.grid.results.as_ref().unwrap().rows_affected, Some(2));
}

#[cfg(feature = "sqlite")]
#[tokio::test]
async fn production_mode_confirms_every_write() {
    let (mut app, _dir) = connected_app(ConnectionMode::Production).await;
    run_sql(&mut app, "select 1").await;
    assert!(app.popups.confirm.is_none());
    run_sql(&mut app, "create table t (id int)").await;
    assert!(app.popups.confirm.as_ref().unwrap().message.contains("PRODUCTION"));
}

/// A write under a DELIMITER's terminator, after a read: asked about on
/// production, as it would be alone (the server runs both).
#[cfg(feature = "mysql")]
#[tokio::test]
async fn a_write_behind_a_delimiter_asks_on_production() {
    let Some((mut app, _dir)) = super::live_mysql_app(ConnectionMode::Production).await else { return };
    run_sql(&mut app, "DELIMITER //\nSELECT 1; DELETE FROM podelis_nowhere//").await;
    let confirm = app.popups.confirm.as_ref().expect("the DELETE asks first");
    assert!(confirm.message.contains("PRODUCTION"), "{}", confirm.message);
}

#[cfg(feature = "sqlite")]
#[tokio::test]
async fn tx_modes_auto_manual_and_rollback_only() {
    // Ctrl+K x, then the mode's letter.
    let tx_mode = |app: &mut App, letter: char| {
        app.handle_key(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::CONTROL));
        app.handle_key(key(KeyCode::Char('x')));
        app.handle_key(key(KeyCode::Char(letter)));
    };
    let (mut app, _dir) = connected_app(ConnectionMode::Normal).await;
    assert_eq!(app.connection.tx_mode, TxMode::AutoCommit);
    run_sql(&mut app, "create table t (id int); insert into t values (1), (2)").await;
    assert_eq!(app.connection.txn, TxnState::Idle);

    // Manual commit: writes wait for COMMIT or ROLLBACK; reads run as usual.
    tx_mode(&mut app, 'm');
    assert_eq!(app.connection.tx_mode, TxMode::Manual);
    run_sql(&mut app, "insert into t values (3)").await;
    assert!(app.popups.review.is_some() && app.connection.txn == TxnState::Open);
    // Can't change the mode with a transaction open.
    app.popups.review = None;
    tx_mode(&mut app, 'r');
    assert!(app.popups.tx_picker.is_none() && app.connection.tx_mode == TxMode::Manual);
    app.handle_key(KeyEvent::new(KeyCode::F(8), KeyModifiers::SHIFT));
    finish(&mut app).await;
    run_sql(&mut app, "select count(*) from t").await;
    assert_eq!(app.tab.grid.results.as_ref().unwrap().rows, vec![vec![Some("2".to_string())]]);
    assert!(app.popups.review.is_none(), "reads don't open a review");
    // Scripts with their own transaction control are refused.
    run_sql(&mut app, "begin; delete from t where id = 1; commit").await;
    assert!(app.notes.editor_note_is_error && said(&app).contains("BEGIN/COMMIT"), "{}", said(&app));

    // Rollback-only: the write runs, its count is shown, and it is rolled back.
    tx_mode(&mut app, 'r');
    assert_eq!(app.connection.tx_mode, TxMode::RollbackOnly);
    run_sql(&mut app, "delete from t where id > 0").await;
    finish(&mut app).await;
    assert_eq!(app.connection.txn, TxnState::Idle);
    assert!(app.notes.status.contains("2 rows changed, then rolled back"), "{}", app.notes.status);
    // A failing rollback-only run is rolled back too, keeping its error.
    run_sql(&mut app, "insert into t values (9); insert into nope values (1)").await;
    finish(&mut app).await;
    assert!(
        app.notes.status_is_error && app.notes.status.contains("rolled back (rollback-only)"),
        "{}",
        app.notes.status
    );
    assert_eq!(app.connection.txn, TxnState::Idle);

    tx_mode(&mut app, 'a');
    assert_eq!(app.connection.tx_mode, TxMode::AutoCommit);
    run_sql(&mut app, "select count(*) from t").await;
    assert_eq!(app.tab.grid.results.as_ref().unwrap().rows, vec![vec![Some("2".to_string())]]);
}

#[cfg(feature = "sqlite")]
#[tokio::test]
async fn production_connections_start_in_review() {
    let (app, _dir) = connected_app(ConnectionMode::Production).await;
    assert_eq!(app.connection.tx_mode, TxMode::Manual);
}

#[cfg(feature = "sqlite")]
#[tokio::test]
async fn transaction_tracking_and_disconnect_confirmation() {
    let (mut app, _dir) = connected_app(ConnectionMode::Normal).await;
    run_sql(&mut app, "begin; create table t (id int)").await;
    assert_eq!(app.connection.txn, TxnState::Open);

    let ctrl = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL);
    app.handle_key(ctrl('d'));
    assert!(matches!(app.popups.confirm.as_ref().unwrap().action, ConfirmAction::Disconnect));
    app.handle_key(key(KeyCode::Esc));
    assert!(matches!(app.screen, Screen::Query));

    app.handle_key(ctrl('q'));
    assert!(matches!(app.popups.confirm.as_ref().unwrap().action, ConfirmAction::Quit));
    app.handle_key(key(KeyCode::Char('n')));
    assert!(!app.should_quit);

    run_sql(&mut app, "commit").await;
    assert_eq!(app.connection.txn, TxnState::Idle);
    // With no transaction it's still asked, without the warning colour.
    app.handle_key(ctrl('d'));
    let confirm = app.popups.confirm.as_ref().expect("asked");
    assert!(!confirm.danger && confirm.message.starts_with("Disconnect this tab from"), "{}", confirm.message);
    app.handle_key(key(KeyCode::Char('n')));
    assert!(app.popups.confirm.is_none() && app.connected(), "no: still connected");
    app.handle_key(ctrl('d'));
    app.handle_key(key(KeyCode::Char('y')));
    assert!(matches!(app.screen, Screen::Connections));
}

#[cfg(feature = "sqlite")]
#[tokio::test]
async fn null_refused_on_not_null_column_and_production_confirms() {
    let (mut app, _dir) = Box::pin(edit_cell(ConnectionMode::Normal, "select * from people", 1)).await;
    app.handle_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL));
    assert!(app.notes.status.contains("doesn't allow NULL") && app.cell_edit.is_some());
    app.handle_key(key(KeyCode::Esc));
    assert!(app.cell_edit.is_none());

    let (mut app, _dir) = Box::pin(edit_cell(ConnectionMode::Production, "select * from people", 1)).await;
    type_text(&mut app, "2");
    app.handle_key(key(KeyCode::Enter));
    let confirm = app.popups.confirm.as_ref().expect("production asks first");
    assert!(confirm.details[0].starts_with("UPDATE people SET name = 'Ben2'"), "{:?}", confirm.details);
    app.handle_key(key(KeyCode::Char('y')));
    finish(&mut app).await;
    assert_eq!(app.tab.grid.results.as_ref().unwrap().rows[1][1].as_deref(), Some("Ben2"));
}

/// On production, a read that would read a whole big table asks first;
/// below the limit, or once confirmed, it just runs.
#[cfg(feature = "sqlite")]
#[tokio::test]
async fn heavy_reads_on_production_ask_first() {
    let (mut app, _dir) = connected_app(ConnectionMode::Production).await;
    app.settings.guard_rows = 1000;
    let infos = statements::analyze(&parse("select 7", Engine::Sqlite));
    app.guarded("select 7".into(), 0, infos.clone(), None, &Ok(vec![("orders".into(), 52_000_000.0)]));
    let confirm = app.popups.confirm.as_ref().unwrap();
    assert_eq!(confirm.details, ["orders: about 52.0M rows, read in full"]);
    app.handle_key(key(KeyCode::Char('y')));
    finish(&mut app).await;
    assert_eq!(app.tab.grid.results.as_ref().unwrap().rows[0][0].as_deref(), Some("7"));
    // Small: no question.
    app.guarded("select 8".into(), 0, infos, None, &Ok(vec![("tiny".into(), 10.0)]));
    assert!(app.popups.confirm.is_none());
    finish(&mut app).await;
    assert_eq!(app.tab.grid.results.as_ref().unwrap().rows[0][0].as_deref(), Some("8"));
}

/// On production, a read whose plan couldn't be had (refused, the link)
/// isn't let through as if it read nothing: how much it reads isn't known,
/// so it asks, saying why; no, and nothing runs. One that's wrong itself
/// (a table not there) isn't asked about: its run says what's wrong.
#[cfg(feature = "sqlite")]
#[tokio::test]
async fn a_read_on_production_that_cant_be_planned_asks_first() {
    let (mut app, _dir) = connected_app(ConnectionMode::Production).await;
    app.settings.guard_rows = 1000;
    let infos = statements::analyze(&parse("select 7", Engine::Sqlite));
    app.guarded("select 7".into(), 0, infos, None, &Err("permission denied for EXPLAIN".into()));
    let confirm = app.popups.confirm.as_ref().expect("asked");
    assert!(confirm.message.contains("couldn't be checked"), "{}", confirm.message);
    assert_eq!(confirm.details, ["permission denied for EXPLAIN"]);
    app.handle_key(key(KeyCode::Char('n')));
    assert!(app.popups.confirm.is_none() && !app.tab.run.executing, "not run");
    // Wrong itself: run, and its error said.
    run_sql(&mut app, "select * from nowhere").await;
    assert!(app.popups.confirm.is_none());
    assert!(app.tab.run_error.as_ref().is_some_and(|e| e.contains("nowhere")), "{:?}", app.tab.run_error);
}

#[cfg(feature = "sqlite")]
#[tokio::test]
async fn writes_can_be_reviewed_in_a_transaction() {
    let (mut app, _dir) = connected_app(ConnectionMode::Production).await;
    // In save mode the confirmation offers `t` (review mode needs no asking).
    app.connection.tx_mode = TxMode::AutoCommit;
    let value = async |app: &mut App| {
        app.popups.confirm = None;
        app.tab.editor.set_text("select x from t");
        app.handle_key(key(KeyCode::F(5)));
        finish(app).await;
        app.tab.grid.results.as_ref().unwrap().rows[0][0].clone().unwrap()
    };
    app.tab.editor.set_text("create table t (x int); insert into t values (1)");
    app.handle_key(key(KeyCode::F(5)));
    app.handle_key(key(KeyCode::Char('y')));
    finish(&mut app).await;

    // `t` runs it after BEGIN and asks to commit or roll back.
    app.tab.editor.set_text("update t set x = 2 where x = 1");
    app.handle_key(key(KeyCode::F(5)));
    assert!(app.popups.confirm.as_ref().is_some_and(|c| c.offer_transaction));
    app.handle_key(key(KeyCode::Char('t')));
    finish(&mut app).await;
    assert_eq!(app.popups.review.as_ref().map(|r| r.rows_affected), Some(1));
    assert_eq!(app.connection.txn, TxnState::Open);
    // One statement was run: the BEGIN before it isn't counted.
    assert!(app.notes.status.contains("1 row") && !app.notes.status.contains("statements"), "{}", app.notes.status);
    app.handle_key(key(KeyCode::Char('r')));
    finish(&mut app).await;
    assert_eq!((app.connection.txn, app.notes.status.as_str()), (TxnState::Idle, "Rolled back"));
    assert_eq!(value(&mut app).await, "1");
    // History keeps the statement as typed, without the BEGIN.
    assert!(app.history.entries().iter().any(|h| h == "update t set x = 2 where x = 1"));

    // Esc keeps it open (checkable with SELECTs); F8 commits.
    app.tab.editor.set_text("update t set x = 3 where x = 1");
    app.handle_key(key(KeyCode::F(5)));
    app.handle_key(key(KeyCode::Char('t')));
    finish(&mut app).await;
    app.handle_key(key(KeyCode::Esc));
    assert!(app.popups.review.is_none() && app.connection.txn == TxnState::Open);
    assert_eq!(value(&mut app).await, "3");
    app.handle_key(key(KeyCode::F(8)));
    finish(&mut app).await;
    assert_eq!((app.connection.txn, app.notes.status.as_str()), (TxnState::Idle, "Committed"));
    assert_eq!(value(&mut app).await, "3");
}

#[cfg(feature = "mysql")]
#[tokio::test]
async fn f7_on_mysql_is_the_classic_table_whatever_explain_format_says() {
    let Some((mut app, _dir)) = live_app(Engine::MySql, "podelis_read_plan").await else { return };
    run_sql(&mut app, "create temporary table rp (x int); insert into rp values (1)").await;
    let db = app.connection.database.clone().unwrap();
    let version: Vec<u32> = db.version().split(|c: char| !c.is_ascii_digit()).take(3).flat_map(str::parse).collect();
    if version >= vec![8, 0, 32] {
        db.run("set session explain_format = tree", 1).await.expect("the setting exists from 8.0.32");
    }
    for statement in ["select x from rp where x = 1", "update rp set x = 2"] {
        app.tab.editor.set_text(statement);
        app.handle_key(key(KeyCode::F(7)));
        assert!(app.popups.confirm.is_none(), "{statement}: only planned, nothing to ask");
        finish(&mut app).await;
        assert!(app.tab.plan_swapped, "{statement}: a grid / {}", app.notes.status);
        let columns = &app.tab.grid.results.as_ref().unwrap().columns;
        assert!(["possible_keys", "key_len"].iter().all(|c| columns.iter().any(|n| n == c)), "{columns:?}");
    }
    assert_eq!(app.connection.txn, TxnState::Idle);
    run_sql(&mut app, "select x from rp").await;
    assert_eq!(app.tab.grid.results.as_ref().unwrap().rows, vec![vec![Some("1".to_string())]], "not run");
}

#[tokio::test]
async fn explain_analyze_of_a_write_is_rolled_back() {
    for engine in Engine::BUILT {
        let Some((mut app, _dir)) = live_app(engine, "podelis_analyze").await else { continue };
        // Where the engine runs this statement under EXPLAIN ANALYZE (not
        // MySQL: a one-table UPDATE it plans without running).
        let version = app.connection.database.as_ref().unwrap().version().to_string();
        if engine.dialect().is_none_or(|d| d.explain_analyze("update ea set x = 2", &version).is_none()) {
            continue;
        }
        run_sql(&mut app, "create temporary table ea (x int); insert into ea values (1)").await;
        app.tab.editor.set_text("update ea set x = 2");
        app.handle_key(KeyEvent::new(KeyCode::F(7), KeyModifiers::SHIFT));
        assert!(app.popups.confirm.is_some(), "{engine:?}: a write asks first");
        app.handle_key(key(KeyCode::Char('y')));
        finish(&mut app).await;
        let plan = app.tab.plan.clone().unwrap_or_default().text;
        let ran = crate::db::tests::test_sql(engine).analyzed_shows;
        assert!(plan.contains(ran), "{engine:?}: {plan} / {}", app.notes.status);
        assert_eq!(app.connection.txn, TxnState::Idle, "{engine:?}");
        run_sql(&mut app, "select x from ea").await;
        assert_eq!(app.tab.grid.results.as_ref().unwrap().rows, vec![vec![Some("1".to_string())]], "{engine:?}");
    }
}

#[cfg(feature = "sqlite")]
#[tokio::test]
async fn quitting_from_the_panes_asks_about_an_open_transaction() {
    let (mut app, dir) = test_app();
    let a = file_profile("alpha", &dir.path().join("a.db"));
    connect_profile(&mut app, &a).await;
    run_sql(&mut app, "begin").await;
    assert_eq!(app.connection.txn, TxnState::Open);
    show_projects(&mut app);
    for quit in [ctrl('q'), ctrl('c')] {
        app.handle_key(quit);
        assert!(!app.should_quit);
        assert!(matches!(app.popups.confirm.as_ref().unwrap().action, ConfirmAction::Quit));
        app.handle_key(key(KeyCode::Char('n')));
    }
    // A plain `q` quits nothing: it's pressed by accident.
    app.handle_key(key(KeyCode::Char('q')));
    assert!(!app.should_quit && app.popups.confirm.is_none());
    app.handle_key(key(KeyCode::Right));
    app.handle_key(ctrl('q'));
    assert!(!app.should_quit && app.popups.confirm.is_some());
}

#[test]
fn connect_form_sets_the_transaction_mode() {
    let mut form = ConnectForm::new();
    assert_eq!(form.tx_mode(), TxMode::AutoCommit);
    // Untouched, it follows the mode: production starts in manual commit.
    form.cycle_mode(true);
    assert_eq!((form.mode, form.tx_mode()), (ConnectionMode::Production, TxMode::Manual));
    // Only a choice is saved; the mode's default isn't.
    assert_eq!(form.to_profile().transaction, None);
    form.cycle_transaction(true);
    assert_eq!(form.tx_mode(), TxMode::RollbackOnly);
    // A choice stays when the mode changes.
    form.cycle_mode(false);
    assert_eq!((form.mode, form.tx_mode()), (ConnectionMode::Normal, TxMode::RollbackOnly));
    let profile = form.to_profile();
    assert_eq!(profile.transaction, Some(TxMode::RollbackOnly));
    assert_eq!(ConnectForm::from_profile(&profile, String::new()).tx_mode(), TxMode::RollbackOnly);
}

#[cfg(feature = "sqlite")]
#[tokio::test]
async fn a_connection_starts_in_its_transaction_mode() {
    let (mut app, _dir) = test_app();
    app.connection.form.engine = file_engine();
    app.connection.form.host = ":memory:".into();
    app.connection.form.transaction = Some(TxMode::RollbackOnly);
    let db = Database::connect(app.connection.form.params().unwrap()).await.unwrap();
    let generation = app.generation;
    app.handle_db_event(DbEvent::connection(generation, DbEventKind::Connected(db)));
    assert_eq!(app.connection.tx_mode, TxMode::RollbackOnly);
}

/// The sandbox: writes build on each other in one transaction, a failed
/// step rolls back alone, the last step can be undone, and it ends as a
/// script (nothing kept) or run for real (after a commit asked for).
#[cfg(feature = "sqlite")]
#[tokio::test]
async fn a_sandbox_keeps_steps_until_it_is_ended() {
    let (mut app, _dir) = connected_app(ConnectionMode::Normal).await;
    run_sql(&mut app, "create table t (id integer)").await;
    app.connection.tx_mode = TxMode::Sandbox;
    let count = |app: &App| app.tab.grid.results.as_ref().unwrap().rows[0][0].clone();
    run_sql(&mut app, "insert into t values (1)").await;
    run_sql(&mut app, "insert into t values (2)").await;
    assert_eq!(app.connection.txn, TxnState::Open);
    assert_eq!(app.connection.sandbox.as_ref().unwrap().steps.len(), 2);
    // A step sees the ones before it; a read isn't a step.
    run_sql(&mut app, "select count(*) from t").await;
    assert_eq!(count(&app), Some("2".into()));
    // A failed step goes back to before it; the sandbox goes on.
    run_sql(&mut app, "insert into nope values (1)").await;
    finish(&mut app).await;
    assert!(app.notes.status.contains("step 3 failed"), "{}", app.notes.status);
    assert_eq!(app.connection.sandbox.as_ref().unwrap().steps.len(), 2);
    assert_eq!(app.connection.txn, TxnState::Open);
    // Open long, it says so once (its locks hold up others).
    app.connection.sandbox.as_mut().unwrap().started = Instant::now().checked_sub(Duration::from_mins(16));
    app.tick_running();
    assert!(app.notes.status.contains("has been open 16:00"), "{}", app.notes.status);
    assert!(app.take_notification().is_some());
    app.notes.status.clear();
    app.tick_running();
    assert!(app.notes.status.is_empty(), "once");
    // Ctrl+K x shows it, with what it holds; `u` undoes the last step.
    app.run_action(Action::TxMode);
    assert!(app.popups.sandbox_view.is_some());
    let locks = app.popups.sandbox_view.as_ref().unwrap().locks.clone().unwrap_or_default();
    assert!(locks.contains("write lock"), "{locks}");
    app.handle_key(key(KeyCode::Char('u')));
    finish(&mut app).await;
    assert_eq!(app.connection.sandbox.as_ref().unwrap().steps.len(), 1);
    run_sql(&mut app, "select count(*) from t").await;
    assert_eq!(count(&app), Some("1".into()));
    let summary = app.connection.sandbox.as_ref().unwrap().summary();
    assert_eq!(summary, [("t".to_string(), 1, false)]);
    // `s`: the steps as a script in a new tab; nothing kept.
    app.run_action(Action::TxMode);
    app.handle_key(key(KeyCode::Char('s')));
    finish(&mut app).await;
    assert!(app.connection.sandbox.is_none() && app.connection.txn == TxnState::Idle);
    assert!(app.tab.editor.text().contains("insert into t values (1);"), "{}", app.tab.editor.text());
    app.switch_tab(0);
    run_sql(&mut app, "select count(*) from t").await;
    assert_eq!(count(&app), Some("0".into()));
    // `r`: rolled back, then run for real in manual commit, asked to commit.
    run_sql(&mut app, "insert into t values (3)").await;
    app.run_action(Action::TxMode);
    app.handle_key(key(KeyCode::Char('r')));
    finish(&mut app).await;
    assert_eq!(app.connection.tx_mode, TxMode::Manual);
    assert!(app.popups.review.is_some(), "commit or roll back?");
    app.handle_key(key(KeyCode::Char('c')));
    finish(&mut app).await;
    run_sql(&mut app, "select count(*) from t").await;
    assert_eq!(count(&app), Some("1".into()));
}

/// On the servers: a failed step doesn't spoil a sandbox (PostgreSQL
/// would refuse everything after it until rolled back to its savepoint),
/// and MySQL refuses DDL there (it commits by itself).
#[tokio::test]
async fn a_sandbox_on_the_servers() {
    for engine in [Engine::Postgres, Engine::MySql] {
        let Some((mut app, _dir)) = live_app(engine, "podelis_sandbox").await else { continue };
        // (A database of its own, fresh: nothing to drop first.)
        run_sql(&mut app, "create table podelis_sb (id int primary key)").await;
        app.connection.tx_mode = TxMode::Sandbox;
        run_sql(&mut app, "insert into podelis_sb values (1)").await;
        run_sql(&mut app, "insert into podelis_sb values (1)").await;
        assert!(app.notes.status.contains("step 2 failed"), "{engine:?}: {}", app.notes.status);
        finish(&mut app).await;
        run_sql(&mut app, "insert into podelis_sb values (2)").await;
        run_sql(&mut app, "select count(*) from podelis_sb").await;
        let rows = &app.tab.grid.results.as_ref().unwrap().rows;
        assert_eq!(rows[0][0].as_deref(), Some("2"), "{engine:?}: both steps there");
        if engine == Engine::MySql {
            app.tab.editor.set_text("alter table podelis_sb add c int");
            app.handle_key(key(KeyCode::F(5)));
            assert!(said(&app).contains("saves schema changes at once"), "{engine:?}: {}", said(&app));
        }
        // Its window says what locks the session holds (MySQL: if it may
        // read performance_schema).
        app.run_action(Action::TxMode);
        until_event(&mut app, |k| matches!(k, DbEventKind::Background(Background::SandboxLocks(_)))).await;
        let locks = app.popups.sandbox_view.as_ref().unwrap().locks.clone().unwrap();
        assert!(locks.contains("holds") || locks.contains("can't be seen"), "{engine:?}: {locks}");
        if engine == Engine::Postgres {
            assert!(!locks.contains("holds 0"), "{locks}");
        }
        app.handle_key(key(KeyCode::Char('d')));
        finish(&mut app).await;
        assert_eq!(app.connection.txn, TxnState::Idle);
        app.connection.tx_mode = TxMode::AutoCommit;
        run_sql(&mut app, "select count(*) from podelis_sb").await;
        assert_eq!(app.tab.grid.results.as_ref().unwrap().rows[0][0].as_deref(), Some("0"), "{engine:?}: discarded");
    }
}

/// A SQLite file's sandbox works on a copy: the original isn't touched
/// (nor locked) meanwhile, the copy goes when it ends, and run for real
/// writes the original.
#[cfg(feature = "sqlite")]
#[tokio::test]
async fn a_sqlite_files_sandbox_works_on_a_copy() {
    let (mut app, dir) = test_app();
    app.data_dir = Some(dir.path().to_path_buf());
    let project = project_id(&app);
    let file = dir.path().join("shop.db");
    let profile = file_profile("shop", &file);
    app.store.upsert(&project, None, profile.clone()).unwrap();
    connect_profile(&mut app, &profile).await;
    run_sql(&mut app, "create table t (id integer)").await;
    app.connection.tx_mode = TxMode::Sandbox;
    app.tab.editor.set_text("insert into t values (1)");
    app.handle_key(key(KeyCode::F(5)));
    // The copy is made first, then the step runs on it.
    until_event(&mut app, |k| matches!(k, DbEventKind::SandboxCopied(_))).await;
    finish(&mut app).await;
    let copy = app.connection.sandbox.as_ref().unwrap().copy.as_ref().unwrap().path.clone();
    assert!(copy.exists() && copy.starts_with(dir.path().join("sandbox")));
    assert_ne!(copy.parent().unwrap(), dir.path().join("sandbox"), "a folder of its own");
    run_sql(&mut app, "select count(*) from t").await;
    assert_eq!(app.tab.grid.results.as_ref().unwrap().rows[0][0].as_deref(), Some("1"));
    // The original, from outside: untouched, and writable meanwhile.
    let outside = crate::db::Database::connect(ConnectForm::from_profile(&profile, String::new()).params().unwrap())
        .await
        .unwrap();
    assert_eq!(outside.run("select count(*) from t", 1).await.unwrap().rows[0][0].as_deref(), Some("0"));
    // Run for real: the original gets it; the copy is gone.
    app.run_action(Action::TxMode);
    app.handle_key(key(KeyCode::Char('r')));
    finish(&mut app).await;
    assert!(app.connection.sandbox.is_none());
    assert!(app.popups.review.is_some(), "commit or roll back?");
    app.handle_key(key(KeyCode::Char('c')));
    finish(&mut app).await;
    assert_eq!(outside.run("select count(*) from t", 1).await.unwrap().rows[0][0].as_deref(), Some("1"));
    for _ in 0..50 {
        if !copy.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(!copy.exists(), "the copy is deleted");
}

/// Starting a file's sandbox cleans up only the folders of sandboxes that
/// ended without doing it (a crash): not one that's open, in another tab
/// or another Podėlis (its lock held).
#[cfg(feature = "sqlite")]
#[tokio::test]
async fn a_sandbox_doesnt_remove_another_open_one() {
    let (mut app, dir) = test_app();
    app.data_dir = Some(dir.path().to_path_buf());
    let all = dir.path().join("sandbox");
    let (open, ended) = (all.join("open"), all.join("ended"));
    for folder in [&open, &ended] {
        std::fs::create_dir_all(folder).unwrap();
        std::fs::write(folder.join("copy.db"), "work").unwrap();
    }
    // An older Podėlis's copy, straight in the folder, with its WAL.
    let old = all.join("0123abcd.db");
    std::fs::write(&old, "old copy").unwrap();
    std::fs::write(all.join("0123abcd.db-wal"), "wal").unwrap();
    let days_ago = std::time::SystemTime::now() - Duration::from_hours(48);
    for file in [&old, &all.join("0123abcd.db-wal")] {
        std::fs::File::options().write(true).open(file).unwrap().set_modified(days_ago).unwrap();
    }
    // One an older Podėlis has open now: changed just now.
    let in_use = all.join("4567cdef.db");
    std::fs::write(&in_use, "in use").unwrap();
    // One a crash left as it was made (no lock yet), long ago; one being
    // made now.
    let (crashed, making) = (all.join("crashed"), all.join("making"));
    for folder in [&crashed, &making] {
        std::fs::create_dir_all(folder).unwrap();
        std::fs::write(folder.join("lock.new"), "").unwrap();
    }
    let long_ago = std::time::SystemTime::now() - Duration::from_hours(2);
    std::fs::File::open(&crashed).unwrap().set_modified(long_ago).unwrap();
    let held = std::fs::File::create(open.join("lock")).unwrap();
    held.lock().unwrap();
    std::fs::File::create(ended.join("lock")).unwrap();
    let project = project_id(&app);
    let profile = file_profile("shop", &dir.path().join("shop.db"));
    app.store.upsert(&project, None, profile.clone()).unwrap();
    connect_profile(&mut app, &profile).await;
    run_sql(&mut app, "create table t (id integer)").await;
    app.connection.tx_mode = TxMode::Sandbox;
    app.tab.editor.set_text("insert into t values (1)");
    app.handle_key(key(KeyCode::F(5)));
    until_event(&mut app, |k| matches!(k, DbEventKind::SandboxCopied(_))).await;
    finish(&mut app).await;
    assert!(app.connection.sandbox.as_ref().unwrap().copy.is_some());
    assert_eq!(std::fs::read_to_string(open.join("copy.db")).unwrap(), "work", "the open one's kept");
    assert!(!ended.exists(), "the ended one's gone");
    assert!(!crashed.exists() && making.exists(), "an hour old with no lock: gone; one being made: kept");
    assert!(!old.exists() && !all.join("0123abcd.db-wal").exists(), "an older Podėlis's copy is gone");
    assert!(in_use.exists(), "not one an older Podėlis has open");
    drop(held);
}

#[test]
fn the_sandbox_and_rollback_only_refuse_tables_a_rollback_cant_undo() {
    let (mut app, _dir) = test_app();
    app.connection.form.engine = crate::db::Engine::MySql;
    // MySQL's MyISAM table `audit` (from the catalog), InnoDB `orders`.
    let table =
        |schema: &str, table: &str| crate::app::session::Unrollable { schema: schema.into(), table: table.into() };
    // Not known yet (the catalog still loading, or it couldn't be read):
    // a write is refused, a read isn't.
    app.connection.tx_mode = TxMode::RollbackOnly;
    let unknown = app.unrollable_refusal("INSERT INTO orders VALUES (1)").unwrap();
    assert!(unknown.contains("isn't known"), "{unknown}");
    assert!(app.unrollable_refusal("SELECT * FROM audit").is_none());
    // Couldn't be read: said why, and that a reload tries again.
    app.connection.set_unrollable(Err("access denied".into()));
    let unread = app.unrollable_refusal("INSERT INTO orders VALUES (1)").unwrap();
    assert!(unread.contains("couldn't be read (access denied)") && unread.contains("Reload"), "{unread}");
    app.connection.unrollable = Some(vec![table("shop", "audit"), table("archive", "log")]);
    for mode in [TxMode::Sandbox, TxMode::RollbackOnly] {
        app.connection.tx_mode = mode;
        let refused = app.unrollable_refusal("INSERT INTO shop.`Audit` VALUES (1)").unwrap();
        assert!(refused.contains("Audit") && refused.contains("can't be rolled back"), "{refused}");
        assert!(app.unrollable_refusal("UPDATE orders SET x = 1 WHERE id = 2").is_none(), "InnoDB is fine");
        assert!(app.unrollable_refusal("INSERT INTO archive.log VALUES (1)").is_some(), "another database's");
        // A bare name: whichever database the session is in now (`USE`).
        assert!(app.unrollable_refusal("INSERT INTO log VALUES (1)").is_some(), "maybe archive's, after USE");
        assert!(app.unrollable_refusal("INSERT INTO other.audit VALUES (1)").is_none(), "named: another audit");
    }
    // Other modes say what they do: a write there is meant to stay.
    app.connection.tx_mode = TxMode::Manual;
    assert!(app.unrollable_refusal("INSERT INTO audit VALUES (1)").is_none());
}

/// A failed transaction isn't committed by the commit key (the server
/// would roll it back): it says to roll back. A COMMIT typed in one that
/// the server ends is said rolled back, not committed.
#[cfg(feature = "sqlite")]
#[tokio::test]
async fn a_failed_transaction_isnt_said_committed() {
    let (mut app, _dir) = connected_app(ConnectionMode::Normal).await;
    app.connection.txn = TxnState::Aborted;
    app.end_transaction(podelis_core::db::TxEnd::Commit);
    assert!(!app.tab.run.executing, "COMMIT isn't sent");
    assert!(app.notes.status.contains("can't be committed"), "{}", app.notes.status);
    // As PostgreSQL does: the transaction ends, the server rolls it back.
    app.connection.txn = TxnState::Aborted;
    run_sql(&mut app, "BEGIN; COMMIT").await;
    assert_eq!(app.connection.txn, TxnState::Idle);
    assert!(app.notes.status.contains("rolled back, not committed"), "{}", app.notes.status);
    assert!(app.notes.status_is_error);
}

/// A failed run whose `BEGIN` ran (where the server doesn't say, as on
/// MySQL and SQLite) leaves the transaction open, and said open: its
/// rollback is offered, and undoes what ran before the failure.
#[cfg(feature = "sqlite")]
#[tokio::test]
async fn a_failed_runs_begin_leaves_its_transaction_open() {
    let (mut app, _dir) = connected_app(ConnectionMode::Normal).await;
    run_sql(&mut app, "create table t (x int)").await;
    run_sql(&mut app, "begin; insert into t values (1); select * from missing").await;
    assert!(app.tab.run_error.is_some());
    assert_eq!(app.connection.txn, TxnState::Open, "the BEGIN ran");
    app.end_transaction(podelis_core::db::TxEnd::Rollback);
    finish(&mut app).await;
    assert_eq!(app.connection.txn, TxnState::Idle);
    run_sql(&mut app, "select count(*) from t").await;
    assert_eq!(app.tab.grid.results.as_ref().unwrap().rows[0][0].as_deref(), Some("0"), "rolled back");
}

/// A file's sandbox works on a copy: the copy's health is the tab's then,
/// and a late report of the original's isn't; once it ends, the other way
/// round.
#[cfg(feature = "sqlite")]
#[tokio::test]
async fn a_sandboxs_copy_reports_its_own_health() {
    use crate::db::{Health, HealthState};
    let (mut app, dir) = test_app();
    app.data_dir = Some(dir.path().to_path_buf());
    let project = project_id(&app);
    let profile = file_profile("shop", &dir.path().join("shop.db"));
    app.store.upsert(&project, None, profile.clone()).unwrap();
    connect_profile(&mut app, &profile).await;
    run_sql(&mut app, "create table t (id integer)").await;
    // The original reconnected once before (given up on, reopened).
    let original_db = app.connection.database.clone().unwrap();
    original_db.abandon_session();
    let _ = original_db.run("select 1", 1).await;
    assert_eq!(original_db.health().borrow().reconnects, 1);
    let id = app.connection.slots.active.unwrap();
    let follower = |app: &mut App| app.connection.slot_mut(id).unwrap().follower;
    let original = follower(&mut app);
    app.connection.tx_mode = TxMode::Sandbox;
    app.tab.editor.set_text("insert into t values (1)");
    app.handle_key(key(KeyCode::F(5)));
    until_event(&mut app, |k| matches!(k, DbEventKind::SandboxCopied(_))).await;
    finish(&mut app).await;
    let copy = follower(&mut app);
    assert_ne!(copy, original, "the copy's followed");
    let lost = || Health { next_check: None, ..Health::of(HealthState::Lost("gone".into())) };
    app.handle_db_event(DbEvent::conn(id, ConnEvent::Followed { follower: original, health: lost() }));
    assert!(!app.connection.state.is_lost(), "the original's report isn't the tab's now");
    app.handle_db_event(DbEvent::conn(id, ConnEvent::Followed { follower: copy, health: lost() }));
    assert!(app.connection.state.is_lost(), "the copy's is");
    let back = Health::of(HealthState::Live);
    app.handle_db_event(DbEvent::conn(id, ConnEvent::Followed { follower: copy, health: back }));
    // Ended: the original's followed again, anew.
    app.end_transaction(podelis_core::db::TxEnd::Rollback);
    finish(&mut app).await;
    assert!(app.connection.sandbox.is_none());
    let again = follower(&mut app);
    assert!(again != copy && again != original);
    // Followed again, its reconnect before isn't taken for a new one.
    let slot = app.connection.slot_mut(id).unwrap();
    assert_eq!((slot.reconnects_said, slot.reconnects_logged), (1, 1));
    let live = Health { reconnects: 1, ..Health::of(HealthState::Live) };
    app.handle_db_event(DbEvent::conn(id, ConnEvent::Followed { follower: again, health: live }));
    assert!(!said(&app).contains("reconnected"), "{}", said(&app));
    app.handle_db_event(DbEvent::conn(id, ConnEvent::Followed { follower: copy, health: lost() }));
    assert!(!app.connection.state.is_lost(), "the copy's late report isn't");
}

/// A file's sandbox whose transaction a reconnect lost goes like one
/// ended: the tab's back on the original (not left on the copy), and the
/// copy's folder is deleted.
#[cfg(feature = "sqlite")]
#[tokio::test]
async fn a_reconnect_ends_a_files_sandbox_on_the_original() {
    use crate::db::{Health, HealthState};
    let (mut app, dir) = test_app();
    app.data_dir = Some(dir.path().to_path_buf());
    let project = project_id(&app);
    let profile = file_profile("shop", &dir.path().join("shop.db"));
    app.store.upsert(&project, None, profile.clone()).unwrap();
    connect_profile(&mut app, &profile).await;
    run_sql(&mut app, "create table t (id integer)").await;
    let original = app.connection.database.clone().unwrap();
    app.connection.tx_mode = TxMode::Sandbox;
    app.tab.editor.set_text("insert into t values (1)");
    app.handle_key(key(KeyCode::F(5)));
    until_event(&mut app, |k| matches!(k, DbEventKind::SandboxCopied(_))).await;
    finish(&mut app).await;
    let copy = app.connection.sandbox.as_ref().unwrap().copy.as_ref().unwrap().path.clone();
    let id = app.connection.slots.active.unwrap();
    let follower = app.connection.slot_mut(id).unwrap().follower;
    let reconnected = Health { reconnects: 1, ..Health::of(HealthState::Reconnected) };
    app.handle_db_event(DbEvent::conn(id, ConnEvent::Followed { follower, health: reconnected }));
    assert!(app.connection.sandbox.is_none());
    assert!(app.connection.database.as_ref().unwrap().same(&original), "back on the original");
    for _ in 0..50 {
        if !copy.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(!copy.parent().unwrap().exists(), "the copy's folder deleted");
}
