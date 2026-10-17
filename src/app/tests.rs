use super::*;
use std::time::Instant;

use crate::db::{ConnectionMode, TxnState};
use crate::keys::Action;

fn outcome(columns: &[&str], rows: &[&[Option<&str>]]) -> QueryOutcome {
    QueryOutcome {
        columns: columns.iter().map(std::string::ToString::to_string).collect(),
        column_types: columns.iter().map(|_| String::new()).collect(),
        rows: rows.iter().map(|r| r.iter().map(|c| c.map(str::to_string)).collect()).collect(),
        total_rows: rows.len() as u64,
        rows_affected: None,
        statements: 1,
        elapsed: Duration::from_millis(5),
        reconnected: false,
        ..QueryOutcome::default()
    }
}

/// An app with project "p" open.
fn test_app() -> (App, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let (mut store, warning) = ConnectionStore::load_from(dir.path().join("c.toml"));
    let id = store.add_project("p").unwrap();
    let mut app = App::new(store, warning, None);
    app.open_project(&id);
    (app, dir)
}

fn project_id(app: &App) -> String {
    app.current_project.clone().unwrap()
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

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
fn toggling_engine_updates_default_port_only() {
    let mut form = ConnectForm::new();
    form.cycle_engine(true);
    assert_eq!(form.port, "3306");
    form.port = "13306".into();
    form.cycle_engine(false);
    assert_eq!(form.port, "13306");
}

#[test]
fn ctrl_y_copies_the_focused_field_in_the_settings() {
    let (mut app, _dir) = test_app();
    app.screen = Screen::Connect;
    app.connect_form.host = "db.example.com".into();
    app.connect_form.password = "secret".into();
    let ctrl = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL);
    app.connect_form.focus = ConnectField::Host;
    app.handle_key(ctrl('y'));
    assert_eq!(app.take_clipboard().as_deref(), Some("db.example.com"));
    app.connect_form.focus = ConnectField::Password;
    app.handle_key(ctrl('y'));
    assert_eq!(app.take_clipboard(), None);
    app.connect_form.focus = ConnectField::Mode;
    app.handle_key(ctrl('y'));
    assert_eq!(app.take_clipboard().as_deref(), Some(app.connect_form.mode.label()));
    // Ctrl+C and Ctrl+Q quit.
    app.handle_key(ctrl('c'));
    assert!(app.should_quit);
    app.should_quit = false;
    app.handle_key(ctrl('q'));
    assert!(app.should_quit);
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
    assert!(app.containers.as_ref().is_some_and(|p| p.loading));
    let profile = ConnectionProfile {
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
    let unpublished = crate::containers::Container { name: "hidden".into(), image: "mysql:9".into(), profile: None };
    let shop =
        crate::containers::Container { name: "shop-db".into(), image: "postgres:17".into(), profile: Some(profile) };
    app.containers_found(1, Ok(vec![unpublished, shop]));
    // Not published: nothing to connect to.
    app.handle_key(key(KeyCode::Enter));
    assert!(app.containers.as_ref().and_then(|p| p.note.as_deref()).is_some_and(|m| m.contains("-p")));
    app.handle_key(key(KeyCode::Down));
    assert!(app.containers.as_ref().is_some_and(|p| p.note.is_none()), "the note was about the other row");
    app.handle_key(key(KeyCode::Enter));
    assert!(app.containers.is_none());
    let form = &app.connect_form;
    assert_eq!(app.screen, Screen::Connect);
    assert_eq!((form.name.as_str(), form.port.as_str(), form.user.as_str()), ("shop-db", "5433", "app"));
    assert!(form.original_name.is_none(), "a new connection");
    assert_eq!(form.password_command, "docker exec shop-db printenv POSTGRES_PASSWORD");
    // Esc closes the picker; so does a failure to list.
    app.screen = Screen::Connections;
    app.handle_key(key(KeyCode::Char('c')));
    // A late result of the earlier search is dropped.
    app.containers_found(1, Err("stale".into()));
    assert!(app.containers.as_ref().is_some_and(|p| p.loading && p.error.is_none()));
    app.containers_found(2, Err("docker isn't running".into()));
    assert_eq!(app.containers.as_ref().and_then(|p| p.error.as_deref()), Some("docker isn't running"));
    app.handle_key(key(KeyCode::Esc));
    assert!(app.containers.is_none());
    // Picked again once saved: the saved connection, not a copy.
    let saved = app.connect_form.to_profile_for_tests();
    app.store.upsert(&project_id(&app), None, saved.clone()).unwrap();
    app.handle_key(key(KeyCode::Char('c')));
    let again =
        crate::containers::Container { name: "shop-db".into(), image: "postgres:17".into(), profile: Some(saved) };
    app.containers_found(3, Ok(vec![again]));
    app.handle_key(key(KeyCode::Enter));
    // Opened as `e` would: its saved password is read first.
    assert!(app.loading_profile);
    // The real container search started by `c` may answer first.
    while app.loading_profile {
        let event = app.poll_db_event().await.unwrap();
        app.handle_db_event(event);
    }
    assert_eq!(app.connect_form.original_name.as_deref(), Some("shop-db"));
    // Through an SSH tunnel it's another server: not the same connection.
    let mut tunneled = app.store.project(&project_id(&app)).unwrap().connections[0].clone();
    tunneled.ssh = "bastion".into();
    app.store.upsert(&project_id(&app), Some("shop-db"), tunneled).unwrap();
    app.screen = Screen::Connections;
    app.handle_key(key(KeyCode::Char('c')));
    let local = crate::containers::Container {
        name: "shop-db".into(),
        image: "postgres:17".into(),
        profile: Some(ConnectionProfile { ssh: String::new(), ..app.profiles()[0].clone() }),
    };
    app.containers_found(4, Ok(vec![local]));
    app.handle_key(key(KeyCode::Enter));
    assert_eq!((app.connect_form.original_name.as_deref(), app.connect_form.name.as_str()), (None, "shop-db (2)"));
}

#[test]
fn ssh_destination_is_trimmed_into_params() {
    let mut form = ConnectForm::new();
    form.ssh = "  me@bastion  ".into();
    assert_eq!(form.params().unwrap().ssh.as_deref(), Some("me@bastion"));
    assert!(form.summary().ends_with("via me@bastion"));
}

#[test]
fn visible_columns_scrolls_to_selection() {
    let widths = [10, 10, 10, 10];
    assert_eq!(visible_columns(&widths, 0, 0, 25), (0, 2));
    assert_eq!(visible_columns(&widths, 0, 3, 25), (2, 2));
    assert_eq!(visible_columns(&widths, 3, 1, 25), (1, 2));
    // A column wider than the viewport still shows on its own.
    assert_eq!(visible_columns(&[50, 5], 0, 0, 20), (0, 1));
    assert_eq!(visible_columns(&[], 0, 0, 20), (0, 0));
}

#[test]
fn column_widths_use_content_and_cap() {
    let long = "x".repeat(100);
    let o = outcome(&["id", "name"], &[&[None, Some("ab")], &[Some("1"), Some(&long)]]);
    assert_eq!(column_widths(&o), vec![4, 40]);
    let o = outcome(&["id"], &[&[Some("1")]]);
    assert_eq!(column_widths(&o), vec![4], "header has room for the sort arrow");
}

#[test]
fn delete_requires_confirmation() {
    let (mut app, _dir) = test_app();
    let id = project_id(&app);
    app.store.upsert(&id, None, ConnectForm { name: "x".into(), ..ConnectForm::new() }.to_profile()).unwrap();
    app.handle_key(key(KeyCode::Char('d')));
    assert!(app.confirm_delete);
    app.handle_key(key(KeyCode::Char('n')));
    assert_eq!(app.profiles().len(), 1);
    assert!(!app.confirm_delete);
}

fn type_text(app: &mut App, text: &str) {
    for c in text.chars() {
        app.handle_key(key(KeyCode::Char(c)));
    }
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
    assert!(app.project_prompt.is_some());
    assert!(app.projects_message.as_ref().unwrap().1);
    app.handle_key(key(KeyCode::Esc));
    assert!(app.project_prompt.is_none());

    app.projects_selected = 0;
    app.handle_key(key(KeyCode::Char('r')));
    assert_eq!(app.project_prompt.as_ref().unwrap().text.text(), "shop");
    type_text(&mut app, "-api");
    app.handle_key(key(KeyCode::Enter));
    assert_eq!(app.store.projects[0].name, "shop-api");
    assert!(matches!(app.screen, Screen::Projects));

    app.handle_key(key(KeyCode::Char('d')));
    assert!(app.confirm_delete_project);
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
    assert_eq!(app.projects_selected, 1);
    assert!(app.projects_message.as_ref().unwrap().0.contains("Default"));
}

#[test]
fn move_requires_another_project() {
    let (mut app, _dir) = test_app();
    let id = project_id(&app);
    app.store.upsert(&id, None, ConnectForm { name: "x".into(), ..ConnectForm::new() }.to_profile()).unwrap();
    app.handle_key(key(KeyCode::Char('m')));
    assert!(app.moving_to.is_none());
    assert!(app.connections_message.as_ref().unwrap().1);
    app.store.add_project("other").unwrap();
    app.handle_key(key(KeyCode::Char('m')));
    assert_eq!(app.moving_to, Some(0));
    app.handle_key(key(KeyCode::Esc));
    assert!(app.moving_to.is_none());
    assert_eq!(app.profiles().len(), 1);
}

#[test]
fn stale_events_are_ignored() {
    let (mut app, _dir) = test_app();
    app.screen = Screen::Query;
    app.executing = true;
    let stale_generation = app.generation;
    app.disconnect();
    assert!(!app.executing);
    app.handle_db_event(DbEvent {
        generation: stale_generation,
        run: None,
        kind: DbEventKind::QueryDone(outcome(&["a"], &[&[Some("1")]]), None, None),
    });
    assert!(app.tab.grid.results.is_none());
}

#[test]
fn a_rejected_password_is_ready_to_type_again() {
    let (mut app, _dir) = test_app();
    app.screen = Screen::Connect;
    app.connect_form.password = "wrong".into();
    app.connect_form.focus = ConnectField::Host;
    let failed = |app: &mut App, err: &str| {
        app.connect_form.connecting = true;
        let generation = app.generation;
        app.handle_db_event(DbEvent { generation, run: None, kind: DbEventKind::ConnectFailed(err.to_string()) });
    };
    failed(&mut app, "password authentication failed for user \"app\"");
    let form = &app.connect_form;
    assert_eq!((form.password.as_str(), form.focus), ("", ConnectField::Password));
    assert!(form.error.as_deref().unwrap().contains("type the password and press Enter"));
    // A typed password was wrong: the missing keychain isn't the reason.
    app.connect_form.keychain_note = Some("no Secret Service".into());
    app.connect_form.password = "typo".into();
    failed(&mut app, "password authentication failed for user \"app\"");
    assert!(!app.connect_form.error.as_deref().unwrap().contains("no saved password"));
    failed(&mut app, "password authentication failed for user \"app\"");
    assert!(app.connect_form.error.as_deref().unwrap().contains("no saved password"));
    app.connect_form.keychain_note = None;
    // Other failures, and a password command's password, are left alone.
    app.connect_form.password = "pw".into();
    app.connect_form.focus = ConnectField::Host;
    failed(&mut app, "Connection refused (os error 61)");
    assert_eq!((app.connect_form.password.as_str(), app.connect_form.focus), ("pw", ConnectField::Host));
    app.connect_form.password_command = "pass show db".into();
    failed(&mut app, "Access denied for user 'root'@'h' (using password: YES)");
    assert_eq!(app.connect_form.focus, ConnectField::Host);
}

#[test]
fn results_navigation_and_copy() {
    let (mut app, _dir) = test_app();
    app.screen = Screen::Query;
    app.focus = Focus::Results;
    app.show_results(outcome(&["a", "b"], &[&[Some("1"), None], &[Some("2"), Some("z")]]));
    app.handle_key(key(KeyCode::Down));
    app.handle_key(key(KeyCode::Right));
    app.handle_key(key(KeyCode::Right));
    assert_eq!((app.tab.grid.table_state.selected(), app.tab.grid.selected_col), (Some(1), 1));
    app.handle_key(key(KeyCode::Char('y')));
    assert_eq!(app.take_clipboard().as_deref(), Some("z"));
    app.handle_key(key(KeyCode::Up));
    app.handle_key(key(KeyCode::Char('Y')));
    assert_eq!(app.take_clipboard().as_deref(), Some("1\t"));
}

#[test]
fn truncated_status() {
    let (mut app, _dir) = test_app();
    let mut o = outcome(&["a"], &[&[Some("1")]]);
    o.total_rows = 5000;
    o.statements = 2;
    app.show_results(o);
    assert!(app.status.starts_with("Showing first 1 of 5000 rows"));
    assert!(app.status.ends_with("· 2 statements"));
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

#[tokio::test]
async fn each_query_keeps_its_result_to_go_back_to() {
    let (mut app, _dir) = connected_app(ConnectionMode::Normal).await;
    let shown = |app: &App| app.tab.grid.results.as_ref().map(|r| r.rows[0][0].clone().unwrap_or_default());
    run_sql(&mut app, "select 'one'").await;
    run_sql(&mut app, "select 'two'").await;
    assert_eq!((app.tab.runs.len(), app.tab.run_at), (2, 1));
    assert_eq!(shown(&app).as_deref(), Some("two"));
    // `,` shows the one before, with its SQL (for M, editing, watch…).
    app.focus = Focus::Results;
    app.handle_key(key(KeyCode::Char(',')));
    assert_eq!(shown(&app).as_deref(), Some("one"));
    assert_eq!(app.tab.last_sql.as_ref().map(|(sql, _)| sql.as_str()), Some("select 'one'"));
    app.handle_key(key(KeyCode::Char(',')));
    assert_eq!(said(&app), "This is the oldest kept result");
    app.handle_key(key(KeyCode::Char('.')));
    assert_eq!(shown(&app).as_deref(), Some("two"));
    // A failure is said where its result would be; the next run takes its place.
    run_sql(&mut app, "select nope from nowhere").await;
    assert!(app.tab.grid.results.is_none() && app.tab.run_error.as_ref().is_some_and(|e| e.contains("nowhere")));
    run_sql(&mut app, "select 'three'").await;
    let numbers: Vec<usize> = app.tab.runs.iter().map(|r| r.number).collect();
    assert_eq!(numbers, [1, 2, 4]);
    assert!(app.tab.run_error.is_none());
    // The last few are kept, and pinned ones too.
    app.focus = Focus::Results;
    app.handle_key(key(KeyCode::Char(',')));
    app.handle_key(key(KeyCode::Char(',')));
    app.handle_key(key(KeyCode::Char('p')));
    assert!(app.tab.runs[0].pinned && said(&app).starts_with("Pinned result #1"));
    for i in 0..super::runs::KEEP + 2 {
        run_sql(&mut app, &format!("select {i}")).await;
    }
    assert_eq!(app.tab.runs.len(), super::runs::KEEP + 1);
    assert_eq!(app.tab.runs[0].number, 1, "the pinned one stays");
    app.focus = Focus::Results;
    for _ in 0..super::runs::KEEP {
        app.handle_key(key(KeyCode::Char(',')));
    }
    assert_eq!(shown(&app).as_deref(), Some("one"));
}

#[tokio::test]
async fn an_earlier_result_runs_again_with_its_own_parameters() {
    let (mut app, _dir) = connected_app(ConnectionMode::Normal).await;
    app.tab.editor.set_text("select :v");
    app.handle_key(key(KeyCode::F(5)));
    type_text(&mut app, "7");
    app.handle_key(key(KeyCode::Enter));
    finish(&mut app).await;
    run_sql(&mut app, "select 'other'").await;
    assert!(app.tab.last_filled.is_none());
    app.focus = Focus::Results;
    app.handle_key(key(KeyCode::Char(',')));
    assert_eq!(app.tab.last_filled.as_ref().map(|f| f.original.as_str()), Some("select :v"));
    // After a row change it runs again, replacing that result.
    let (sql, offset) = app.tab.last_sql.clone().unwrap();
    let infos = app.tab.last_query_infos.clone();
    app.rerun_shown(sql, offset, infos);
    finish(&mut app).await;
    assert_eq!(app.tab.runs.len(), 2);
    assert_eq!(app.tab.grid.results.as_ref().unwrap().rows, vec![vec![Some("7".to_string())]]);
    // History kept the placeholder, not the value.
    assert!(app.history.older("").is_some_and(|h| h == "select :v"));
}

#[test]
fn kept_results_are_dropped_past_a_row_budget() {
    let (mut app, _dir) = test_app();
    app.screen = Screen::Query;
    let big = |n: usize| QueryOutcome {
        columns: vec!["n".into()],
        column_types: vec![String::new()],
        rows: vec![vec![None]; n],
        total_rows: n as u64,
        statements: 1,
        ..QueryOutcome::default()
    };
    let rows = super::runs::KEEP_ROWS / 2 + 1;
    for _ in 0..4 {
        app.open_run_for_tests(0..0);
        app.show_results_for_tests(big(rows));
    }
    // Besides the newest kept, two were past the budget: the oldest went.
    let numbers: Vec<usize> = app.tab.runs.iter().map(|r| r.number).collect();
    assert_eq!(numbers, [2, 3, 4]);
    // Over a plan's table `,` stays put.
    app.tab.plan_swapped = true;
    app.focus = Focus::Results;
    app.handle_key(key(KeyCode::Char(',')));
    assert_eq!(app.tab.run_at, 2);
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
    assert_eq!(app.message_selected, Some(1));
    app.handle_key(key(KeyCode::Up));
    app.handle_key(key(KeyCode::Up));
    assert_eq!(app.message_selected, Some(0));
    app.handle_key(key(KeyCode::Down));
    app.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
    assert!(app.take_clipboard().is_some_and(|c| c.contains("LINE 1:")));
    app.handle_key(KeyEvent::new(KeyCode::Char('Y'), KeyModifiers::SHIFT));
    assert!(
        app.take_clipboard()
            .is_some_and(|c| c.starts_with(&format!("{}  Connected\n", app.messages[0].time)) && c.contains("LINE 1:"))
    );
    app.handle_key(key(KeyCode::Enter));
    let view = app.cell_view.as_ref().unwrap();
    assert!(view.title.contains("error") && view.text.contains("LINE 1:"));
    app.handle_key(key(KeyCode::Char('y')));
    assert!(app.take_clipboard().is_some_and(|c| c.starts_with("relation")));
}

/// An app connected to an in-memory SQLite database, on the query screen.
async fn connected_app(mode: ConnectionMode) -> (App, tempfile::TempDir) {
    let (mut app, dir) = test_app();
    app.connect_form.engine = Engine::Sqlite;
    app.connect_form.host = ":memory:".into();
    app.connect_form.mode = mode;
    let db = Database::connect(app.connect_form.params().unwrap()).await.unwrap();
    let generation = app.generation;
    app.handle_db_event(DbEvent { generation, run: None, kind: DbEventKind::Connected(db) });
    assert!(matches!(app.screen, Screen::Query));
    (app, dir)
}

/// Runs `sql` through the normal key path and waits for the result.
async fn run_sql(app: &mut App, sql: &str) {
    app.tab.editor.set_text(sql);
    app.handle_key(key(KeyCode::F(5)));
    finish(app).await;
}

/// Handles events (catalog loads etc.) until the running query is done.
async fn finish(app: &mut App) {
    while app.executing {
        let event = app.poll_db_event().await.unwrap();
        app.handle_db_event(event);
    }
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
    assert!(app.statement_zoom && !app.zoomed);
    app.handle_key(key(KeyCode::F(5)));
    finish(&mut app).await;
    let shown = app.tab.grid.results.as_ref().map(|r| r.rows.clone());
    assert_eq!(shown, Some(vec![vec![Some("two".to_string())]]));
    // Esc drops a selection first, then shows the whole text again.
    app.focus = Focus::Editor;
    app.tab.editor.select_offsets(0, 3);
    app.handle_key(key(KeyCode::Esc));
    assert!(app.statement_zoom && app.tab.editor.selection().is_none());
    app.handle_key(key(KeyCode::Esc));
    assert!(!app.statement_zoom);
    // Another pane taking the keys (an Explain's plan) leaves it open.
    app.run_action(Action::ZoomStatement);
    app.focus = Focus::Results;
    app.handle_key(key(KeyCode::Char(']')));
    assert!(app.statement_zoom);
    app.run_action(Action::ZoomStatement);
    assert!(!app.statement_zoom);
    // The pane zoom and this one don't go together.
    app.run_action(Action::ZoomStatement);
    app.run_action(Action::Zoom);
    assert!(app.zoomed && !app.statement_zoom);
}

#[tokio::test]
async fn rows_open_in_the_app_for_their_format() {
    use crate::app::pipes::OpenFormat;
    use crate::settings::Viewer;
    let (mut app, _dir) = connected_app(ConnectionMode::Normal).await;
    run_sql(&mut app, "select 1 as n, 'a' as s").await;
    // (Which program opens a format, by default: `Settings::viewer_for`'s test.)
    app.settings.viewers.insert("tsv".into(), Viewer::Full { command: "less -S {}".into(), terminal: true });
    app.settings.viewers.insert("csv".into(), Viewer::Command("libreoffice --calc".into()));
    for (format, given) in [("json", "open"), ("sql", "xdg-open"), ("txt", "open")] {
        app.settings.viewers.insert(format.into(), Viewer::Command(given.into()));
    }
    assert_eq!(app.viewer_for(OpenFormat::Csv), Viewer::Command("libreoffice --calc".into()));
    app.open_viewer_menu();
    let keys: Vec<String> = app.menu.as_ref().unwrap().items.iter().map(|i| i.keys.clone()).collect();
    assert_eq!(keys, ["libreoffice", "its app", "its app", "here: less", "its app"]);
    // One in the terminal: the main loop runs it, on a file of the user's alone.
    app.open_rows_as(OpenFormat::Tsv);
    let external = app.take_external().expect("run in the terminal");
    assert!(external.command.starts_with("less -S '") && external.command.ends_with(".tsv'"), "{}", external.command);
    assert_eq!(std::fs::read_to_string(&external.file).unwrap(), "n\ts\n1\ta\n");
    use std::os::unix::fs::PermissionsExt as _;
    let mode = |p: &std::path::Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!((mode(&external.file), mode(external.file.parent().unwrap())), (0o600, 0o700));
}

#[tokio::test]
async fn a_cancel_the_server_cant_be_told_lets_esc_stop_waiting() {
    let (mut app, _dir) = connected_app(ConnectionMode::Normal).await;
    // A run that never ends (as on a link that died), and Esc.
    app.executing = true;
    app.task = Some(tokio::spawn(std::future::pending()));
    app.handle_key(key(KeyCode::Esc));
    assert!(app.cancelling);
    let generation = app.generation;
    // A server that answers and refuses (no privilege): just said.
    let refused = DbEventKind::CancelFailed("Access denied; you need the PROCESS privilege".into(), false);
    app.handle_db_event(DbEvent { generation, run: None, kind: refused });
    assert!(!app.cancel_failed && !app.connection_lost && app.executing);
    app.handle_key(key(KeyCode::Esc));
    let failed = DbEventKind::CancelFailed("pool timed out while waiting for an open connection".into(), true);
    app.handle_db_event(DbEvent { generation, run: None, kind: failed });
    assert!(app.cancel_failed && app.connection_lost && app.executing);
    assert!(said(&app).starts_with("Couldn't cancel: Esc again stops waiting"), "{}", said(&app));
    // Esc again: no longer waiting; the next run checks the server first.
    app.handle_key(key(KeyCode::Esc));
    assert!(!app.executing && app.task.is_none() && !app.cancel_failed);
    assert!(app.tab.run_error.as_ref().is_some_and(|e| e.contains("stopped waiting")));
    assert!(app.connection_lost);
    run_sql(&mut app, "select 7").await;
    assert_eq!(app.tab.grid.results.as_ref().unwrap().rows, vec![vec![Some("7".to_string())]]);
    assert!(!app.connection_lost);
    // A query that ends after a failed cancel leaves nothing armed.
    app.executing = true;
    app.task = Some(tokio::spawn(std::future::pending()));
    app.handle_key(key(KeyCode::Esc));
    let failed = DbEventKind::CancelFailed("pool timed out".into(), true);
    app.handle_db_event(DbEvent { generation: app.generation, run: None, kind: failed });
    app.task = None;
    app.executing = false;
    run_sql(&mut app, "select 8").await;
    assert!(!app.cancel_failed, "the next Esc asks the server again");
}

#[test]
fn memory_changes_name_the_fact() {
    use super::memory_view::GatewayPurpose;
    let (mut app, _dir) = test_app();
    let project = project_id(&app);
    app.current_project = Some(project.clone());
    // Confirmed or forgotten: the fact as the window showed it (the answer
    // has only its number).
    let shown = Some("orders.total unit euro cents".to_string());
    app.gateway_done(
        GatewayPurpose::MemoryChanged("Confirmed", project.clone(), shown),
        Ok(serde_json::json!({ "id": 8 })),
    );
    assert_eq!(app.status, "Confirmed orders.total unit euro cents");
    // Added: the fact the gateway kept, worded the same way.
    let kept = serde_json::json!({ "id": 9, "subject": "customers.email", "relation": "means", "value": "unique, lower-case" });
    app.gateway_done(GatewayPurpose::MemoryChanged("Remembered", project, None), Ok(kept));
    assert_eq!(app.status, "Remembered customers.email means unique, lower-case");
}

#[tokio::test]
async fn esc_stops_only_its_own_tabs_query_and_others_say_whose_runs() {
    let (mut app, dir) = test_app();
    app.data_dir = Some(dir.path().to_path_buf());
    let project = project_id(&app);
    let (a, b) = (sqlite_profile("alpha", &dir.path().join("a.db")), sqlite_profile("beta", &dir.path().join("b.db")));
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
    assert!(app.executing);
    app.handle_key(alt('2'));
    assert_eq!((app.active_tab, app.connect_form.name.as_str()), (1, "beta"));
    // Esc here doesn't stop it: it says whose it is, and how to get there.
    app.focus = Focus::Editor;
    app.handle_key(key(KeyCode::Esc));
    assert!(!app.cancelling && app.executing);
    assert_eq!(said(&app), "Tab “Query 1” on alpha is running a query: Alt+1 goes to it, Esc there stops it");
    // A connection without a saved name (from a URL) is named by its address.
    app.handle_key(alt('1'));
    app.connect_form.name.clear();
    app.handle_key(alt('2'));
    app.focus = Focus::Editor;
    app.handle_key(key(KeyCode::Esc));
    assert!(said(&app).starts_with("Tab “Query 1” on ") && said(&app).contains("a.db"), "{}", said(&app));
    // (Once it's under way: a cancel before the run starts is cleared by it.)
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    // In its own tab, Esc stops it on its own connection.
    app.handle_key(alt('1'));
    app.handle_key(key(KeyCode::Esc));
    assert!(app.cancelling);
    tokio::time::timeout(std::time::Duration::from_secs(5), finish(&mut app)).await.expect("stopped");
    assert!(!app.executing && !app.cancel_failed);
    assert!(!app.status.contains("Couldn't cancel"), "{}", app.status);
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

#[tokio::test]
async fn read_only_mode_refuses_writes() {
    let (mut app, _dir) = connected_app(ConnectionMode::ReadOnly).await;
    run_sql(&mut app, "select 1; delete from t where id = 1").await;
    assert!(!app.executing && app.confirm.is_none());
    assert!(app.editor_note_is_error && said(&app).contains("Read-only"), "{}", said(&app));
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
    assert!(app.confirm.is_none() && !app.executing);

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
    assert_eq!(app.tx_mode, TxMode::AutoCommit);
    run_sql(&mut app, "create table t (id int); insert into t values (1), (2)").await;
    assert_eq!(app.txn, TxnState::Idle);

    // Manual commit: writes wait for COMMIT or ROLLBACK; reads run as usual.
    tx_mode(&mut app, 'm');
    assert_eq!(app.tx_mode, TxMode::Manual);
    run_sql(&mut app, "insert into t values (3)").await;
    assert!(app.review.is_some() && app.txn == TxnState::Open);
    // Can't change the mode with a transaction open.
    app.review = None;
    tx_mode(&mut app, 'r');
    assert!(app.tx_picker.is_none() && app.tx_mode == TxMode::Manual);
    app.handle_key(KeyEvent::new(KeyCode::F(8), KeyModifiers::SHIFT));
    finish(&mut app).await;
    run_sql(&mut app, "select count(*) from t").await;
    assert_eq!(app.tab.grid.results.as_ref().unwrap().rows, vec![vec![Some("2".to_string())]]);
    assert!(app.review.is_none(), "reads don't open a review");
    // Scripts with their own transaction control are refused.
    run_sql(&mut app, "begin; delete from t where id = 1; commit").await;
    assert!(app.editor_note_is_error && said(&app).contains("BEGIN/COMMIT"), "{}", said(&app));

    // Rollback-only: the write runs, its count is shown, and it is rolled back.
    tx_mode(&mut app, 'r');
    assert_eq!(app.tx_mode, TxMode::RollbackOnly);
    run_sql(&mut app, "delete from t where id > 0").await;
    finish(&mut app).await;
    assert_eq!(app.txn, TxnState::Idle);
    assert!(app.status.contains("2 row(s) changed, then rolled back"), "{}", app.status);
    // A failing rollback-only run is rolled back too, keeping its error.
    run_sql(&mut app, "insert into t values (9); insert into nope values (1)").await;
    finish(&mut app).await;
    assert!(app.status_is_error && app.status.contains("rolled back (rollback-only)"), "{}", app.status);
    assert_eq!(app.txn, TxnState::Idle);

    tx_mode(&mut app, 'a');
    assert_eq!(app.tx_mode, TxMode::AutoCommit);
    run_sql(&mut app, "select count(*) from t").await;
    assert_eq!(app.tab.grid.results.as_ref().unwrap().rows, vec![vec![Some("2".to_string())]]);
}

#[tokio::test]
async fn run_outcomes_are_for_the_results_pane() {
    let (mut app, _dir) = connected_app(ConnectionMode::Normal).await;
    run_sql(&mut app, "select 1").await;
    assert!(app.status_in_results && app.status.contains("1 row"), "{}", app.status);
    run_sql(&mut app, "select * from nope").await;
    assert!(app.status_in_results && app.status_is_error, "{}", app.status);
    // Other messages stay in the status bar.
    app.set_status_for_tests("Copied");
    assert!(!app.status_in_results);
}

#[tokio::test]
async fn production_connections_start_in_review() {
    let (app, _dir) = connected_app(ConnectionMode::Production).await;
    assert_eq!(app.tx_mode, TxMode::Manual);
}

#[tokio::test]
async fn transaction_tracking_and_disconnect_confirmation() {
    let (mut app, _dir) = connected_app(ConnectionMode::Normal).await;
    run_sql(&mut app, "begin; create table t (id int)").await;
    assert_eq!(app.txn, TxnState::Open);

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
    assert_eq!(app.txn, TxnState::Idle);
    app.handle_key(ctrl('d'));
    assert!(app.confirm.is_none());
    assert!(matches!(app.screen, Screen::Connections));
}

#[test]
fn view_filters_and_sorts_numerically_with_nulls_last() {
    let o = outcome(
        &["n", "name"],
        &[&[Some("10"), Some("b")], &[None, Some("a")], &[Some("9"), Some("B")], &[Some("x"), None]],
    );
    assert_eq!(compute_view(&o, "", None), [0, 1, 2, 3]);
    assert_eq!(compute_view(&o, "", Some((0, false))), [2, 0, 3, 1]);
    assert_eq!(compute_view(&o, "", Some((0, true))), [3, 0, 2, 1]);
    assert_eq!(compute_view(&o, "B", None), [0, 2]);
    assert_eq!(compute_view(&o, "null", None), [1, 3]);
    // Non-ASCII text is matched ignoring case too.
    let o = outcome(&["name"], &[&[Some("Žąsis")], &[Some("antis")]]);
    assert_eq!(compute_view(&o, "ŽĄ", None), [0]);
}

#[test]
fn sorting_a_mixed_column_is_consistent_and_never_panics() {
    // Numbers by value, then text, then NULLs.
    let o = outcome(&["v"], &[&[Some("1a")], &[Some("10")], &[None], &[Some("2")], &[Some("b")]]);
    assert_eq!(compute_view(&o, "", Some((0, false))), [3, 1, 0, 4, 2]);
    assert_eq!(compute_view(&o, "", Some((0, true))), [4, 0, 1, 3, 2]);
    // Mixed columns used to make an inconsistent order (2 < 10 < 1a < 2),
    // on which Rust's sort may panic: random ones, sorted both ways.
    let mut seed: u64 = 42;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    for round in 0..200 {
        let rows: Vec<Vec<Option<String>>> = (0..20 + next() % 500)
            .map(|_| {
                let v = next() % 200;
                vec![match next() % 5 {
                    0 => Some(format!("{v}")),
                    1 => Some(format!("{v}a")),
                    2 => Some(format!("{v}.5")),
                    3 => Some(format!("1{v}x")),
                    _ => None,
                }]
            })
            .collect();
        let o = QueryOutcome { columns: vec!["v".into()], rows, ..QueryOutcome::default() };
        assert_eq!(compute_view(&o, "", Some((0, round % 2 == 0))).len(), o.rows.len());
    }
}

#[test]
fn pretty_value_formats_json_only() {
    assert_eq!(pretty_value("{\"a\":[1,2]}"), "{\n  \"a\": [\n    1,\n    2\n  ]\n}");
    assert_eq!(pretty_value("{not json"), "{not json");
    assert_eq!(pretty_value("plain"), "plain");
}

#[tokio::test]
async fn results_filter_sort_view_and_load_more() {
    let (mut app, _dir) = connected_app(ConnectionMode::Normal).await;
    app.settings.row_limit = 2;
    let five = "WITH RECURSIVE s(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM s WHERE n < 5) \
                SELECT n, '{\"k\":' || n || '}' AS j FROM s";
    run_sql(&mut app, five).await;
    assert!(app.status.contains("more load as you scroll"), "{}", app.status);
    app.focus = Focus::Results;

    // M fetches more rows from the open stream.
    app.handle_key(key(KeyCode::Char('M')));
    let event = app.poll_db_event().await.unwrap();
    app.handle_db_event(event);
    assert_eq!(app.tab.grid.results.as_ref().unwrap().rows.len(), 5);

    app.handle_key(key(KeyCode::Char('s')));
    app.handle_key(key(KeyCode::Char('s')));
    assert_eq!(app.tab.grid.sort, Some((0, true)));
    assert_eq!(app.selected_row().unwrap()[0].as_deref(), Some("5"));

    app.handle_key(key(KeyCode::Char('/')));
    type_text(&mut app, "3");
    app.handle_key(key(KeyCode::Enter));
    assert_eq!(app.tab.grid.view.len(), 1);
    app.handle_key(key(KeyCode::Right));
    app.handle_key(key(KeyCode::Enter));
    assert_eq!(app.cell_view.as_ref().unwrap().text, "{\n  \"k\": 3\n}");
    app.handle_key(key(KeyCode::Esc));
    assert!(app.cell_view.is_none());
    app.handle_key(key(KeyCode::Esc));
    assert!(app.tab.grid.filter.is_empty() && app.tab.grid.view.len() == 5);

    // Load-more refuses scripts that write.
    run_sql(&mut app, "create table t (n int); insert into t values (1),(2),(3); select n from t").await;
    app.focus = Focus::Results;
    app.handle_key(key(KeyCode::Char('M')));
    assert!(!app.executing && said_error(&app));
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

/// The agent activity window: sessions, a session's calls, a call in full,
/// opening one in a tab; and the status bar's note until it's looked at.
#[test]
fn agent_activity_by_session() {
    use crate::agents::{Entry, append, log_path};
    let (mut app, dir) = test_app();
    app.set_data_dir(dir.path().to_path_buf());
    let shared = ConnectForm { name: "dev".into(), agents: crate::db::AgentAccess::Read, ..ConnectForm::new() };
    app.store.upsert(&project_id(&app), None, shared.to_profile()).unwrap();
    let log = log_path(dir.path(), &project_id(&app));
    let entry = |session: &str, at: &str, sql: &str| Entry {
        session: session.into(),
        at: format!("2026-10-13T{at}:00+03:00"),
        agent: "claude-code".into(),
        agent_version: Some("2.0".into()),
        dir: Some("/src/shop".into()),
        connection: "dev".into(),
        tool: "query".into(),
        sql: sql.into(),
        ..Entry::default()
    };
    append(&log, entry("a", "09:00", "select 1")).unwrap();
    append(&log, Entry { name: Some("billing".into()), ..entry("b", "10:00", "select * from invoices") }).unwrap();
    append(&log, Entry { refused: Some("destructive".into()), ..entry("a", "11:00", "delete from users") }).unwrap();

    // Nothing looked at yet: the note says what's worth a look.
    app.agent_checked = None;
    app.update_agent_alerts();
    assert_eq!(app.agent_note().as_deref(), Some("agents: 1 refused"));

    app.open_agents();
    assert_eq!(app.agent_note(), None, "looked at");
    let labels: Vec<&str> = app.agents_rows().iter().map(|&i| app.agent_sessions[i].label()).collect();
    assert_eq!(labels, ["claude-code", "billing"], "the most recently active first");
    // Only what's worth a look; then search.
    app.handle_agents_key(key(KeyCode::Char('!')));
    assert_eq!(app.agents_rows().len(), 1);
    app.handle_agents_key(key(KeyCode::Char('!')));
    app.handle_agents_key(key(KeyCode::Char('/')));
    for c in "billing".chars() {
        app.handle_agents_key(key(KeyCode::Char(c)));
    }
    app.handle_agents_key(key(KeyCode::Enter));
    assert_eq!(app.agents_rows().len(), 1);
    app.handle_agents_key(key(KeyCode::Char('/')));
    app.handle_agents_key(key(KeyCode::Esc));

    // A session's calls, newest first, and one in full.
    app.handle_agents_key(key(KeyCode::Enter));
    let calls: Vec<&str> = app.agents_rows().iter().map(|&i| app.agent_log[i].entry.sql.as_str()).collect();
    assert_eq!(calls, ["delete from users", "select 1"]);
    app.handle_agents_key(key(KeyCode::Enter));
    let detail = &app.cell_view.as_ref().unwrap().text;
    assert!(detail.contains("Agent       claude-code 2.0") && detail.contains("Refused: destructive"), "{detail}");
    assert_eq!(app.top_overlay(), Some(Overlay::CellView), "in front of the window");
    app.cell_view = None;

    // Open in a tab: named for the agent; the status says where it ran.
    app.handle_agents_key(key(KeyCode::Char('o')));
    assert!(app.agents_view.is_none());
    assert_eq!(app.tab.editor.text(), "delete from users");
    assert_eq!(app.tabs[app.active_tab].name, "claude-code");
    assert!(app.status.contains("ran this on dev"), "{}", app.status);

    // Something new after looking brings the note back.
    append(&log, Entry { changed: Some(3), ..entry("a", "12:00", "update t set x = 1 where id = 1") }).unwrap();
    app.agent_checked = None;
    app.update_agent_alerts();
    assert_eq!(app.agent_note().as_deref(), Some("agents: 1 wrote"));

    // Another project, nothing shared: no note, nothing from this one.
    let other = app.store.add_project("other").unwrap();
    app.open_project(&other);
    assert_eq!(app.agent_note(), None);
    assert!(app.agent_log.is_empty() && app.agents_view.is_none());
}

/// An agent's write waiting for the user: asked about with its SQL and dry
/// run (and a bell); y approves, anything else says no.
#[test]
fn agent_writes_wait_for_your_answer() {
    use crate::agents::{Request, answer, ask, pending_dir, read_answer};
    let (mut app, dir) = test_app();
    app.set_data_dir(dir.path().to_path_buf());
    let shared = ConnectForm { name: "prod".into(), agents: crate::db::AgentAccess::Approve, ..ConnectForm::new() };
    app.store.upsert(&project_id(&app), None, shared.to_profile()).unwrap();
    let pending = pending_dir(dir.path(), &project_id(&app));
    let request = |id: &str| Request {
        id: id.into(),
        agent: "billing".into(),
        connection: "prod".into(),
        production: true,
        sql: "update invoices set paid = true\nwhere id = 7".into(),
        dry_run: "dry run: would change 1 row(s)".into(),
    };
    ask(&pending, &request("1")).unwrap();
    ask(&pending, &request("2")).unwrap();
    app.approvals_tick();
    // A key typed as it appears, or any but y / n / Esc, doesn't answer.
    app.handle_key(key(KeyCode::Char('y')));
    assert_eq!(read_answer(&pending, "1"), None, "too soon");
    app.agent_asked = None;
    app.handle_key(key(KeyCode::Char('x')));
    app.handle_key(key(KeyCode::Enter));
    assert_eq!(read_answer(&pending, "1"), None);
    let confirm = app.confirm.as_ref().unwrap();
    assert_eq!(confirm.message, "billing asks to write on prod (PRODUCTION). Allow it?");
    assert_eq!(
        confirm.details,
        ["update invoices set paid = true", "where id = 7", "", "dry run: would change 1 row(s)"]
    );
    assert!(confirm.danger);
    assert!(app.take_notification().is_some_and(|n| n.contains("billing asks to write")));
    app.handle_key(key(KeyCode::Char('y')));
    assert_eq!(read_answer(&pending, "1"), Some(true));
    // The next one, once this is answered; Esc says no.
    app.approvals_tick();
    app.agent_asked = None;
    app.handle_key(key(KeyCode::Esc));
    assert_eq!(read_answer(&pending, "2"), Some(false));
    app.approvals_tick();
    assert!(app.confirm.is_none());
    // The agent gave up while the question was up.
    ask(&pending, &request("3")).unwrap();
    app.approvals_tick();
    app.agent_asked = None;
    crate::agents::forget(&pending, "3");
    app.handle_key(key(KeyCode::Char('y')));
    assert!(app.status.contains("stopped waiting"), "{}", app.status);
    assert!(!answer(&pending, "3", true));
}

/// While the agent window is open, new calls don't move its rows; Esc out
/// of a session found by searching goes back to it, still searched.
#[test]
fn agent_window_stays_put() {
    use crate::agents::{Entry, append, log_path};
    let (mut app, dir) = test_app();
    app.set_data_dir(dir.path().to_path_buf());
    let shared = ConnectForm { name: "dev".into(), agents: crate::db::AgentAccess::Read, ..ConnectForm::new() };
    app.store.upsert(&project_id(&app), None, shared.to_profile()).unwrap();
    let log = log_path(dir.path(), &project_id(&app));
    for (session, at, name) in [("a", "09", "alpha"), ("b", "10", "billing"), ("c", "11", "cron")] {
        let at = format!("2026-10-13T{at}:00:00+03:00");
        let entry =
            Entry { session: session.into(), at, agent: "bot".into(), name: Some(name.into()), ..Entry::default() };
        append(&log, entry).unwrap();
    }
    app.open_agents();
    app.handle_agents_key(key(KeyCode::Char('/')));
    for c in "bill".chars() {
        app.handle_agents_key(key(KeyCode::Char(c)));
    }
    app.handle_agents_key(key(KeyCode::Enter));
    app.handle_agents_key(key(KeyCode::Enter));
    assert_eq!(app.agents_view.as_ref().unwrap().session.map(|i| app.agent_sessions[i].label()), Some("billing"));

    // Another agent call meanwhile: the open window isn't reloaded.
    let late = Entry { session: "d".into(), at: "2026-10-13T12:00:00+03:00".into(), ..Entry::default() };
    append(&log, late).unwrap();
    app.agent_checked = None;
    app.agents_tick();
    assert_eq!(app.agent_sessions.len(), 3);

    app.handle_agents_key(key(KeyCode::Esc));
    let view = app.agents_view.as_ref().unwrap();
    assert_eq!(view.filter.text(), "bill");
    let rows = app.agents_rows();
    assert_eq!(app.agent_sessions[rows[view.selected]].label(), "billing");
}

/// A SQLite connection's timeout stops a query that runs too long.
#[tokio::test]
async fn sqlite_queries_time_out() {
    let (mut app, _dir) = connected_app(ConnectionMode::Normal).await;
    app.connect_form.timeout = "1".into();
    app.tab.editor.set_text("WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n) SELECT count(*) FROM n");
    app.handle_key(key(KeyCode::F(5)));
    assert!(app.executing);
    tokio::time::sleep(Duration::from_millis(1100)).await;
    app.tick_running();
    tokio::time::timeout(Duration::from_secs(5), finish(&mut app)).await.expect("the query was stopped");
    assert!(app.status.starts_with("Timed out after 1 s"), "{}", app.status);
}

/// "Export all rows" writes every row of a streamed result, not just those
/// loaded, without loading them into the grid; Esc stops it.
#[tokio::test]
async fn export_all_rows_of_a_streamed_result() {
    let (mut app, dir) = connected_app(ConnectionMode::Normal).await;
    app.settings.row_limit = 10;
    app.settings.export_dir = Some(dir.path().to_string_lossy().into_owned());
    let many = "WITH RECURSIVE s(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM s WHERE n < 25000) SELECT n FROM s";
    run_sql(&mut app, many).await;
    app.focus = Focus::Results;
    // A plain export says only the loaded rows went, and how to get all.
    app.handle_key(key(KeyCode::Char('e')));
    assert!(said(&app).contains("10 rows loaded") && said(&app).contains("export all"), "{}", said(&app));
    let exports = || {
        let mut files: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|e| e == "csv"))
            .collect();
        files.sort();
        files
    };
    for file in exports() {
        std::fs::remove_file(file).unwrap();
    }

    app.export_all(ExportFormat::Csv);
    loop {
        let event = app.poll_db_event().await.unwrap();
        app.handle_db_event(event);
        if app.export_task.is_none() {
            break;
        }
    }
    assert!(app.status.starts_with("Exported all 25000 rows to"), "{}", app.status);
    let text = std::fs::read_to_string(&exports()[0]).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!((lines.len(), lines[0], lines[1], lines[25000]), (25001, "n", "1", "25000"));
    // The grid kept what it had; nothing more streams.
    assert_eq!(app.tab.grid.results.as_ref().unwrap().rows.len(), 10);
    assert!(app.row_stream.is_none());

    // Stopped part-way: the unfinished file goes.
    std::fs::remove_file(&exports()[0]).unwrap();
    run_sql(&mut app, many).await;
    app.focus = Focus::Results;
    app.export_all(ExportFormat::Json);
    assert!(app.export_task.is_some());
    app.handle_key(key(KeyCode::Esc));
    assert!(app.export_task.is_none() && said(&app).contains("Export stopped"), "{}", said(&app));
    // The export cleans up, then says it's done (and that's ignored).
    loop {
        let event = app.poll_db_event().await.unwrap();
        let done = matches!(event.kind, DbEventKind::ExportDone(..));
        app.handle_db_event(event);
        if done {
            break;
        }
    }
    assert!(said(&app).contains("Export stopped"), "{}", said(&app));
    let json_files = std::fs::read_dir(dir.path())
        .unwrap()
        .filter(|e| e.as_ref().unwrap().path().extension().is_some_and(|x| x == "json"))
        .count();
    assert_eq!(json_files, 0);
    // The connection is free for the next query.
    run_sql(&mut app, "select 42").await;
    assert_eq!(app.tab.grid.results.as_ref().unwrap().rows[0][0].as_deref(), Some("42"));
}

#[tokio::test]
async fn autocomplete_from_catalog() {
    let (mut app, _dir) = connected_app(ConnectionMode::Normal).await;
    run_sql(&mut app, "create table users (id int, email text)").await;
    // DDL triggers a catalog reload; wait for it.
    while app.schema.as_ref().is_none_or(|s| s.loading) {
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

/// Makes the pending syntax check due now and runs it to completion.
async fn check_syntax_now(app: &mut App) {
    app.lint_due = Some(Instant::now());
    // The check runs in the background (with or without the server).
    app.tick();
    loop {
        let event = app.poll_db_event().await.unwrap();
        let done = matches!(event.kind, DbEventKind::SyntaxChecked { .. });
        app.handle_db_event(event);
        if done {
            break;
        }
    }
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

#[tokio::test]
async fn long_queries_announce_their_end_unless_you_are_watching() {
    let (mut app, _dir) = connected_app(ConnectionMode::Normal).await;
    let long_ago = || Some(Instant::now().checked_sub(Duration::from_secs(30)).unwrap());
    // A quick query: nothing.
    run_sql(&mut app, "select 1").await;
    assert_eq!(app.take_notification(), None);

    app.run_extra.started = long_ago();
    app.notify_if_long(|took| format!("took {}", took.as_secs()));
    assert_eq!(app.take_notification().as_deref(), Some("took 30"));

    // Looking at the terminal: you saw it end.
    app.set_terminal_focus(true);
    app.run_extra.started = long_ago();
    app.notify_if_long(|_| "x".into());
    assert_eq!(app.take_notification(), None);

    app.set_terminal_focus(false);
    app.settings.notify_after = 0;
    app.run_extra.started = long_ago();
    app.notify_if_long(|_| "x".into());
    assert_eq!(app.take_notification(), None);
}

#[tokio::test]
async fn a_script_shows_every_result_set() {
    let (mut app, _dir) = connected_app(ConnectionMode::Normal).await;
    run_sql(&mut app, "create table t (id int); insert into t values (1), (2)").await;
    run_sql(
        &mut app,
        "select 1 as a;\ninsert into t values (3);\nselect * from t where id > 5;\nselect count(*) as n from t",
    )
    .await;
    // The last result is shown; the earlier SELECTs (even an empty one) are kept.
    assert_eq!(app.tab.grid.result_sets.len(), 3);
    assert_eq!(app.tab.grid.result_set, 2);
    let shown =
        |app: &App| app.tab.grid.results.as_ref().map(|r| (r.statement, r.columns.clone(), r.rows.len())).unwrap();
    assert_eq!(shown(&app), (4, vec!["n".to_string()], 1));
    assert!(app.status.contains("3 result sets"), "{}", app.status);

    app.focus = Focus::Results;
    app.handle_key(key(KeyCode::Char('>')));
    assert_eq!((app.tab.grid.result_set, shown(&app)), (0, (1, vec!["a".to_string()], 1)));
    app.handle_key(key(KeyCode::Char('>')));
    assert_eq!((app.tab.grid.result_set, shown(&app)), (1, (3, vec!["id".to_string()], 0)));
    app.handle_key(key(KeyCode::Char('<')));
    assert_eq!(app.tab.grid.result_set, 0);

    // One statement: no sets.
    run_sql(&mut app, "select 1").await;
    assert!(app.tab.grid.result_sets.is_empty());
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

#[tokio::test]
async fn the_syntax_check_runs_off_the_ui_thread_and_drops_stale_results() {
    let (mut app, _dir) = test_app();
    app.tab.editor.set_text("delete from t");
    app.lint_due = Some(Instant::now());
    app.tick();
    // Nothing yet: parsing and linting happen in the background.
    assert_eq!(app.tab.syntax_hint, None);
    let event = app.poll_db_event().await.unwrap();
    app.handle_db_event(event);
    assert!(app.tab.syntax_hint.as_ref().is_some_and(|h| h.source == HintSource::Lint("no-where")));

    // The text changed while the check ran: its result is dropped.
    app.tab.syntax_hint = None;
    app.lint_due = Some(Instant::now());
    app.tick();
    app.tab.editor.set_text("delete from t where id = 1");
    let event = app.poll_db_event().await.unwrap();
    app.handle_db_event(event);
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
    assert_eq!((app.connect_form.host.as_str(), app.connect_form.port.as_str()), ("db", "3307"));
    assert!(app.connect_form.info.is_some());
    // Ordinary text still goes into the focused field.
    app.connect_form.focus = ConnectField::Name;
    app.handle_paste("primary");
    assert!(app.connect_form.name.ends_with("primary"));
}

#[tokio::test]
async fn f7_shows_the_plan_without_running() {
    let (mut app, _dir) = connected_app(ConnectionMode::Normal).await;
    run_sql(&mut app, "create table t (id integer primary key, v text); insert into t values (1, 'a')").await;
    app.tab.editor.set_text("delete from t where id = 1");
    app.handle_key(key(KeyCode::F(7)));
    assert!(app.confirm.is_none(), "EXPLAIN of a DELETE runs nothing, so no confirmation");
    finish(&mut app).await;
    // The plan goes to the QUERY PLAN tab; the row is still there.
    assert_eq!(app.tab.result_tab, ResultTab::Plan);
    let plan = app.tab.plan.as_ref().map(|p| p.text.as_str()).expect("plan");
    assert!(plan.contains("SEARCH t"), "{plan}");
    run_sql(&mut app, "select count(*) from t").await;
    assert_eq!(app.tab.result_tab, ResultTab::Rows, "a normal run switches back to the rows");
    assert_eq!(app.tab.grid.results.as_ref().unwrap().rows, vec![vec![Some("1".to_string())]]);
}

#[tokio::test]
async fn runs_show_their_tab_at_once_and_f7_waits_for_a_running_query() {
    let (mut app, _dir) = connected_app(ConnectionMode::Normal).await;
    run_sql(&mut app, "create table t (id integer primary key)").await;
    // F7 shows QUERY PLAN as it starts, F5 the RESULT tab.
    app.tab.editor.set_text("select * from t");
    app.handle_key(key(KeyCode::F(7)));
    assert!(app.executing && app.tab.result_tab == ResultTab::Plan);
    finish(&mut app).await;
    // (Showing the plan moved to the results; back to typing.)
    app.focus = Focus::Editor;
    app.handle_key(key(KeyCode::F(5)));
    assert!(app.executing && app.tab.result_tab == ResultTab::Rows);
    // While it runs, F5 says why it does nothing, and F7 waits for it.
    app.handle_key(key(KeyCode::F(5)));
    assert!(app.editor_note_is_error && said(&app).starts_with("A query is running"), "{}", said(&app));
    app.tab.editor.set_text("select id from t where id = 1");
    app.handle_key(key(KeyCode::F(7)));
    assert!(said(&app).starts_with("The plan follows"), "{}", said(&app));
    // In the editor's footer: the results pane still says it's running.
    assert_eq!(app.status, "Running… (Esc to cancel)");
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
    assert!(app.status.contains("nope") && app.status.ends_with(" · the plan wasn't made"), "{}", app.status);
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
async fn copying_names_rows_as_csv_and_where_messages_show() {
    let (mut app, _dir) = connected_app(ConnectionMode::Normal).await;
    run_sql(&mut app, "create table pets (id integer primary key, name text)").await;
    finish_catalog(&mut app).await;
    run_sql(&mut app, "insert into pets (name) values ('Rex'), ('Tom')").await;
    run_sql(&mut app, "select * from pets order by id").await;
    // The navigator: a table as written in SQL, with its schema.
    app.focus = Focus::Schema;
    let panel = app.schema.as_mut().unwrap();
    panel.selected = panel.nodes().iter().position(|n| matches!(n, SchemaNode::Table(_))).unwrap();
    app.handle_key(ctrl('c'));
    assert_eq!(app.take_clipboard().as_deref(), Some("main.pets"));
    app.handle_key(key(KeyCode::Char('y')));
    assert!(app.take_clipboard().is_some());
    // Rows as CSV: those marked, else all shown.
    app.focus = Focus::Results;
    app.run_command(crate::app::palette::Command::CopyRows(ExportFormat::Csv));
    assert_eq!(app.take_clipboard().as_deref(), Some("id,name\r\n1,Rex\r\n2,Tom\r\n"));
    // A results key's message is in the results' footer, and in MESSAGES.
    app.handle_key(key(KeyCode::Char('Y')));
    assert_eq!(app.results_note.as_ref().map(|(m, _)| m.as_str()), Some("Copied row (tab-separated)"));
    assert_eq!(
        (app.status.as_str(), app.messages.last().map(|m| m.text.as_str())),
        ("", Some("Copied row (tab-separated)"))
    );
    // A run shows how long it's been going.
    assert_eq!(app.running_label(), None);
    app.executing = true;
    app.running_since = Instant::now().checked_sub(Duration::from_secs(75));
    assert_eq!(app.running_label().as_deref(), Some("running… 1:15"));
    app.running_since = Instant::now().checked_sub(Duration::from_secs(3));
    assert_eq!(app.running_label().as_deref(), Some("running… 3 s"));
    app.executing = false;
}

#[tokio::test]
async fn other_tabs_can_be_used_while_a_read_runs() {
    let (mut app, _dir) = connected_app(ConnectionMode::Normal).await;
    run_sql(&mut app, "create table t (id integer primary key); insert into t values (1), (2)").await;
    // A page of one row: the read streams.
    app.settings.row_limit = 1;
    app.tab.editor.set_text("select id from t");
    app.handle_key(key(KeyCode::F(5)));
    assert!(app.executing && app.run_owner() == Some(0));
    // A new tab opens meanwhile; there, the run is another tab's.
    app.new_tab(None, "select 2", None);
    assert_eq!(app.active_tab, 1);
    assert!(app.status.starts_with("Tab “Query 1” on :memory: is running a query: Alt+1 goes to it"), "{}", app.status);
    assert_eq!(app.running_label(), None, "this tab's results aren't running");
    app.handle_key(key(KeyCode::F(5)));
    assert!(said(&app).ends_with("wait for it, or Esc there stops it"), "{}", said(&app));
    // (Esc here says whose it is: `esc_stops_only_its_own_tabs_query_and_others_say_whose_runs`.)
    // The outcome lands in its own tab, and the status bar says so here.
    finish(&mut app).await;
    assert!(app.tab.grid.results.is_none());
    assert!(app.status.starts_with("Query 1: "), "{}", app.status);
    // Its stream was closed while hidden: M runs it again for the rest.
    assert!(app.row_stream.is_none());
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
    assert!(app.executing && app.run_owner().is_none());
    app.switch_tab(1);
    assert_eq!(app.active_tab, 0);
    finish(&mut app).await;
}

#[tokio::test]
async fn result_tabs_messages_and_help() {
    let (mut app, _dir) = connected_app(ConnectionMode::Normal).await;
    run_sql(&mut app, "select 1").await;
    run_sql(&mut app, "select nope").await;
    assert_eq!(app.messages.iter().filter(|m| m.is_error).count(), 1);
    assert!(app.messages.iter().any(|m| m.text.starts_with("Connected to")));

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

/// Edits a cell of `select … from people` and returns the app.
async fn edit_cell(mode: ConnectionMode, select: &str, col: usize) -> (App, tempfile::TempDir) {
    // Set up in normal mode, then switch to the mode under test.
    let (mut app, dir) = connected_app(ConnectionMode::Normal).await;
    run_sql(&mut app, "create table people (id integer primary key, name text not null, note text)").await;
    run_sql(&mut app, "insert into people values (1, 'Ann', 'x'), (2, 'Ben', null)").await;
    while app.schema.as_ref().is_none_or(|s| s.loading) {
        let event = app.poll_db_event().await.unwrap();
        app.handle_db_event(event);
    }
    app.active_mode = mode;
    run_sql(&mut app, select).await;
    app.focus = Focus::Results;
    app.handle_key(key(KeyCode::Down));
    app.tab.grid.selected_col = col;
    app.handle_key(key(KeyCode::Char('c')));
    (app, dir)
}

#[tokio::test]
async fn edit_a_cell_by_primary_key() {
    let (mut app, _dir) = Box::pin(edit_cell(ConnectionMode::Normal, "select * from people order by id", 2)).await;
    let edit = app.cell_edit.as_ref().expect("prompt opens");
    assert_eq!((edit.column.as_str(), edit.text.text()), ("note", ""));
    type_text(&mut app, r"it's \ fine?");
    assert!(app.palette.is_none(), "? is typed into the value, not the keys");
    app.handle_key(key(KeyCode::Enter));
    finish(&mut app).await;
    assert_eq!(app.tab.grid.results.as_ref().unwrap().rows[1][2].as_deref(), Some(r"it's \ fine?"));
    assert!(app.status.contains("updated 1 row"), "{}", app.status);

    // The database really changed, and only that row.
    run_sql(&mut app, "select note from people order by id").await;
    let rows = &app.tab.grid.results.as_ref().unwrap().rows;
    assert_eq!((rows[0][0].as_deref(), rows[1][0].as_deref()), (Some("x"), Some(r"it's \ fine?")));

    // Ctrl+N sets NULL on a nullable column.
    run_sql(&mut app, "select id, note from people order by id").await;
    app.focus = Focus::Results;
    app.tab.grid.selected_col = 1;
    app.handle_key(key(KeyCode::Char('c')));
    app.handle_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL));
    finish(&mut app).await;
    assert_eq!(app.tab.grid.results.as_ref().unwrap().rows[0][1], None);
}

#[tokio::test]
async fn editing_is_refused_when_rows_cant_be_identified() {
    for (select, col, why) in [
        ("select name from people", 0, "primary key column id"),
        ("select id, upper(name) as name from people", 1, "computed"),
        ("select p.id, q.name from people p join people q on q.id = p.id", 1, "plain SELECT"),
    ] {
        let (app, _dir) = Box::pin(edit_cell(ConnectionMode::Normal, select, col)).await;
        assert!(app.cell_edit.is_none(), "{select}");
        assert!(said(&app).contains(why), "{select}: {}", said(&app));
    }
    let (app, _dir) = Box::pin(edit_cell(ConnectionMode::ReadOnly, "select * from people", 1)).await;
    assert!(app.cell_edit.is_none() && said(&app).contains("read-only"), "{}", said(&app));
}

#[tokio::test]
async fn null_refused_on_not_null_column_and_production_confirms() {
    let (mut app, _dir) = Box::pin(edit_cell(ConnectionMode::Normal, "select * from people", 1)).await;
    app.handle_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL));
    assert!(app.status.contains("doesn't allow NULL") && app.cell_edit.is_some());
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

#[test]
fn sql_literals_escape_per_engine() {
    use crate::db::sql_literal;
    assert_eq!(sql_literal("it's", Engine::Postgres), "'it''s'");
    assert_eq!(sql_literal(r"a\b", Engine::Postgres), r"'a\b'");
    assert_eq!(sql_literal(r"a\b'", Engine::MySql), r"'a\\b'''");
}

#[tokio::test]
async fn large_results_stream_in_as_you_scroll() {
    let (mut app, _dir) = connected_app(ConnectionMode::Normal).await;
    app.settings.row_limit = 10;
    let rows = "WITH RECURSIVE s(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM s WHERE n < 125) SELECT n FROM s";
    run_sql(&mut app, rows).await;
    assert_eq!(app.tab.grid.results.as_ref().unwrap().rows.len(), 10);
    assert!(app.row_stream.is_some(), "more rows to come");

    // Moving near the end fetches the next page.
    app.focus = Focus::Results;
    app.handle_key(key(KeyCode::End));
    assert!(app.fetching_more);
    let event = app.poll_db_event().await.unwrap();
    app.handle_db_event(event);
    assert_eq!(app.tab.grid.results.as_ref().unwrap().rows.len(), 20);

    // M fetches ten pages at once; the stream ends when the rows do.
    app.handle_key(key(KeyCode::Char('M')));
    let event = app.poll_db_event().await.unwrap();
    app.handle_db_event(event);
    assert_eq!(app.tab.grid.results.as_ref().unwrap().rows.len(), 120);
    app.handle_key(key(KeyCode::Char('M')));
    let event = app.poll_db_event().await.unwrap();
    app.handle_db_event(event);
    assert_eq!(app.tab.grid.results.as_ref().unwrap().rows.len(), 125);
    assert!(app.row_stream.is_none());

    // A new query while a stream is open closes it and runs normally.
    run_sql(&mut app, rows).await;
    assert!(app.row_stream.is_some());
    run_sql(&mut app, "select 42").await;
    assert_eq!(app.tab.grid.results.as_ref().unwrap().rows, vec![vec![Some("42".to_string())]]);
    assert!(app.row_stream.is_none());

    // Scripts and writes don't stream.
    run_sql(&mut app, &format!("select 1; {rows}")).await;
    assert!(app.row_stream.is_none() && app.tab.grid.results.as_ref().unwrap().truncated());
}

#[tokio::test]
async fn insert_export_round_trips_through_the_database() {
    let (mut app, dir) = connected_app(ConnectionMode::Normal).await;
    run_sql(
        &mut app,
        "create table src (id integer primary key, name text, score real); \
                       insert into src values (1, 'a''b', 1.5), (2, null, 2)",
    )
    .await;
    run_sql(&mut app, "select * from src order by id").await;
    app.settings.export_dir = Some(dir.path().to_string_lossy().into_owned());
    app.focus = Focus::Results;
    app.handle_key(key(KeyCode::Char('i')));
    let file = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(Result::ok)
        .find(|e| e.file_name().to_string_lossy().ends_with(".sql"))
        .expect("export written");
    let inserts = std::fs::read_to_string(file.path()).unwrap();
    assert!(inserts.starts_with("INSERT INTO src (id, name, score) VALUES (1, 'a''b', 1.5);"), "{inserts}");
    // Replaying the export into a copy reproduces the rows.
    run_sql(
        &mut app,
        &format!("create table copy as select * from src where 0; {}", inserts.replace("INTO src", "INTO copy")),
    )
    .await;
    run_sql(&mut app, "select count(*) from copy where name = 'a''b' or name is null").await;
    assert_eq!(app.tab.grid.results.as_ref().unwrap().rows, vec![vec![Some("2".to_string())]]);
}

fn alt(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::ALT)
}

/// The last message: in the editor's or the results' footer, else the
/// status bar.
fn said(app: &App) -> &str {
    let results = app.results_note.as_ref().map(|(m, _)| m.as_str());
    app.editor_note.as_deref().or(results).unwrap_or(&app.status)
}

/// Whether that message is an error.
fn said_error(app: &App) -> bool {
    match (&app.editor_note, &app.results_note) {
        (Some(_), _) => app.editor_note_is_error,
        (None, Some((_, is_error))) => *is_error,
        (None, None) => app.status_is_error,
    }
}

fn ctrl(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
}

/// Ctrl+K p: the projects and connections.
fn show_projects(app: &mut App) {
    app.handle_key(ctrl('k'));
    app.handle_key(key(KeyCode::Char('p')));
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
    assert_eq!(app.library_items(), [LibraryItem::History("select 42 as answer".into())]);
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

#[tokio::test]
async fn parameters_are_asked_for_and_remembered() {
    let (mut app, _dir) = connected_app(ConnectionMode::Normal).await;
    run_sql(&mut app, "create table t (id integer, name text); insert into t values (1, 'a'), (2, 'b')").await;
    app.tab.editor.set_text("select name from t where id = :id or name = :name");
    app.handle_key(key(KeyCode::F(5)));
    assert!(!app.executing);
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
    assert!(app.param_form.is_none() && !app.executing);

    // An error in the filled SQL is marked at the right place in the editor.
    app.tab.editor.set_text("select :v from nope");
    app.handle_key(key(KeyCode::F(5)));
    type_text(&mut app, "12345678");
    app.handle_key(key(KeyCode::Enter));
    finish(&mut app).await;
    assert!(app.status_is_error);
}

#[tokio::test]
async fn schema_details_load_on_expand_and_definitions_open() {
    let (mut app, _dir) = connected_app(ConnectionMode::Normal).await;
    run_sql(
        &mut app,
        "create table parent (id integer primary key);
         create table child (id integer primary key, parent_id integer references parent (id));
         create index child_parent on child (parent_id);",
    )
    .await;
    while app.schema.as_ref().is_none_or(|s| s.loading) {
        let event = app.poll_db_event().await.unwrap();
        app.handle_db_event(event);
    }
    let wait_details = async |app: &mut App| {
        while app.schema.as_ref().unwrap().tables.iter().any(|t| t.details_loading) {
            let event = app.poll_db_event().await.unwrap();
            app.handle_db_event(event);
        }
    };
    app.focus = Focus::Schema;
    let panel = app.schema.as_mut().unwrap();
    let child = panel.tables.iter().position(|t| t.info.name == "child").unwrap();
    panel.selected = panel.nodes().iter().position(|n| *n == SchemaNode::Table(child)).unwrap();
    app.handle_key(key(KeyCode::Enter));
    wait_details(&mut app).await;
    let panel = app.schema.as_ref().unwrap();
    assert!(panel.nodes().contains(&SchemaNode::Index(child, 0)));
    assert_eq!(panel.tables[child].foreign_key("parent_id"), Some("parent.id"));

    // `d` shows the table's CREATE statement (SQLite keeps it).
    app.handle_key(key(KeyCode::Char('d')));
    let view = app.cell_view.as_ref().expect("definition shown");
    assert!(view.title.contains("child") && view.text.contains("references parent"), "{}", view.text);
    app.handle_key(key(KeyCode::Esc));

    // On a table whose details aren't loaded yet, `d` opens it once they arrive.
    let panel = app.schema.as_mut().unwrap();
    let parent = panel.tables.iter().position(|t| t.info.name == "parent").unwrap();
    panel.selected = panel.nodes().iter().position(|n| *n == SchemaNode::Table(parent)).unwrap();
    app.handle_key(key(KeyCode::Char('d')));
    assert!(app.cell_view.is_none());
    wait_details(&mut app).await;
    assert!(app.cell_view.as_ref().is_some_and(|v| v.text.contains("CREATE TABLE parent")));
}

#[tokio::test]
async fn system_tables_toggle_in_their_own_section() {
    let (mut app, _dir) = connected_app(ConnectionMode::Normal).await;
    run_sql(&mut app, "create table t (id integer primary key autoincrement); insert into t default values;").await;
    finish_catalog(&mut app).await;
    let sections = |app: &App| -> Vec<SchemaSection> {
        let nodes = app.schema.as_ref().unwrap().nodes();
        nodes.into_iter().filter_map(|n| if let SchemaNode::Section(s) = n { Some(s) } else { None }).collect()
    };
    assert_eq!(sections(&app), [SchemaSection::Tables]);
    assert!(!table_names(&app).iter().any(|n| n.starts_with("sqlite_")));

    app.focus = Focus::Schema;
    app.handle_key(key(KeyCode::Char('.')));
    finish_catalog(&mut app).await;
    assert_eq!(sections(&app), [SchemaSection::Tables, SchemaSection::System]);
    let names = table_names(&app);
    assert!(names.contains(&"sqlite_schema".into()) && names.contains(&"sqlite_sequence".into()), "{names:?}");

    app.handle_key(key(KeyCode::Char('.')));
    finish_catalog(&mut app).await;
    assert_eq!(sections(&app), [SchemaSection::Tables]);
}

#[tokio::test]
async fn a_missing_sqlite_file_is_created_only_when_asked_again() {
    let (mut app, dir) = test_app();
    let path = dir.path().join("typo.db");
    app.connect_form = ConnectForm::from_profile(&sqlite_profile("t", &path), String::new());
    app.screen = Screen::Connect;
    app.handle_key(key(KeyCode::Enter));
    assert!(!app.connect_form.connecting && !path.exists());
    let info = app.connect_form.info.clone().unwrap_or_default();
    assert!(info.contains("doesn't exist") && info.contains("Enter again"), "{info}");

    // Another path asks again.
    let other = dir.path().join("other.db");
    app.connect_form.host = other.to_string_lossy().into_owned();
    app.handle_key(key(KeyCode::Enter));
    assert!(!app.connect_form.connecting && !other.exists());

    // Enter again for the same path: created and connected.
    app.handle_key(key(KeyCode::Enter));
    assert!(app.connect_form.connecting);
    while app.connect_form.connecting {
        let event = app.poll_db_event().await.unwrap();
        app.handle_db_event(event);
    }
    assert!(other.exists() && app.database.is_some());
}

fn sqlite_profile(name: &str, path: &std::path::Path) -> ConnectionProfile {
    ConnectionProfile {
        name: name.into(),
        engine: Engine::Sqlite,
        host: path.to_string_lossy().into_owned(),
        port: String::new(),
        user: String::new(),
        database: String::new(),
        ssl_mode: SslMode::Disable,
        mode: ConnectionMode::Normal,
        transaction: None,
        ssh: String::new(),
        timeout: String::new(),
        password_command: Vec::new(),
        agents: crate::db::AgentAccess::None,
        legacy_keychain: false,
    }
}

/// Connects the active tab to a saved SQLite connection, as picking it does.
async fn connect_profile(app: &mut App, profile: &ConnectionProfile) {
    app.connect_form = ConnectForm::from_profile(profile, String::new());
    let db = Database::connect(app.connect_form.params().unwrap()).await.unwrap();
    let generation = app.generation;
    app.handle_db_event(DbEvent { generation, run: None, kind: DbEventKind::Connected(db) });
    finish_catalog(app).await;
}

async fn finish_catalog(app: &mut App) {
    while app.schema.as_ref().is_none_or(|s| s.loading) || app.executing {
        let event = app.poll_db_event().await.unwrap();
        app.handle_db_event(event);
    }
}

fn table_names(app: &App) -> Vec<String> {
    app.schema.as_ref().map(|s| s.tables.iter().map(|t| t.info.name.clone()).collect()).unwrap_or_default()
}

#[tokio::test]
async fn each_tab_keeps_its_own_connection() {
    let (mut app, dir) = test_app();
    let project = project_id(&app);
    let (a, b) = (sqlite_profile("alpha", &dir.path().join("a.db")), sqlite_profile("beta", &dir.path().join("b.db")));
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
    assert!(app.database.is_none());
    connect_profile(&mut app, &b).await;
    run_sql(&mut app, "create table in_beta (y int)").await;
    finish_catalog(&mut app).await;
    assert_eq!(table_names(&app), ["in_beta"]);
    assert_eq!(app.connect_form.name, "beta");

    app.handle_key(alt('1'));
    assert_eq!((app.connect_form.name.as_str(), table_names(&app)), ("alpha", vec!["in_alpha".to_string()]));
    run_sql(&mut app, "select count(*) from in_alpha").await;
    assert!(!app.status_is_error, "{}", app.status);

    // Ctrl+D disconnects only this tab; the others keep theirs.
    app.handle_key(alt('3'));
    app.handle_key(ctrl('d'));
    assert!(matches!(app.screen, Screen::Connections));
    assert!(app.tabs[2].conn.is_none() && app.tabs[0].conn.is_some());
    // Esc goes back to the tabs, dropping the empty unconnected one.
    app.tab.editor.set_text("");
    app.handle_key(key(KeyCode::Esc));
    assert!(matches!(app.screen, Screen::Query));
    assert_eq!(app.tabs.len(), 2);
    assert_eq!(app.connect_form.name, "alpha");
}

/// Waits for the next event of a kind and handles everything until then.
async fn until_event(app: &mut App, done: fn(&DbEventKind) -> bool) {
    loop {
        let event = app.poll_db_event().await.unwrap();
        let last = done(&event.kind);
        app.handle_db_event(event);
        if last {
            return;
        }
    }
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
    assert!(app.watch.is_none() && !app.executing, "{}", app.status);
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

/// `b` charts the selected column of the rows shown.
#[tokio::test]
async fn chart_a_result_column() {
    let (mut app, _dir) = connected_app(ConnectionMode::Normal).await;
    run_sql(&mut app, "select 'a' as day, 3 as n union all select 'b', 6").await;
    app.focus = Focus::Results;
    app.handle_key(key(KeyCode::Char('b')));
    assert!(said_error(&app) && said(&app).contains("day has no numbers"), "{}", said(&app));
    app.handle_key(key(KeyCode::Right));
    app.handle_key(key(KeyCode::Char('b')));
    let view = app.cell_view.as_ref().unwrap();
    assert_eq!(view.title, "CHART · n");
    assert!(view.text.contains(&format!("b │{} 6", "█".repeat(40))), "{}", view.text);
}

/// A schema snapshot saved, then the schema changed: comparing with the
/// snapshot shows what changed since, and the SQL for it.
#[tokio::test]
async fn schema_snapshots_show_what_changed_since() {
    let (mut app, dir) = connected_app(ConnectionMode::Normal).await;
    app.set_data_dir(dir.path().to_path_buf());
    run_sql(&mut app, "create table people (id integer primary key, name text)").await;
    app.snapshot_schema();
    until_event(&mut app, |k| matches!(k, DbEventKind::SnapshotSaved(_))).await;
    assert!(app.status.starts_with("Saved a schema snapshot (1 tables)"), "{}", app.status);
    assert_eq!(app.snapshots().len(), 1);
    app.open_palette(false);
    let entry = app.palette_command_text(super::palette::Command::CompareSnapshot(0));
    app.palette = None;
    assert!(entry.starts_with("Compare the schema with the snapshot: "), "{entry}");

    run_sql(&mut app, "alter table people add column email text; create table orders (id integer primary key)").await;
    app.compare_snapshot(0);
    until_event(&mut app, |k| matches!(k, DbEventKind::SnapshotCompared(_))).await;
    assert!(app.status.starts_with("2 change(s) since the snapshot"), "{}", app.status);
    let text = &app.cell_view.as_ref().unwrap().text;
    assert!(text.contains("ONLY HERE\n  table orders") && text.contains("column email TEXT: only here"), "{text}");
    assert!(text.contains("people ADD COLUMN email TEXT;"), "{text}");
}

#[tokio::test]
async fn a_read_finishes_in_its_tab_while_one_on_another_connection_is_shown() {
    let (mut app, dir) = test_app();
    let project = project_id(&app);
    let (a, b) = (sqlite_profile("alpha", &dir.path().join("a.db")), sqlite_profile("beta", &dir.path().join("b.db")));
    app.store.upsert(&project, None, a.clone()).unwrap();
    app.store.upsert(&project, None, b.clone()).unwrap();
    connect_profile(&mut app, &a).await;
    run_sql(&mut app, "create table here (x int); insert into here values (1)").await;
    finish_catalog(&mut app).await;
    app.handle_key(ctrl('o'));
    connect_profile(&mut app, &b).await;
    app.handle_key(alt('1'));
    assert_eq!(app.connect_form.name, "alpha");

    app.tab.editor.set_text("select x from here");
    app.handle_key(key(KeyCode::F(5)));
    app.handle_key(alt('2'));
    assert_eq!((app.active_tab, app.connect_form.name.as_str()), (1, "beta"));
    finish(&mut app).await;
    // Still on beta's tab, with beta's connection; alpha's tab has its rows.
    assert_eq!((app.active_tab, app.connect_form.name.as_str()), (1, "beta"));
    assert!(app.tab.grid.results.is_none());
    assert!(app.status.starts_with("Query 1: 1 row(s)"), "{}", app.status);
    app.handle_key(alt('1'));
    assert_eq!(app.connect_form.name, "alpha");
    assert_eq!(app.tab.grid.results.as_ref().unwrap().rows, vec![vec![Some("1".to_string())]]);
    // Both connections still work.
    run_sql(&mut app, "select count(*) from here").await;
    assert_eq!(app.tab.grid.results.as_ref().unwrap().rows, vec![vec![Some("1".to_string())]]);
}

#[tokio::test]
async fn compare_schemas_and_rows_with_another_tabs_connection() {
    let (mut app, dir) = test_app();
    let project = project_id(&app);
    let (a, b) =
        (sqlite_profile("staging", &dir.path().join("a.db")), sqlite_profile("prod", &dir.path().join("b.db")));
    app.store.upsert(&project, None, a.clone()).unwrap();
    app.store.upsert(&project, None, b.clone()).unwrap();
    connect_profile(&mut app, &a).await;
    // Nothing to compare with yet.
    app.compare_schema(None);
    assert!(app.status.contains("Open another connection"), "{}", app.status);

    run_sql(&mut app, "create table users (id integer primary key, email text, plan text); create index users_email on users (email); insert into users values (1, 'a', 'free'), (2, 'b', 'pro'), (3, 'c', 'pro')").await;
    finish_catalog(&mut app).await;
    app.handle_key(ctrl('o'));
    connect_profile(&mut app, &b).await;
    run_sql(&mut app, "create table users (id integer primary key, email text); insert into users values (1, 'a'), (2, 'bb'), (4, 'd')").await;
    finish_catalog(&mut app).await;
    app.handle_key(alt('1'));
    assert_eq!(app.connect_form.name, "staging");

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
    assert_eq!((app.tabs.len(), app.active_tab, app.connect_form.name.as_str()), (3, 2, "prod"));
    assert!(app.tab.editor.text().starts_with("-- Makes prod match staging"), "{}", app.tab.editor.text());

    app.handle_key(alt('1'));
    app.tab.editor.set_text("select id, email from users order by id");
    app.compare_rows(None);
    until_event(&mut app, |k| matches!(k, DbEventKind::RowsCompared { .. })).await;
    assert!(app.status.contains("1 only here, 1 only there, 1 changed, 1 the same"), "{}", app.status);
    let rows: Vec<String> = app.tab.grid.results.as_ref().unwrap().rows.iter().map(|r| r[0].clone().unwrap()).collect();
    assert_eq!(rows, ["≠ here", "≠ there", "− here", "+ there"]);
    // A comparison isn't a table: no editing.
    app.focus = Focus::Results;
    app.handle_key(key(KeyCode::Char('c')));
    assert!(app.cell_edit.is_none());

    // Writes aren't compared (they'd run twice).
    app.tab.editor.set_text("delete from users");
    app.compare_rows(None);
    assert!(app.status.contains("Only reads"), "{}", app.status);
}

#[tokio::test]
async fn session_is_restored_on_startup() {
    let (mut app, dir) = test_app();
    app.data_dir = Some(dir.path().to_path_buf());
    let project = project_id(&app);
    app.store.set_last_project(&project).unwrap();
    let (a, b) = (sqlite_profile("alpha", &dir.path().join("a.db")), sqlite_profile("beta", &dir.path().join("b.db")));
    app.store.upsert(&project, None, a.clone()).unwrap();
    app.store.upsert(&project, None, b.clone()).unwrap();
    app.load_tabs(&project);
    connect_profile(&mut app, &a).await;
    run_sql(&mut app, "create table t1 (x int); create table t2 (y int)").await;
    finish_catalog(&mut app).await;
    app.tab.editor.set_text("select 1;\nselect 2");
    app.tab.editor.restore_cursor((1, 3), 0);
    // Expand t2 in the navigator.
    let panel = app.schema.as_mut().unwrap();
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
    while !app.reconnecting().is_empty() || app.schema.as_ref().is_none_or(|s| s.loading) {
        let event = app.poll_db_event().await.unwrap();
        app.handle_db_event(event);
    }
    assert!(app.splash.is_none(), "gone once everything is reopened");
    assert_eq!(app.connect_form.name, "alpha");
    let panel = app.schema.as_ref().unwrap();
    assert!(panel.tables.iter().any(|t| t.info.name == "t2" && t.expanded));
    app.handle_key(alt('2'));
    assert_eq!((app.connect_form.name.as_str(), app.tab.editor.text().as_str()), ("beta", "select 'beta'"));

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
    assert!(app.status.contains("doesn't exist"), "{}", app.status);
    // Once it's back, a run in its tab opens it again and goes ahead: no
    // need to pick the connection again, or to restart.
    app.handle_key(alt('2'));
    assert!(app.database.is_none());
    std::fs::write(dir.path().join("b.db"), "").unwrap();
    app.handle_key(key(KeyCode::F(5)));
    assert!(said(&app).starts_with("Reconnecting to beta"), "{}", said(&app));
    while app.tab.grid.results.is_none() {
        let event = app.poll_db_event().await.unwrap();
        app.handle_db_event(event);
    }
    assert_eq!(app.connect_form.name, "beta");
    assert_eq!(app.tab.grid.results.as_ref().unwrap().rows, vec![vec![Some("beta".to_string())]]);
    // Disconnected on purpose: a run says so, and doesn't open it again.
    app.disconnect();
    app.screen = Screen::Query;
    app.handle_key(key(KeyCode::F(5)));
    assert!(app.reconnecting().is_empty() && app.database.is_none());
    assert!(said(&app).contains("isn't connected"), "{}", said(&app));

    // With restore_session = false it stays on the projects screen.
    let (store, _) = ConnectionStore::load_from(dir.path().join("c.toml"));
    let mut app = App::new(store, None, Some(dir.path().to_path_buf()));
    app.settings.restore_session = false;
    app.restore_session();
    assert!(matches!(app.screen, Screen::Projects));
}

#[tokio::test]
async fn rebound_keys_take_effect() {
    let (mut app, _dir) = connected_app(ConnectionMode::Normal).await;
    let overrides = std::collections::HashMap::from([("run".to_string(), crate::keys::KeySpec::One("Ctrl+G".into()))]);
    app.keymap = crate::keys::KeyMap::new(&overrides).0;
    app.tab.editor.set_text("select 7");
    app.handle_key(key(KeyCode::F(5)));
    assert!(!app.executing, "F5 is no longer bound to run");
    app.handle_key(ctrl('g'));
    finish(&mut app).await;
    assert_eq!(app.tab.grid.results.as_ref().unwrap().rows, vec![vec![Some("7".to_string())]]);
}

/// The live PostgreSQL test server as a URL, from the same
/// `PODELIS_TEST_PG_*` variables as the database tests (CI sets a password).
fn live_pg_url() -> Option<String> {
    let var = |name: &str| std::env::var(format!("PODELIS_TEST_PG_{name}")).ok();
    let port = var("PORT")?;
    let host = var("HOST").unwrap_or_else(|| "127.0.0.1".into());
    let user = var("USER").unwrap_or_else(|| "postgres".into());
    let mut url = url::Url::parse(&format!("postgres://{host}:{port}/postgres")).ok()?;
    url.set_username(&user).ok()?;
    if let Some(password) = var("PASSWORD").filter(|p| !p.is_empty()) {
        url.set_password(Some(&password)).ok()?;
    }
    Some(url.to_string())
}

#[tokio::test]
async fn f7_during_a_slow_query_shows_the_plan_at_once() {
    let Some(url) = live_pg_url() else { return };
    let (mut app, _dir) = test_app();
    app.connect_form.apply_url(&url).unwrap();
    let db = Database::connect(app.connect_form.params().unwrap()).await.unwrap();
    let generation = app.generation;
    app.handle_db_event(DbEvent { generation, run: None, kind: DbEventKind::Connected(db) });
    finish_catalog(&mut app).await;
    app.tab.editor.set_text("select pg_sleep(2)");
    app.handle_key(key(KeyCode::F(5)));
    app.tab.editor.set_text("select 1");
    app.handle_key(key(KeyCode::F(7)));
    // The plan arrives while the query still runs.
    while app.tab.plan.is_none() {
        let event = app.poll_db_event().await.unwrap();
        app.handle_db_event(event);
    }
    assert!(app.executing && app.tab.result_tab == ResultTab::Plan);
    assert!(app.queued_plan.is_none());
    // The query's result doesn't take the plan out of view.
    finish(&mut app).await;
    assert_eq!(app.tab.result_tab, ResultTab::Plan);
    assert_eq!(app.tab.grid.results.as_ref().unwrap().columns, ["pg_sleep"]);
}

#[tokio::test]
async fn queries_are_cancelled_at_the_connection_timeout() {
    let Some(url) = live_pg_url() else { return };
    let (mut app, _dir) = test_app();
    app.connect_form.apply_url(&url).unwrap();
    app.connect_form.timeout = "1".into();
    let db = Database::connect(app.connect_form.params().unwrap()).await.unwrap();
    let generation = app.generation;
    app.handle_db_event(DbEvent { generation, run: None, kind: DbEventKind::Connected(db) });
    app.tab.editor.set_text("select pg_sleep(10)");
    app.handle_key(key(KeyCode::F(5)));
    let started = Instant::now();
    while app.executing {
        tokio::select! {
            event = app.poll_db_event() => app.handle_db_event(event.unwrap()),
            () = tokio::time::sleep(std::time::Duration::from_millis(200)) => app.tick_running(),
        }
    }
    assert!(started.elapsed() < std::time::Duration::from_secs(5));
    assert!(app.status.starts_with("Timed out after 1 s"), "{}", app.status);
}

#[tokio::test]
async fn writes_can_be_reviewed_in_a_transaction() {
    let (mut app, _dir) = connected_app(ConnectionMode::Production).await;
    // In save mode the confirmation offers `t` (review mode needs no asking).
    app.tx_mode = TxMode::AutoCommit;
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
    assert_eq!(app.txn, TxnState::Open);
    app.handle_key(key(KeyCode::Char('r')));
    finish(&mut app).await;
    assert_eq!((app.txn, app.status.as_str()), (TxnState::Idle, "Rolled back"));
    assert_eq!(value(&mut app).await, "1");
    // History keeps the statement as typed, without the BEGIN.
    assert!(app.history.entries().iter().any(|h| h == "update t set x = 2 where x = 1"));

    // Esc keeps it open (checkable with SELECTs); F8 commits.
    app.tab.editor.set_text("update t set x = 3 where x = 1");
    app.handle_key(key(KeyCode::F(5)));
    app.handle_key(key(KeyCode::Char('t')));
    finish(&mut app).await;
    app.handle_key(key(KeyCode::Esc));
    assert!(app.review.is_none() && app.txn == TxnState::Open);
    assert_eq!(value(&mut app).await, "3");
    app.handle_key(key(KeyCode::F(8)));
    finish(&mut app).await;
    assert_eq!((app.txn, app.status.as_str()), (TxnState::Idle, "Committed"));
    assert_eq!(value(&mut app).await, "3");
}

#[tokio::test]
async fn record_view_shows_a_row_vertically() {
    let (mut app, _dir) = connected_app(ConnectionMode::Normal).await;
    run_sql(&mut app, "select 1 as id, 'Ana' as long_name, null as note union all select 2, 'Ben', 'a\nb'").await;
    app.focus = Focus::Results;
    app.handle_key(key(KeyCode::Char('x')));
    let view = app.cell_view.as_ref().unwrap();
    assert_eq!(view.title, "ROW 1 of 2");
    assert_eq!(view.text, "id         1\nlong_name  Ana\nnote       NULL\n");
    app.handle_key(key(KeyCode::Right));
    let view = app.cell_view.as_ref().unwrap();
    assert_eq!(view.title, "ROW 2 of 2");
    assert!(view.text.ends_with("note       a\n           b\n"), "{:?}", view.text);
    app.handle_key(key(KeyCode::Esc));
    assert!(app.cell_view.is_none());
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
}

#[test]
fn command_palette_finds_and_runs_commands() {
    let (mut app, _dir) = test_app();
    app.screen = Screen::Query;
    app.tab.editor.set_text("select a,b from t");
    // Ctrl+K waits for a letter; its menu shows only after a pause.
    app.handle_key(ctrl('k'));
    assert!(app.palette.as_ref().is_some_and(|p| !p.menu) && app.status == "^K…");
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
    assert!(app.row_stream.is_some(), "still streaming");
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
    assert!(app.row_stream.is_none());

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
    let found = super::follow::Followed {
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
    app.connect_latency = None;
    assert!(app.wants_ticks());
    app.tick_running();
    assert!(app.pinging);
    while app.pinging {
        let event = app.poll_db_event().await.unwrap();
        app.handle_db_event(event);
    }
    assert!(app.connect_latency.is_some());

    let report = app.diagnostics();
    assert!(report.contains(env!("CARGO_PKG_VERSION")) && report.contains("SQLite"), "{report}");
    assert!(!report.contains(":memory:"), "no file names or hosts: {report}");
}

#[test]
fn explain_statements_per_engine() {
    assert!(Engine::Sqlite.dialect().explain_analyze("select 1", "").is_none());
    assert_eq!(
        Engine::Postgres.dialect().explain_analyze("select 1", "").unwrap(),
        "EXPLAIN (ANALYZE, BUFFERS, FORMAT JSON) select 1"
    );
    assert_eq!(Engine::Postgres.dialect().explain("select 1", ""), "EXPLAIN (FORMAT JSON) select 1");
    assert_eq!(Engine::MySql.dialect().explain("select 1", ""), "EXPLAIN FORMAT=TREE select 1");
    assert_eq!(Engine::Sqlite.dialect().explain("select 1", ""), "EXPLAIN QUERY PLAN select 1");
}

#[tokio::test]
async fn explain_analyze_of_a_write_is_rolled_back() {
    let Some(url) = live_pg_url() else { return };
    let (mut app, _dir) = test_app();
    app.connect_form.apply_url(&url).unwrap();
    let db = Database::connect(app.connect_form.params().unwrap()).await.unwrap();
    let generation = app.generation;
    app.handle_db_event(DbEvent { generation, run: None, kind: DbEventKind::Connected(db) });
    run_sql(&mut app, "create temporary table ea (x int); insert into ea values (1)").await;
    app.tab.editor.set_text("update ea set x = 2");
    app.handle_key(KeyEvent::new(KeyCode::F(7), KeyModifiers::SHIFT));
    assert!(app.confirm.is_some(), "a write asks first");
    app.handle_key(key(KeyCode::Char('y')));
    finish(&mut app).await;
    let plan = app.tab.plan.clone().unwrap_or_default().text;
    assert!(plan.contains("actual time"), "{plan} / {}", app.status);
    assert_eq!(app.txn, TxnState::Idle);
    run_sql(&mut app, "select x from ea").await;
    assert_eq!(app.tab.grid.results.as_ref().unwrap().rows, vec![vec![Some("1".to_string())]]);
}

#[tokio::test]
async fn add_mark_delete_and_copy_rows() {
    let (mut app, _dir) = connected_app(ConnectionMode::Normal).await;
    run_sql(&mut app, "create table pets (id integer primary key, name text not null, kind text default 'cat')").await;
    finish_catalog(&mut app).await;
    run_sql(&mut app, "insert into pets (name) values ('Rex'), ('Tom'), ('Kit')").await;
    run_sql(&mut app, "select * from pets order by id").await;
    app.focus = Focus::Results;

    // `a` opens a form of the table's columns; empty ones get defaults.
    app.handle_key(key(KeyCode::Char('a')));
    let form = app.row_form.as_ref().expect("add-row form");
    assert_eq!(form.columns.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["id", "name", "kind"]);
    app.handle_key(key(KeyCode::Down));
    type_text(&mut app, "Žąsis");
    app.handle_key(key(KeyCode::Enter));
    finish(&mut app).await;
    assert!(app.status.starts_with("Added 1 row(s) · 4 row(s)"), "{}", app.status);
    let rows = &app.tab.grid.results.as_ref().unwrap().rows;
    assert_eq!(rows.len(), 4);
    assert_eq!(rows[3], vec![Some("4".to_string()), Some("Žąsis".to_string()), Some("cat".to_string())]);

    // Mark two rows, copy them as Markdown, delete them (asks first).
    app.tab.grid.table_state.select(Some(0));
    app.handle_key(key(KeyCode::Char(' ')));
    app.handle_key(key(KeyCode::Down));
    app.handle_key(key(KeyCode::Char(' ')));
    assert_eq!(app.tab.grid.marked.len(), 2);
    app.handle_key(key(KeyCode::Char('C')));
    assert_eq!(
        app.take_clipboard().unwrap(),
        "| id | name | kind |\n| --- | --- | --- |\n| 1 | Rex | cat |\n| 3 | Kit | cat |\n"
    );
    // `Y` and Ctrl+C copy them too, a line each.
    app.handle_key(key(KeyCode::Char('Y')));
    assert_eq!(app.take_clipboard().unwrap(), "1\tRex\tcat\n3\tKit\tcat");
    app.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
    assert_eq!(app.take_clipboard().unwrap(), "1\tRex\tcat\n3\tKit\tcat");
    assert_eq!(said(&app), "Copied 2 rows (tab-separated, a line each)");
    app.handle_key(key(KeyCode::Char('d')));
    let confirm = app.confirm.as_ref().expect("asks before deleting");
    assert_eq!(confirm.message, "Delete 2 row(s) from pets?");
    app.handle_key(key(KeyCode::Char('y')));
    finish(&mut app).await;
    let names: Vec<_> = app.tab.grid.results.as_ref().unwrap().rows.iter().map(|r| r[1].clone().unwrap()).collect();
    assert_eq!(names, ["Tom", "Žąsis"]);
    assert!(app.tab.grid.marked.is_empty());
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

#[test]
fn snippets_complete_by_name_and_insert() {
    let (mut app, dir) = test_app();
    app.screen = Screen::Query;
    app.library = crate::library::Library::load_from(Some(dir.path().join("q.json")));
    app.library.add("snippets/recent", "select *\nfrom orders\nwhere placed_at > now() - interval '7 days'").unwrap();
    app.library.add("reports/churn", "select 1").unwrap();
    app.tab.editor.set_text("rec");
    app.handle_key(key(KeyCode::Tab));
    let c = app.completion.as_ref().expect("snippet offered");
    let snippet = c.items.iter().position(|i| i.kind == crate::complete::CandKind::Snippet).unwrap();
    assert_eq!(c.items[snippet].label, "recent");
    app.completion.as_mut().unwrap().selected = snippet;
    app.handle_key(key(KeyCode::Enter));
    assert!(app.tab.editor.text().starts_with("select *\nfrom orders"));

    // Ctrl+E inserts any saved query at the cursor.
    app.tab.editor.set_text("-- here: ");
    app.handle_key(ctrl('e'));
    let view = app.library_view.as_ref().unwrap();
    assert!(view.insert);
    for _ in 0.."snippets".len() {
        app.handle_key(key(KeyCode::Backspace));
    }
    type_text(&mut app, "churn");
    app.handle_key(key(KeyCode::Enter));
    assert_eq!(app.tab.editor.text(), "-- here: select 1");
    assert_eq!(app.tabs.len(), 1, "inserted, not opened in a tab");
}

#[tokio::test]
async fn explain_moves_focus_to_the_plan() {
    let (mut app, _dir) = connected_app(ConnectionMode::Normal).await;
    run_sql(&mut app, "create table t (x int)").await;
    app.tab.editor.set_text("select * from t");
    app.focus = Focus::Editor;
    app.handle_key(key(KeyCode::F(7)));
    finish(&mut app).await;
    assert_eq!((app.focus, app.tab.result_tab), (Focus::Results, ResultTab::Plan));
    app.handle_key(key(KeyCode::Char('1')));
    assert_eq!(app.tab.result_tab, ResultTab::Rows);
}

#[tokio::test]
async fn heartbeat_can_be_switched_on_and_reports_a_lost_connection() {
    let (mut app, _dir) = connected_app(ConnectionMode::Normal).await;
    assert_eq!(app.settings.keepalive, 0, "off by default");
    app.run_action(crate::keys::Action::Heartbeat);
    assert_eq!(app.settings.keepalive, 30);
    assert!(app.pinging, "checks right away");
    while app.pinging {
        let event = app.poll_db_event().await.unwrap();
        app.handle_db_event(event);
    }
    assert!(app.connect_latency.is_some() && !app.connection_lost);
    // A failed check marks the connection lost until something succeeds.
    app.pinged(Err(("connection refused".into(), super::Ping::Failed)));
    assert!(app.connection_lost && app.status.contains("hint:"), "{}", app.status);
    run_sql(&mut app, "select 1").await;
    assert!(!app.connection_lost);
    // A heartbeat with no answer once: said, the session kept (an open
    // transaction would go with it).
    run_sql(&mut app, "create table hb (x int); begin; insert into hb values (1)").await;
    assert_eq!(app.txn, TxnState::Open);
    app.pinged(Err(("no answer in 5 s".into(), super::Ping::NoAnswer)));
    app.pinged(Err(("no answer in 5 s".into(), super::Ping::NoAnswer)));
    assert!(app.connection_lost);
    run_sql(&mut app, "select count(*) from hb").await;
    assert_eq!(app.tab.grid.results.as_ref().unwrap().rows, vec![vec![Some("1".to_string())]], "still its transaction");
    // A check on its way when the connection shown changes isn't waited
    // for any more (its answer is dropped).
    app.pinging = true;
    app.switch_conn(None);
    assert!(!app.pinging);
    app.run_action(crate::keys::Action::Heartbeat);
    assert_eq!(app.settings.keepalive, 0);
}

#[tokio::test]
async fn the_projects_pane_switches_adds_renames_and_deletes_over_the_work() {
    let (mut app, dir) = test_app();
    app.data_dir = Some(dir.path().to_path_buf());
    let first = project_id(&app);
    let second = app.store.add_project("second").unwrap();
    let a = sqlite_profile("alpha", &dir.path().join("a.db"));
    app.store.upsert(&first, None, a.clone()).unwrap();
    app.load_tabs(&first);
    connect_profile(&mut app, &a).await;
    app.tab.editor.set_text("select 'kept'");

    // Ctrl+K p shows the projects, the current one selected.
    show_projects(&mut app);
    assert!(matches!(app.screen, Screen::Projects));
    assert_eq!(app.store.projects[app.projects_selected].id, first);
    // The current one: its connections; Esc goes back to the work.
    app.handle_key(key(KeyCode::Enter));
    assert!(matches!(app.screen, Screen::Connections));
    app.handle_key(key(KeyCode::Esc));
    assert!(matches!(app.screen, Screen::Query) && app.database.is_some());

    // Rename the other one, then switch to it.
    show_projects(&mut app);
    app.projects_selected = app.store.projects.iter().position(|p| p.id == second).unwrap();
    app.handle_key(key(KeyCode::Char('r')));
    for _ in 0.."second".len() {
        app.handle_key(key(KeyCode::Backspace));
    }
    type_text(&mut app, "reports");
    app.handle_key(key(KeyCode::Enter));
    assert_eq!(app.store.project(&second).unwrap().name, "reports");
    // The open project can't be deleted from here.
    app.projects_selected = app.store.projects.iter().position(|p| p.id == first).unwrap();
    app.handle_key(key(KeyCode::Char('d')));
    assert!(app.projects_message.as_ref().is_some_and(|(m, _)| m.contains("switch to another")));
    app.projects_selected = app.store.projects.iter().position(|p| p.id == second).unwrap();
    app.handle_key(key(KeyCode::Enter));
    assert!(matches!(app.screen, Screen::Connections));
    assert_eq!(app.current_project.as_deref(), Some(second.as_str()));
    assert!(app.database.is_none());

    // Back to the first: its tab comes back and reconnects.
    app.screen = Screen::Query;
    show_projects(&mut app);
    app.projects_selected = app.store.projects.iter().position(|p| p.id == first).unwrap();
    app.handle_key(key(KeyCode::Enter));
    assert_eq!(app.current_project.as_deref(), Some(first.as_str()));
    assert!(matches!(app.screen, Screen::Query));
    assert_eq!(app.tab.editor.text(), "select 'kept'");
    while app.database.is_none() {
        let event = app.poll_db_event().await.unwrap();
        app.handle_db_event(event);
    }
    assert_eq!(app.connect_form.name, "alpha");

    // Delete the other project; then a new one opens straight away.
    show_projects(&mut app);
    app.projects_selected = app.store.projects.iter().position(|p| p.id == second).unwrap();
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
    let (a, b) = (sqlite_profile("alpha", &dir.path().join("a.db")), sqlite_profile("beta", &dir.path().join("b.db")));
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
    app.connections_selected = 1;
    app.handle_key(key(KeyCode::Enter));
    while app.database.is_none() || app.connect_form.connecting || app.loading_profile {
        let event = app.poll_db_event().await.unwrap();
        app.handle_db_event(event);
    }
    assert!(matches!(app.screen, Screen::Query));
    assert_eq!((app.tabs.len(), app.connect_form.name.as_str()), (2, "beta"));
    assert_eq!(app.tab_text(0), "select 'alpha'");

    // e edits the settings in place, without a new tab: the tab keeps its
    // connection, and gets its form back when the editing ends.
    show_projects(&mut app);
    app.handle_key(key(KeyCode::Right));
    app.connections_selected = 0;
    app.handle_key(key(KeyCode::Char('e')));
    while app.loading_profile {
        let event = app.poll_db_event().await.unwrap();
        app.handle_db_event(event);
    }
    assert!(matches!(app.screen, Screen::Connect));
    assert_eq!(app.connect_form.original_name.as_deref(), Some("alpha"));
    assert_eq!((app.tabs.len(), app.active_tab), (2, 1));
    // The tabs saved meanwhile still name this tab's connection.
    assert_eq!(app.tab_connection_name(1).as_deref(), Some("beta"));
    app.handle_key(key(KeyCode::Esc));
    assert!(matches!(app.screen, Screen::Connections));
    assert_eq!(app.connect_form.name, "beta");
    app.handle_key(key(KeyCode::Esc));
    assert!(matches!(app.screen, Screen::Query));
    assert_eq!((app.tabs.len(), app.active_tab), (2, 1));
    assert!(app.database.is_some());

    // Connecting from the edited form opens a new tab for it.
    show_projects(&mut app);
    app.handle_key(key(KeyCode::Right));
    app.connections_selected = 0;
    app.handle_key(key(KeyCode::Char('e')));
    while app.loading_profile {
        let event = app.poll_db_event().await.unwrap();
        app.handle_db_event(event);
    }
    app.handle_key(key(KeyCode::Enter));
    while app.connect_form.connecting {
        let event = app.poll_db_event().await.unwrap();
        app.handle_db_event(event);
    }
    assert_eq!((app.tabs.len(), app.active_tab), (3, 2));
    assert_eq!(app.connect_form.name, "alpha");
    app.handle_key(alt('2'));
    assert_eq!(app.connect_form.name, "beta");

    // Looking at the projects and going back changes nothing.
    show_projects(&mut app);
    app.handle_key(key(KeyCode::Esc));
    assert!(matches!(app.screen, Screen::Query));
    assert_eq!((app.tabs.len(), app.connect_form.name.as_str()), (3, "beta"));
}

/// Waits for a connection's password to come from the keychain.
async fn read_password(app: &mut App) {
    while app.loading_profile {
        let event = app.poll_db_event().await.unwrap();
        app.handle_db_event(event);
    }
}

#[tokio::test]
async fn tab_and_shift_tab_go_round_the_panes() {
    let (mut app, dir) = test_app();
    let project = project_id(&app);
    app.store.add_project("other").unwrap();
    app.store.upsert(&project, None, sqlite_profile("alpha", &dir.path().join("a.db"))).unwrap();
    app.screen = Screen::Query;
    show_projects(&mut app);
    // Another project selected: Tab opens it, into its connections; then
    // back to the first one the same way.
    let other = app.store.projects[1].id.clone();
    app.projects_selected = 1;
    app.handle_key(key(KeyCode::Tab));
    assert!(matches!(app.screen, Screen::Connections));
    assert_eq!(app.current_project.as_deref(), Some(other.as_str()));
    app.handle_key(key(KeyCode::BackTab));
    app.projects_selected = 0;
    app.handle_key(key(KeyCode::Right));
    assert!(matches!(app.screen, Screen::Connections));
    assert_eq!(app.current_project.as_deref(), Some(project.as_str()));
    app.connections_selected = 0;
    app.handle_key(key(KeyCode::Tab));
    read_password(&mut app).await;
    assert!(matches!(app.screen, Screen::Connect));
    // In the settings ↑↓ move between fields, Tab goes round to the projects.
    app.handle_key(key(KeyCode::Down));
    assert_eq!(app.connect_form.focus, ConnectField::Engine);
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

#[tokio::test]
async fn quitting_from_the_panes_asks_about_an_open_transaction() {
    let (mut app, dir) = test_app();
    let a = sqlite_profile("alpha", &dir.path().join("a.db"));
    connect_profile(&mut app, &a).await;
    run_sql(&mut app, "begin").await;
    assert_eq!(app.txn, TxnState::Open);
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

#[tokio::test]
async fn leaving_the_panes_drops_a_password_still_being_read() {
    let (mut app, dir) = test_app();
    let project = project_id(&app);
    let a = sqlite_profile("alpha", &dir.path().join("a.db"));
    app.store.upsert(&project, None, a).unwrap();
    app.handle_key(key(KeyCode::Enter));
    assert!(app.loading_profile);
    // Clicking the project's name in the top bar.
    app.open_manager(Screen::Projects);
    assert!(!app.loading_profile && matches!(app.screen, Screen::Projects));
    // The password arrives late: it no longer opens the form or connects.
    let event = app.poll_db_event().await.unwrap();
    app.handle_db_event(event);
    assert!(matches!(app.screen, Screen::Projects));
    assert!(!app.connect_form.connecting);
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

#[test]
fn no_completions_is_said_in_the_editor_footer() {
    let (mut app, _dir) = test_app();
    app.screen = Screen::Query;
    type_text(&mut app, "select zzqx");
    app.handle_key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::CONTROL));
    assert!(app.completion.is_none());
    assert_eq!(app.editor_note.as_deref(), Some("No completions"));
    assert!(app.status.is_empty(), "not in the status bar: {}", app.status);
    // Gone with the next key.
    app.handle_key(key(KeyCode::Char('y')));
    assert!(app.editor_note.is_none());
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
    app.connect_form.engine = Engine::Sqlite;
    app.connect_form.host = ":memory:".into();
    app.connect_form.transaction = Some(TxMode::RollbackOnly);
    let db = Database::connect(app.connect_form.params().unwrap()).await.unwrap();
    let generation = app.generation;
    app.handle_db_event(DbEvent { generation, run: None, kind: DbEventKind::Connected(db) });
    assert_eq!(app.tx_mode, TxMode::RollbackOnly);
}

#[tokio::test]
async fn a_new_connection_is_kept_once_it_works() {
    let (mut app, dir) = test_app();
    let file = dir.path().join("shop.db");
    let connect = async |app: &mut App, name: &str| {
        app.connect_form = ConnectForm::new();
        app.connect_form.engine = Engine::Sqlite;
        app.connect_form.host = file.to_string_lossy().into_owned();
        app.connect_form.name = name.into();
        let db = Database::connect(app.connect_form.params().unwrap()).await.unwrap();
        let generation = app.generation;
        app.handle_db_event(DbEvent { generation, run: None, kind: DbEventKind::Connected(db) });
    };
    // Unnamed: named after the file, and saved in the project.
    connect(&mut app, "").await;
    let saved = |app: &App| app.profiles().iter().map(|p| p.name.clone()).collect::<Vec<_>>();
    assert_eq!(saved(&app), vec!["shop.db"]);
    assert!(app.status.contains("Saved as 'shop.db'"), "{}", app.status);
    // Another unnamed one to the same place is that saved one, not a copy.
    connect(&mut app, "").await;
    assert_eq!(saved(&app), vec!["shop.db"]);
    assert!(app.status.contains("Already saved as 'shop.db'"), "{}", app.status);
    assert_eq!(app.connect_form.original_name.as_deref(), Some("shop.db"));
    // Named, it's saved under that name even if it goes to the same place.
    connect(&mut app, "shop copy").await;
    assert_eq!(saved(&app), vec!["shop.db", "shop copy"]);
    // A saved connection opened again isn't saved twice.
    let profile = app.profiles()[0].clone();
    app.connect_form = ConnectForm::from_profile(&profile, String::new());
    let db = Database::connect(app.connect_form.params().unwrap()).await.unwrap();
    let generation = app.generation;
    app.handle_db_event(DbEvent { generation, run: None, kind: DbEventKind::Connected(db) });
    assert_eq!(saved(&app).len(), 2);
}

#[test]
fn space_in_the_schema_opens_the_command_menu() {
    let (mut app, _dir) = test_app();
    app.screen = Screen::Query;
    app.handle_key(ctrl('k'));
    assert!(app.palette.as_ref().is_some_and(|p| !p.searching));
    // The same key again searches every command.
    app.handle_key(ctrl('k'));
    assert!(app.palette.as_ref().unwrap().searching);
    app.handle_key(key(KeyCode::Esc));

    app.schema = Some(SchemaPanel::default());
    app.focus = Focus::Schema;
    app.handle_key(key(KeyCode::Char(' ')));
    assert!(app.palette.is_some());
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
fn ctrl_w_deletes_a_word_in_a_filter_instead_of_closing_the_tab() {
    let (mut app, _dir) = test_app();
    app.screen = Screen::Query;
    app.show_results_for_tests(outcome(&["city"], &[&[Some("Vilnius")], &[Some("Kaunas")]]));
    app.tab.editor = TextEditor::with_text("select * from cities");
    app.focus = Focus::Results;
    app.handle_key(key(KeyCode::Char('/')));
    type_text(&mut app, "kau nas");
    app.handle_key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL));
    assert_eq!(app.tab.grid.filter.text(), "kau ");
    assert_eq!(app.tab.editor.text(), "select * from cities", "the tab is still open");
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
fn a_tabular_plan_takes_the_results_keys() {
    let (mut app, _dir) = test_app();
    app.connect_form.engine = Engine::MySql;
    app.show_results_for_tests(outcome(&["id"], &[&[Some("1")], &[Some("2")]]));
    let plan = outcome(
        &["id", "select_type", "table", "type", "rows"],
        &[
            &[Some("1"), Some("PRIMARY"), Some("orders"), Some("ALL"), Some("5")],
            &[Some("2"), Some("SUBQUERY"), Some("users"), Some("eq_ref"), Some("1")],
        ],
    );
    app.show_plan(plan);
    assert!(app.tab.plan_swapped && app.tab.result_tab == ResultTab::Plan);
    assert_eq!(app.tab.grid.results.as_ref().unwrap().columns.len(), 5);
    // Results keys work on it: next column, sort, copy the cell.
    app.handle_results_key(key(KeyCode::Right));
    app.handle_results_key(key(KeyCode::Char('s')));
    assert_eq!(app.tab.grid.sort, Some((1, false)));
    app.handle_results_key(key(KeyCode::Char('y')));
    assert_eq!(app.take_clipboard().as_deref(), Some("PRIMARY"));
    // Not the ones that change data.
    app.handle_results_key(key(KeyCode::Char('c')));
    assert!(app.cell_edit.is_none() && app.status.contains("not the plan"));
    // RESULT is the query's grid again; QUERY PLAN the plan's.
    app.handle_results_key(key(KeyCode::Char('1')));
    assert!(!app.tab.plan_swapped);
    assert_eq!(app.tab.grid.results.as_ref().unwrap().columns, ["id"]);
    app.handle_results_key(key(KeyCode::Char('3')));
    assert!(app.tab.plan_swapped);
    // A query's outcome arriving meanwhile goes to the result, not the plan,
    // which stays in view (the RESULT tab was shown as the query started).
    let generation = app.generation;
    app.handle_db_event(DbEvent {
        generation,
        run: None,
        kind: DbEventKind::QueryDone(outcome(&["n"], &[&[Some("9")]]), None, None),
    });
    assert_eq!(app.tab.result_tab, ResultTab::Plan);
    assert!(app.tab.plan_swapped);
    assert_eq!(app.tab.grid.results.as_ref().unwrap().columns.len(), 5);
    app.handle_results_key(key(KeyCode::Char('1')));
    assert_eq!(app.tab.grid.results.as_ref().unwrap().columns, ["n"]);
}

#[test]
fn a_results_filter_stays_with_its_grid() {
    let (mut app, _dir) = test_app();
    app.connect_form.engine = Engine::MySql;
    app.show_results_for_tests(outcome(&["id"], &[&[Some("1")], &[Some("74")]]));
    app.handle_results_key(key(KeyCode::Char('/')));
    for c in "74".chars() {
        app.handle_filter_key(key(KeyCode::Char(c)));
    }
    assert_eq!(app.tab.grid.view.len(), 1);
    app.show_plan(outcome(&["id", "table"], &[&[Some("1"), Some("t")]]));
    assert!(!app.tab.grid.filter_editing && app.tab.grid.filter.text().is_empty(), "the plan's grid is unfiltered");
    app.handle_results_key(key(KeyCode::Char('1')));
    assert_eq!(app.tab.grid.filter.text(), "74");
    assert!(!app.tab.grid.filter_editing);
}

#[test]
fn a_plan_remembers_its_statement_and_time() {
    let (mut app, _dir) = test_app();
    app.plan_request = Some(("select * from users where id = 1".into(), true, 0));
    app.show_plan(outcome(&["QUERY PLAN"], &[&[Some("[]")]]));
    let source = app.tab.plan_source.clone().unwrap();
    assert_eq!((source.sql.as_str(), source.analyzed), ("select * from users where id = 1", true));
    assert!((chrono::Local::now() - source.at).num_seconds() < 5);
    assert!(app.plan_request.is_none());
}

#[test]
fn the_statement_to_explain_and_its_line() {
    let (mut app, _dir) = test_app();
    app.tab.editor.set_text("select 1;\n\n  select *\n  from t;\nselect 3");
    app.tab.editor.set_cursor((3, 3));
    assert_eq!(app.statement_to_explain(), Some(("select *\n  from t".to_string(), 2)));
    let source = PlanSource::new("select 3".into(), false, 4);
    assert_eq!(source.find_in("select 3\nx\nx\nx\nselect 3\nselect 3"), Some((15, 4)), "the copy nearest line 5");
    assert_eq!(source.find_in("select 4"), None);
}

#[test]
fn s_in_the_navigator_adds_the_select_after_everything() {
    let (mut app, _dir) = test_app();
    app.schema = Some(SchemaPanel {
        tables: vec![SchemaTable {
            info: crate::db::TableInfo {
                schema: "public".into(),
                name: "users".into(),
                is_view: false,
                rows: None,
                bytes: None,
                system: false,
            },
            expanded: false,
            columns: Vec::new(),
            details: None,
            details_loading: false,
        }],
        selected: 1,
        ..SchemaPanel::default()
    });
    app.focus = Focus::Schema;
    app.tab.editor.set_text("select a,\n  b\nfrom t;");
    app.tab.editor.set_cursor((1, 1));
    app.handle_schema_key(key(KeyCode::Char('s')));
    assert_eq!(app.tab.editor.text(), "select a,\n  b\nfrom t;\n\nSELECT * FROM public.users LIMIT 100;");
    assert_eq!((app.focus, app.tab.editor.cursor_row), (Focus::Editor, 4));
}

#[test]
fn arrows_in_a_definition_go_to_the_next_table() {
    let (mut app, _dir) = test_app();
    let table = |name: &str| SchemaTable {
        info: crate::db::TableInfo {
            schema: "main".into(),
            name: name.into(),
            is_view: false,
            rows: None,
            bytes: None,
            system: false,
        },
        expanded: false,
        columns: Vec::new(),
        details: Some(crate::db::TableDetails::default()),
        details_loading: false,
    };
    app.schema = Some(SchemaPanel { tables: vec![table("a"), table("b")], selected: 1, ..SchemaPanel::default() });
    app.focus = Focus::Schema;
    app.handle_schema_key(key(KeyCode::Char('d')));
    assert_eq!(app.cell_view.as_ref().unwrap().title, "TABLE a");
    app.handle_cell_view_key(key(KeyCode::Right));
    assert_eq!(app.cell_view.as_ref().unwrap().title, "TABLE b");
    assert_eq!(app.schema.as_ref().unwrap().selected, 2, "the navigator follows");
    app.handle_cell_view_key(key(KeyCode::Right));
    assert_eq!(app.cell_view.as_ref().unwrap().title, "TABLE b", "stays on the last");
    app.handle_cell_view_key(key(KeyCode::Left));
    assert_eq!(app.cell_view.as_ref().unwrap().title, "TABLE a");
    app.handle_cell_view_key(key(KeyCode::Char('y')));
    assert!(app.take_clipboard().is_some_and(|t| t.contains("main")));
}

#[tokio::test]
async fn s_in_the_navigator_runs_only_its_select() {
    let (mut app, _dir) = Box::pin(connected_app(ConnectionMode::Normal)).await;
    run_sql(&mut app, "create table people (id integer primary key, name text)").await;
    run_sql(&mut app, "insert into people values (1, 'Ann')").await;
    while app.schema.as_ref().is_none_or(|s| s.loading || s.tables.is_empty()) {
        let event = app.poll_db_event().await.unwrap();
        app.handle_db_event(event);
    }
    // Other statements in the editor, one of them broken: they're not run.
    app.tab.editor.set_text("select nope from nowhere;\ndelete from people;");
    app.tab.editor.set_cursor((0, 3));
    let panel = app.schema.as_mut().unwrap();
    panel.selected = panel.nodes().iter().position(|n| matches!(n, SchemaNode::Table(_))).unwrap();
    app.focus = Focus::Schema;
    app.handle_key(key(KeyCode::Char('s')));
    finish(&mut app).await;
    let results = app.tab.grid.results.as_ref().unwrap();
    assert_eq!(
        (results.columns.as_slice(), results.rows.len()),
        (["id".to_string(), "name".to_string()].as_slice(), 1)
    );
    assert_eq!(results.statements, 1, "just the SELECT");
}

/// An app on the saved SQLite connection `shop` of project `p`, its data
/// (and gateway) in `dir`, with `config` as config.toml.
async fn asking_app(dir: &std::path::Path, config: &str) -> App {
    crate::gateway::tests::saved_shop(dir).await;
    std::fs::write(dir.join("config.toml"), config).unwrap();
    let (store, _) = ConnectionStore::load_from(dir.join("connections.toml"));
    let mut app = App::new(store, None, Some(dir.to_path_buf()));
    let id = app.store.project_by_name("p").unwrap().id.clone();
    app.open_project(&id);
    app.connect_form.engine = Engine::Sqlite;
    app.connect_form.host = dir.join("shop.db").to_string_lossy().into_owned();
    app.connect_form.name = "shop".into();
    let db = Database::connect(app.connect_form.params().unwrap()).await.unwrap();
    let generation = app.generation;
    app.handle_db_event(DbEvent { generation, run: None, kind: DbEventKind::Connected(db) });
    app.connect_form.name = "shop".into();
    app
}

/// As if the question that came by itself had been on screen a while
/// (keys in its first moment don't answer it).
fn settle(app: &mut App) {
    app.agent_asked = Instant::now().checked_sub(Duration::from_secs(2));
}

/// Handles events until the question is answered.
async fn until_answered(app: &mut App) {
    while app.asking.is_some() {
        let event = tokio::time::timeout(Duration::from_secs(10), app.poll_db_event()).await.unwrap().unwrap();
        app.handle_db_event(event);
    }
}

#[tokio::test]
async fn a_question_puts_the_models_sql_in_the_editor_without_running_it() {
    let answer = "```sql\nSELECT c.country, count(*) AS orders FROM orders o JOIN customers c ON c.id = o.customer_id GROUP BY c.country\n```\nCounts orders per country.";
    let (url, _server) =
        crate::gateway::tests::fake_model(vec![(200, crate::gateway::tests::claude_answer(answer))]).await;
    let dir = tempfile::tempdir().unwrap();
    let config =
        format!("[[models]]\nname = \"near\"\napi = \"anthropic\"\nurl = \"{url}\"\nmodel = \"claude-opus-5-5\"\n");
    let mut app = asking_app(dir.path(), &config).await;
    let _gateway = crate::gateway::tests::start(dir.path(), None);
    app.tab.editor.set_text("select 1;");
    app.run_action(Action::Ask);
    assert!(app.ask_prompt.is_some());
    type_text(&mut app, "orders per country");
    app.handle_key(key(KeyCode::Enter));
    assert!(app.asking.is_some() && said(&app).starts_with("Asking…"));
    until_answered(&mut app).await;
    assert_eq!(
        app.tab.editor.text(),
        "select 1;\n\n-- orders per country\n-- Counts orders per country.\n\
         SELECT c.country, count(*) AS orders FROM orders o JOIN customers c ON c.id = o.customer_id GROUP BY c.country;"
    );
    assert!(said(&app).contains("nothing ran"), "{}", said(&app));
    assert!(!app.executing && app.tab.grid.results.is_none(), "nothing ran");
    assert_eq!(app.focus, Focus::Editor);
}

#[tokio::test]
async fn a_far_model_sees_the_schema_once_allowed_and_always_saves_it() {
    let dir = tempfile::tempdir().unwrap();
    let config = "[[models]]\nname = \"far\"\napi = \"openai\"\nurl = \"https://models.invalid/v1\"\nmodel = \"x\"\nkey_command = \"echo sk-x\"\n";
    let mut app = asking_app(dir.path(), config).await;
    let _gateway = crate::gateway::tests::start(dir.path(), None);
    app.run_action(Action::Ask);
    type_text(&mut app, "customers by country");
    app.handle_key(key(KeyCode::Enter));
    until_answered(&mut app).await;
    let confirm = app.confirm.as_ref().expect("asks whether to send the schema");
    assert!(matches!(confirm.action, ConfirmAction::ShareSchema { .. }));
    assert!(confirm.message.contains("Send p's table and column names"), "{}", confirm.message);
    // Keys typed as it appears don't answer it; nor do others.
    app.handle_key(key(KeyCode::Char('a')));
    app.handle_key(key(KeyCode::Char('x')));
    assert!(app.confirm.is_some() && app.asking.is_none());
    assert_eq!(app.store.project_by_name("p").unwrap().share_schema, crate::store::ShareSchema::Local);
    settle(&mut app);
    // `a`: always, for the project; the question goes again.
    app.handle_key(key(KeyCode::Char('a')));
    assert_eq!(app.store.project_by_name("p").unwrap().share_schema, crate::store::ShareSchema::All);
    let (saved, _) = ConnectionStore::load_from(dir.path().join("connections.toml"));
    assert_eq!(saved.project_by_name("p").unwrap().share_schema, crate::store::ShareSchema::All);
    assert!(app.asking.is_some());
    until_answered(&mut app).await;
    // (The address is made up: it fails, but it was sent.)
    assert!(said(&app).starts_with("Couldn't ask") && said(&app).contains("models.invalid"), "{}", said(&app));
}

#[test]
fn asking_needs_a_saved_connection() {
    let (mut app, _dir) = test_app();
    app.screen = Screen::Query;
    app.run_action(Action::Ask);
    assert!(app.ask_prompt.is_none());
    assert!(said_error(&app), "{}", said(&app));
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

/// Handles events until the memory window has its facts again.
async fn until_memory_loaded(app: &mut App, facts: usize) {
    for _ in 0..50 {
        if app.memory_view.as_ref().is_some_and(|v| !v.loading && v.facts.len() == facts) {
            return;
        }
        let event = tokio::time::timeout(Duration::from_secs(10), app.poll_db_event()).await.unwrap().unwrap();
        app.handle_db_event(event);
    }
    panic!("the memory window didn't show {facts} facts");
}

#[tokio::test]
async fn the_memory_window_adds_confirms_and_forgets_facts() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = asking_app(dir.path(), "").await;
    let _gateway = crate::gateway::tests::start(dir.path(), None);
    // A model's guess, waiting.
    let mut conn = crate::gateway::memory::open(dir.path(), &app.project().unwrap().id).await.unwrap();
    crate::gateway::memory::add(&mut conn, "orders.total", "means", "VAT included", "model:near", false).await.unwrap();
    drop(conn);
    app.run_action(Action::Memory);
    until_memory_loaded(&mut app, 1).await;
    assert_eq!(app.memory_view.as_ref().unwrap().facts[0]["confirmed"], serde_json::json!(false));
    // `a`: as `podelis memory add`.
    app.handle_key(key(KeyCode::Char('a')));
    type_text(&mut app, "revenue is orders.total minus refunds");
    app.handle_key(key(KeyCode::Enter));
    until_memory_loaded(&mut app, 2).await;
    let added = &app.memory_view.as_ref().unwrap().facts[1];
    assert_eq!((added["relation"].as_str(), added["value"].as_str()), (Some("is"), Some("orders.total minus refunds")));
    // `c` on the guess confirms it.
    app.handle_key(key(KeyCode::Char('c')));
    for _ in 0..50 {
        if app.memory_view.as_ref().unwrap().facts[0]["confirmed"] == serde_json::json!(true) {
            break;
        }
        let event = tokio::time::timeout(Duration::from_secs(10), app.poll_db_event()).await.unwrap().unwrap();
        app.handle_db_event(event);
    }
    assert_eq!(app.memory_view.as_ref().unwrap().facts[0]["confirmed"], serde_json::json!(true));
    // `d` forgets the selected one.
    app.handle_key(key(KeyCode::Down));
    app.handle_key(key(KeyCode::Char('d')));
    until_memory_loaded(&mut app, 1).await;
    app.handle_key(key(KeyCode::Esc));
    assert!(app.memory_view.is_none());
}

#[tokio::test]
async fn allowing_the_schema_asks_again_about_the_tab_it_came_from() {
    let dir = tempfile::tempdir().unwrap();
    let config = "[[models]]\nname = \"far\"\napi = \"openai\"\nurl = \"https://models.invalid/v1\"\nmodel = \"x\"\nkey_command = \"echo sk-x\"\n";
    let mut app = asking_app(dir.path(), config).await;
    let _gateway = crate::gateway::tests::start(dir.path(), None);
    app.run_action(Action::Ask);
    type_text(&mut app, "customers by country");
    app.handle_key(key(KeyCode::Enter));
    // Another tab meanwhile, not connected.
    app.new_tab(None, "", None);
    app.switch_conn(None);
    assert_eq!(app.active_tab, 1);
    until_answered(&mut app).await;
    assert!(app.confirm.is_some());
    settle(&mut app);
    app.handle_key(key(KeyCode::Char('y')));
    let asking = app.asking.as_ref().expect("asked again");
    assert_eq!((asking.question.tab, asking.question.connection.as_str()), (0, "shop"));
    // Leaving the project stops it: no answer for a tab of another one.
    app.leave_project();
    assert!(app.asking.is_none());
}

#[tokio::test]
async fn the_sharing_question_waits_for_a_confirmation_already_open() {
    let dir = tempfile::tempdir().unwrap();
    let config = "[[models]]\nname = \"far\"\napi = \"openai\"\nurl = \"https://models.invalid/v1\"\nmodel = \"x\"\nkey_command = \"echo sk-x\"\n";
    let mut app = asking_app(dir.path(), config).await;
    let _gateway = crate::gateway::tests::start(dir.path(), None);
    app.run_action(Action::Ask);
    type_text(&mut app, "customers by country");
    app.handle_key(key(KeyCode::Enter));
    // Meanwhile, the user asks to close the tab (it asks first).
    app.confirm = Some(Confirm {
        message: "Close?".into(),
        details: Vec::new(),
        danger: false,
        action: ConfirmAction::CloseTab,
        offer_transaction: false,
    });
    until_answered(&mut app).await;
    assert!(matches!(app.confirm.as_ref().unwrap().action, ConfirmAction::CloseTab), "not replaced");
    // Answered: then the sharing question shows, at once.
    app.handle_key(key(KeyCode::Char('n')));
    assert!(matches!(app.confirm.as_ref().unwrap().action, ConfirmAction::ShareSchema { .. }));
}

#[tokio::test]
async fn esc_while_asking_does_its_own_job_first() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = asking_app(
        dir.path(),
        "[[models]]\nname = \"m\"\napi = \"openai\"\nurl = \"http://127.0.0.1:9/v1\"\nmodel = \"x\"\n",
    )
    .await;
    let _gateway = crate::gateway::tests::start(dir.path(), None);
    app.run_action(Action::Ask);
    type_text(&mut app, "customers");
    app.handle_key(key(KeyCode::Enter));
    assert!(app.asking.is_some());
    // A selection in the editor: Esc drops it, the question goes on.
    app.tab.editor.set_text("select 1");
    app.tab.editor.select_all();
    app.handle_key(key(KeyCode::Esc));
    assert!(app.asking.is_some() && app.tab.editor.selection().is_none());
    // Nothing else to do: Esc stops asking, and says so.
    app.handle_key(key(KeyCode::Esc));
    assert!(app.asking.is_none());
    assert_eq!(said(&app), "Stopped asking");
}

#[test]
fn a_late_memory_answer_for_another_project_is_dropped() {
    let (mut app, _dir) = test_app();
    let shown = app.current_project.clone().unwrap();
    app.memory_view = Some(super::memory_view::MemoryView { project_id: shown, ..Default::default() });
    let facts = serde_json::json!({ "facts": [{ "id": 1, "subject": "x" }] });
    let other = super::memory_view::GatewayPurpose::MemoryList("another-project".into());
    app.gateway_done(other, Ok(facts.clone()));
    assert!(app.memory_view.as_ref().unwrap().facts.is_empty());
    let mine = super::memory_view::GatewayPurpose::MemoryList(app.current_project.clone().unwrap());
    app.gateway_done(mine, Ok(facts));
    assert_eq!(app.memory_view.as_ref().unwrap().facts.len(), 1);
}

#[test]
fn deleting_a_project_deletes_its_memory() {
    let dir = tempfile::tempdir().unwrap();
    let (mut store, _) = ConnectionStore::load_from(dir.path().join("c.toml"));
    let id = store.add_project("gone").unwrap();
    store.add_project("kept").unwrap();
    let mut app = App::new(store, None, Some(dir.path().to_path_buf()));
    let memory = crate::gateway::memory::path(dir.path(), &id);
    std::fs::create_dir_all(memory.parent().unwrap()).unwrap();
    std::fs::write(&memory, "facts").unwrap();
    app.projects_selected = app.store.projects.iter().position(|p| p.id == id).unwrap();
    app.delete_selected_project();
    assert!(!memory.exists());
}

#[tokio::test]
async fn closing_another_tab_while_asking_keeps_the_question() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = asking_app(
        dir.path(),
        "[[models]]\nname = \"m\"\napi = \"openai\"\nurl = \"http://127.0.0.1:9/v1\"\nmodel = \"x\"\n",
    )
    .await;
    app.new_tab(None, "", None);
    app.new_tab(None, "", None);
    app.switch_tab(1);
    app.connect_form.name = "shop".into();
    let _gateway = crate::gateway::tests::start(dir.path(), None);
    app.run_action(Action::Ask);
    type_text(&mut app, "customers");
    app.handle_key(key(KeyCode::Enter));
    assert_eq!(app.asking.as_ref().unwrap().question.tab, 1);
    // Tab 0 closes: the question follows its tab to place 0.
    app.switch_tab(0);
    app.close_tab();
    assert_eq!(app.asking.as_ref().map(|a| a.question.tab), Some(0));
    // Its own tab closes: it stops.
    app.switch_tab(0);
    app.close_tab();
    assert!(app.asking.is_none());
}
