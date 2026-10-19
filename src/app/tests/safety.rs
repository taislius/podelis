use super::*;

#[test]
fn delete_requires_confirmation() {
    let (mut app, _dir) = test_app();
    let id = project_id(&app);
    app.store.upsert(&id, None, ConnectForm { name: "x".into(), ..ConnectForm::new() }.to_profile()).unwrap();
    app.handle_key(key(KeyCode::Char('d')));
    assert!(app.manager.confirm_delete);
    app.handle_key(key(KeyCode::Char('n')));
    assert_eq!(app.profiles().len(), 1);
    assert!(!app.manager.confirm_delete);
}

#[tokio::test]
async fn read_only_mode_refuses_writes() {
    let (mut app, _dir) = connected_app(ConnectionMode::ReadOnly).await;
    run_sql(&mut app, "select 1; delete from t where id = 1").await;
    assert!(!app.run.executing && app.confirm.is_none());
    assert!(app.notes.editor_note_is_error && said(&app).contains("Read-only"), "{}", said(&app));
    run_sql(&mut app, "select 42").await;
    assert_eq!(app.tab.grid.results.as_ref().unwrap().rows, vec![vec![Some("42".to_string())]]);
}

#[tokio::test]
async fn destructive_statements_need_confirmation() {
    let (mut app, _dir) = connected_app(ConnectionMode::Normal).await;
    run_sql(&mut app, "create table t (id int); insert into t values (1), (2)").await;
    assert!(app.confirm.is_none(), "plain writes run directly in normal mode");

    run_sql(&mut app, "delete from t").await;
    let confirm = app.confirm.as_ref().expect("asks first");
    assert!(confirm.details[0].starts_with("DELETE without WHERE"));
    app.handle_key(key(KeyCode::Char('n')));
    assert!(app.confirm.is_none() && !app.run.executing);

    run_sql(&mut app, "delete from t").await;
    app.handle_key(key(KeyCode::Char('y')));
    finish(&mut app).await;
    assert_eq!(app.tab.grid.results.as_ref().unwrap().rows_affected, Some(2));
}

#[tokio::test]
async fn production_mode_confirms_every_write() {
    let (mut app, _dir) = connected_app(ConnectionMode::Production).await;
    run_sql(&mut app, "select 1").await;
    assert!(app.confirm.is_none());
    run_sql(&mut app, "create table t (id int)").await;
    assert!(app.confirm.as_ref().unwrap().message.contains("PRODUCTION"));
}

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
    assert!(app.review.is_some() && app.connection.txn == TxnState::Open);
    // Can't change the mode with a transaction open.
    app.review = None;
    tx_mode(&mut app, 'r');
    assert!(app.tx_picker.is_none() && app.connection.tx_mode == TxMode::Manual);
    app.handle_key(KeyEvent::new(KeyCode::F(8), KeyModifiers::SHIFT));
    finish(&mut app).await;
    run_sql(&mut app, "select count(*) from t").await;
    assert_eq!(app.tab.grid.results.as_ref().unwrap().rows, vec![vec![Some("2".to_string())]]);
    assert!(app.review.is_none(), "reads don't open a review");
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

#[tokio::test]
async fn production_connections_start_in_review() {
    let (app, _dir) = connected_app(ConnectionMode::Production).await;
    assert_eq!(app.connection.tx_mode, TxMode::Manual);
}

#[tokio::test]
async fn transaction_tracking_and_disconnect_confirmation() {
    let (mut app, _dir) = connected_app(ConnectionMode::Normal).await;
    run_sql(&mut app, "begin; create table t (id int)").await;
    assert_eq!(app.connection.txn, TxnState::Open);

    let ctrl = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL);
    app.handle_key(ctrl('d'));
    assert!(matches!(app.confirm.as_ref().unwrap().action, ConfirmAction::Disconnect));
    app.handle_key(key(KeyCode::Esc));
    assert!(matches!(app.screen, Screen::Query));

    app.handle_key(ctrl('q'));
    assert!(matches!(app.confirm.as_ref().unwrap().action, ConfirmAction::Quit));
    app.handle_key(key(KeyCode::Char('n')));
    assert!(!app.should_quit);

    run_sql(&mut app, "commit").await;
    assert_eq!(app.connection.txn, TxnState::Idle);
    // With no transaction it's still asked, without the warning colour.
    app.handle_key(ctrl('d'));
    let confirm = app.confirm.as_ref().expect("asked");
    assert!(!confirm.danger && confirm.message.starts_with("Disconnect this tab from"), "{}", confirm.message);
    app.handle_key(key(KeyCode::Char('n')));
    assert!(app.confirm.is_none() && app.connected(), "no: still connected");
    app.handle_key(ctrl('d'));
    app.handle_key(key(KeyCode::Char('y')));
    assert!(matches!(app.screen, Screen::Connections));
}

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
    let confirm = app.confirm.as_ref().expect("production asks first");
    assert!(confirm.details[0].starts_with("UPDATE people SET name = 'Ben2'"), "{:?}", confirm.details);
    app.handle_key(key(KeyCode::Char('y')));
    finish(&mut app).await;
    assert_eq!(app.tab.grid.results.as_ref().unwrap().rows[1][1].as_deref(), Some("Ben2"));
}

/// On production, a read that would read a whole big table asks first;
/// below the limit, or once confirmed, it just runs.
#[tokio::test]
async fn heavy_reads_on_production_ask_first() {
    let (mut app, _dir) = connected_app(ConnectionMode::Production).await;
    app.settings.guard_rows = 1000;
    let infos = analyze::analyze(&parse("select 7", Engine::Sqlite));
    app.guarded("select 7".into(), 0, infos.clone(), None, &[("orders".into(), 52_000_000.0)]);
    let confirm = app.confirm.as_ref().unwrap();
    assert_eq!(confirm.details, ["orders: about 52.0M rows, read in full"]);
    app.handle_key(key(KeyCode::Char('y')));
    finish(&mut app).await;
    assert_eq!(app.tab.grid.results.as_ref().unwrap().rows[0][0].as_deref(), Some("7"));
    // Small: no question.
    app.guarded("select 8".into(), 0, infos, None, &[("tiny".into(), 10.0)]);
    assert!(app.confirm.is_none());
    finish(&mut app).await;
    assert_eq!(app.tab.grid.results.as_ref().unwrap().rows[0][0].as_deref(), Some("8"));
}

#[tokio::test]
async fn writes_can_be_reviewed_in_a_transaction() {
    let (mut app, _dir) = connected_app(ConnectionMode::Production).await;
    // In save mode the confirmation offers `t` (review mode needs no asking).
    app.connection.tx_mode = TxMode::AutoCommit;
    let value = async |app: &mut App| {
        app.confirm = None;
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
    assert!(app.confirm.as_ref().is_some_and(|c| c.offer_transaction));
    app.handle_key(key(KeyCode::Char('t')));
    finish(&mut app).await;
    assert_eq!(app.review.as_ref().map(|r| r.rows_affected), Some(1));
    assert_eq!(app.connection.txn, TxnState::Open);
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
    assert!(app.review.is_none() && app.connection.txn == TxnState::Open);
    assert_eq!(value(&mut app).await, "3");
    app.handle_key(key(KeyCode::F(8)));
    finish(&mut app).await;
    assert_eq!((app.connection.txn, app.notes.status.as_str()), (TxnState::Idle, "Committed"));
    assert_eq!(value(&mut app).await, "3");
}

#[tokio::test]
async fn explain_analyze_of_a_write_is_rolled_back() {
    for engine in Engine::BUILT {
        let Some((mut app, _dir)) = live_app(engine, "podelis_analyze").await else { continue };
        // Where the engine runs this statement under EXPLAIN ANALYZE (not
        // MySQL: a one-table UPDATE it plans without running).
        let version = app.connection.database.as_ref().unwrap().version().to_string();
        if engine.dialect().explain_analyze("update ea set x = 2", &version).is_none() {
            continue;
        }
        run_sql(&mut app, "create temporary table ea (x int); insert into ea values (1)").await;
        app.tab.editor.set_text("update ea set x = 2");
        app.handle_key(KeyEvent::new(KeyCode::F(7), KeyModifiers::SHIFT));
        assert!(app.confirm.is_some(), "{engine:?}: a write asks first");
        app.handle_key(key(KeyCode::Char('y')));
        finish(&mut app).await;
        let plan = app.tab.plan.clone().unwrap_or_default().text;
        assert!(plan.contains("actual time"), "{engine:?}: {plan} / {}", app.notes.status);
        assert_eq!(app.connection.txn, TxnState::Idle, "{engine:?}");
        run_sql(&mut app, "select x from ea").await;
        assert_eq!(app.tab.grid.results.as_ref().unwrap().rows, vec![vec![Some("1".to_string())]], "{engine:?}");
    }
}

#[tokio::test]
async fn quitting_from_the_panes_asks_about_an_open_transaction() {
    let (mut app, dir) = test_app();
    let a = file_profile("alpha", &dir.path().join("a.db"));
    connect_profile(&mut app, &a).await;
    run_sql(&mut app, "begin").await;
    assert_eq!(app.connection.txn, TxnState::Open);
    show_projects(&mut app);
    for quit in [key(KeyCode::Char('q')), ctrl('c')] {
        app.handle_key(quit);
        assert!(!app.should_quit);
        assert!(matches!(app.confirm.as_ref().unwrap().action, ConfirmAction::Quit));
        app.handle_key(key(KeyCode::Char('n')));
    }
    app.handle_key(key(KeyCode::Right));
    app.handle_key(key(KeyCode::Char('q')));
    assert!(!app.should_quit && app.confirm.is_some());
}

#[test]
fn connect_form_sets_the_transaction_mode() {
    let mut form = ConnectForm::new();
    assert_eq!(form.tx_mode(), TxMode::AutoCommit);
    // Untouched, it follows the mode: production starts in manual commit.
    form.cycle_mode(true);
    assert_eq!((form.mode, form.tx_mode()), (ConnectionMode::Production, TxMode::Manual));
    // Only a choice is saved; the mode's default isn't.
    assert_eq!(form.to_profile_for_tests().transaction, None);
    form.cycle_transaction(true);
    assert_eq!(form.tx_mode(), TxMode::RollbackOnly);
    // A choice stays when the mode changes.
    form.cycle_mode(false);
    assert_eq!((form.mode, form.tx_mode()), (ConnectionMode::Normal, TxMode::RollbackOnly));
    let profile = form.to_profile_for_tests();
    assert_eq!(profile.transaction, Some(TxMode::RollbackOnly));
    assert_eq!(ConnectForm::from_profile(&profile, String::new()).tx_mode(), TxMode::RollbackOnly);
}

#[tokio::test]
async fn a_connection_starts_in_its_transaction_mode() {
    let (mut app, _dir) = test_app();
    app.connection.form.engine = file_engine();
    app.connection.form.host = ":memory:".into();
    app.connection.form.transaction = Some(TxMode::RollbackOnly);
    let db = Database::connect(app.connection.form.params().unwrap()).await.unwrap();
    let generation = app.generation;
    app.handle_db_event(DbEvent { generation, run: None, kind: DbEventKind::Connected(db) });
    assert_eq!(app.connection.tx_mode, TxMode::RollbackOnly);
}
