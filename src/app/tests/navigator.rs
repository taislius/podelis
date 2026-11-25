use super::*;

#[cfg(feature = "sqlite")]
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
    while app.connection.schema.as_ref().is_none_or(|s| s.loading) {
        let event = app.poll_db_event().await.unwrap();
        app.handle_db_event(event);
    }
    let wait_details = async |app: &mut App| {
        while app.connection.schema.as_ref().unwrap().tables.iter().any(|t| t.details_loading) {
            let event = app.poll_db_event().await.unwrap();
            app.handle_db_event(event);
        }
    };
    app.focus = Focus::Schema;
    let panel = app.connection.schema.as_mut().unwrap();
    let child = panel.tables.iter().position(|t| t.info.name == "child").unwrap();
    panel.selected = panel.nodes().iter().position(|n| *n == SchemaNode::Table(child)).unwrap();
    app.handle_key(key(KeyCode::Enter));
    wait_details(&mut app).await;
    let panel = app.connection.schema.as_ref().unwrap();
    assert!(panel.nodes().contains(&SchemaNode::Index(child, 0)));
    assert_eq!(panel.tables[child].foreign_key("parent_id"), Some("parent.id"));

    // `d` shows the table's CREATE statement (SQLite keeps it).
    app.handle_key(key(KeyCode::Char('d')));
    let view = app.popups.cell_view.as_ref().expect("definition shown");
    assert!(view.title.contains("child") && view.text.contains("references parent"), "{}", view.text);
    app.handle_key(key(KeyCode::Esc));

    // On a table whose details aren't loaded yet, `d` says so at once, and
    // shows it once they arrive.
    let panel = app.connection.schema.as_mut().unwrap();
    let parent = panel.tables.iter().position(|t| t.info.name == "parent").unwrap();
    panel.selected = panel.nodes().iter().position(|n| *n == SchemaNode::Table(parent)).unwrap();
    app.handle_key(key(KeyCode::Char('d')));
    assert_eq!(app.popups.cell_view.as_ref().map(|v| v.text.as_str()), Some(t!("navigator.loading_tables")));
    wait_details(&mut app).await;
    assert!(app.popups.cell_view.as_ref().is_some_and(|v| v.text.contains("CREATE TABLE parent")));
}

#[cfg(feature = "sqlite")]
#[tokio::test]
async fn system_tables_toggle_in_their_own_section() {
    let (mut app, _dir) = connected_app(ConnectionMode::Normal).await;
    run_sql(&mut app, "create table t (id integer primary key autoincrement); insert into t default values;").await;
    finish_catalog(&mut app).await;
    let sections = |app: &App| -> Vec<SchemaSection> {
        let nodes = app.connection.schema.as_ref().unwrap().nodes();
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

/// A schema snapshot saved, then the schema changed: comparing with the
/// snapshot shows what changed since, and the SQL for it.
#[cfg(feature = "sqlite")]
#[tokio::test]
async fn schema_snapshots_show_what_changed_since() {
    let (mut app, dir) = connected_app(ConnectionMode::Normal).await;
    app.set_data_dir(dir.path().to_path_buf());
    run_sql(&mut app, "create table people (id integer primary key, name text)").await;
    app.snapshot_schema();
    until_event(&mut app, |k| matches!(k, DbEventKind::SnapshotSaved(_))).await;
    assert!(app.notes.status.starts_with("Saved a schema snapshot (1 tables)"), "{}", app.notes.status);
    assert_eq!(app.snapshots().len(), 1);
    app.open_palette(false);
    let entry = app.palette_command_text(crate::app::commands::Command::CompareSnapshot(0));
    app.popups.palette = None;
    assert!(entry.starts_with("Compare the schema with the snapshot: "), "{entry}");

    run_sql(&mut app, "alter table people add column email text; create table orders (id integer primary key)").await;
    app.compare_snapshot(0);
    until_event(&mut app, |k| matches!(k, DbEventKind::SnapshotCompared(_))).await;
    assert!(app.notes.status.starts_with("2 changes since the snapshot"), "{}", app.notes.status);
    let text = &app.popups.cell_view.as_ref().unwrap().text;
    assert!(text.contains("ONLY HERE\n  table orders") && text.contains("column email TEXT: only here"), "{text}");
    assert!(text.contains("people ADD COLUMN email TEXT;"), "{text}");
}

#[test]
fn space_in_the_schema_opens_the_command_menu() {
    let (mut app, _dir) = test_app();
    app.screen = Screen::Query;
    app.handle_key(ctrl('k'));
    assert!(app.popups.palette.as_ref().is_some_and(|p| !p.searching()));
    // The same key again searches every command.
    app.handle_key(ctrl('k'));
    assert!(app.popups.palette.as_ref().unwrap().searching());
    app.handle_key(key(KeyCode::Esc));

    app.connection.schema = Some(SchemaPanel::default());
    app.focus = Focus::Schema;
    app.handle_key(key(KeyCode::Char(' ')));
    assert!(app.popups.palette.is_some());
}

#[cfg(sql_engine)] // SQL's own: not with MongoDB alone
#[test]
fn s_in_the_navigator_adds_the_select_after_everything() {
    let (mut app, _dir) = test_app();
    app.connection.schema = Some(SchemaPanel {
        tables: vec![SchemaTable {
            info: crate::db::TableInfo {
                schema: "public".into(),
                name: "users".into(),
                is_view: false,
                rows: None,
                bytes: None,
                system: false,
                partition_of: None,
                materialized: false,
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
    // The keys stay in the navigator, for the next table.
    assert_eq!((app.focus, app.tab.editor.cursor_row), (Focus::Schema, 4));
}

/// While the filter is typed: the arrows pick among what's left, Enter
/// opens the one picked, and a paste goes into it, opening it (found by
/// hand, 2026-11-09).
#[test]
fn the_navigators_filter_picks_opens_and_takes_a_paste() {
    let (mut app, _dir) = test_app();
    let table = |name: &str| SchemaTable {
        info: crate::db::TableInfo {
            schema: "main".into(),
            name: name.into(),
            is_view: false,
            rows: None,
            bytes: None,
            system: false,
            partition_of: None,
            materialized: false,
        },
        expanded: false,
        columns: Vec::new(),
        details: None,
        details_loading: false,
    };
    let tables = vec![table("orders"), table("order_items"), table("users")];
    app.screen = Screen::Query;
    app.connection.schema = Some(SchemaPanel { tables, selected: 1, ..SchemaPanel::default() });
    app.focus = Focus::Schema;
    app.paste_into_pane("order");
    let panel = app.connection.schema.as_ref().unwrap();
    assert!(panel.filter_editing && panel.filter.text() == "order", "a paste is a name to look for");
    let picked = |app: &App| {
        let panel = app.connection.schema.as_ref().unwrap();
        panel.nodes().get(panel.selected).and_then(|n| n.table()).map(|t| panel.tables[t].info.name.clone())
    };
    let first = picked(&app);
    app.handle_schema_key(key(KeyCode::Down));
    let second = picked(&app);
    assert!(first.is_some() && second.is_some() && first != second, "the arrows pick: {first:?} {second:?}");
    app.handle_schema_key(key(KeyCode::Enter));
    let panel = app.connection.schema.as_ref().unwrap();
    assert!(!panel.filter_editing && panel.filter.text() == "order", "the filter stays");
    let t = panel.nodes()[panel.selected].table().unwrap();
    assert!(panel.tables[t].expanded, "Enter opened the one picked");
}

/// Tab stops the filter's typing, so d and s act on the one picked; a
/// cleared filter keeps it picked.
#[test]
fn tab_in_the_navigators_filter_hands_the_keys_to_the_one_picked() {
    let (mut app, _dir) = test_app();
    let table = |name: &str| SchemaTable {
        info: crate::db::TableInfo {
            schema: "main".into(),
            name: name.into(),
            is_view: false,
            rows: None,
            bytes: None,
            system: false,
            partition_of: None,
            materialized: false,
        },
        expanded: false,
        columns: Vec::new(),
        details: Some(crate::db::TableDetails::default()),
        details_loading: false,
    };
    let tables = vec![table("accounts"), table("orders"), table("users")];
    app.screen = Screen::Query;
    app.connection.schema = Some(SchemaPanel { tables, selected: 1, ..SchemaPanel::default() });
    app.focus = Focus::Schema;
    app.handle_key(key(KeyCode::Char('/')));
    for c in "use".chars() {
        app.handle_key(key(KeyCode::Char(c)));
    }
    app.handle_key(key(KeyCode::Tab));
    let panel = app.connection.schema.as_ref().unwrap();
    assert!(!panel.filter_editing && panel.filter.text() == "use", "typing stopped, the filter kept");
    assert_eq!(app.focus, Focus::Schema);
    app.handle_key(key(KeyCode::Char('d')));
    assert_eq!(app.popups.cell_view.as_ref().map(|v| v.title.as_str()), Some("TABLE users"));
    app.handle_key(key(KeyCode::Esc));
    // Esc clears the filter; users stays picked, among all of them.
    app.handle_key(key(KeyCode::Esc));
    let panel = app.connection.schema.as_ref().unwrap();
    assert!(panel.filter.is_empty());
    let t = panel.nodes()[panel.selected].table().unwrap();
    assert_eq!(panel.tables[t].info.name, "users");
}

/// `d` lists a table's foreign keys on their own, under its columns.
#[test]
fn a_definition_lists_the_foreign_keys() {
    let (mut app, _dir) = test_app();
    let details = crate::db::TableDetails {
        foreign_keys: vec![crate::db::ForeignKey {
            column: "user_id".into(),
            target: "users.id".into(),
            target_schema: "public".into(),
            target_table: "users".into(),
            target_column: Some("id".into()),
            on_delete: Some("CASCADE".into()),
            on_update: None,
        }],
        checks: vec![crate::db::CheckInfo { name: "positive".into(), definition: "CHECK ((total > 0))".into() }],
        ..crate::db::TableDetails::default()
    };
    let col = |name: &str| crate::db::ColumnInfo {
        name: name.into(),
        data_type: "integer".into(),
        nullable: false,
        primary_key: name == "id",
    };
    let orders = SchemaTable {
        info: crate::db::TableInfo {
            schema: "public".into(),
            name: "orders".into(),
            is_view: false,
            rows: None,
            bytes: None,
            system: false,
            partition_of: None,
            materialized: false,
        },
        expanded: false,
        columns: vec![col("id"), col("user_id")],
        details: Some(details),
        details_loading: false,
    };
    app.connection.schema = Some(SchemaPanel { tables: vec![orders], selected: 1, ..SchemaPanel::default() });
    app.focus = Focus::Schema;
    app.handle_schema_key(key(KeyCode::Char('d')));
    let text = &app.popups.cell_view.as_ref().expect("definition").text;
    assert!(text.contains("-- foreign keys") && text.contains("user_id  → users.id ON DELETE CASCADE"), "{text}");
    assert!(text.contains("-- check constraints\nCONSTRAINT positive CHECK ((total > 0))"), "{text}");
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
            partition_of: None,
            materialized: false,
        },
        expanded: false,
        columns: Vec::new(),
        details: Some(crate::db::TableDetails::default()),
        details_loading: false,
    };
    app.connection.schema =
        Some(SchemaPanel { tables: vec![table("a"), table("b")], selected: 1, ..SchemaPanel::default() });
    app.focus = Focus::Schema;
    app.handle_schema_key(key(KeyCode::Char('d')));
    assert_eq!(app.popups.cell_view.as_ref().unwrap().title, "TABLE a");
    app.handle_cell_view_key(key(KeyCode::Right));
    assert_eq!(app.popups.cell_view.as_ref().unwrap().title, "TABLE b");
    assert_eq!(app.connection.schema.as_ref().unwrap().selected, 2, "the navigator follows");
    app.handle_cell_view_key(key(KeyCode::Right));
    assert_eq!(app.popups.cell_view.as_ref().unwrap().title, "TABLE b", "stays on the last");
    app.handle_cell_view_key(key(KeyCode::Left));
    assert_eq!(app.popups.cell_view.as_ref().unwrap().title, "TABLE a");
    app.handle_cell_view_key(key(KeyCode::Char('y')));
    assert!(app.take_clipboard().is_some_and(|t| t.contains("main")));
}

#[cfg(feature = "sqlite")]
#[tokio::test]
async fn s_in_the_navigator_runs_only_its_select() {
    let (mut app, _dir) = Box::pin(connected_app(ConnectionMode::Normal)).await;
    run_sql(&mut app, "create table people (id integer primary key, name text)").await;
    run_sql(&mut app, "insert into people values (1, 'Ann')").await;
    while app.connection.schema.as_ref().is_none_or(|s| s.loading || s.tables.is_empty()) {
        let event = app.poll_db_event().await.unwrap();
        app.handle_db_event(event);
    }
    // Other statements in the editor, one of them broken: they're not run.
    app.tab.editor.set_text("select nope from nowhere;\ndelete from people;");
    app.tab.editor.set_cursor((0, 3));
    let panel = app.connection.schema.as_mut().unwrap();
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

#[cfg(feature = "sqlite")]
#[tokio::test]
async fn a_far_model_sees_the_schema_once_allowed_and_always_saves_it() {
    let dir = tempfile::tempdir().unwrap();
    let config = "[[models]]\nname = \"far\"\napi = \"openai\"\nurl = \"https://models.invalid/v1\"\nmodel = \"x\"\nkey_command = \"echo sk-x\"\n";
    let mut app = asking_app(dir.path(), config).await;
    let _gateway = crate::gateway::tests::start(dir.path(), None);
    // A question in the editor: Ctrl+K q starts it, Ctrl+K q on it asks it.
    app.focus = Focus::Editor;
    app.run_action(Action::Ask);
    type_text(&mut app, "customers by country");
    app.run_action(Action::Ask);
    until_answered(&mut app).await;
    let confirm = app.popups.confirm.as_ref().expect("asks whether to send the schema");
    assert!(matches!(confirm.action, ConfirmAction::ShareSchema { .. }));
    assert!(confirm.message.contains("Send p's table and column names"), "{}", confirm.message);
    // Keys typed as it appears don't answer it; nor do others.
    app.handle_key(key(KeyCode::Char('a')));
    app.handle_key(key(KeyCode::Char('x')));
    assert!(app.popups.confirm.is_some() && app.ask.asking.is_none());
    assert_eq!(app.store.project_by_name("p").unwrap().share_schema, crate::config::store::ShareSchema::Local);
    settle(&mut app);
    // `a`: always, for the project; the question goes again.
    app.handle_key(key(KeyCode::Char('a')));
    assert_eq!(app.store.project_by_name("p").unwrap().share_schema, crate::config::store::ShareSchema::All);
    let (saved, _) = ConnectionStore::load_from(dir.path().join("connections.toml"));
    assert_eq!(saved.project_by_name("p").unwrap().share_schema, crate::config::store::ShareSchema::All);
    assert!(app.ask.asking.is_some());
    until_answered(&mut app).await;
    // (The address is made up: it fails, but it was sent.)
    assert!(said(&app).starts_with("Couldn't ask") && said(&app).contains("models.invalid"), "{}", said(&app));
}

/// `a` updates the selected table's statistics, as each engine words it,
/// run alone as `s` runs its select.
#[cfg(feature = "sqlite")]
#[tokio::test]
async fn a_in_the_navigator_analyzes_the_table() {
    let (mut app, _dir) = Box::pin(connected_app(ConnectionMode::Normal)).await;
    run_sql(&mut app, "create table people (id integer primary key, name text)").await;
    while app.connection.schema.as_ref().is_none_or(|s| s.loading || s.tables.is_empty()) {
        let event = app.poll_db_event().await.unwrap();
        app.handle_db_event(event);
    }
    let panel = app.connection.schema.as_mut().unwrap();
    panel.selected = panel.nodes().iter().position(|n| matches!(n, SchemaNode::Table(_))).unwrap();
    app.focus = Focus::Schema;
    app.handle_key(key(KeyCode::Char('a')));
    finish(&mut app).await;
    assert!(app.tab.editor.text().ends_with("ANALYZE main.people;"), "{}", app.tab.editor.text());
    assert!(app.tab.run_error.is_none(), "{:?}", app.tab.run_error);
    assert_eq!(app.focus, Focus::Schema);
}

/// Two tables, `orders` (expanded, a column and its details) and `users`
/// (its details on their way), on PostgreSQL (not connected), for the `d`
/// and `c` tests below.
fn definition_app() -> (App, tempfile::TempDir) {
    let (mut app, dir) = test_app();
    app.connection.form.name = "pg".into();
    app.connection.form.engine = Engine::Postgres;
    let info = |name: &str| crate::db::TableInfo {
        schema: "public".into(),
        name: name.into(),
        is_view: false,
        rows: None,
        bytes: None,
        system: false,
        partition_of: None,
        materialized: false,
    };
    let column =
        crate::db::ColumnInfo { name: "id".into(), data_type: "integer".into(), nullable: false, primary_key: true };
    let mut orders = SchemaTable::new(info("orders"), vec![column]);
    orders.expanded = true;
    orders.details = Some(crate::db::TableDetails::default());
    let mut users = SchemaTable::new(info("users"), Vec::new());
    users.details_loading = true;
    app.connection.schema = Some(SchemaPanel { tables: vec![orders, users], ..SchemaPanel::default() });
    app.focus = Focus::Schema;
    (app, dir)
}

fn select_node(app: &mut App, node: SchemaNode) {
    let panel = app.connection.schema.as_mut().unwrap();
    panel.selected = panel.nodes().iter().position(|n| *n == node).unwrap();
}

fn send_details(app: &mut App, table: &str, result: Result<crate::db::TableDetails, String>) {
    let generation = app.generation;
    let (schema, table) = ("public".to_string(), table.to_string());
    app.handle_db_event(DbEvent::connection(generation, DbEventKind::TableDetails { schema, table, result }));
}

fn send_ddl(app: &mut App, id: u64, result: Result<Option<String>, String>) {
    let generation = app.generation;
    app.handle_db_event(DbEvent::connection(generation, DbEventKind::TableDdl { id, table: "orders".into(), result }));
}

/// The popup as if `c` had asked for its CREATE TABLE as request `id`.
fn waiting_for_ddl(app: &mut App, id: u64) {
    let view = app.popups.cell_view.as_mut().unwrap();
    view.overview = Some(std::mem::replace(&mut view.text, t!("navigator.loading_tables").to_string()));
    view.awaiting = Some(id);
}

/// `d` shows a table's overview, on a column of it too; `c` there asks for
/// its CREATE TABLE (not connected: said, the overview staying), which
/// lands in that popup only while it waits for that very request; `c`
/// again goes back to the overview.
#[test]
fn c_in_a_definition_shows_the_create_table_and_back() {
    let (mut app, _dir) = definition_app();
    select_node(&mut app, SchemaNode::Column(0, 0));
    app.handle_schema_key(key(KeyCode::Char('d')));
    let view = app.popups.cell_view.as_ref().unwrap();
    assert_eq!(view.title, "TABLE orders");
    assert_eq!(view.create_table, Some(("public".to_string(), "orders".to_string())));
    let overview = view.text.clone();
    assert!(overview.contains("public.orders"), "{overview}");

    app.handle_cell_view_key(key(KeyCode::Char('c')));
    let view = app.popups.cell_view.as_ref().unwrap();
    assert_eq!((view.text.as_str(), view.awaiting), (overview.as_str(), None), "not connected: nothing asked");
    assert!(app.notes.status.contains("orders"), "said why: {}", app.notes.status);

    // An answer to another request, or to a closed popup, doesn't land.
    waiting_for_ddl(&mut app, 7);
    send_ddl(&mut app, 6, Ok(Some("CREATE TABLE old ();".into())));
    assert_eq!(app.popups.cell_view.as_ref().unwrap().text, t!("navigator.loading_tables"));
    send_ddl(&mut app, 7, Ok(Some("CREATE TABLE public.orders ();".into())));
    let view = app.popups.cell_view.as_ref().unwrap();
    assert_eq!((view.text.as_str(), view.awaiting, view.browse), ("CREATE TABLE public.orders ();", None, true));
    app.handle_cell_view_key(key(KeyCode::Char('c')));
    let view = app.popups.cell_view.as_ref().unwrap();
    assert_eq!((view.text.as_str(), view.overview.is_none()), (overview.as_str(), true), "back to the overview");

    // Failed: back to the overview, and why it's said.
    waiting_for_ddl(&mut app, 8);
    send_ddl(&mut app, 8, Err("permission denied".into()));
    assert_eq!(app.popups.cell_view.as_ref().unwrap().text, overview);
    assert!(app.notes.status.contains("permission denied"));

    // Closed meanwhile: it stays closed.
    waiting_for_ddl(&mut app, 9);
    app.popups.cell_view = None;
    send_ddl(&mut app, 9, Ok(Some("CREATE TABLE public.orders ();".into())));
    assert!(app.popups.cell_view.is_none());
}

/// `c` on a table the server no longer has: back to the overview, and
/// said, not silently.
#[test]
fn c_on_a_table_gone_from_the_server_says_so() {
    let (mut app, _dir) = definition_app();
    select_node(&mut app, SchemaNode::Table(0));
    app.handle_schema_key(key(KeyCode::Char('d')));
    let overview = app.popups.cell_view.as_ref().unwrap().text.clone();
    waiting_for_ddl(&mut app, 3);
    send_ddl(&mut app, 3, Ok(None));
    assert_eq!(app.popups.cell_view.as_ref().unwrap().text, overview);
    assert!(app.notes.status.contains(t!("schema.ddl_no_table")), "{}", app.notes.status);
}

/// A function's definition opens the popup only if it's the one asked for
/// last and still wanted: not once a table's is shown there, nor after the
/// popup was closed.
#[test]
fn a_late_function_definition_doesnt_take_a_tables_popup() {
    let (mut app, _dir) = definition_app();
    let routine = |app: &mut App, id: u64| {
        let generation = app.generation;
        let (title, text) = ("FUNCTION f".to_string(), Ok("CREATE FUNCTION f()".to_string()));
        app.handle_db_event(DbEvent::connection(generation, DbEventKind::RoutineDefinition { id, title, text }));
    };
    // Asked for, then a table stepped to: the table's stays.
    app.routine_wanted = Some(4);
    select_node(&mut app, SchemaNode::Table(0));
    app.handle_schema_key(key(KeyCode::Char('d')));
    routine(&mut app, 4);
    assert_eq!(app.popups.cell_view.as_ref().unwrap().title, "TABLE orders");
    // An older one than the one asked for last: not it.
    app.routine_wanted = Some(6);
    routine(&mut app, 5);
    assert_eq!(app.popups.cell_view.as_ref().unwrap().title, "TABLE orders");
    // Closed: it stays closed.
    app.handle_cell_view_key(key(KeyCode::Esc));
    routine(&mut app, 6);
    assert!(app.popups.cell_view.is_none());
    // The one asked for, still wanted: shown, browsable.
    app.routine_wanted = Some(7);
    routine(&mut app, 7);
    let view = app.popups.cell_view.as_ref().unwrap();
    assert_eq!((view.title.as_str(), view.browse), ("FUNCTION f", true));
}

/// No `c` where the server writes no CREATE TABLE: a view (its query is
/// its definition), SQLite (its stored CREATE is the definition).
#[test]
fn c_is_offered_only_where_the_server_writes_one() {
    let (mut app, _dir) = definition_app();
    app.connection.schema.as_mut().unwrap().tables[0].info.is_view = true;
    app.connection.schema.as_mut().unwrap().tables[0].details.as_mut().unwrap().definition = Some("SELECT 1".into());
    select_node(&mut app, SchemaNode::Table(0));
    app.handle_schema_key(key(KeyCode::Char('d')));
    assert!(app.popups.cell_view.as_ref().unwrap().create_table.is_none());
    #[cfg(feature = "sqlite")]
    {
        let (mut app, _dir) = definition_app();
        app.connection.form.engine = file_engine();
        select_node(&mut app, SchemaNode::Table(0));
        app.handle_schema_key(key(KeyCode::Char('d')));
        assert!(app.popups.cell_view.as_ref().unwrap().create_table.is_none());
    }
}

/// `d` on a table whose details are on their way says so at once; they
/// open in it when they come, unless it was closed or moved from
/// meanwhile.
#[test]
fn a_definition_waiting_on_details_opens_only_where_asked() {
    let (mut app, _dir) = definition_app();
    let on_their_way = |app: &mut App| {
        let users = &mut app.connection.schema.as_mut().unwrap().tables[1];
        (users.details, users.details_loading) = (None, true);
    };
    select_node(&mut app, SchemaNode::Table(1));
    app.handle_schema_key(key(KeyCode::Char('d')));
    let view = app.popups.cell_view.as_ref().expect("said at once");
    assert_eq!((view.title.as_str(), view.text.as_str()), ("TABLE users", t!("navigator.loading_tables")));
    // Closed: the details don't open it again.
    app.popups.cell_view = None;
    send_details(&mut app, "users", Ok(crate::db::TableDetails::default()));
    assert!(app.popups.cell_view.is_none());

    // Moved on to another table: not shown there under the wrong row.
    on_their_way(&mut app);
    select_node(&mut app, SchemaNode::Table(1));
    app.handle_schema_key(key(KeyCode::Char('d')));
    select_node(&mut app, SchemaNode::Table(0));
    send_details(&mut app, "users", Ok(crate::db::TableDetails::default()));
    assert_eq!(app.popups.cell_view.as_ref().unwrap().text, t!("navigator.loading_tables"));

    // Still on it: the definition opens.
    on_their_way(&mut app);
    select_node(&mut app, SchemaNode::Table(1));
    app.handle_schema_key(key(KeyCode::Char('d')));
    send_details(&mut app, "users", Ok(crate::db::TableDetails::default()));
    assert!(app.popups.cell_view.as_ref().unwrap().text.contains("public.users"));

    // Not connected and none on their way: what's known, not "Loading…".
    app.connection.schema.as_mut().unwrap().tables[1].details = None;
    app.handle_schema_key(key(KeyCode::Char('d')));
    assert!(app.popups.cell_view.as_ref().unwrap().text.contains("public.users"));
}

/// Another table's details failing doesn't drop a definition waiting on
/// its own; its own failing is said in its popup, not "Loading…" on.
#[test]
fn a_failed_details_load_is_said_in_the_popup_waiting_on_it() {
    let (mut app, _dir) = definition_app();
    select_node(&mut app, SchemaNode::Table(1));
    app.handle_schema_key(key(KeyCode::Char('d')));
    send_details(&mut app, "orders", Err("boom".into()));
    let wanted = app.connection.schema.as_ref().unwrap().show_definition_of.clone();
    assert_eq!(wanted, Some(("public".to_string(), "users".to_string())));
    send_details(&mut app, "users", Err("no privilege".into()));
    let text = &app.popups.cell_view.as_ref().unwrap().text;
    assert!(text.contains("users") && text.contains("no privilege"), "{text}");
}

/// `d`, then `c`, on a live PostgreSQL table: the server's CREATE TABLE.
#[tokio::test]
async fn live_c_shows_the_servers_create_table() {
    let Some((mut app, _dir)) = live_app(Engine::Postgres, "podelis_create_table").await else { return };
    run_sql(&mut app, "CREATE TABLE IF NOT EXISTS ddl_orders (id int PRIMARY KEY, total numeric DEFAULT 0)").await;
    finish_catalog(&mut app).await;
    let panel = app.connection.schema.as_mut().unwrap();
    let t = panel.tables.iter().position(|t| t.info.name == "ddl_orders").expect("listed");
    panel.selected = panel.nodes().iter().position(|n| *n == SchemaNode::Table(t)).unwrap();
    app.focus = Focus::Schema;
    app.handle_key(key(KeyCode::Char('d')));
    while app.popups.cell_view.as_ref().is_some_and(|v| v.text == t!("navigator.loading_tables")) {
        let event = app.poll_db_event().await.unwrap();
        app.handle_db_event(event);
    }
    app.handle_key(key(KeyCode::Char('c')));
    while app.popups.cell_view.as_ref().is_some_and(|v| v.awaiting.is_some()) {
        let event = app.poll_db_event().await.unwrap();
        app.handle_db_event(event);
    }
    let text = &app.popups.cell_view.as_ref().unwrap().text;
    assert!(text.starts_with("CREATE TABLE public.ddl_orders (") && text.contains("DEFAULT 0"), "{text}");
}

/// MySQL's RESTRICT is its NO ACTION: a definition doesn't say it on
/// every key; elsewhere RESTRICT is its own rule, and said.
#[test]
fn restrict_is_said_only_where_it_differs() {
    let fk = crate::db::ForeignKey {
        column: "user_id".into(),
        target: "users.id".into(),
        target_schema: "public".into(),
        target_table: "users".into(),
        target_column: Some("id".into()),
        on_delete: Some("RESTRICT".into()),
        on_update: Some("CASCADE".into()),
    };
    assert_eq!(fk.actions_where(true), " ON UPDATE CASCADE");
    assert_eq!(fk.actions_where(false), " ON DELETE RESTRICT ON UPDATE CASCADE");
}
