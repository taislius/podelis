use super::*;

#[test]
fn toggling_engine_updates_default_port_only() {
    let mut form = ConnectForm::new();
    form.cycle_engine(true);
    assert_eq!(form.port, "3306");
    form.port = "13306".into();
    form.cycle_engine(false);
    assert_eq!(form.port, "13306");
}

#[test]
fn sqlite_form_shows_only_file_fields() {
    let mut form = ConnectForm::new();
    form.cycle_engine(false);
    assert_eq!(form.engine, Engine::Sqlite);
    assert!(form.host.is_empty() && form.port.is_empty());
    assert_eq!(form.visible_fields().len(), 7);
    assert!(form.params().unwrap_err().contains("file"));
    form.host = "/tmp/x.db".into();
    form.mode = ConnectionMode::ReadOnly;
    let params = form.params().unwrap();
    assert!(params.read_only && params.ssh.is_none());
    form.cycle_engine(false);
    assert_eq!((form.engine, form.host.as_str(), form.port.as_str()), (Engine::MySql, "/tmp/x.db", "3306"));
    // An empty file name becomes localhost again.
    form.cycle_engine(true);
    form.host.clear();
    form.cycle_engine(false);
    assert_eq!(form.host, "localhost");
}

#[test]
fn password_command_replaces_the_password() {
    let mut form = ConnectForm::new();
    form.password = "typed".into();
    assert_eq!(form.params().unwrap().password, "typed");
    form.password_command = "op read 'op://My Vault/db/password'".into();
    let params = form.params().unwrap();
    assert_eq!(params.password, "");
    assert_eq!(params.password_command, ["op", "read", "op://My Vault/db/password"]);
    // Saved as words, shown again as typed.
    let profile = form.to_profile_for_tests();
    assert_eq!(profile.password_command, params.password_command);
    assert_eq!(ConnectForm::from_profile(&profile, String::new()).password_command, form.password_command);
    form.password_command = "pass show 'db".into();
    assert!(form.params().unwrap_err().contains("unclosed"));
    form.password_command = "'' show".into();
    assert!(form.params().unwrap_err().contains("program is empty"));
    // A file has no password: the command isn't kept for it.
    form.password_command = "pass show db".into();
    form.cycle_engine(false);
    form.host = "/tmp/x.db".into();
    assert!(form.params().unwrap().password_command.is_empty());
}

#[test]
fn a_missing_keychain_is_blamed_only_without_another_password() {
    let mut form = ConnectForm::new();
    assert_eq!(form.connect_failure("refused"), "refused");
    form.keychain_note = Some("no Secret Service".into());
    assert_eq!(form.connect_failure("denied"), "denied (no saved password: no Secret Service)");
    form.password = "typed".into();
    assert_eq!(form.connect_failure("denied"), "denied");
    form.password.clear();
    form.password_command = "pass show db".into();
    assert_eq!(form.connect_failure("denied"), "denied");
}

#[tokio::test]
async fn picking_a_database_container_fills_in_a_new_connection() {
    let (mut app, _dir) = test_app();
    app.screen = Screen::Connections;
    app.handle_key(key(KeyCode::Char('c')));
    assert!(app.manager.containers.as_ref().is_some_and(|p| p.loading));
    let profile = ConnectionProfile {
        unknown: crate::config::store::Unknown::default(),
        name: "shop-db".into(),
        engine: Engine::Postgres,
        host: "localhost".into(),
        port: "5433".into(),
        user: "app".into(),
        database: "shop".into(),
        ssl_mode: crate::db::SslMode::Prefer,
        mode: ConnectionMode::Normal,
        transaction: None,
        ssh: String::new(),
        timeout: String::new(),
        password_command: vec![
            "docker".into(),
            "exec".into(),
            "shop-db".into(),
            "printenv".into(),
            "POSTGRES_PASSWORD".into(),
        ],
        agents: crate::db::AgentAccess::None,
        legacy_keychain: false,
    };
    let unpublished =
        crate::system::containers::Container { name: "hidden".into(), image: "mysql:9".into(), profile: None };
    let shop = crate::system::containers::Container {
        name: "shop-db".into(),
        image: "postgres:17".into(),
        profile: Some(profile),
    };
    app.containers_found(1, Ok(vec![unpublished, shop]));
    // Not published: nothing to connect to.
    app.handle_key(key(KeyCode::Enter));
    assert!(app.manager.containers.as_ref().and_then(|p| p.note.as_deref()).is_some_and(|m| m.contains("-p")));
    app.handle_key(key(KeyCode::Down));
    assert!(app.manager.containers.as_ref().is_some_and(|p| p.note.is_none()), "the note was about the other row");
    app.handle_key(key(KeyCode::Enter));
    assert!(app.manager.containers.is_none());
    let form = &app.connection.form;
    assert_eq!(app.screen, Screen::Connect);
    assert_eq!((form.name.as_str(), form.port.as_str(), form.user.as_str()), ("shop-db", "5433", "app"));
    assert!(form.original_name.is_none(), "a new connection");
    assert_eq!(form.password_command, "docker exec shop-db printenv POSTGRES_PASSWORD");
    // Esc closes the picker; so does a failure to list.
    app.screen = Screen::Connections;
    app.handle_key(key(KeyCode::Char('c')));
    // A late result of the earlier search is dropped.
    app.containers_found(1, Err("stale".into()));
    assert!(app.manager.containers.as_ref().is_some_and(|p| p.loading && p.error.is_none()));
    app.containers_found(2, Err("docker isn't running".into()));
    assert_eq!(app.manager.containers.as_ref().and_then(|p| p.error.as_deref()), Some("docker isn't running"));
    app.handle_key(key(KeyCode::Esc));
    assert!(app.manager.containers.is_none());
    // Picked again once saved: the saved connection, not a copy.
    let saved = app.connection.form.to_profile_for_tests();
    app.store.upsert(&project_id(&app), None, saved.clone()).unwrap();
    app.handle_key(key(KeyCode::Char('c')));
    let again = crate::system::containers::Container {
        name: "shop-db".into(),
        image: "postgres:17".into(),
        profile: Some(saved),
    };
    app.containers_found(3, Ok(vec![again]));
    app.handle_key(key(KeyCode::Enter));
    // Opened as `e` would: its saved password is read first.
    assert!(app.manager.loading_profile);
    // The real container search started by `c` may answer first.
    while app.manager.loading_profile {
        let event = app.poll_db_event().await.unwrap();
        app.handle_db_event(event);
    }
    assert_eq!(app.connection.form.original_name.as_deref(), Some("shop-db"));
    // Through an SSH tunnel it's another server: not the same connection.
    let mut tunneled = app.store.project(&project_id(&app)).unwrap().connections[0].clone();
    tunneled.ssh = "bastion".into();
    app.store.upsert(&project_id(&app), Some("shop-db"), tunneled).unwrap();
    app.screen = Screen::Connections;
    app.handle_key(key(KeyCode::Char('c')));
    let local = crate::system::containers::Container {
        name: "shop-db".into(),
        image: "postgres:17".into(),
        profile: Some(ConnectionProfile { ssh: String::new(), ..app.profiles()[0].clone() }),
    };
    app.containers_found(4, Ok(vec![local]));
    app.handle_key(key(KeyCode::Enter));
    assert_eq!(
        (app.connection.form.original_name.as_deref(), app.connection.form.name.as_str()),
        (None, "shop-db (2)")
    );
}

#[test]
fn ssh_destination_is_trimmed_into_params() {
    let mut form = ConnectForm::new();
    form.ssh = "  me@bastion  ".into();
    assert_eq!(form.params().unwrap().ssh.as_deref(), Some("me@bastion"));
    assert!(form.summary().ends_with("via me@bastion"));
}

#[test]
fn create_rename_and_delete_projects() {
    let dir = tempfile::tempdir().unwrap();
    let (store, _) = ConnectionStore::load_from(dir.path().join("c.toml"));
    let mut app = App::new(store, None, None);
    assert!(matches!(app.screen, Screen::Projects));

    // Enter on "+ New project" prompts; the new project opens right away.
    app.handle_key(key(KeyCode::Enter));
    type_text(&mut app, "shop");
    app.handle_key(key(KeyCode::Enter));
    assert!(matches!(app.screen, Screen::Connections));
    assert_eq!(app.project().unwrap().name, "shop");

    // Esc stays in the project: its tabs, not connected yet.
    app.handle_key(key(KeyCode::Esc));
    assert!(matches!(app.screen, Screen::Query));
    assert_eq!(app.project().unwrap().name, "shop");

    // The project list (as at a start with no project open): duplicate
    // names keep the prompt open.
    app.leave_project();
    assert!(matches!(app.screen, Screen::Projects));
    app.handle_key(key(KeyCode::Char('n')));
    type_text(&mut app, "SHOP");
    app.handle_key(key(KeyCode::Enter));
    assert!(app.manager.project_prompt.is_some());
    assert!(app.manager.project_message.as_ref().unwrap().1);
    app.handle_key(key(KeyCode::Esc));
    assert!(app.manager.project_prompt.is_none());

    app.manager.project_selected = 0;
    app.handle_key(key(KeyCode::Char('r')));
    assert_eq!(app.manager.project_prompt.as_ref().unwrap().text.text(), "shop");
    type_text(&mut app, "-api");
    app.handle_key(key(KeyCode::Enter));
    assert_eq!(app.store.projects[0].name, "shop-api");
    assert!(matches!(app.screen, Screen::Projects));

    app.handle_key(key(KeyCode::Char('d')));
    assert!(app.manager.confirm_delete_project);
    app.handle_key(key(KeyCode::Char('y')));
    assert_eq!(app.store.projects, Vec::<Project>::new());
    assert!(app.current_project.is_none());
}

#[test]
fn projects_keep_separate_connections_and_history() {
    let dir = tempfile::tempdir().unwrap();
    let (mut store, _) = ConnectionStore::load_from(dir.path().join("c.toml"));
    let a = store.add_project("a").unwrap();
    let b = store.add_project("b").unwrap();
    let conn = |name: &str| ConnectForm { name: name.into(), ..ConnectForm::new() }.to_profile();
    store.upsert(&a, None, conn("local")).unwrap();
    store.upsert(&b, None, conn("local")).unwrap();
    store.upsert(&b, None, conn("staging")).unwrap();
    let mut app = App::new(store, None, Some(dir.path().to_path_buf()));

    app.open_project_by_name("A").unwrap();
    assert_eq!(app.profiles().len(), 1);
    app.history.push("select 'a'");
    app.open_project_by_name("b").unwrap();
    assert_eq!(app.profiles().len(), 2);
    assert_eq!(app.history.older(""), None);
    app.open_project_by_name("a").unwrap();
    assert_eq!(app.history.older(""), Some("select 'a'"));
    assert_eq!(app.store.last_project.as_deref(), Some(a.as_str()));
    assert!(app.open_project_by_name("nope").is_err());
}

#[test]
fn new_app_preselects_last_project_and_reports_migration() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("c.toml");
    std::fs::write(
        &path,
        "[[connections]]\nname = \"x\"\nengine = \"Postgres\"\nhost = \"h\"\nport = \"1\"\n\
         user = \"u\"\ndatabase = \"d\"\n",
    )
    .unwrap();
    let (mut store, warning) = ConnectionStore::load_from(path);
    let other = store.add_project("other").unwrap();
    store.set_last_project(&other).unwrap();
    let app = App::new(store, warning, None);
    assert_eq!(app.manager.project_selected, 1);
    assert!(app.manager.project_message.as_ref().unwrap().0.contains("Default"));
}

#[test]
fn move_requires_another_project() {
    let (mut app, _dir) = test_app();
    let id = project_id(&app);
    app.store.upsert(&id, None, ConnectForm { name: "x".into(), ..ConnectForm::new() }.to_profile()).unwrap();
    app.handle_key(key(KeyCode::Char('m')));
    assert!(app.manager.moving_to.is_none());
    assert!(app.manager.connection_message.as_ref().unwrap().1);
    app.store.add_project("other").unwrap();
    app.handle_key(key(KeyCode::Char('m')));
    assert_eq!(app.manager.moving_to, Some(0));
    app.handle_key(key(KeyCode::Esc));
    assert!(app.manager.moving_to.is_none());
    assert_eq!(app.profiles().len(), 1);
}

#[test]
fn a_rejected_password_is_ready_to_type_again() {
    let (mut app, _dir) = test_app();
    app.screen = Screen::Connect;
    app.connection.form.password = "wrong".into();
    app.connection.form.focus = ConnectField::Host;
    let failed = |app: &mut App, err: &str| {
        app.connection.form.connecting = true;
        let generation = app.generation;
        app.handle_db_event(DbEvent { generation, run: None, kind: DbEventKind::ConnectFailed(err.to_string()) });
    };
    failed(&mut app, "password authentication failed for user \"app\"");
    let form = &app.connection.form;
    assert_eq!((form.password.as_str(), form.focus), ("", ConnectField::Password));
    assert!(form.error.as_deref().unwrap().contains("type the password and press Enter"));
    // A typed password was wrong: the missing keychain isn't the reason.
    app.connection.form.keychain_note = Some("no Secret Service".into());
    app.connection.form.password = "typo".into();
    failed(&mut app, "password authentication failed for user \"app\"");
    assert!(!app.connection.form.error.as_deref().unwrap().contains("no saved password"));
    failed(&mut app, "password authentication failed for user \"app\"");
    assert!(app.connection.form.error.as_deref().unwrap().contains("no saved password"));
    app.connection.form.keychain_note = None;
    // Other failures, and a password command's password, are left alone.
    app.connection.form.password = "pw".into();
    app.connection.form.focus = ConnectField::Host;
    failed(&mut app, "Connection refused (os error 61)");
    assert_eq!((app.connection.form.password.as_str(), app.connection.form.focus), ("pw", ConnectField::Host));
    app.connection.form.password_command = "pass show db".into();
    failed(&mut app, "Access denied for user 'root'@'h' (using password: YES)");
    assert_eq!(app.connection.form.focus, ConnectField::Host);
}

#[tokio::test]
async fn an_address_on_the_first_run_opens_a_default_project_that_keeps_the_tabs() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("c.toml");
    let file = dir.path().join("shop.db");
    std::fs::write(&file, "").unwrap();
    let address = file.to_string_lossy().into_owned();
    let (store, _) = ConnectionStore::load_from(path.clone());
    let mut app = App::new(store, None, Some(dir.path().to_path_buf()));
    app.start(None, Some(&address));
    assert_eq!(app.store.project_by_name("default").map(|p| Some(p.id.clone())), Some(app.current_project.clone()));
    app.tab.editor.insert_str("select 'kept'");
    app.save_draft();
    // The next run, with the same address, finds that project and its tabs.
    let (store, _) = ConnectionStore::load_from(path);
    let mut app = App::new(store, None, Some(dir.path().to_path_buf()));
    app.start(None, Some(&address));
    assert_eq!(app.store.projects.len(), 1);
    assert_eq!(app.tab.editor.text(), "select 'kept'");
}

#[tokio::test]
async fn a_cancel_the_server_cant_be_told_lets_esc_stop_waiting() {
    let (mut app, _dir) = connected_app(ConnectionMode::Normal).await;
    // A run that never ends (as on a link that died), and Esc.
    app.tab.run.executing = true;
    app.tab.run.task = Some(tokio::spawn(std::future::pending()));
    app.handle_key(key(KeyCode::Esc));
    assert!(app.tab.run.cancelling);
    let generation = app.generation;
    // A server that answers and refuses (no privilege): just said.
    let refused = DbEventKind::CancelFailed("Access denied; you need the PROCESS privilege".into(), false);
    app.handle_db_event(DbEvent { generation, run: None, kind: refused });
    assert!(!app.tab.run.cancel_failed && !app.connection_lost && app.tab.run.executing);
    app.handle_key(key(KeyCode::Esc));
    let failed = DbEventKind::CancelFailed("pool timed out while waiting for an open connection".into(), true);
    app.handle_db_event(DbEvent { generation, run: None, kind: failed });
    assert!(app.tab.run.cancel_failed && app.connection_lost && app.tab.run.executing);
    assert!(said(&app).starts_with("Couldn't cancel: Esc again stops waiting"), "{}", said(&app));
    // Esc again: no longer waiting; the next run checks the server first.
    app.handle_key(key(KeyCode::Esc));
    assert!(!app.tab.run.executing && app.tab.run.task.is_none() && !app.tab.run.cancel_failed);
    assert!(app.tab.run_error.as_ref().is_some_and(|e| e.contains("stopped waiting")));
    assert!(app.connection_lost);
    run_sql(&mut app, "select 7").await;
    assert_eq!(app.tab.grid.results.as_ref().unwrap().rows, vec![vec![Some("7".to_string())]]);
    assert!(!app.connection_lost);
    // A query that ends after a failed cancel leaves nothing armed.
    app.tab.run.executing = true;
    app.tab.run.task = Some(tokio::spawn(std::future::pending()));
    app.handle_key(key(KeyCode::Esc));
    let failed = DbEventKind::CancelFailed("pool timed out".into(), true);
    app.handle_db_event(DbEvent { generation: app.generation, run: None, kind: failed });
    app.tab.run.task = None;
    app.tab.run.executing = false;
    run_sql(&mut app, "select 8").await;
    assert!(!app.tab.run.cancel_failed, "the next Esc asks the server again");
}

#[tokio::test]
async fn a_lost_connection_is_checked_again_and_said_when_back() {
    let (mut app, _dir) = connected_app(ConnectionMode::Normal).await;
    app.connection_lost = true;
    // Ticks go on for it, heartbeat or not; something done checks it now.
    assert!(app.settings.keepalive == 0 && app.wants_ticks());
    app.last_activity = Instant::now().checked_sub(Duration::from_secs(3)).unwrap();
    app.handle_key(key(KeyCode::Down));
    assert!(app.pinging);
    while app.pinging {
        let event = app.poll_db_event().await.unwrap();
        app.handle_db_event(event);
    }
    assert!(!app.connection_lost && !app.wants_ticks());
    assert_eq!(said(&app), "The connection is back");
}

/// A SQLite connection's timeout stops a query that runs too long.
#[tokio::test]
async fn sqlite_queries_time_out() {
    let (mut app, _dir) = connected_app(ConnectionMode::Normal).await;
    app.connection.form.timeout = "1".into();
    app.tab.editor.set_text("WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n) SELECT count(*) FROM n");
    app.handle_key(key(KeyCode::F(5)));
    assert!(app.tab.run.executing);
    tokio::time::sleep(Duration::from_millis(1100)).await;
    app.tick_running();
    tokio::time::timeout(Duration::from_secs(5), finish(&mut app)).await.expect("the query was stopped");
    assert!(app.notes.status.starts_with("Timed out after 1 s"), "{}", app.notes.status);
}

#[tokio::test]
async fn server_syntax_check_after_typing_pause() {
    let (mut app, _dir) = connected_app(ConnectionMode::Normal).await;
    app.tab.editor.set_text("select 1;\nselect * fro t");
    check_syntax_now(&mut app).await;
    let hint = app.tab.syntax_hint.clone().expect("server reports the error");
    assert!(hint.source == HintSource::Server && hint.message.contains("syntax error"), "{hint:?}");
    let text = app.tab.editor.text();
    assert_eq!(&text[hint.offset.unwrap()..hint.offset.unwrap() + hint.len.unwrap()], "fro");

    // Unknown tables are reported, unless the script creates objects first.
    app.tab.editor.set_text("select * from nope");
    check_syntax_now(&mut app).await;
    assert!(app.tab.syntax_hint.as_ref().is_some_and(|h| h.message.contains("no such table")));
    app.tab.editor.set_text("create table nope (id int); select * from nope");
    check_syntax_now(&mut app).await;
    assert_eq!(app.tab.syntax_hint, None);

    // Inside a transaction the server isn't asked; the local parser is used.
    run_sql(&mut app, "begin").await;
    app.tab.editor.set_text("select * fro t");
    check_syntax_now(&mut app).await;
    assert!(app.tab.syntax_hint.as_ref().is_some_and(|h| h.source == HintSource::Parser));
}

#[test]
fn connection_urls_fill_the_form() {
    let mut f = ConnectForm::new();
    f.apply_url("postgresql://ann:p%40ss%3Aw@db.example.com:6543/sales?sslmode=require").unwrap();
    assert_eq!(
        (f.engine, f.host.as_str(), f.port.as_str(), f.user.as_str(), f.password.as_str(), f.database.as_str()),
        (Engine::Postgres, "db.example.com", "6543", "ann", "p@ss:w", "sales")
    );
    assert_eq!(f.ssl_mode, SslMode::Require);

    f.apply_url("mysql://root@[::1]/app").unwrap();
    assert_eq!((f.engine, f.host.as_str(), f.port.as_str(), f.password.as_str()), (Engine::MySql, "::1", "3306", ""));

    f.apply_url("sqlite:///var/data/app.db").unwrap();
    assert_eq!((f.engine, f.host.as_str()), (Engine::Sqlite, "/var/data/app.db"));
    f.apply_url("sqlite:local.db?mode=ro").unwrap();
    assert_eq!(f.host, "local.db");

    assert!(f.apply_url("redis://x").unwrap_err().contains("Unsupported"));
}

#[test]
fn pasting_a_url_into_the_connect_form() {
    let (mut app, _dir) = test_app();
    app.screen = Screen::Connect;
    app.handle_paste("  mysql://me:secret@db:3307/shop  ");
    assert_eq!((app.connection.form.host.as_str(), app.connection.form.port.as_str()), ("db", "3307"));
    assert!(app.connection.form.info.is_some());
    // Ordinary text still goes into the focused field.
    app.connection.form.focus = ConnectField::Name;
    app.handle_paste("primary");
    assert!(app.connection.form.name.ends_with("primary"));
}

#[test]
fn sql_literals_escape_per_engine() {
    use crate::db::sql_literal;
    assert_eq!(sql_literal("it's", Engine::Postgres), "'it''s'");
    assert_eq!(sql_literal(r"a\b", Engine::Postgres), r"'a\b'");
    assert_eq!(sql_literal(r"a\b'", Engine::MySql), r"'a\\b'''");
}

#[tokio::test]
async fn a_missing_sqlite_file_is_created_only_when_asked_again() {
    let (mut app, dir) = test_app();
    let path = dir.path().join("typo.db");
    app.connection.form = ConnectForm::from_profile(&file_profile("t", &path), String::new());
    app.screen = Screen::Connect;
    app.handle_key(key(KeyCode::Enter));
    assert!(!app.connection.form.connecting && !path.exists());
    let info = app.connection.form.info.clone().unwrap_or_default();
    assert!(info.contains("doesn't exist") && info.contains("Enter again"), "{info}");

    // Another path asks again.
    let other = dir.path().join("other.db");
    app.connection.form.host = other.to_string_lossy().into_owned();
    app.handle_key(key(KeyCode::Enter));
    assert!(!app.connection.form.connecting && !other.exists());

    // Enter again for the same path: created and connected.
    app.handle_key(key(KeyCode::Enter));
    assert!(app.connection.form.connecting);
    while app.connection.form.connecting {
        let event = app.poll_db_event().await.unwrap();
        app.handle_db_event(event);
    }
    assert!(other.exists() && app.connection.database.is_some());
}

#[tokio::test]
async fn each_tab_keeps_its_own_connection() {
    let (mut app, dir) = test_app();
    let project = project_id(&app);
    let (a, b) = (file_profile("alpha", &dir.path().join("a.db")), file_profile("beta", &dir.path().join("b.db")));
    app.store.upsert(&project, None, a.clone()).unwrap();
    app.store.upsert(&project, None, b.clone()).unwrap();
    connect_profile(&mut app, &a).await;
    run_sql(&mut app, "create table in_alpha (x int)").await;
    finish_catalog(&mut app).await;

    // Ctrl+T shares the connection; Ctrl+O opens another in a new tab.
    app.handle_key(ctrl('t'));
    assert_eq!(app.tabs[1].conn, app.tabs[0].conn);
    app.handle_key(ctrl('o'));
    assert!(matches!(app.screen, Screen::Connections));
    assert_eq!((app.tabs.len(), app.active_tab), (3, 2));
    assert!(app.connection.database.is_none());
    connect_profile(&mut app, &b).await;
    run_sql(&mut app, "create table in_beta (y int)").await;
    finish_catalog(&mut app).await;
    assert_eq!(table_names(&app), ["in_beta"]);
    assert_eq!(app.connection.form.name, "beta");

    app.handle_key(alt('1'));
    assert_eq!((app.connection.form.name.as_str(), table_names(&app)), ("alpha", vec!["in_alpha".to_string()]));
    run_sql(&mut app, "select count(*) from in_alpha").await;
    assert!(!app.notes.status_is_error, "{}", app.notes.status);

    // Ctrl+D disconnects only this tab; the others keep theirs.
    app.handle_key(alt('3'));
    app.handle_key(ctrl('d'));
    // Asked first (Ctrl+K d is easily pressed for the navigator's `d`).
    assert!(matches!(app.confirm.as_ref().unwrap().action, ConfirmAction::Disconnect));
    app.handle_key(key(KeyCode::Char('y')));
    assert!(matches!(app.screen, Screen::Connections));
    assert!(app.tabs[2].conn.is_none() && app.tabs[0].conn.is_some());
    // Esc goes back to the tabs, dropping the empty unconnected one.
    app.tab.editor.set_text("");
    app.handle_key(key(KeyCode::Esc));
    assert!(matches!(app.screen, Screen::Query));
    assert_eq!(app.tabs.len(), 2);
    assert_eq!(app.connection.form.name, "alpha");
}

#[tokio::test]
async fn a_read_finishes_in_its_tab_while_one_on_another_connection_is_shown() {
    let (mut app, dir) = test_app();
    let project = project_id(&app);
    let (a, b) = (file_profile("alpha", &dir.path().join("a.db")), file_profile("beta", &dir.path().join("b.db")));
    app.store.upsert(&project, None, a.clone()).unwrap();
    app.store.upsert(&project, None, b.clone()).unwrap();
    connect_profile(&mut app, &a).await;
    run_sql(&mut app, "create table here (x int); insert into here values (1)").await;
    finish_catalog(&mut app).await;
    app.handle_key(ctrl('o'));
    connect_profile(&mut app, &b).await;
    app.handle_key(alt('1'));
    assert_eq!(app.connection.form.name, "alpha");

    app.tab.editor.set_text("select x from here");
    app.handle_key(key(KeyCode::F(5)));
    app.handle_key(alt('2'));
    assert_eq!((app.active_tab, app.connection.form.name.as_str()), (1, "beta"));
    finish(&mut app).await;
    // Still on beta's tab, with beta's connection; alpha's tab has its rows.
    assert_eq!((app.active_tab, app.connection.form.name.as_str()), (1, "beta"));
    assert!(app.tab.grid.results.is_none());
    assert!(app.notes.status.starts_with("Query 1: 1 row in"), "{}", app.notes.status);
    app.handle_key(alt('1'));
    assert_eq!(app.connection.form.name, "alpha");
    assert_eq!(app.tab.grid.results.as_ref().unwrap().rows, vec![vec![Some("1".to_string())]]);
    // Both connections still work.
    run_sql(&mut app, "select count(*) from here").await;
    assert_eq!(app.tab.grid.results.as_ref().unwrap().rows, vec![vec![Some("1".to_string())]]);
}

#[tokio::test]
async fn compare_schemas_and_rows_with_another_tabs_connection() {
    let (mut app, dir) = test_app();
    let project = project_id(&app);
    let (a, b) = (file_profile("staging", &dir.path().join("a.db")), file_profile("prod", &dir.path().join("b.db")));
    app.store.upsert(&project, None, a.clone()).unwrap();
    app.store.upsert(&project, None, b.clone()).unwrap();
    connect_profile(&mut app, &a).await;
    // Nothing to compare with yet.
    app.compare_schema(None);
    assert!(app.notes.status.contains("Open another connection"), "{}", app.notes.status);

    run_sql(&mut app, "create table users (id integer primary key, email text, plan text); create index users_email on users (email); insert into users values (1, 'a', 'free'), (2, 'b', 'pro'), (3, 'c', 'pro')").await;
    finish_catalog(&mut app).await;
    app.handle_key(ctrl('o'));
    connect_profile(&mut app, &b).await;
    run_sql(&mut app, "create table users (id integer primary key, email text); insert into users values (1, 'a'), (2, 'bb'), (4, 'd')").await;
    finish_catalog(&mut app).await;
    app.handle_key(alt('1'));
    assert_eq!(app.connection.form.name, "staging");

    // The palette offers the other connection by name.
    let texts: Vec<String> = app.palette_items().iter().map(|c| c.text(&app)).collect();
    assert!(texts.iter().any(|t| t == "Compare the schema with prod"), "{texts:?}");

    app.compare_schema(None);
    until_event(&mut app, |k| matches!(k, DbEventKind::SchemaCompared { .. })).await;
    let view = app.cell_view.as_ref().expect("the comparison");
    assert!(view.text.contains("column plan TEXT: only here"), "{}", view.text);
    assert!(view.text.contains("index users_email (email): only here"), "{}", view.text);
    assert!(view.text.contains("ALTER TABLE main.users ADD COLUMN plan TEXT;"), "{}", view.text);

    // `e` opens the SQL in a new tab on prod.
    app.handle_key(key(KeyCode::Char('e')));
    assert_eq!((app.tabs.len(), app.active_tab, app.connection.form.name.as_str()), (3, 2, "prod"));
    assert!(app.tab.editor.text().starts_with("-- Makes prod match staging"), "{}", app.tab.editor.text());

    app.handle_key(alt('1'));
    app.tab.editor.set_text("select id, email from users order by id");
    app.compare_rows(None);
    until_event(&mut app, |k| matches!(k, DbEventKind::RowsCompared { .. })).await;
    assert!(app.notes.status.contains("1 only here, 1 only there, 1 changed, 1 the same"), "{}", app.notes.status);
    let rows: Vec<String> = app.tab.grid.results.as_ref().unwrap().rows.iter().map(|r| r[0].clone().unwrap()).collect();
    assert_eq!(rows, ["≠ here", "≠ there", "− here", "+ there"]);
    // A comparison isn't a table: no editing.
    app.focus = Focus::Results;
    app.handle_key(key(KeyCode::Char('c')));
    assert!(app.cell_edit.is_none());

    // Writes aren't compared (they'd run twice).
    app.tab.editor.set_text("delete from users");
    app.compare_rows(None);
    assert!(app.notes.status.contains("Only reads"), "{}", app.notes.status);
}

#[tokio::test]
async fn session_is_restored_on_startup() {
    let (mut app, dir) = test_app();
    app.data_dir = Some(dir.path().to_path_buf());
    let project = project_id(&app);
    app.store.set_last_project(&project).unwrap();
    let (a, b) = (file_profile("alpha", &dir.path().join("a.db")), file_profile("beta", &dir.path().join("b.db")));
    app.store.upsert(&project, None, a.clone()).unwrap();
    app.store.upsert(&project, None, b.clone()).unwrap();
    app.load_tabs(&project);
    connect_profile(&mut app, &a).await;
    run_sql(&mut app, "create table t1 (x int); create table t2 (y int)").await;
    finish_catalog(&mut app).await;
    app.tab.editor.set_text("select 1;\nselect 2");
    app.tab.editor.restore_cursor((1, 3), 0);
    // Expand t2 in the navigator.
    let panel = app.connection.schema.as_mut().unwrap();
    let t2 = panel.tables.iter().position(|t| t.info.name == "t2").unwrap();
    panel.tables[t2].expanded = true;
    app.handle_key(ctrl('o'));
    connect_profile(&mut app, &b).await;
    app.tab.editor.set_text("select 'beta'");
    app.handle_key(alt('1'));
    app.focus = Focus::Schema;
    app.save_draft();

    // A new app (a restart) with the same config.
    let (store, _) = ConnectionStore::load_from(dir.path().join("c.toml"));
    let mut app = App::new(store, None, Some(dir.path().to_path_buf()));
    app.restore_session();
    assert!(matches!(app.screen, Screen::Query));
    assert_eq!(app.tabs.len(), 2);
    assert_eq!(app.active_tab, 0);
    assert_eq!((app.tab.editor.cursor_row, app.tab.editor.cursor_col), (1, 3));
    assert_eq!(app.focus, Focus::Schema);
    // The reopening card waits a moment before showing (a fast reopen never does).
    assert!(app.splash.as_ref().is_some_and(|s| !s.shown) && app.splash_due().is_some());
    app.show_splash();
    assert!(app.splash.as_ref().is_some_and(|s| s.shown));
    while !app.reconnecting().is_empty() || app.connection.schema.as_ref().is_none_or(|s| s.loading) {
        let event = app.poll_db_event().await.unwrap();
        app.handle_db_event(event);
    }
    assert!(app.splash.is_none(), "gone once everything is reopened");
    assert_eq!(app.connection.form.name, "alpha");
    let panel = app.connection.schema.as_ref().unwrap();
    assert!(panel.tables.iter().any(|t| t.info.name == "t2" && t.expanded));
    app.handle_key(alt('2'));
    assert_eq!((app.connection.form.name.as_str(), app.tab.editor.text().as_str()), ("beta", "select 'beta'"));

    // A database file that's gone isn't recreated as an empty one.
    std::fs::remove_file(dir.path().join("b.db")).unwrap();
    let (store, _) = ConnectionStore::load_from(dir.path().join("c.toml"));
    let mut app = App::new(store, None, Some(dir.path().to_path_buf()));
    app.restore_session();
    while !app.reconnecting().is_empty() {
        let event = app.poll_db_event().await.unwrap();
        app.handle_db_event(event);
    }
    assert!(!dir.path().join("b.db").exists());
    assert!(app.notes.status.contains("doesn't exist"), "{}", app.notes.status);
    // Once it's back, a run in its tab opens it again and goes ahead: no
    // need to pick the connection again, or to restart.
    app.handle_key(alt('2'));
    assert!(app.connection.database.is_none());
    std::fs::write(dir.path().join("b.db"), "").unwrap();
    app.handle_key(key(KeyCode::F(5)));
    assert!(said(&app).starts_with("Reconnecting to beta"), "{}", said(&app));
    while app.tab.grid.results.is_none() {
        let event = app.poll_db_event().await.unwrap();
        app.handle_db_event(event);
    }
    assert_eq!(app.connection.form.name, "beta");
    assert_eq!(app.tab.grid.results.as_ref().unwrap().rows, vec![vec![Some("beta".to_string())]]);
    // Disconnected on purpose: a run says so, and doesn't open it again.
    app.disconnect();
    app.screen = Screen::Query;
    app.handle_key(key(KeyCode::F(5)));
    assert!(app.reconnecting().is_empty() && app.connection.database.is_none());
    assert!(said(&app).contains("isn't connected"), "{}", said(&app));

    // With restore_session = false it stays on the projects screen.
    let (store, _) = ConnectionStore::load_from(dir.path().join("c.toml"));
    let mut app = App::new(store, None, Some(dir.path().to_path_buf()));
    app.settings.restore_session = false;
    app.restore_session();
    assert!(matches!(app.screen, Screen::Projects));
}

#[tokio::test]
async fn queries_are_cancelled_at_the_connection_timeout() {
    for engine in Engine::BUILT {
        let Some(sleep) = crate::db::tests::test_sql(engine).sleep else { continue };
        let Some((mut app, _dir)) = live_app(engine, "podelis_timeout").await else { continue };
        app.connection.form.timeout = "1".into();
        app.tab.editor.set_text(&sleep.replace("{s}", "10"));
        app.handle_key(key(KeyCode::F(5)));
        let started = Instant::now();
        while app.tab.run.executing {
            tokio::select! {
                event = app.poll_db_event() => app.handle_db_event(event.unwrap()),
                () = tokio::time::sleep(std::time::Duration::from_millis(200)) => app.tick_running(),
            }
        }
        assert!(started.elapsed() < std::time::Duration::from_secs(5), "{engine:?}");
        // Stopped with an error, or early with a result (MySQL's SLEEP).
        assert!(app.notes.status.starts_with("Timed out after 1 s"), "{engine:?}: {}", app.notes.status);
    }
}

#[tokio::test]
async fn connection_info_keepalive_and_diagnostics() {
    let (mut app, _dir) = connected_app(ConnectionMode::Normal).await;
    app.show_connection_info();
    while app.cell_view.is_none() {
        let event = app.poll_db_event().await.unwrap();
        app.handle_db_event(event);
    }
    let info = &app.cell_view.as_ref().unwrap().text;
    assert!(info.contains("Journal mode") && info.contains("Latency"), "{info}");
    app.cell_view = None;

    // Keep-alive: an idle connection gets checked, and the latency updated.
    app.settings.keepalive = 1;
    app.last_activity = Instant::now().checked_sub(std::time::Duration::from_secs(5)).unwrap();
    app.connection.latency = None;
    assert!(app.wants_ticks());
    app.tick_running();
    assert!(app.pinging);
    while app.pinging {
        let event = app.poll_db_event().await.unwrap();
        app.handle_db_event(event);
    }
    assert!(app.connection.latency.is_some());

    let report = app.diagnostics();
    assert!(report.contains(env!("CARGO_PKG_VERSION")) && report.contains(file_engine().label()), "{report}");
    assert!(!report.contains(":memory:"), "no file names or hosts: {report}");
}

#[tokio::test]
async fn heartbeat_can_be_switched_on_and_reports_a_lost_connection() {
    let (mut app, _dir) = connected_app(ConnectionMode::Normal).await;
    assert_eq!(app.settings.keepalive, 0, "off by default");
    app.run_action(crate::config::keys::Action::Heartbeat);
    assert_eq!(app.settings.keepalive, 30);
    assert!(app.pinging, "checks right away");
    while app.pinging {
        let event = app.poll_db_event().await.unwrap();
        app.handle_db_event(event);
    }
    assert!(app.connection.latency.is_some() && !app.connection_lost);
    // A failed check marks the connection lost until something succeeds.
    app.pinged(Err(("connection refused".into(), crate::app::Ping::Failed)));
    assert!(app.connection_lost && app.notes.status.contains("hint:"), "{}", app.notes.status);
    run_sql(&mut app, "select 1").await;
    assert!(!app.connection_lost);
    // A heartbeat with no answer once: said, the session kept (an open
    // transaction would go with it).
    run_sql(&mut app, "create table hb (x int); begin; insert into hb values (1)").await;
    assert_eq!(app.connection.txn, TxnState::Open);
    app.pinged(Err(("no answer in 5 s".into(), crate::app::Ping::NoAnswer)));
    app.pinged(Err(("no answer in 5 s".into(), crate::app::Ping::NoAnswer)));
    assert!(app.connection_lost);
    run_sql(&mut app, "select count(*) from hb").await;
    assert_eq!(app.tab.grid.results.as_ref().unwrap().rows, vec![vec![Some("1".to_string())]], "still its transaction");
    // A check on its way when the connection shown changes isn't waited
    // for any more (its answer is dropped).
    app.pinging = true;
    app.switch_conn(None);
    assert!(!app.pinging);
    app.run_action(crate::config::keys::Action::Heartbeat);
    assert_eq!(app.settings.keepalive, 0);
}

#[tokio::test]
async fn the_projects_pane_switches_adds_renames_and_deletes_over_the_work() {
    let (mut app, dir) = test_app();
    app.data_dir = Some(dir.path().to_path_buf());
    let first = project_id(&app);
    let second = app.store.add_project("second").unwrap();
    let a = file_profile("alpha", &dir.path().join("a.db"));
    app.store.upsert(&first, None, a.clone()).unwrap();
    app.load_tabs(&first);
    connect_profile(&mut app, &a).await;
    app.tab.editor.set_text("select 'kept'");

    // Ctrl+K p shows the projects, the current one selected.
    show_projects(&mut app);
    assert!(matches!(app.screen, Screen::Projects));
    assert_eq!(app.store.projects[app.manager.project_selected].id, first);
    // The current one: its connections; Esc goes back to the work.
    app.handle_key(key(KeyCode::Enter));
    assert!(matches!(app.screen, Screen::Connections));
    app.handle_key(key(KeyCode::Esc));
    assert!(matches!(app.screen, Screen::Query) && app.connection.database.is_some());

    // Rename the other one, then switch to it.
    show_projects(&mut app);
    app.manager.project_selected = app.store.projects.iter().position(|p| p.id == second).unwrap();
    app.handle_key(key(KeyCode::Char('r')));
    for _ in 0.."second".len() {
        app.handle_key(key(KeyCode::Backspace));
    }
    type_text(&mut app, "reports");
    app.handle_key(key(KeyCode::Enter));
    assert_eq!(app.store.project(&second).unwrap().name, "reports");
    // The open project can't be deleted from here.
    app.manager.project_selected = app.store.projects.iter().position(|p| p.id == first).unwrap();
    app.handle_key(key(KeyCode::Char('d')));
    assert!(app.manager.project_message.as_ref().is_some_and(|(m, _)| m.contains("switch to another")));
    app.manager.project_selected = app.store.projects.iter().position(|p| p.id == second).unwrap();
    app.handle_key(key(KeyCode::Enter));
    assert!(matches!(app.screen, Screen::Connections));
    assert_eq!(app.current_project.as_deref(), Some(second.as_str()));
    assert!(app.connection.database.is_none());

    // Back to the first: its tab comes back and reconnects.
    app.screen = Screen::Query;
    show_projects(&mut app);
    app.manager.project_selected = app.store.projects.iter().position(|p| p.id == first).unwrap();
    app.handle_key(key(KeyCode::Enter));
    assert_eq!(app.current_project.as_deref(), Some(first.as_str()));
    assert!(matches!(app.screen, Screen::Query));
    assert_eq!(app.tab.editor.text(), "select 'kept'");
    while app.connection.database.is_none() {
        let event = app.poll_db_event().await.unwrap();
        app.handle_db_event(event);
    }
    assert_eq!(app.connection.form.name, "alpha");

    // Delete the other project; then a new one opens straight away.
    show_projects(&mut app);
    app.manager.project_selected = app.store.projects.iter().position(|p| p.id == second).unwrap();
    app.handle_key(key(KeyCode::Char('d')));
    app.handle_key(key(KeyCode::Char('y')));
    assert!(app.store.project(&second).is_none());
    app.handle_key(key(KeyCode::Char('n')));
    type_text(&mut app, "fresh");
    app.handle_key(key(KeyCode::Enter));
    assert_eq!(app.project().map(|p| p.name.as_str()), Some("fresh"));
}

#[tokio::test]
async fn the_connections_pane_connects_edits_and_goes_back() {
    let (mut app, dir) = test_app();
    let project = project_id(&app);
    let (a, b) = (file_profile("alpha", &dir.path().join("a.db")), file_profile("beta", &dir.path().join("b.db")));
    for path in [&a.host, &b.host] {
        std::fs::write(path, b"").unwrap();
    }
    app.store.upsert(&project, None, a.clone()).unwrap();
    app.store.upsert(&project, None, b.clone()).unwrap();
    connect_profile(&mut app, &a).await;
    app.tab.editor.set_text("select 'alpha'");

    // Ctrl+K p, then → into the open project's connections.
    show_projects(&mut app);
    app.handle_key(key(KeyCode::Right));
    assert!(matches!(app.screen, Screen::Connections));
    // ← goes back to the projects; another project can't be entered with →.
    app.handle_key(key(KeyCode::Left));
    assert!(matches!(app.screen, Screen::Projects));
    app.handle_key(key(KeyCode::Tab));
    assert!(matches!(app.screen, Screen::Connections));

    // Enter on a connection connects it right away, in a new tab since this
    // one is connected.
    app.manager.connection_selected = 1;
    app.handle_key(key(KeyCode::Enter));
    while app.connection.database.is_none() || app.connection.form.connecting || app.manager.loading_profile {
        let event = app.poll_db_event().await.unwrap();
        app.handle_db_event(event);
    }
    assert!(matches!(app.screen, Screen::Query));
    assert_eq!((app.tabs.len(), app.connection.form.name.as_str()), (2, "beta"));
    assert_eq!(app.tab_text(0), "select 'alpha'");

    // e edits the settings in place, without a new tab: the tab keeps its
    // connection, and gets its form back when the editing ends.
    show_projects(&mut app);
    app.handle_key(key(KeyCode::Right));
    app.manager.connection_selected = 0;
    app.handle_key(key(KeyCode::Char('e')));
    while app.manager.loading_profile {
        let event = app.poll_db_event().await.unwrap();
        app.handle_db_event(event);
    }
    assert!(matches!(app.screen, Screen::Connect));
    assert_eq!(app.connection.form.original_name.as_deref(), Some("alpha"));
    assert_eq!((app.tabs.len(), app.active_tab), (2, 1));
    // The tabs saved meanwhile still name this tab's connection.
    assert_eq!(app.tab_connection_name(1).as_deref(), Some("beta"));
    app.handle_key(key(KeyCode::Esc));
    assert!(matches!(app.screen, Screen::Connections));
    assert_eq!(app.connection.form.name, "beta");
    app.handle_key(key(KeyCode::Esc));
    assert!(matches!(app.screen, Screen::Query));
    assert_eq!((app.tabs.len(), app.active_tab), (2, 1));
    assert!(app.connection.database.is_some());

    // Connecting from the edited form opens a new tab for it.
    show_projects(&mut app);
    app.handle_key(key(KeyCode::Right));
    app.manager.connection_selected = 0;
    app.handle_key(key(KeyCode::Char('e')));
    while app.manager.loading_profile {
        let event = app.poll_db_event().await.unwrap();
        app.handle_db_event(event);
    }
    app.handle_key(key(KeyCode::Enter));
    while app.connection.form.connecting {
        let event = app.poll_db_event().await.unwrap();
        app.handle_db_event(event);
    }
    assert_eq!((app.tabs.len(), app.active_tab), (3, 2));
    assert_eq!(app.connection.form.name, "alpha");
    app.handle_key(alt('2'));
    assert_eq!(app.connection.form.name, "beta");

    // Looking at the projects and going back changes nothing.
    show_projects(&mut app);
    app.handle_key(key(KeyCode::Esc));
    assert!(matches!(app.screen, Screen::Query));
    assert_eq!((app.tabs.len(), app.connection.form.name.as_str()), (3, "beta"));
}

#[tokio::test]
async fn leaving_the_panes_drops_a_password_still_being_read() {
    let (mut app, dir) = test_app();
    let project = project_id(&app);
    let a = file_profile("alpha", &dir.path().join("a.db"));
    app.store.upsert(&project, None, a).unwrap();
    app.handle_key(key(KeyCode::Enter));
    assert!(app.manager.loading_profile);
    // Clicking the project's name in the top bar.
    app.open_manager(Screen::Projects);
    assert!(!app.manager.loading_profile && matches!(app.screen, Screen::Projects));
    // The password arrives late: it no longer opens the form or connects.
    let event = app.poll_db_event().await.unwrap();
    app.handle_db_event(event);
    assert!(matches!(app.screen, Screen::Projects));
    assert!(!app.connection.form.connecting);
}

#[test]
fn looking_at_the_projects_keeps_an_empty_tab() {
    let (mut app, _dir) = test_app();
    app.screen = Screen::Query;
    app.new_tab(None, "", None);
    assert_eq!(app.tabs.len(), 2);
    show_projects(&mut app);
    app.handle_key(key(KeyCode::Esc));
    assert!(matches!(app.screen, Screen::Query));
    assert_eq!(app.tabs.len(), 2);
    // So does picking a connection and not taking one: the tab was there.
    app.handle_key(ctrl('o'));
    app.handle_key(key(KeyCode::Esc));
    assert_eq!(app.tabs.len(), 2);
}

#[tokio::test]
async fn a_new_connection_is_kept_once_it_works() {
    let (mut app, dir) = test_app();
    let file = dir.path().join("shop.db");
    let connect = async |app: &mut App, name: &str| {
        app.connection.form = ConnectForm::new();
        app.connection.form.engine = file_engine();
        app.connection.form.host = file.to_string_lossy().into_owned();
        app.connection.form.name = name.into();
        let db = Database::connect(app.connection.form.params().unwrap()).await.unwrap();
        let generation = app.generation;
        app.handle_db_event(DbEvent { generation, run: None, kind: DbEventKind::Connected(db) });
    };
    // Unnamed: named after the file, and saved in the project.
    connect(&mut app, "").await;
    let saved = |app: &App| app.profiles().iter().map(|p| p.name.clone()).collect::<Vec<_>>();
    assert_eq!(saved(&app), vec!["shop.db"]);
    assert!(app.notes.status.contains("Saved as 'shop.db'"), "{}", app.notes.status);
    // Another unnamed one to the same place is that saved one, not a copy.
    connect(&mut app, "").await;
    assert_eq!(saved(&app), vec!["shop.db"]);
    assert!(app.notes.status.contains("Already saved as 'shop.db'"), "{}", app.notes.status);
    assert_eq!(app.connection.form.original_name.as_deref(), Some("shop.db"));
    // Named, it's saved under that name even if it goes to the same place.
    connect(&mut app, "shop copy").await;
    assert_eq!(saved(&app), vec!["shop.db", "shop copy"]);
    // A saved connection opened again isn't saved twice.
    let profile = app.profiles()[0].clone();
    app.connection.form = ConnectForm::from_profile(&profile, String::new());
    let db = Database::connect(app.connection.form.params().unwrap()).await.unwrap();
    let generation = app.generation;
    app.handle_db_event(DbEvent { generation, run: None, kind: DbEventKind::Connected(db) });
    assert_eq!(saved(&app).len(), 2);
}
