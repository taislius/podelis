use super::*;

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
    for i in 0..crate::app::runs::KEEP + 2 {
        run_sql(&mut app, &format!("select {i}")).await;
    }
    assert_eq!(app.tab.runs.len(), crate::app::runs::KEEP + 1);
    assert_eq!(app.tab.runs[0].number, 1, "the pinned one stays");
    app.focus = Focus::Results;
    for _ in 0..crate::app::runs::KEEP {
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
    let rows = crate::app::runs::KEEP_ROWS / 2 + 1;
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

#[tokio::test]
async fn rows_open_in_the_app_for_their_format() {
    use crate::app::pipes::OpenFormat;
    use crate::config::settings::Viewer;
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
async fn run_outcomes_are_for_the_results_pane() {
    let (mut app, _dir) = connected_app(ConnectionMode::Normal).await;
    run_sql(&mut app, "select 1").await;
    assert!(app.notes.status_in_results && app.notes.status.contains("1 row"), "{}", app.notes.status);
    run_sql(&mut app, "select * from nope").await;
    assert!(app.notes.status_in_results && app.notes.status_is_error, "{}", app.notes.status);
    // Other messages stay in the status bar.
    app.set_status_for_tests("Copied");
    assert!(!app.notes.status_in_results);
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
    assert!(app.notes.status.contains("more load as you scroll"), "{}", app.notes.status);
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
    assert!(!app.run.executing && said_error(&app));
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
    assert!(app.notes.status.starts_with("Exported all 25000 rows to"), "{}", app.notes.status);
    let text = std::fs::read_to_string(&exports()[0]).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!((lines.len(), lines[0], lines[1], lines[25000]), (25001, "n", "1", "25000"));
    // The grid kept what it had; nothing more streams.
    assert_eq!(app.tab.grid.results.as_ref().unwrap().rows.len(), 10);
    assert!(app.run.stream.is_none());

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
    assert!(app.notes.status.contains("3 result sets"), "{}", app.notes.status);

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

#[tokio::test]
async fn copying_names_rows_as_csv_and_where_messages_show() {
    let (mut app, _dir) = connected_app(ConnectionMode::Normal).await;
    run_sql(&mut app, "create table pets (id integer primary key, name text)").await;
    finish_catalog(&mut app).await;
    run_sql(&mut app, "insert into pets (name) values ('Rex'), ('Tom')").await;
    run_sql(&mut app, "select * from pets order by id").await;
    // The navigator: a table as written in SQL, with its schema.
    app.focus = Focus::Schema;
    let panel = app.connection.schema.as_mut().unwrap();
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
    assert_eq!(app.notes.results_note.as_ref().map(|(m, _)| m.as_str()), Some("Copied row (tab-separated)"));
    assert_eq!(
        (app.notes.status.as_str(), app.notes.log.last().map(|m| m.text.as_str())),
        ("", Some("Copied row (tab-separated)"))
    );
    // A run shows how long it's been going.
    assert_eq!(app.running_label(), None);
    app.run.executing = true;
    app.run.since = Instant::now().checked_sub(Duration::from_secs(75));
    assert_eq!(app.running_label().as_deref(), Some("running… 1:15"));
    app.run.since = Instant::now().checked_sub(Duration::from_secs(3));
    assert_eq!(app.running_label().as_deref(), Some("running… 3 s"));
    app.run.executing = false;
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
    assert!(app.notes.status.contains("updated 1 row"), "{}", app.notes.status);

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
async fn large_results_stream_in_as_you_scroll() {
    let (mut app, _dir) = connected_app(ConnectionMode::Normal).await;
    app.settings.row_limit = 10;
    let rows = "WITH RECURSIVE s(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM s WHERE n < 125) SELECT n FROM s";
    run_sql(&mut app, rows).await;
    assert_eq!(app.tab.grid.results.as_ref().unwrap().rows.len(), 10);
    assert!(app.run.stream.is_some(), "more rows to come");

    // Moving near the end fetches the next page.
    app.focus = Focus::Results;
    app.handle_key(key(KeyCode::End));
    assert!(app.run.fetching_more);
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
    assert!(app.run.stream.is_none());

    // A new query while a stream is open closes it and runs normally.
    run_sql(&mut app, rows).await;
    assert!(app.run.stream.is_some());
    run_sql(&mut app, "select 42").await;
    assert_eq!(app.tab.grid.results.as_ref().unwrap().rows, vec![vec![Some("42".to_string())]]);
    assert!(app.run.stream.is_none());

    // Scripts and writes don't stream.
    run_sql(&mut app, &format!("select 1; {rows}")).await;
    assert!(app.run.stream.is_none() && app.tab.grid.results.as_ref().unwrap().truncated());
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
    assert!(app.notes.status.starts_with("Added 1 row · 4 rows"), "{}", app.notes.status);
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
    assert_eq!(confirm.message, "Delete 2 rows from pets?");
    app.handle_key(key(KeyCode::Char('y')));
    finish(&mut app).await;
    let names: Vec<_> = app.tab.grid.results.as_ref().unwrap().rows.iter().map(|r| r[1].clone().unwrap()).collect();
    assert_eq!(names, ["Tom", "Žąsis"]);
    assert!(app.tab.grid.marked.is_empty());
}

#[test]
fn snippets_complete_by_name_and_insert() {
    let (mut app, dir) = test_app();
    app.screen = Screen::Query;
    app.library = crate::config::library::Library::load_from(Some(dir.path().join("q.json")));
    app.library.add("snippets/recent", "select *\nfrom orders\nwhere placed_at > now() - interval '7 days'").unwrap();
    app.library.add("reports/churn", "select 1").unwrap();
    app.tab.editor.set_text("rec");
    app.handle_key(key(KeyCode::Tab));
    let c = app.completion.as_ref().expect("snippet offered");
    let snippet = c.items.iter().position(|i| i.kind == crate::sql::complete::CandKind::Snippet).unwrap();
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
fn a_tabular_plan_takes_the_results_keys() {
    let (mut app, _dir) = test_app();
    app.connection.form.engine = Engine::MySql;
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
    assert!(app.cell_edit.is_none() && app.notes.status.contains("not the plan"));
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
    app.connection.form.engine = Engine::MySql;
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
