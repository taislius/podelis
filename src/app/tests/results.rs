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

#[cfg(feature = "sqlite")]
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
    for i in 0..crate::app::query_screen::results::runs::KEEP + 2 {
        run_sql(&mut app, &format!("select {i}")).await;
    }
    assert_eq!(app.tab.runs.len(), crate::app::query_screen::results::runs::KEEP + 1);
    assert_eq!(app.tab.runs[0].number, 1, "the pinned one stays");
    app.focus = Focus::Results;
    for _ in 0..crate::app::query_screen::results::runs::KEEP {
        app.handle_key(key(KeyCode::Char(',')));
    }
    assert_eq!(shown(&app).as_deref(), Some("one"));
}

#[cfg(feature = "sqlite")]
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
    let rows = crate::app::query_screen::results::runs::KEEP_ROWS / 2 + 1;
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

#[cfg(feature = "sqlite")]
#[tokio::test]
async fn rows_open_in_the_app_for_their_format() {
    use crate::config::settings::Viewer;
    let (mut app, _dir) = connected_app(ConnectionMode::Normal).await;
    run_sql(&mut app, "select 1 as n, 'a' as s").await;
    // (Which program opens a format, by default: `Settings::viewer_for`'s test.)
    app.settings.viewers.insert("tsv".into(), Viewer::Full { command: "less -S {}".into(), terminal: true });
    app.settings.viewers.insert("csv".into(), Viewer::Command("libreoffice --calc".into()));
    for (format, given) in [("json", "open"), ("sql", "xdg-open"), ("txt", "open")] {
        app.settings.viewers.insert(format.into(), Viewer::Command(given.into()));
    }
    assert_eq!(app.viewer_for(Format::Csv), Viewer::Command("libreoffice --calc".into()));
    app.open_viewer_menu();
    let keys: Vec<String> = app.popups.menu.as_ref().unwrap().items.iter().map(|i| i.keys.clone()).collect();
    assert_eq!(keys, ["libreoffice", "here: less", "its app", "its app", "its app", "its app"]);
    // One in the terminal: the main loop runs it, on a file of the user's alone.
    app.open_rows_as(Format::Tsv);
    let external = app.take_external().expect("run in the terminal");
    assert!(external.command.starts_with("less -S '") && external.command.ends_with(".tsv'"), "{}", external.command);
    assert_eq!(std::fs::read_to_string(&external.file).unwrap(), "n\ts\n1\ta\n");
    use std::os::unix::fs::PermissionsExt as _;
    let mode = |p: &std::path::Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!((mode(&external.file), mode(external.file.parent().unwrap())), (0o600, 0o700));
}

#[cfg(feature = "sqlite")]
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

#[cfg(feature = "sqlite")]
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
    assert_eq!(app.popups.cell_view.as_ref().unwrap().text, "{\n  \"k\": 3\n}");
    app.handle_key(key(KeyCode::Esc));
    assert!(app.popups.cell_view.is_none());
    app.handle_key(key(KeyCode::Esc));
    assert!(app.tab.grid.filter.is_empty() && app.tab.grid.view.len() == 5);

    // Load-more refuses scripts that write.
    run_sql(&mut app, "create table t (n int); insert into t values (1),(2),(3); select n from t").await;
    app.focus = Focus::Results;
    app.handle_key(key(KeyCode::Char('M')));
    assert!(!app.tab.run.executing && said_error(&app));
}

/// Parquet: the rows shown, and every row of a streamed result, page by
/// page, its column typed (SQLite's `n` an integer).
#[cfg(feature = "sqlite")]
#[tokio::test]
async fn export_parquet_shown_and_all_rows() {
    use parquet::file::reader::{FileReader, SerializedFileReader};
    let (mut app, dir) = connected_app(ConnectionMode::Normal).await;
    app.settings.row_limit = 10;
    app.settings.export_dir = Some(dir.path().to_string_lossy().into_owned());
    let parquet_files = || -> Vec<std::path::PathBuf> {
        std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|e| e == "parquet"))
            .collect()
    };
    let rows_in = |path: &std::path::Path| {
        let reader = SerializedFileReader::new(std::fs::File::open(path).unwrap()).unwrap();
        reader.metadata().file_metadata().num_rows()
    };
    run_sql(&mut app, "WITH RECURSIVE s(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM s WHERE n < 2500) SELECT n FROM s")
        .await;
    app.focus = Focus::Results;
    app.run_command(crate::app::commands::Command::Rows { to: Dest::ExportShown, format: Format::Parquet });
    // Written off the UI thread: done when its word comes.
    while app.exports.task.is_some() {
        let event = app.poll_db_event().await.unwrap();
        app.handle_db_event(event);
    }
    assert!(said(&app).contains("10 rows loaded"), "{}", said(&app));
    let shown = parquet_files();
    assert_eq!(rows_in(&shown[0]), 10);
    std::fs::remove_file(&shown[0]).unwrap();

    app.export_all(Format::Parquet);
    loop {
        let event = app.poll_db_event().await.unwrap();
        app.handle_db_event(event);
        if app.exports.task.is_none() {
            break;
        }
    }
    assert!(app.notes.status.starts_with("Exported all 2500 rows to"), "{}", app.notes.status);
    assert_eq!(rows_in(&parquet_files()[0]), 2500);
}

/// An export streaming 25000 rows, ready to start, with these limits.
#[cfg(feature = "sqlite")]
async fn big_export(min_free: u64, ask_every: u64, max: u64) -> (App, tempfile::TempDir) {
    let (mut app, dir) = connected_app(ConnectionMode::Normal).await;
    app.settings.row_limit = 10;
    app.settings.export_dir = Some(dir.path().to_string_lossy().into_owned());
    app.exports.limits_for_tests = Some(crate::transfer::limits::ExportLimits { min_free, ask_every, max });
    run_sql(
        &mut app,
        "WITH RECURSIVE s(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM s WHERE n < 25000) SELECT n FROM s",
    )
    .await;
    app.focus = Focus::Results;
    (app, dir)
}

#[cfg(feature = "sqlite")]
fn export_files(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let entries = std::fs::read_dir(dir).unwrap().filter_map(|e| e.ok().map(|e| e.path()));
    entries.filter(|p| p.file_name().is_some_and(|n| n.to_string_lossy().starts_with("podelis-export"))).collect()
}

/// Handles events until the unfinished file is gone (it's the last thing a
/// stopped export does).
#[cfg(feature = "sqlite")]
async fn until_removed(app: &mut App, dir: &std::path::Path) {
    tokio::time::timeout(Duration::from_secs(30), async {
        while !export_files(dir).is_empty() {
            let event = app.poll_db_event().await.unwrap();
            app.handle_db_event(event);
        }
    })
    .await
    .expect("the file removed");
}

#[cfg(feature = "sqlite")]
#[tokio::test]
async fn an_export_asks_at_its_size_and_no_stops_it_and_removes_its_file() {
    let (mut app, dir) = big_export(0, 1, 0).await;
    app.export_all(Format::Csv);
    while app.popups.confirm.is_none() {
        let event = app.poll_db_event().await.unwrap();
        app.handle_db_event(event);
    }
    let confirm = app.popups.confirm.as_ref().unwrap();
    assert!(matches!(confirm.action, ConfirmAction::KeepExporting));
    assert!(confirm.message.contains("has reached"), "{}", confirm.message);
    assert!(app.exports.task.is_some(), "it waits");
    // It came by itself: keys being typed don't answer, nor at once a y.
    for stray in [KeyCode::Char('s'), KeyCode::Enter, KeyCode::Down, KeyCode::Char('y')] {
        app.handle_key(key(stray));
        assert!(app.popups.confirm.is_some() && app.exports.task.is_some(), "{stray:?}");
    }
    app.exports.asked = None;
    for stray in [KeyCode::Char('s'), KeyCode::Enter, KeyCode::Down] {
        app.handle_key(key(stray));
        assert!(app.popups.confirm.is_some() && app.exports.task.is_some(), "{stray:?}, later");
    }
    answer(&mut app, 'n');
    assert!(app.exports.task.is_none() && app.popups.confirm.is_none());
    assert!(said(&app).contains("stopped"), "{}", said(&app));
    until_removed(&mut app, dir.path()).await;
}

#[cfg(feature = "sqlite")]
#[tokio::test]
async fn an_export_goes_on_when_told_to_and_says_its_size() {
    let (mut app, dir) = big_export(0, 1, 0).await;
    app.export_all(Format::Csv);
    let mut asked = 0;
    let mut sized = false;
    while app.exports.task.is_some() {
        let event = app.poll_db_event().await.unwrap();
        app.handle_db_event(event);
        sized = sized || app.notes.status.contains(" kB") || said(&app).contains(" kB");
        if app.popups.confirm.is_some() {
            asked += 1;
            answer(&mut app, 'y');
        }
    }
    assert!(asked >= 1);
    assert!(sized, "the progress says how big it is");
    assert!(said(&app).contains("Exported all 25000 rows"), "{}", said(&app));
    let text = std::fs::read_to_string(&export_files(dir.path())[0]).unwrap();
    assert_eq!(text.lines().count(), 25001);
}

#[cfg(feature = "sqlite")]
#[tokio::test]
async fn an_export_stops_at_the_largest_file_without_asking() {
    let (mut app, dir) = big_export(0, 0, 1).await;
    app.export_all(Format::Csv);
    while app.exports.task.is_some() {
        let event = app.poll_db_event().await.unwrap();
        app.handle_db_event(event);
        assert!(app.popups.confirm.is_none());
    }
    assert!(said_error(&app) && said(&app).contains("largest allowed"), "{}", said(&app));
    assert!(export_files(dir.path()).is_empty(), "nothing left behind");
}

#[cfg(feature = "sqlite")]
#[tokio::test]
async fn parquet_stops_at_its_size_too_counting_its_scratch_file() {
    let (mut app, dir) = big_export(0, 0, 1).await;
    app.export_all(Format::Parquet);
    while app.exports.task.is_some() {
        let event = app.poll_db_event().await.unwrap();
        app.handle_db_event(event);
    }
    assert!(said(&app).contains("largest allowed"), "{}", said(&app));
    until_removed(&mut app, dir.path()).await;
}

#[cfg(feature = "sqlite")]
#[tokio::test]
async fn no_export_starts_without_the_room_to_keep_free() {
    let (mut app, dir) = big_export(u64::MAX, 0, 0).await;
    for run in [
        crate::app::commands::Command::Rows { to: Dest::ExportAll, format: Format::Csv },
        crate::app::commands::Command::Rows { to: Dest::ExportAll, format: Format::Parquet },
    ] {
        app.run_command(run);
        assert!(app.exports.task.is_none(), "{run:?}");
        assert!(said_error(&app) && said(&app).contains("kept free"), "{}", said(&app));
        assert!(export_files(dir.path()).is_empty(), "{run:?}");
    }
    // The clipboard takes no room on the disk.
    app.run_command(crate::app::commands::Command::Rows { to: Dest::Copy, format: Format::Csv });
    assert!(!said_error(&app), "{}", said(&app));
}

/// Free space can't always be read: a question says the check isn't made.
#[cfg(feature = "sqlite")]
#[tokio::test]
async fn a_question_says_when_the_free_space_isnt_known() {
    let (mut app, _dir) = big_export(5, 1, 0).await;
    app.exports.task = Some(crate::app::query_screen::results::export::ExportRun::for_tests());
    app.export_ask(10, 2000, None);
    let details = app.popups.confirm.as_ref().expect("the question").details.join(" / ");
    assert!(details.contains("can't be read"), "{details}");
    assert!(!details.contains("It stops if the disk"), "it can't promise that: {details}");
    app.popups.confirm = None;
    app.export_ask(10, 2000, Some(50_000_000_000));
    let details = app.popups.confirm.as_ref().expect("the question").details.join(" / ");
    assert!(details.contains("46.6 GB free") && !details.contains("can't be read"), "{details}");
}

#[cfg(feature = "sqlite")]
#[tokio::test]
async fn off_nothing_is_asked_or_stopped() {
    let (mut app, dir) = big_export(0, 0, 0).await;
    app.export_all(Format::Csv);
    while app.exports.task.is_some() {
        let event = app.poll_db_event().await.unwrap();
        app.handle_db_event(event);
        assert!(app.popups.confirm.is_none());
    }
    assert!(said(&app).contains("Exported all 25000 rows"), "{}", said(&app));
    assert_eq!(export_files(dir.path()).len(), 1);
}

/// A question about an export's size, answered after a moment.
#[cfg(feature = "sqlite")]
fn answer(app: &mut App, letter: char) {
    app.exports.asked = None;
    app.handle_key(key(KeyCode::Char(letter)));
}

/// The plan's answer to a request of the rows now shown.
#[cfg(feature = "sqlite")]
fn estimated(app: &mut App, rows: Option<f64>) {
    let run = app.tab.run.id;
    app.exports.estimating = Some(crate::app::query_screen::results::export::EstimateRequest { id: 7, run });
    app.export_estimated(7, Format::Csv, rows, 100.0);
}

const COUNT_25000: &str =
    "WITH RECURSIVE s(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM s WHERE n < 25000) SELECT n FROM s";

#[cfg(feature = "sqlite")]
#[tokio::test]
async fn an_export_the_plan_says_is_big_asks_before_it_starts() {
    // Asks at 1 kB: this export reaches that, but not the size agreed to.
    let (mut app, dir) = big_export(0, 1000, 0).await;
    // Big: it asks, saying about how big and what's free; no: nothing starts.
    estimated(&mut app, Some(50_000_000.0));
    let confirm = app.popups.confirm.as_ref().expect("the question");
    assert!(confirm.message.contains("about 4.7 GB") && confirm.message.contains("by the plan"), "{}", confirm.message);
    assert!(confirm.details.iter().any(|d| d.contains("free")), "{:?}", confirm.details);
    answer(&mut app, 'n');
    assert!(app.exports.task.is_none() && export_files(dir.path()).is_empty());

    // Yes: it starts, and doesn't ask again before the size agreed to.
    estimated(&mut app, Some(50_000_000.0));
    answer(&mut app, 'y');
    assert!(app.exports.task.is_some());
    while app.exports.task.is_some() {
        let event = app.poll_db_event().await.unwrap();
        app.handle_db_event(event);
        assert!(app.popups.confirm.is_none());
    }
    assert!(said(&app).contains("Exported all"), "{}", said(&app));
    std::fs::remove_file(&export_files(dir.path())[0]).unwrap();

    // Small, or the plan doesn't say: it just starts.
    for rows in [Some(1.0), None] {
        run_sql(&mut app, COUNT_25000).await;
        estimated(&mut app, rows);
        assert!(app.popups.confirm.is_none(), "{rows:?}");
        assert!(app.exports.task.is_some(), "{rows:?}");
        app.stop_export();
        until_removed(&mut app, dir.path()).await;
    }
}

#[cfg(feature = "sqlite")]
#[tokio::test]
async fn an_estimates_answer_only_counts_for_what_it_was_asked_about() {
    use crate::app::query_screen::results::export::EstimateRequest;
    let (mut app, dir) = big_export(0, 1, 0).await;
    let this_run = app.tab.run.id;
    let nothing_happened = |app: &App| {
        assert!(app.exports.task.is_none() && app.popups.confirm.is_none(), "{}", said(app));
        assert!(export_files(dir.path()).is_empty());
    };
    // An older request's answer, or one stopped with Esc, starts nothing.
    app.exports.estimating = Some(EstimateRequest { id: 8, run: this_run });
    app.export_estimated(7, Format::Csv, Some(1.0), 1.0);
    nothing_happened(&app);
    assert!(app.exports.estimating.is_some(), "still waiting for its own");
    app.handle_key(key(KeyCode::Esc));
    assert!(app.exports.estimating.is_none());
    assert!(said(&app).contains("not started") && !said(&app).contains("file"), "{}", said(&app));
    app.export_estimated(8, Format::Csv, Some(1.0), 1.0);
    nothing_happened(&app);

    // Another run's rows by now (another query, or another tab): not those.
    app.exports.estimating = Some(EstimateRequest { id: 9, run: this_run + 1 });
    app.export_estimated(9, Format::Csv, Some(1.0), 1.0);
    nothing_happened(&app);
    assert!(app.notes.status_is_error && app.notes.status.contains("aren't the ones shown"), "{}", app.notes.status);

    // Another question is open: it stays, and a y there can't start this.
    app.popups.confirm = Some(Confirm {
        message: "Run this write?".into(),
        details: vec![],
        danger: true,
        action: ConfirmAction::Quit,
        offer_transaction: false,
    });
    estimated(&mut app, Some(1e9));
    assert_eq!(app.popups.confirm.as_ref().map(|c| c.message.as_str()), Some("Run this write?"));
    assert!(app.notes.status_is_error && app.notes.status.contains("question"), "{}", app.notes.status);
    app.popups.confirm = None;

    // A page on its way would go to the grid, not the file.
    app.tab.run.fetching_more = true;
    app.start_export_all(Format::Csv, 0);
    nothing_happened(&app);
    assert!(app.notes.status_is_error, "{}", app.notes.status);
    app.tab.run.fetching_more = false;

    // The stream gone meanwhile: it says so.
    app.close_stream();
    app.notes.status.clear();
    app.start_export_all(Format::Csv, 0);
    nothing_happened(&app);
    assert!(app.notes.status.contains("aren't the ones shown"), "{}", app.notes.status);
}

/// All of it is written by the last page: it's kept, not asked about.
#[cfg(feature = "sqlite")]
#[tokio::test]
async fn the_last_page_is_neither_asked_about_nor_thrown_away() {
    // One page: everything's there at the first check.
    let (mut app, dir) = connected_app(ConnectionMode::Normal).await;
    app.settings.row_limit = 10;
    app.settings.export_dir = Some(dir.path().to_string_lossy().into_owned());
    run_sql(&mut app, "WITH RECURSIVE s(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM s WHERE n < 2500) SELECT n FROM s")
        .await;
    app.focus = Focus::Results;
    app.exports.limits_for_tests = Some(crate::transfer::limits::ExportLimits { min_free: 0, ask_every: 1, max: 1 });
    app.export_all(Format::Csv);
    while app.exports.task.is_some() {
        let event = app.poll_db_event().await.unwrap();
        app.handle_db_event(event);
        assert!(app.popups.confirm.is_none());
    }
    assert!(said(&app).contains("Exported all 2500 rows"), "{}", said(&app));
    assert_eq!(export_files(dir.path()).len(), 1);
}

#[cfg(feature = "sqlite")]
#[tokio::test]
async fn keep_going_doesnt_count_whats_written_twice() {
    const GB: u64 = 1_000_000_000;
    let (mut app, _dir) = big_export(5 * GB, GB, 0).await;
    app.exports.task = Some(crate::app::query_screen::results::export::ExportRun::for_tests());
    // 1 GB written, 500 MB to go before the floor.
    app.export_ask(10, GB, Some(5 * GB + 500_000_000));
    let details = app.popups.confirm.as_ref().expect("the question").details.join(" / ");
    assert!(!details.contains("won't fit"), "{details}");
}

/// Once rows were agreed to, the largest file (or the disk's floor) ends
/// the export, but what's written stays, named as a part.
#[cfg(feature = "sqlite")]
#[tokio::test]
async fn rows_agreed_to_stay_when_a_limit_ends_the_export() {
    // Asks after the first page (about 50 kB), stops after the second.
    let (mut app, dir) = big_export(0, 1, 80_000).await;
    app.export_all(Format::Csv);
    while app.exports.task.is_some() {
        let event = app.poll_db_event().await.unwrap();
        app.handle_db_event(event);
        if app.popups.confirm.is_some() {
            answer(&mut app, 'y');
        }
    }
    assert!(said_error(&app) && said(&app).contains("largest allowed"), "{}", said(&app));
    assert!(said(&app).contains("rows kept") && !said(&app).contains("removed"), "{}", said(&app));
    let files = export_files(dir.path());
    assert!(files.len() == 1 && files[0].to_string_lossy().contains(".partial.csv"), "{files:?}");
}

/// Every row of a result that's all loaded is a file of known size: it
/// only has to fit, as the rows shown.
#[cfg(feature = "sqlite")]
#[tokio::test]
async fn every_row_of_a_loaded_result_only_has_to_fit() {
    let (mut app, dir) = connected_app(ConnectionMode::Normal).await;
    app.settings.export_dir = Some(dir.path().to_string_lossy().into_owned());
    app.exports.limits_for_tests =
        Some(crate::transfer::limits::ExportLimits { min_free: u64::MAX, ask_every: 0, max: 0 });
    run_sql(&mut app, "SELECT 1 AS n UNION ALL SELECT 2").await;
    app.focus = Focus::Results;
    app.export_all(Format::Csv);
    assert!(!said_error(&app) && said(&app).contains("Exported"), "{}", said(&app));
    assert_eq!(export_files(dir.path()).len(), 1);
}

/// An export of every row reads on its session: no query runs there
/// meanwhile, and Ctrl+Q at its question still quits.
#[cfg(feature = "sqlite")]
#[tokio::test]
async fn no_query_runs_on_a_session_an_export_reads() {
    let (mut app, _dir) = big_export(0, 1, 0).await;
    app.export_all(Format::Csv);
    while app.popups.confirm.is_none() {
        let event = app.poll_db_event().await.unwrap();
        app.handle_db_event(event);
    }
    app.execute("select 1".to_string(), 0);
    assert!(!app.tab.run.executing, "it doesn't start");
    assert!(said_error(&app) && said(&app).contains("export of every row"), "{}", said(&app));
    // Ctrl+C (which copies) doesn't quit from a question that came by itself.
    app.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
    assert!(!app.should_quit && app.popups.confirm.is_some());
    app.handle_key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::CONTROL));
    assert!(app.should_quit);
}

/// Esc cancels a query running here before it stops an export, as the
/// query's message says.
#[cfg(feature = "sqlite")]
#[tokio::test]
async fn esc_cancels_the_query_here_before_the_export() {
    let (mut app, _dir) = big_export(0, 0, 0).await;
    app.exports.task = Some(crate::app::query_screen::results::export::ExportRun::for_tests());
    app.tab.run.executing = true;
    app.focus = Focus::Results;
    app.handle_key(key(KeyCode::Esc));
    assert!(app.exports.task.is_some(), "{}", said(&app));
    app.tab.run.executing = false;
    app.handle_key(key(KeyCode::Esc));
    assert!(app.exports.task.is_none());
}

/// Paused behind another question, the export says so once, not every
/// second over what's said since.
#[cfg(feature = "sqlite")]
#[tokio::test]
async fn a_paused_export_says_so_once() {
    let (mut app, _dir) = big_export(0, 1, 0).await;
    app.exports.task = Some(crate::app::query_screen::results::export::ExportRun::for_tests());
    app.popups.confirm = Some(Confirm {
        message: "Run this write?".into(),
        details: vec![],
        danger: true,
        action: ConfirmAction::Quit,
        offer_transaction: false,
    });
    app.export_ask(10, 2000, None);
    assert!(app.notes.status.contains("paused"), "{}", app.notes.status);
    app.notes.status = "An agent's word".into();
    app.offer_export_question();
    assert_eq!(app.notes.status, "An agent's word");
}

#[cfg(feature = "sqlite")]
#[tokio::test]
async fn a_shown_parquet_export_is_held_to_about_its_size() {
    let (mut app, dir) = big_export(0, 0, 1).await;
    app.run_command(crate::app::commands::Command::Rows { to: Dest::ExportShown, format: Format::Parquet });
    assert!(app.exports.task.is_none() && export_files(dir.path()).is_empty());
    assert!(said_error(&app) && said(&app).contains("largest allowed"), "{}", said(&app));
}

/// What a key started is said on in the status bar: not left in the footer.
#[cfg(feature = "sqlite")]
#[tokio::test]
async fn the_footers_note_goes_when_the_export_says_more() {
    let (mut app, _dir) = big_export(0, 0, 0).await;
    app.notes.editor_note = Some(t!("export.exporting").to_string());
    app.notes.results_note = Some((t!("export.estimating").to_string(), false));
    app.clear_export_note();
    assert!(app.notes.editor_note.is_none() && app.notes.results_note.is_none());
    app.notes.editor_note = Some("Something else".to_string());
    app.clear_export_note();
    assert!(app.notes.editor_note.is_some());
}

/// On a server, with the rows still coming: the plan is made beside them,
/// and says how many a whole table has; a filter's rows it doesn't say.
#[tokio::test]
async fn a_servers_plan_warns_before_a_big_export() {
    for engine in Engine::BUILT.into_iter().filter(|e| !e.is_file_based() && e.dialect().is_some()) {
        let Some((mut app, dir)) = live_app(engine, "podelis_big_export").await else { continue };
        app.settings.row_limit = 10;
        app.settings.export_dir = Some(dir.path().to_string_lossy().into_owned());
        app.exports.limits_for_tests =
            Some(crate::transfer::limits::ExportLimits { min_free: 0, ask_every: 1000, max: 0 });
        let values: Vec<String> = (1..=2000).map(|n| format!("({n}, 'row number {n}')")).collect();
        let db = app.connection.database.clone().unwrap();
        let analyze = crate::db::tests::test_sql(engine).analyze.replace("{table}", "big_export");
        for setup in [
            "drop table if exists big_export".to_string(),
            "create table big_export (id int primary key, note varchar(40))".to_string(),
            format!("insert into big_export values {}", values.join(",")),
            analyze,
        ] {
            db.run(&setup, 1).await.unwrap_or_else(|e| panic!("{engine:?}: {setup}: {e}"));
        }

        run_sql(&mut app, "select * from big_export").await;
        app.focus = Focus::Results;
        app.export_all(Format::Csv);
        assert!(app.exports.estimating.is_some(), "{engine:?}: {}", said(&app));
        assert!(app.exports.task.is_none(), "{engine:?}: nothing's written yet");
        while app.exports.estimating.is_some() {
            let event = app.poll_db_event().await.unwrap();
            app.handle_db_event(event);
        }
        let confirm = app.popups.confirm.as_ref().unwrap_or_else(|| panic!("{engine:?}: {}", said(&app)));
        assert!(confirm.message.contains("by the plan") && confirm.message.contains("2.0k"), "{}", confirm.message);
        assert!(matches!(confirm.action, ConfirmAction::ExportAll(Format::Csv, ..)));
        // Yes: every row, in full, the stream having waited.
        app.exports.asked = None;
        app.handle_key(key(KeyCode::Char('y')));
        while app.exports.task.is_some() {
            let event = app.poll_db_event().await.unwrap();
            app.handle_db_event(event);
            // Grown past what was said (the first rows are the shortest): asked again.
            if app.popups.confirm.is_some() {
                app.exports.asked = None;
                app.handle_key(key(KeyCode::Char('y')));
            }
        }
        assert!(said(&app).contains("Exported all 2000 rows"), "{engine:?}: {}", said(&app));
        db.run("drop table big_export", 1).await.unwrap();
    }
}

/// Left waiting at "keep going?", nothing reads the rows. PostgreSQL waits;
/// MySQL drops the query after `net_write_timeout` (here a second): what's
/// written is kept, named as what it is.
#[tokio::test]
async fn a_long_wait_at_keep_going_loses_no_rows_already_written() {
    for engine in Engine::BUILT.into_iter().filter(|e| !e.is_file_based() && e.dialect().is_some()) {
        let Some((mut app, dir)) = live_app(engine, "podelis_export_wait").await else { continue };
        app.settings.row_limit = 10;
        app.settings.export_dir = Some(dir.path().to_string_lossy().into_owned());
        app.exports.limits_for_tests =
            Some(crate::transfer::limits::ExportLimits { min_free: 0, ask_every: 1, max: 0 });
        let db = app.connection.database.clone().unwrap();
        db.run("drop table if exists export_wait", 1).await.unwrap();
        db.run("create table export_wait (id int primary key, note text)", 1).await.unwrap();
        // More than the sockets' buffers hold, so the server has to wait.
        let pad = "x".repeat(2000);
        for chunk in 0..30 {
            let values: Vec<String> = (0..1000).map(|n| format!("({}, '{pad}')", chunk * 1000 + n)).collect();
            db.run(&format!("insert into export_wait values {}", values.join(",")), 1).await.unwrap();
        }
        if engine == Engine::MySql {
            db.run("set session net_write_timeout = 1", 1).await.unwrap();
        }
        run_sql(&mut app, "select * from export_wait").await;
        app.focus = Focus::Results;
        app.start_export_all(Format::Csv, 0);
        let wait = Duration::from_secs(90);
        while app.popups.confirm.is_none() && app.exports.task.is_some() {
            let event = tokio::time::timeout(wait, app.poll_db_event()).await.expect("to the question").unwrap();
            app.handle_db_event(event);
        }
        let details = app.popups.confirm.as_ref().expect("the question").details.join(" / ");
        assert!(details.contains("net_write_timeout"), "{details}");
        tokio::time::sleep(Duration::from_secs(6)).await;
        while app.exports.task.is_some() {
            if app.popups.confirm.is_some() {
                app.exports.asked = None;
                app.handle_key(key(KeyCode::Char('y')));
            }
            let event = tokio::time::timeout(wait, app.poll_db_event()).await.expect("not left hanging").unwrap();
            app.handle_db_event(event);
        }
        let files: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.file_name().is_some_and(|n| n.to_string_lossy().starts_with("podelis-export")))
            .collect();
        assert_eq!(files.len(), 1, "{engine:?}: {files:?} / {}", said(&app));
        let name = files[0].file_name().unwrap().to_string_lossy().into_owned();
        let lines = std::fs::read_to_string(&files[0]).unwrap().lines().count();
        if engine == Engine::MySql {
            assert!(name.contains(".partial.csv"), "{name}");
            assert!(said_error(&app) && said(&app).contains("lost while this export waited"), "{}", said(&app));
            assert!(said(&app).contains("kept, incomplete"), "{}", said(&app));
            assert!((1_001..30_001).contains(&lines), "a page or more of rows, not all: {lines}");
        } else {
            assert!(!name.contains("partial"), "{name}");
            assert_eq!(lines, 30_001);
        }
        app.close_stream();
    }
}

/// A running export's question can't be lost to another: it waits, says so,
/// and asks again; stopped, its question goes.
#[cfg(feature = "sqlite")]
#[tokio::test]
async fn an_exports_question_survives_another_and_goes_with_it() {
    let (mut app, dir) = big_export(0, 1, 0).await;
    app.export_all(Format::Csv);
    while app.popups.confirm.is_none() {
        let event = app.poll_db_event().await.unwrap();
        app.handle_db_event(event);
    }
    let other = |message: &str| Confirm {
        message: message.into(),
        details: vec![],
        danger: false,
        action: ConfirmAction::Quit,
        offer_transaction: false,
    };
    // Something else takes the place of the question (a menu's Disconnect).
    app.popups.confirm = Some(other("Disconnect?"));
    app.tick_running();
    assert_eq!(app.popups.confirm.as_ref().map(|c| c.message.as_str()), Some("Disconnect?"), "that one stays");
    assert!(app.notes.status.contains("paused"), "{}", app.notes.status);
    assert!(app.exports.task.as_ref().is_some_and(|run| run.paused.is_some()));
    // Answered: the export's comes back, and it never went on meanwhile.
    app.popups.confirm = None;
    app.tick_running();
    assert!(matches!(app.popups.confirm.as_ref().map(|c| &c.action), Some(ConfirmAction::KeepExporting)));
    // Stopped by another way (a new run does it): nothing left to answer.
    app.stop_export();
    assert!(app.popups.confirm.is_none() && app.exports.task.is_none());
    app.tick_running();
    assert!(app.popups.confirm.is_none(), "not asked again");
    until_removed(&mut app, dir.path()).await;
}

/// Its question comes back as soon as the other is answered, with no tick
/// needed (nothing else may be keeping one going).
#[cfg(feature = "sqlite")]
#[tokio::test]
async fn an_exports_question_comes_back_when_the_other_is_answered() {
    let (mut app, dir) = big_export(0, 1, 0).await;
    app.export_all(Format::Csv);
    while app.popups.confirm.is_none() {
        let event = app.poll_db_event().await.unwrap();
        app.handle_db_event(event);
    }
    app.popups.confirm = Some(Confirm {
        message: "Close this tab?".into(),
        details: vec![],
        danger: false,
        action: ConfirmAction::CloseTab,
        offer_transaction: false,
    });
    assert!(app.wants_ticks(), "paused: the main loop keeps checking");
    app.handle_key(key(KeyCode::Char('n')));
    assert!(
        matches!(app.popups.confirm.as_ref().map(|c| &c.action), Some(ConfirmAction::KeepExporting)),
        "at once, without a tick"
    );
    app.stop_export();
    until_removed(&mut app, dir.path()).await;
}

/// The disk's floor is for exports of every row: a few rows shown are
/// written on a disk with less free, so long as they fit.
#[cfg(feature = "sqlite")]
#[tokio::test]
async fn a_few_rows_shown_are_written_below_the_floor() {
    let (mut app, dir) = big_export(u64::MAX - 1, 0, 0).await;
    app.run_command(crate::app::commands::Command::Rows { to: Dest::ExportShown, format: Format::Csv });
    assert!(!said_error(&app), "{}", said(&app));
    assert_eq!(export_files(dir.path()).len(), 1);
    app.run_command(crate::app::commands::Command::Rows { to: Dest::ExportAll, format: Format::Csv });
    assert!(said_error(&app) && said(&app).contains("kept free"), "every row still keeps it: {}", said(&app));
}

#[cfg(feature = "sqlite")]
#[tokio::test]
async fn a_size_question_is_about_the_rows_it_was_asked_of() {
    let (mut app, dir) = big_export(0, 1, 0).await;
    estimated(&mut app, Some(1e9));
    let details = app.popups.confirm.as_ref().expect("the question").details.join(" / ");
    assert!(details.contains("net_write_timeout"), "it can cost the query too: {details}");
    // Watch mode ran it again meanwhile.
    app.tab.run.id += 1;
    answer(&mut app, 'y');
    assert!(app.exports.task.is_none() && export_files(dir.path()).is_empty());
    assert!(app.notes.status_is_error || said_error(&app), "{}", said(&app));
}

#[cfg(feature = "sqlite")]
#[tokio::test]
async fn a_rows_size_is_measured_in_the_format_its_going_to() {
    let (app, _dir) = big_export(0, 0, 0).await;
    let csv = app.export_row_bytes(Format::Csv);
    let json = app.export_row_bytes(Format::Json);
    let sql = app.export_row_bytes(Format::Sql);
    assert!(csv > 0.0 && json > csv && sql > json, "{csv} {json} {sql}");
    assert!(app.export_row_bytes(Format::Parquet) > 0.0);
}

#[cfg(feature = "sqlite")]
#[tokio::test]
async fn a_shown_parquet_export_leaves_no_note_behind() {
    let (mut app, _dir) = big_export(0, 0, 0).await;
    app.run_command(crate::app::commands::Command::Rows { to: Dest::ExportShown, format: Format::Parquet });
    app.notes.results_note = Some((t!("export.exporting").to_string(), false));
    while app.exports.task.is_some() {
        let event = app.poll_db_event().await.unwrap();
        app.handle_db_event(event);
    }
    assert!(app.notes.results_note.is_none());
    assert!(said(&app).contains("Exported"), "{}", said(&app));
}

/// After an answer, what the server says later is what's said, and the file
/// agreed to stays; a connection killed during the wait is said as that.
#[tokio::test]
async fn what_ends_an_agreed_export_is_said_and_its_file_stays() {
    if !Engine::BUILT.contains(&Engine::Postgres) {
        return;
    }
    let wait = Duration::from_secs(60);
    let files = |dir: &std::path::Path| -> Vec<std::path::PathBuf> {
        let all = std::fs::read_dir(dir).unwrap().flatten().map(|e| e.path());
        all.filter(|p| p.file_name().is_some_and(|n| n.to_string_lossy().starts_with("podelis-export"))).collect()
    };
    for killed in [false, true] {
        let Some((mut app, dir)) = live_app(Engine::Postgres, "podelis_export_ended").await else { return };
        app.settings.row_limit = 10;
        app.settings.export_dir = Some(dir.path().to_string_lossy().into_owned());
        app.exports.limits_for_tests =
            Some(crate::transfer::limits::ExportLimits { min_free: 0, ask_every: 1, max: 0 });
        // Fails at row 25000, past the first answers; a page is 10000 rows.
        let sql = if killed {
            "select n, repeat('x', 2000) from generate_series(1, 200000) n"
        } else {
            "select 1 / (25000 - n) from generate_series(1, 200000) n"
        };
        run_sql(&mut app, sql).await;
        app.focus = Focus::Results;
        app.start_export_all(Format::Csv, 0);
        let mut pauses = 0;
        while app.exports.task.is_some() {
            if app.popups.confirm.is_some() {
                pauses += 1;
                if killed && pauses == 1 {
                    let db = app.connection.database.clone().unwrap();
                    db.run_aside(
                        "select pg_terminate_backend(pid) from pg_stat_activity \
                         where query like '%repeat(''x'', 2000) from generate_series%' and pid <> pg_backend_pid()",
                        1,
                    )
                    .await
                    .unwrap();
                }
                app.exports.asked = None;
                app.handle_key(key(KeyCode::Char('y')));
            }
            let event = tokio::time::timeout(wait, app.poll_db_event()).await.expect("not left hanging").unwrap();
            app.handle_db_event(event);
        }
        let found = files(dir.path());
        assert_eq!(found.len(), 1, "{found:?} / {}", said(&app));
        assert!(found[0].to_string_lossy().contains(".partial.csv"), "{found:?}");
        if killed {
            assert!(said(&app).contains("lost while this export waited"), "{}", said(&app));
        } else {
            assert!(said(&app).contains("division by zero"), "the server's own words: {}", said(&app));
            assert!(!said(&app).contains("waited"), "{}", said(&app));
        }
        assert!(said(&app).contains("kept, incomplete"), "{}", said(&app));
    }
}

/// A real one, for the hand: `cargo test real_gigabyte -- --ignored`, with
/// the room for it. Writes 1.3 GB and removes it.
#[cfg(feature = "sqlite")]
#[tokio::test]
#[ignore = "writes more than a gigabyte"]
async fn a_real_gigabyte_export_asks_and_finishes() {
    let (mut app, dir) = connected_app(ConnectionMode::Normal).await;
    app.settings.row_limit = 10;
    app.settings.export_dir = Some(dir.path().to_string_lossy().into_owned());
    // The settings as they ship.
    assert_eq!((app.settings.export_min_free_gb, app.settings.export_ask_gb), (5, 1));
    run_sql(
        &mut app,
        "WITH RECURSIVE s(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM s WHERE n < 1300000) \
         SELECT n, printf('%0990d', n) FROM s",
    )
    .await;
    app.focus = Focus::Results;
    app.export_all(Format::Csv);
    let (mut asked, mut size_at_question) = (0, String::new());
    while app.exports.task.is_some() {
        let event = app.poll_db_event().await.unwrap();
        app.handle_db_event(event);
        if let Some(confirm) = &app.popups.confirm {
            asked += 1;
            size_at_question.clone_from(&confirm.message);
            answer(&mut app, 'y');
        }
    }
    let path = &export_files(dir.path())[0];
    let bytes = std::fs::metadata(path).unwrap().len();
    assert!(said(&app).contains("Exported all 1300000 rows"), "{}", said(&app));
    assert_eq!(asked, 1, "{size_at_question}");
    assert!(size_at_question.contains("1.0 GB"), "{size_at_question}");
    assert!(bytes > 1_200_000_000, "{bytes}");
}

/// A result all here: "every row" is every row, whatever a filter shows, in
/// Parquet as in the text formats.
#[cfg(feature = "sqlite")]
#[tokio::test]
async fn export_every_row_as_parquet_ignores_a_filter() {
    use parquet::file::reader::{FileReader, SerializedFileReader};
    let (mut app, dir) = connected_app(ConnectionMode::Normal).await;
    app.settings.export_dir = Some(dir.path().to_string_lossy().into_owned());
    run_sql(&mut app, "SELECT 1 AS n UNION ALL SELECT 2 UNION ALL SELECT 3").await;
    app.focus = Focus::Results;
    while app.tab.run.stream.is_some() {
        let event = app.poll_db_event().await.unwrap();
        app.handle_db_event(event);
    }
    app.tab.grid.filter.insert_str("2");
    app.tab.grid.refresh_view();
    assert_eq!(app.tab.grid.view.len(), 1);
    app.run_command(crate::app::commands::Command::Rows { to: Dest::ExportAll, format: Format::Parquet });
    while app.exports.task.is_some() {
        let event = app.poll_db_event().await.unwrap();
        app.handle_db_event(event);
    }
    let file = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .find(|p| p.extension().is_some_and(|e| e == "parquet"))
        .expect("the file");
    let reader = SerializedFileReader::new(std::fs::File::open(&file).unwrap()).unwrap();
    assert_eq!(reader.metadata().file_metadata().num_rows(), 3);
}

/// "Export every row" writes every row of a streamed result, not just those
/// loaded, without loading them into the grid; Esc stops it.
#[cfg(feature = "sqlite")]
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
    assert!(said(&app).contains("10 rows loaded") && said(&app).contains("Export every row"), "{}", said(&app));
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

    app.export_all(Format::Csv);
    loop {
        let event = app.poll_db_event().await.unwrap();
        app.handle_db_event(event);
        if app.exports.task.is_none() {
            break;
        }
    }
    assert!(app.notes.status.starts_with("Exported all 25000 rows to"), "{}", app.notes.status);
    let text = std::fs::read_to_string(&exports()[0]).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!((lines.len(), lines[0], lines[1], lines[25000]), (25001, "n", "1", "25000"));
    // The grid kept what it had; nothing more streams.
    assert_eq!(app.tab.grid.results.as_ref().unwrap().rows.len(), 10);
    assert!(app.tab.run.stream.is_none());

    // Stopped part-way: the unfinished file goes.
    std::fs::remove_file(&exports()[0]).unwrap();
    run_sql(&mut app, many).await;
    app.focus = Focus::Results;
    app.export_all(Format::Json);
    assert!(app.exports.task.is_some());
    app.handle_key(key(KeyCode::Esc));
    assert!(app.exports.task.is_none() && said(&app).contains("Export stopped"), "{}", said(&app));
    // The export cleans up, then says it's done (and that's ignored).
    loop {
        let event = app.poll_db_event().await.unwrap();
        let done = matches!(event.kind, DbEventKind::Background(Background::ExportDone(..)));
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

#[cfg(feature = "sqlite")]
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

#[cfg(sql_engine)] // SQL's own: not with MongoDB alone
#[tokio::test]
async fn the_syntax_check_runs_off_the_ui_thread_and_drops_stale_results() {
    let (mut app, _dir) = test_app();
    app.tab.editor.set_text("delete from t");
    app.editing.lint_due = Some(Instant::now());
    app.tick();
    // Nothing yet: parsing and linting happen in the background.
    assert_eq!(app.tab.syntax_hint, None);
    let event = app.poll_db_event().await.unwrap();
    app.handle_db_event(event);
    assert!(app.tab.syntax_hint.as_ref().is_some_and(|h| h.source == HintSource::Lint("no-where")));

    // The text changed while the check ran: its result is dropped.
    app.tab.syntax_hint = None;
    app.editing.lint_due = Some(Instant::now());
    app.tick();
    app.tab.editor.set_text("delete from t where id = 1");
    let event = app.poll_db_event().await.unwrap();
    app.handle_db_event(event);
    assert_eq!(app.tab.syntax_hint, None);
}

#[cfg(feature = "sqlite")]
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
    app.run_command(crate::app::commands::Command::Rows { to: Dest::Copy, format: Format::Csv });
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
    app.tab.run.executing = true;
    app.tab.run.since = Instant::now().checked_sub(Duration::from_secs(75));
    assert_eq!(app.running_label().as_deref(), Some("running… 1:15"));
    app.tab.run.since = Instant::now().checked_sub(Duration::from_secs(3));
    assert_eq!(app.running_label().as_deref(), Some("running… 3 s"));
    app.tab.run.executing = false;
}

#[cfg(feature = "sqlite")]
#[tokio::test]
async fn edit_a_cell_by_primary_key() {
    let (mut app, _dir) = Box::pin(edit_cell(ConnectionMode::Normal, "select * from people order by id", 2)).await;
    let edit = app.cell_edit.as_ref().expect("prompt opens");
    assert_eq!((edit.column.as_str(), edit.text.text()), ("note", ""));
    type_text(&mut app, r"it's \ fine?");
    assert!(app.popups.palette.is_none(), "? is typed into the value, not the keys");
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

#[cfg(feature = "sqlite")]
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

#[cfg(feature = "sqlite")]
#[tokio::test]
async fn m_while_a_page_comes_follows_it_and_finds_new_rows_when_sorted() {
    let (mut app, _dir) = connected_app(ConnectionMode::Normal).await;
    app.settings.row_limit = 10;
    let rows = "WITH RECURSIVE s(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM s WHERE n < 500) SELECT n FROM s";
    run_sql(&mut app, rows).await;
    app.focus = Focus::Results;
    // A page is coming (from scrolling); M then waits for it, then fetches.
    app.handle_key(key(KeyCode::End));
    assert!(app.tab.run.fetching_more);
    app.handle_key(key(KeyCode::Char('M')));
    let note = app.notes.results_note.as_ref().map(|(n, _)| n.as_str());
    assert!(note.is_some_and(|n| n.contains("on its way")), "{note:?}");
    let event = app.poll_db_event().await.unwrap();
    app.handle_db_event(event);
    assert_eq!(app.tab.grid.results.as_ref().unwrap().rows.len(), 20);
    assert!(app.tab.run.fetching_more, "M's rows asked for");
    let event = app.poll_db_event().await.unwrap();
    app.handle_db_event(event);
    assert_eq!(app.tab.grid.results.as_ref().unwrap().rows.len(), 120);
    assert_eq!(app.tab.grid.table_state.selected(), Some(20));
    // Sorted biggest first: the new rows go to the top, and are selected.
    app.handle_key(key(KeyCode::Char('s')));
    app.handle_key(key(KeyCode::Char('s')));
    app.handle_key(key(KeyCode::Char('M')));
    let event = app.poll_db_event().await.unwrap();
    app.handle_db_event(event);
    assert_eq!(app.tab.grid.results.as_ref().unwrap().rows.len(), 220);
    assert_eq!(app.tab.grid.table_state.selected(), Some(0), "the first new row in the sorted view");
}

#[cfg(feature = "sqlite")]
#[tokio::test]
async fn m_waiting_on_the_last_page_shows_its_rows() {
    let (mut app, _dir) = connected_app(ConnectionMode::Normal).await;
    app.settings.row_limit = 10;
    let rows = "WITH RECURSIVE s(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM s WHERE n < 15) SELECT n FROM s";
    run_sql(&mut app, rows).await;
    app.focus = Focus::Results;
    app.handle_key(key(KeyCode::Up));
    app.handle_key(key(KeyCode::End));
    assert!(app.tab.run.fetching_more);
    app.handle_key(key(KeyCode::Home));
    app.handle_key(key(KeyCode::Char('M')));
    let event = app.poll_db_event().await.unwrap();
    app.handle_db_event(event);
    // The stream ended with that page: nothing more to fetch, and M's rows
    // are those, selected.
    assert_eq!(app.tab.grid.results.as_ref().unwrap().rows.len(), 15);
    assert!(app.tab.run.stream.is_none() && !app.tab.run.fetching_more);
    assert_eq!(app.tab.grid.table_state.selected(), Some(10));
}

#[cfg(feature = "sqlite")]
#[tokio::test]
async fn large_results_stream_in_as_you_scroll() {
    let (mut app, _dir) = connected_app(ConnectionMode::Normal).await;
    app.settings.row_limit = 10;
    let rows = "WITH RECURSIVE s(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM s WHERE n < 125) SELECT n FROM s";
    run_sql(&mut app, rows).await;
    assert_eq!(app.tab.grid.results.as_ref().unwrap().rows.len(), 10);
    assert!(app.tab.run.stream.is_some(), "more rows to come");

    // Moving near the end fetches the next page.
    app.focus = Focus::Results;
    app.handle_key(key(KeyCode::End));
    assert!(app.tab.run.fetching_more);
    let event = app.poll_db_event().await.unwrap();
    app.handle_db_event(event);
    assert_eq!(app.tab.grid.results.as_ref().unwrap().rows.len(), 20);

    // M fetches ten pages at once; the stream ends when the rows do.
    app.handle_key(key(KeyCode::Char('M')));
    let event = app.poll_db_event().await.unwrap();
    app.handle_db_event(event);
    assert_eq!(app.tab.grid.results.as_ref().unwrap().rows.len(), 120);
    assert_eq!(app.tab.grid.table_state.selected(), Some(20), "the first of the new rows, in sight");
    app.handle_key(key(KeyCode::Char('M')));
    let event = app.poll_db_event().await.unwrap();
    app.handle_db_event(event);
    assert_eq!(app.tab.grid.results.as_ref().unwrap().rows.len(), 125);
    assert!(app.tab.run.stream.is_none());

    // A new query while a stream is open closes it and runs normally.
    run_sql(&mut app, rows).await;
    assert!(app.tab.run.stream.is_some());
    run_sql(&mut app, "select 42").await;
    assert_eq!(app.tab.grid.results.as_ref().unwrap().rows, vec![vec![Some("42".to_string())]]);
    assert!(app.tab.run.stream.is_none());

    // Scripts and writes don't stream.
    run_sql(&mut app, &format!("select 1; {rows}")).await;
    assert!(app.tab.run.stream.is_none() && app.tab.grid.results.as_ref().unwrap().truncated());
}

#[cfg(feature = "sqlite")]
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
#[cfg(feature = "sqlite")]
#[tokio::test]
async fn chart_a_result_column() {
    let (mut app, _dir) = connected_app(ConnectionMode::Normal).await;
    run_sql(&mut app, "select 'a' as day, 3 as n union all select 'b', 6").await;
    app.focus = Focus::Results;
    app.handle_key(key(KeyCode::Char('b')));
    assert!(said_error(&app) && said(&app).contains("day has no numbers"), "{}", said(&app));
    app.handle_key(key(KeyCode::Right));
    app.handle_key(key(KeyCode::Char('b')));
    let view = app.popups.cell_view.as_ref().unwrap();
    assert_eq!(view.title, "CHART · n");
    assert!(view.text.contains(&format!("b │{} 6", "█".repeat(40))), "{}", view.text);
}

#[cfg(feature = "sqlite")]
#[tokio::test]
async fn record_view_shows_a_row_vertically() {
    let (mut app, _dir) = connected_app(ConnectionMode::Normal).await;
    run_sql(&mut app, "select 1 as id, 'Ana' as long_name, null as note union all select 2, 'Ben', 'a\nb'").await;
    app.focus = Focus::Results;
    app.handle_key(key(KeyCode::Char('x')));
    let view = app.popups.cell_view.as_ref().unwrap();
    assert_eq!(view.title, "ROW 1 of 2");
    assert_eq!(view.text, "id         1\nlong_name  Ana\nnote       NULL\n");
    app.handle_key(key(KeyCode::Right));
    let view = app.popups.cell_view.as_ref().unwrap();
    assert_eq!(view.title, "ROW 2 of 2");
    assert!(view.text.ends_with("note       a\n           b\n"), "{:?}", view.text);
    app.handle_key(key(KeyCode::Esc));
    assert!(app.popups.cell_view.is_none());
}

#[cfg(feature = "sqlite")]
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
    let form = app.popups.row_form.as_ref().expect("add-row form");
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
    // An edit from the palette goes ahead, and to the rows.
    app.focus = Focus::Editor;
    app.run_command(crate::app::commands::Command::EditCell);
    assert!(app.cell_edit.is_some() && app.focus == Focus::Results);
    app.handle_key(key(KeyCode::Esc));
    // A filter hiding the marked rows doesn't unmark them: they can still go.
    app.tab.grid.filter.insert_str("nothing like this");
    app.tab.grid.refresh_view();
    assert!(app.tab.grid.view.is_empty());
    assert_eq!(app.can(crate::app::commands::Command::DeleteRows), crate::app::commands::Can::Yes);
    app.tab.grid.filter.clear();
    app.tab.grid.refresh_view();
    // `C` opens the formats; `C` again is Markdown.
    app.handle_key(key(KeyCode::Char('C')));
    assert!(app.take_clipboard().is_none() && app.popups.menu.is_some());
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
    let confirm = app.popups.confirm.as_ref().expect("asks before deleting");
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
    let c = app.popups.completion.as_ref().expect("snippet offered");
    let snippet = c.items.iter().position(|i| i.kind == crate::sql::complete::CandKind::Snippet).unwrap();
    assert_eq!(c.items[snippet].label, "recent");
    app.popups.completion.as_mut().unwrap().selected = snippet;
    app.handle_key(key(KeyCode::Enter));
    assert!(app.tab.editor.text().starts_with("select *\nfrom orders"));

    // Ctrl+E inserts any saved query at the cursor.
    app.tab.editor.set_text("-- here: ");
    app.handle_key(ctrl('e'));
    let view = app.popups.library_view.as_ref().unwrap();
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
