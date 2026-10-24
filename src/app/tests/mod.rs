use super::*;
use std::time::Instant;

use crate::config::keys::Action;
use crate::db::tests::{file_engine, file_profile};
use crate::db::{ConnectionMode, TxnState};

mod agents;
mod connections;
mod editor;
mod navigator;
mod plans;
mod results;
mod running;
mod safety;
mod settings;
mod tabs;

pub(super) fn outcome(columns: &[&str], rows: &[&[Option<&str>]]) -> QueryOutcome {
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
pub(super) fn test_app() -> (App, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let (mut store, warning) = ConnectionStore::load_from(dir.path().join("c.toml"));
    let id = store.add_project("p").unwrap();
    let mut app = App::new(store, warning, None);
    app.open_project(&id);
    (app, dir)
}

pub(super) fn project_id(app: &App) -> String {
    app.current_project.clone().unwrap()
}

pub(super) fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

pub(super) fn type_text(app: &mut App, text: &str) {
    for c in text.chars() {
        app.handle_key(key(KeyCode::Char(c)));
    }
}

/// An app connected to an in-memory file database (SQLite), on the query screen.
pub(super) async fn connected_app(mode: ConnectionMode) -> (App, tempfile::TempDir) {
    let (mut app, dir) = test_app();
    app.connection.form.engine = file_engine();
    app.connection.form.host = ":memory:".into();
    app.connection.form.mode = mode;
    let db = Database::connect(app.connection.form.params().unwrap()).await.unwrap();
    let generation = app.generation;
    app.handle_db_event(DbEvent { generation, run: None, kind: DbEventKind::Connected(db) });
    assert!(matches!(app.screen, Screen::Query));
    (app, dir)
}

/// Runs `sql` through the normal key path and waits for the result.
pub(super) async fn run_sql(app: &mut App, sql: &str) {
    app.tab.editor.set_text(sql);
    app.handle_key(key(KeyCode::F(5)));
    finish(app).await;
}

/// Handles events (catalog loads etc.) until every tab's running query is
/// done.
pub(super) async fn finish(app: &mut App) {
    while !app.running_tabs().is_empty() {
        let event = app.poll_db_event().await.unwrap();
        app.handle_db_event(event);
    }
}

/// Makes the pending syntax check due now and runs it to completion.
pub(super) async fn check_syntax_now(app: &mut App) {
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

/// Edits a cell of `select … from people` and returns the app.
pub(super) async fn edit_cell(mode: ConnectionMode, select: &str, col: usize) -> (App, tempfile::TempDir) {
    // Set up in normal mode, then switch to the mode under test.
    let (mut app, dir) = connected_app(ConnectionMode::Normal).await;
    run_sql(&mut app, "create table people (id integer primary key, name text not null, note text)").await;
    run_sql(&mut app, "insert into people values (1, 'Ann', 'x'), (2, 'Ben', null)").await;
    while app.connection.schema.as_ref().is_none_or(|s| s.loading) {
        let event = app.poll_db_event().await.unwrap();
        app.handle_db_event(event);
    }
    app.connection.mode = mode;
    run_sql(&mut app, select).await;
    app.focus = Focus::Results;
    app.handle_key(key(KeyCode::Down));
    app.tab.grid.selected_col = col;
    app.handle_key(key(KeyCode::Char('c')));
    (app, dir)
}

pub(super) fn alt(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::ALT)
}

/// The last message: in the editor's or the results' footer, else the
/// status bar.
pub(super) fn said(app: &App) -> &str {
    let results = app.notes.results_note.as_ref().map(|(m, _)| m.as_str());
    app.notes.editor_note.as_deref().or(results).unwrap_or(&app.notes.status)
}

/// Whether that message is an error.
pub(super) fn said_error(app: &App) -> bool {
    match (&app.notes.editor_note, &app.notes.results_note) {
        (Some(_), _) => app.notes.editor_note_is_error,
        (None, Some((_, is_error))) => *is_error,
        (None, None) => app.notes.status_is_error,
    }
}

pub(super) fn ctrl(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
}

/// Ctrl+K p: the projects and connections.
pub(super) fn show_projects(app: &mut App) {
    app.handle_key(ctrl('k'));
    app.handle_key(key(KeyCode::Char('p')));
}

/// Connects the active tab to a saved SQLite connection, as picking it does.
pub(super) async fn connect_profile(app: &mut App, profile: &ConnectionProfile) {
    app.connection.form = ConnectForm::from_profile(profile, String::new());
    let db = Database::connect(app.connection.form.params().unwrap()).await.unwrap();
    let generation = app.generation;
    app.handle_db_event(DbEvent { generation, run: None, kind: DbEventKind::Connected(db) });
    finish_catalog(app).await;
}

pub(super) async fn finish_catalog(app: &mut App) {
    while app.connection.schema.as_ref().is_none_or(|s| s.loading) || app.tab.run.executing {
        let event = app.poll_db_event().await.unwrap();
        app.handle_db_event(event);
    }
}

pub(super) fn table_names(app: &App) -> Vec<String> {
    app.connection.schema.as_ref().map(|s| s.tables.iter().map(|t| t.info.name.clone()).collect()).unwrap_or_default()
}

/// Waits for the next event of a kind and handles everything until then.
pub(super) async fn until_event(app: &mut App, done: fn(&DbEventKind) -> bool) {
    loop {
        let event = app.poll_db_event().await.unwrap();
        let last = done(&event.kind);
        app.handle_db_event(event);
        if last {
            return;
        }
    }
}

/// An app connected to a database of the test's own, `name`, on `engine`
/// (`db::tests::own_live_db`): `None` where it has no test server set.
pub(super) async fn live_app(engine: Engine, name: &str) -> Option<(App, tempfile::TempDir)> {
    let (db, params) = crate::db::tests::own_live_db(engine, name).await?;
    let (mut app, dir) = test_app();
    let form = &mut app.connection.form;
    form.engine = engine;
    form.host = params.host;
    form.port = if params.port == 0 { String::new() } else { params.port.to_string() };
    form.user = params.user;
    form.password = params.password;
    form.database = params.database;
    let generation = app.generation;
    app.handle_db_event(DbEvent { generation, run: None, kind: DbEventKind::Connected(db) });
    Some((app, dir))
}

/// Waits for a connection's password to come from the keychain.
pub(super) async fn read_password(app: &mut App) {
    while app.manager.loading_profile {
        let event = app.poll_db_event().await.unwrap();
        app.handle_db_event(event);
    }
}

/// An app on the saved SQLite connection `shop` of project `p`, its data
/// (and gateway) in `dir`, with `config` as config.toml.
pub(super) async fn asking_app(dir: &std::path::Path, config: &str) -> App {
    crate::gateway::tests::saved_shop(dir).await;
    std::fs::write(dir.join("config.toml"), config).unwrap();
    let (store, _) = ConnectionStore::load_from(dir.join("connections.toml"));
    let mut app = App::new(store, None, Some(dir.to_path_buf()));
    let id = app.store.project_by_name("p").unwrap().id.clone();
    app.open_project(&id);
    app.connection.form.engine = file_engine();
    app.connection.form.host = dir.join("shop.db").to_string_lossy().into_owned();
    app.connection.form.name = "shop".into();
    let db = Database::connect(app.connection.form.params().unwrap()).await.unwrap();
    let generation = app.generation;
    app.handle_db_event(DbEvent { generation, run: None, kind: DbEventKind::Connected(db) });
    app.connection.form.name = "shop".into();
    app
}

/// As if the question that came by itself had been on screen a while
/// (keys in its first moment don't answer it).
pub(super) fn settle(app: &mut App) {
    app.agents.asked = Instant::now().checked_sub(Duration::from_secs(2));
}

/// Handles events until the question is answered.
pub(super) async fn until_answered(app: &mut App) {
    while app.asking.is_some() {
        let event = tokio::time::timeout(Duration::from_secs(10), app.poll_db_event()).await.unwrap().unwrap();
        app.handle_db_event(event);
    }
}

/// Handles events until the memory window has its facts again.
pub(super) async fn until_memory_loaded(app: &mut App, facts: usize) {
    for _ in 0..50 {
        if app.memory_view.as_ref().is_some_and(|v| !v.loading && v.facts.len() == facts) {
            return;
        }
        let event = tokio::time::timeout(Duration::from_secs(10), app.poll_db_event()).await.unwrap().unwrap();
        app.handle_db_event(event);
    }
    panic!("the memory window didn't show {facts} facts");
}
