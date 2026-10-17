use super::*;

#[test]
fn projects_screen() {
    let (mut app, _dir) = app();
    app.screen = Screen::Projects;
    assert_snapshot!(render(&mut app, 80, 16));
}

#[test]
fn connections_screen_with_delete_confirmation() {
    let (mut app, _dir) = app();
    app.screen = Screen::Connections;
    app.connections_selected = 1;
    app.confirm_delete = true;
    assert_snapshot!(render(&mut app, 80, 14));
}

#[test]
fn connect_form_server_and_sqlite() {
    let (mut app, _dir) = app();
    app.screen = Screen::Connect;
    app.connect_form.engine = Engine::Postgres;
    app.connect_form.mode = ConnectionMode::Production;
    app.connect_form.ssh = "me@bastion".into();
    app.connect_form.password = "secret".into();
    let server = render(&mut app, 80, 22);
    assert!(!server.contains("secret"), "password must be masked");
    assert_snapshot!("connect_form_server", server);

    app.connect_form.engine = crate::db::tests::file_engine();
    app.connect_form.host = "./shop.db".into();
    assert_snapshot!("connect_form_sqlite", render(&mut app, 80, 22));
}

#[test]
fn production_confirmation_dialog_and_badges() {
    let (mut app, _dir) = query_app();
    app.active_mode = ConnectionMode::Production;
    app.txn = TxnState::Open;
    app.confirm = Some(Confirm {
        message: "PRODUCTION: 1 statement(s) will change data. Run?".into(),
        details: vec!["DELETE without WHERE: delete from users".into()],
        danger: true,
        action: ConfirmAction::Quit,
        offer_transaction: true,
    });
    assert_snapshot!(render(&mut app, 90, 20));
}

#[test]
fn a_long_file_path_gives_way_to_the_message_and_keeps_its_name() {
    let (mut app, _dir) = query_app();
    app.connect_form.engine = crate::db::tests::file_engine();
    app.connect_form.host = format!("/data/{}/projects/shop.db", "deep/".repeat(12));
    app.set_status_for_tests("Couldn't cancel: Esc again stops waiting · pool timed out");
    let screen = render(&mut app, 120, 24);
    let bar = screen.lines().last().unwrap();
    assert!(bar.contains("…") && bar.contains("projects/shop.db"), "{bar}");
    assert!(bar.contains("Couldn't cancel: Esc again stops waiting · pool timed out"), "{bar}");
}

#[test]
fn status_bar_shows_the_connection_heartbeat_and_lost_connection() {
    let (mut app, _dir) = query_app();
    app.settings.keepalive = 30;
    app.status.clear();
    let status_bar = |app: &mut App| render(app, 120, 12).lines().last().unwrap().to_string();
    let bar = status_bar(&mut app);
    assert!(bar.contains("♥ prod") && bar.contains("MySQL"), "{bar}");
    // The top bar no longer repeats it.
    assert!(!render(&mut app, 120, 12).lines().next().unwrap().contains("prod /"));
    app.connection_lost = true;
    assert!(status_bar(&mut app).contains("connection lost"));
}

#[test]
fn tx_mode_picker() {
    let (mut app, _dir) = query_app();
    app.handle_key(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::CONTROL));
    app.handle_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
    assert_eq!(app.tx_picker, Some(crate::app::TxMode::AutoCommit));
    app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    let screen = render(&mut app, 90, 20);
    assert!(screen.contains("TRANSACTION MODE") && screen.contains("always rolled back"), "{screen}");
    assert_snapshot!(screen);
    // Enter picks the highlighted mode.
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(app.tx_picker.is_none() && app.tx_mode == crate::app::TxMode::Manual);
}

#[test]
fn status_bar_names_only_vims_mode() {
    let (mut app, _dir) = query_app();
    app.status.clear();
    app.focus = Focus::Results;
    assert!(!render(&mut app, 120, 12).lines().last().unwrap().contains("RESULTS"));
    app.settings.editor_mode = crate::config::settings::EditorMode::Vim;
    app.focus = Focus::Editor;
    assert!(render(&mut app, 120, 12).lines().last().unwrap().contains(" NORMAL "));
}

#[test]
fn welcome_on_first_run_and_the_reopening_card() {
    let dir = tempfile::tempdir().unwrap();
    let (store, _) = crate::config::store::ConnectionStore::load_from(dir.path().join("c.toml"));
    let mut app = App::new(store, None, None);
    let screen = render(&mut app, 90, 24);
    assert!(screen.contains("Welcome") && screen.contains("podelis app.db"), "{screen}");
    assert_snapshot!("welcome", screen);

    let (mut app, _dir) = query_app();
    app.splash = Some(crate::app::Splash::for_tests(true));
    let screen = render(&mut app, 90, 24);
    assert!(screen.contains("reopening shop") && screen.contains("any key continues"), "{screen}");
    assert_snapshot!("reopening", screen);
}
