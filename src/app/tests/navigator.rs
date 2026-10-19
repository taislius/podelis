use super::*;

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
    let view = app.cell_view.as_ref().expect("definition shown");
    assert!(view.title.contains("child") && view.text.contains("references parent"), "{}", view.text);
    app.handle_key(key(KeyCode::Esc));

    // On a table whose details aren't loaded yet, `d` opens it once they arrive.
    let panel = app.connection.schema.as_mut().unwrap();
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
    let entry = app.palette_command_text(crate::app::palette::Command::CompareSnapshot(0));
    app.palette = None;
    assert!(entry.starts_with("Compare the schema with the snapshot: "), "{entry}");

    run_sql(&mut app, "alter table people add column email text; create table orders (id integer primary key)").await;
    app.compare_snapshot(0);
    until_event(&mut app, |k| matches!(k, DbEventKind::SnapshotCompared(_))).await;
    assert!(app.notes.status.starts_with("2 changes since the snapshot"), "{}", app.notes.status);
    let text = &app.cell_view.as_ref().unwrap().text;
    assert!(text.contains("ONLY HERE\n  table orders") && text.contains("column email TEXT: only here"), "{text}");
    assert!(text.contains("people ADD COLUMN email TEXT;"), "{text}");
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

    app.connection.schema = Some(SchemaPanel::default());
    app.focus = Focus::Schema;
    app.handle_key(key(KeyCode::Char(' ')));
    assert!(app.palette.is_some());
}

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
    app.connection.schema =
        Some(SchemaPanel { tables: vec![table("a"), table("b")], selected: 1, ..SchemaPanel::default() });
    app.focus = Focus::Schema;
    app.handle_schema_key(key(KeyCode::Char('d')));
    assert_eq!(app.cell_view.as_ref().unwrap().title, "TABLE a");
    app.handle_cell_view_key(key(KeyCode::Right));
    assert_eq!(app.cell_view.as_ref().unwrap().title, "TABLE b");
    assert_eq!(app.connection.schema.as_ref().unwrap().selected, 2, "the navigator follows");
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
    assert_eq!(app.store.project_by_name("p").unwrap().share_schema, crate::config::store::ShareSchema::Local);
    settle(&mut app);
    // `a`: always, for the project; the question goes again.
    app.handle_key(key(KeyCode::Char('a')));
    assert_eq!(app.store.project_by_name("p").unwrap().share_schema, crate::config::store::ShareSchema::All);
    let (saved, _) = ConnectionStore::load_from(dir.path().join("connections.toml"));
    assert_eq!(saved.project_by_name("p").unwrap().share_schema, crate::config::store::ShareSchema::All);
    assert!(app.asking.is_some());
    until_answered(&mut app).await;
    // (The address is made up: it fails, but it was sent.)
    assert!(said(&app).starts_with("Couldn't ask") && said(&app).contains("models.invalid"), "{}", said(&app));
}
