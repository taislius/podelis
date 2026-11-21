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
    app.manager.connection_selected = 1;
    app.manager.confirm_delete = true;
    assert_snapshot!(render(&mut app, 80, 14));
}

#[cfg(feature = "sqlite")]
#[test]
fn connect_form_server_and_sqlite() {
    let (mut app, _dir) = app();
    app.screen = Screen::Connect;
    app.connection.form.engine = Engine::Postgres;
    app.connection.form.mode = ConnectionMode::Production;
    app.connection.form.ssh = "me@bastion".into();
    app.connection.form.password = "secret".into();
    let server = render(&mut app, 80, 22);
    assert!(!server.contains("secret"), "password must be masked");
    assert_snapshot!("connect_form_server", server);

    app.connection.form.engine = crate::db::tests::file_engine();
    app.connection.form.host = "./shop.db".into();
    assert_snapshot!("connect_form_sqlite", render(&mut app, 80, 22));
}

#[test]
fn production_confirmation_dialog_and_badges() {
    let (mut app, _dir) = query_app();
    app.connection.mode = ConnectionMode::Production;
    app.connection.txn = TxnState::Open;
    app.popups.confirm = Some(Confirm {
        message: "PRODUCTION: 1 statement(s) will change data. Run?".into(),
        details: vec!["DELETE without WHERE: delete from users".into()],
        danger: true,
        action: ConfirmAction::Quit,
        offer_transaction: true,
    });
    assert_snapshot!(render(&mut app, 90, 20));
}

#[cfg(feature = "sqlite")]
#[test]
fn a_long_file_path_gives_way_to_the_message_and_keeps_its_name() {
    let (mut app, _dir) = query_app();
    app.connection.form.engine = crate::db::tests::file_engine();
    app.connection.form.host = format!("/data/{}/projects/shop.db", "deep/".repeat(12));
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
    app.notes.status.clear();
    let status_bar = |app: &mut App| render(app, 120, 12).lines().last().unwrap().to_string();
    let bar = status_bar(&mut app);
    assert!(bar.contains("♥ prod") && bar.contains("MySQL"), "{bar}");
    // The top bar no longer repeats it.
    assert!(!render(&mut app, 120, 12).lines().next().unwrap().contains("prod /"));
    app.connection.state.lose_for_tests();
    let lost = status_bar(&mut app);
    assert!(lost.contains("× prod") && lost.contains("connection lost"), "{lost}");
    assert!(lost.contains("trying again in"), "when it's tried again: {lost}");
}

/// Each state of the connection is said in words where it isn't simply
/// working, in the status bar and the navigator: never a green dot over
/// a blank navigator.
#[test]
fn the_connections_state_is_said_in_words() {
    use crate::app::ConnState;
    let (mut app, _dir) = query_app();
    app.notes.status.clear();
    app.panes.schema_visible = true;
    app.connection.schema = None;
    let bar = |app: &mut App| render(app, 120, 16).lines().last().unwrap().to_string();
    app.connection.state = ConnState::Ready { verified: false };
    assert!(bar(&mut app).contains("prod · checking…"), "{}", bar(&mut app));
    assert!(render(&mut app, 120, 16).contains("Loading…"), "the navigator isn't blank");
    app.connection.state = ConnState::Degraded { why: "The tables didn't load in 30 s".into() };
    assert!(bar(&mut app).contains("prod · tables didn't load"), "{}", bar(&mut app));
    app.connection.state = ConnState::Failed { why: "can't reach db.internal:3306: connection refused".into() };
    app.connection.form.name = "prod".into();
    let screen = render(&mut app, 120, 16);
    assert!(bar(&mut app).contains("prod didn't reconnect"), "{}", bar(&mut app));
    assert!(screen.contains("Couldn't reconnect prod") && screen.contains("can't reach"), "{screen}");
    app.connection.state = ConnState::Opening { attempt: 1 };
    let screen = render(&mut app, 120, 16);
    assert!(bar(&mut app).contains("reconnecting…"), "{}", bar(&mut app));
    assert!(!screen.contains("● prod"), "{screen}");
}

#[test]
fn tx_mode_picker() {
    let (mut app, _dir) = query_app();
    app.handle_key(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::CONTROL));
    app.handle_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
    assert_eq!(app.popups.tx_picker, Some(crate::app::TxMode::AutoCommit));
    app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    let screen = render(&mut app, 90, 20);
    assert!(screen.contains("TRANSACTION MODE") && screen.contains("always rolled back"), "{screen}");
    assert_snapshot!(screen);
    // Enter picks the highlighted mode.
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(app.popups.tx_picker.is_none() && app.connection.tx_mode == crate::app::TxMode::Manual);
}

#[test]
fn status_bar_names_only_vims_mode() {
    let (mut app, _dir) = query_app();
    app.notes.status.clear();
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

#[test]
fn status_bar_says_whether_ai_is_on() {
    let (mut app, _dir) = query_app();
    app.notes.status.clear();
    let status_bar = |app: &mut App| render(app, 140, 12).lines().last().unwrap().to_string();
    app.settings.models.clear();
    assert!(status_bar(&mut app).contains("AI off"), "{}", status_bar(&mut app));
    let model = |url: &str| -> podelis_ai::models::ModelConfig {
        toml::from_str(&format!("name = \"claude\"\napi = \"anthropic\"\nmodel = \"claude-opus-5-5\"\nurl = \"{url}\""))
            .unwrap()
    };
    // A model on this machine: what it's shown stays here.
    app.settings.models = vec![model("http://localhost:11434")];
    let bar = status_bar(&mut app);
    assert!(bar.contains("AI claude") && !bar.contains("asks to share"), "{bar}");
    // A provider's, with the project sharing schemas only locally.
    app.settings.models = vec![model("https://api.anthropic.com")];
    if app.project().is_none_or(|p| p.share_schema == crate::config::store::ShareSchema::Local) {
        assert!(status_bar(&mut app).contains("AI claude · asks to share"), "{}", status_bar(&mut app));
    }
}

#[test]
fn the_open_connections_list_says_its_keys() {
    let (mut app, _dir) = query_app();
    app.tabs[0].name = "orders".into();
    app.handle_key(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::CONTROL));
    app.handle_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE));
    let screen = render(&mut app, 110, 20);
    assert!(screen.contains("OPEN CONNECTIONS") && screen.contains("Ctrl+D close it"), "{screen}");
    assert!(screen.contains("orders") && screen.contains("(this one)"), "{screen}");
    // A short terminal: it fits (no drawing past the screen's edge).
    for height in 4..12 {
        render(&mut app, 60, height);
    }
}

/// Ctrl+←/→ move the borders on the projects and connections screens too.
#[test]
fn ctrl_arrows_resize_the_managers_panes() {
    let ctrl = |code| crossterm::event::KeyEvent::new(code, crossterm::event::KeyModifiers::CONTROL);
    let (mut app, _dir) = app();
    app.screen = Screen::Projects;
    render(&mut app, 140, 16);
    let before = app.hits.manager_panes.0.unwrap().width;
    app.handle_key(ctrl(KeyCode::Right));
    render(&mut app, 140, 16);
    assert_eq!(app.hits.manager_panes.0.unwrap().width, before + 2, "the projects pane is wider");
    app.screen = Screen::Connections;
    render(&mut app, 140, 16);
    let before = app.hits.manager_panes.1.unwrap().width;
    app.handle_key(ctrl(KeyCode::Right));
    render(&mut app, 140, 16);
    assert_eq!(app.hits.manager_panes.1.unwrap().width, before - 2, "the border moved right: settings narrower");
    // Editing a connection: the same border.
    app.screen = Screen::Connect;
    render(&mut app, 140, 16);
    let before = app.hits.manager_panes.1.unwrap().width;
    app.handle_key(ctrl(KeyCode::Left));
    render(&mut app, 140, 16);
    assert_eq!(app.hits.manager_panes.1.unwrap().width, before + 2, "the form wider");
}

/// The connection being edited says what saving and connecting do, and
/// whether it differs from what's saved; the cursor is in its field.
#[test]
fn the_connection_form_says_its_keys_and_unsaved_changes() {
    let (mut app, _dir) = app();
    let saved = app.profiles()[0].clone();
    app.connection.form = crate::app::ConnectForm::from_profile(&saved, String::new());
    app.screen = Screen::Connect;
    let screen = render(&mut app, 140, 30);
    assert!(screen.contains("Ctrl+S save") && !screen.contains("unsaved changes"), "{screen}");
    app.handle_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
    let screen = render(&mut app, 140, 30);
    assert!(screen.contains("unsaved changes"), "{screen}");
}

/// The top bar names the project and the tab's connection, each a
/// drop-down: a click lists the projects to switch to (with the projects
/// window after them), or the project's connections for this tab (with its
/// databases and the connections window after them), hanging under it.
#[test]
fn the_top_bar_has_the_project_and_the_connection_as_drop_downs() {
    use crate::app::commands::Command;
    let (mut app, _dir) = query_app();
    let screen = render(&mut app, 120, 24);
    let top = screen.lines().next().unwrap();
    assert!(top.contains("project") && top.contains("connection") && top.matches('▼').count() == 2, "{top}");
    let conn = app.hits.conn_menu;
    assert!(conn.width > 0);
    // A click on it, as the mouse does.
    app.open_connection_menu((conn.x, conn.y + 1));
    let items = app.palette_items();
    assert!(items.iter().any(|c| matches!(c, Command::OpenSaved(_))), "{items:?}");
    assert!(items.contains(&Command::PickDatabase));
    let screen = render(&mut app, 120, 24);
    let below = screen.lines().nth(1).unwrap();
    assert!(below.contains("CONNECTIONS"), "hangs under the bar: {below}");
    assert!(
        screen.contains("prod  · PostgreSQL") || screen.contains("prod  · MySQL") || screen.contains("prod  · "),
        "{screen}"
    );
    app.popups.palette = None;
    app.open_project_menu((app.hits.project.x, 1));
    let items = app.palette_items();
    assert!(matches!(items[0], Command::SwitchProject(_)), "{items:?}");
    assert_eq!(items.last(), Some(&Command::Action(crate::config::keys::Action::Projects)));
    let screen = render(&mut app, 120, 24);
    assert!(screen.contains("PROJECTS") && screen.contains("· open"), "{screen}");
}
