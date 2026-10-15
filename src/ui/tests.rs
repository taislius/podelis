//! Rendering tests: each screen is drawn into an in-memory terminal and the
//! text compared against a reviewed snapshot (`src/ui/snapshots/`). After an
//! intended UI change, review and accept with `cargo insta review` (or rerun
//! with `INSTA_UPDATE=always` and check the diff).

use std::time::Duration;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use insta::assert_snapshot;
use ratatui::Terminal;
use ratatui::backend::TestBackend;

use super::draw;
use crate::analyze::{HintSource, SyntaxHint};
use crate::app::{
    App, CellView, Completion, Confirm, ConfirmAction, ConnectField, Focus, ResultTab, SchemaPanel, SchemaTable, Screen,
};
use crate::complete::{CandKind, Candidate};
use crate::db::{ColumnInfo, ConnectionMode, Engine, QueryOutcome, TableInfo, TxnState};
use crate::store::ConnectionStore;

fn render(app: &mut App, width: u16, height: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|f| draw(f, app)).unwrap();
    terminal.backend().to_string()
}

/// An app with projects "shop" (two connections) and "analytics".
fn app() -> (App, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let (mut store, _) = ConnectionStore::load_from(dir.path().join("c.toml"));
    let shop = store.add_project("shop").unwrap();
    store.add_project("analytics").unwrap();
    let mut app = App::new(store, None, None);
    app.open_project_by_name("shop").unwrap();
    for (name, engine, host) in [("local", Engine::Postgres, "localhost"), ("prod", Engine::MySql, "db.internal")] {
        app.connect_form.name = name.into();
        app.connect_form.engine = engine;
        app.connect_form.host = host.into();
        app.connect_form.port = engine.default_port().into();
        let mut profile = app.connect_form.to_profile_for_tests();
        profile.database = "shop".into();
        app.store.upsert(&shop, None, profile).unwrap();
    }
    (app, dir)
}

/// The query screen with a small result set.
fn query_app() -> (App, tempfile::TempDir) {
    let (mut app, dir) = app();
    app.screen = Screen::Query;
    app.pretend_connected();
    app.schema_visible = true;
    app.connection_summary = "[shop] PostgreSQL as me@localhost:5432/shop".into();
    app.tab.editor.set_text("select id, email, note\nfrom users\nwhere id < 4;");
    let outcome = QueryOutcome {
        columns: vec!["id".into(), "email".into(), "note".into()],
        column_types: vec!["INT4".into(), "TEXT".into(), "TEXT".into()],
        rows: vec![
            vec![Some("1".into()), Some("ana@example.com".into()), None],
            vec![Some("2".into()), Some("ben@example.com".into()), Some("line one\nline two".into())],
            vec![Some("3".into()), Some("cleo@example.com".into()), Some("{\"vip\": true}".into())],
        ],
        total_rows: 3,
        rows_affected: None,
        statements: 1,
        elapsed: Duration::from_millis(4),
        reconnected: false,
        ..QueryOutcome::default()
    };
    app.show_results_for_tests(outcome);
    let col = |name: &str, ty: &str, pk: bool| ColumnInfo {
        name: name.into(),
        data_type: ty.into(),
        nullable: !pk,
        primary_key: pk,
    };
    let table = |name: &str, rows: Option<i64>, is_view: bool, expanded: bool, columns: Vec<ColumnInfo>| SchemaTable {
        info: TableInfo { schema: "public".into(), name: name.into(), is_view, rows, bytes: None, system: false },
        expanded,
        columns,
        details: None,
        details_loading: false,
    };
    app.schema = Some(SchemaPanel {
        tables: vec![
            table("orders", Some(942_000), false, false, vec![]),
            table(
                "users",
                Some(24_800),
                false,
                true,
                vec![col("id", "integer", true), col("email", "text", false), col("note", "text", false)],
            ),
            table("active_users", None, true, false, vec![]),
        ],
        selected: 2,
        ..SchemaPanel::default()
    });
    (app, dir)
}

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

    app.connect_form.engine = Engine::Sqlite;
    app.connect_form.host = "./shop.db".into();
    assert_snapshot!("connect_form_sqlite", render(&mut app, 80, 22));
}

#[test]
fn query_screen_with_results_sort_and_filter() {
    let (mut app, _dir) = query_app();
    app.focus = Focus::Results;
    for c in ['s', 's', '/', 'c', 'l', 'e', 'o'] {
        app.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
    }
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    app.tab.syntax_hint = Some(SyntaxHint {
        message: "relation \"users\" does not exist".into(),
        offset: Some(28),
        len: Some(5),
        source: HintSource::Server,
        nth: None,
    });
    assert_snapshot!(render(&mut app, 90, 20));
}

#[test]
fn plan_findings_point_at_their_steps() {
    let (mut app, _dir) = query_app();
    let note = |text: &str, line| crate::db::PlanNote { text: text.into(), line: Some(line) };
    app.tab.plan = Some(crate::db::Plan {
        text: "Sort  (cost=900.00..950.00 rows=10)\n  Sort Key: total\n  ->  Seq Scan on orders  (cost=0.00..800.00 rows=10)\n        Filter: (total > 10)".into(),
        notes: vec![
            note("Costliest step: Seq Scan on orders, 84% of the estimated cost", 2),
            note("Seq Scan on orders with a filter over a big table: an index may help", 2),
        ],
    });
    app.tab.result_tab = crate::app::ResultTab::Plan;
    assert_snapshot!(render(&mut app, 100, 20));
}

#[test]
fn a_wide_plan_scrolls_across() {
    let (mut app, _dir) = query_app();
    let wide = format!("-> Table scan on orders  (cost=1.0 rows=3) {}END", "x".repeat(150));
    app.tab.plan = Some(crate::db::Plan { text: wide, notes: Vec::new() });
    app.tab.result_tab = crate::app::ResultTab::Plan;
    app.focus = Focus::Results;
    let screen = render(&mut app, 100, 20);
    assert!(!screen.contains("END") && screen.contains("cols 1–"), "{screen}");
    for _ in 0..40 {
        app.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
    }
    let screen = render(&mut app, 100, 20);
    assert!(screen.contains("END") && screen.contains("◀ cols"), "stops at the widest line:\n{screen}");
    app.handle_key(KeyEvent::new(KeyCode::Char('0'), KeyModifiers::NONE));
    assert_eq!(app.tab.tab_hscroll, 0);
}

#[test]
fn results_one_column_per_line() {
    let (mut app, _dir) = query_app();
    app.focus = Focus::Results;
    let press = |app: &mut App, code| app.handle_key(KeyEvent::new(code, KeyModifiers::NONE));
    press(&mut app, KeyCode::Char('X'));
    assert!(app.tab.grid.vertical);
    let screen = render(&mut app, 100, 30);
    assert!(screen.contains("1. row") && screen.contains("email: ana@example.com"), "{screen}");
    // ↓ goes field by field, then on to the next row.
    for _ in 0..3 {
        press(&mut app, KeyCode::Down);
    }
    assert_eq!((app.tab.grid.table_state.selected(), app.tab.grid.selected_col), (Some(1), 0));
    press(&mut app, KeyCode::Up);
    assert_eq!((app.tab.grid.table_state.selected(), app.tab.grid.selected_col), (Some(0), 2));
    press(&mut app, KeyCode::PageDown);
    assert_eq!(app.tab.grid.table_state.selected(), Some(1));
    assert_snapshot!(render(&mut app, 70, 24));
    press(&mut app, KeyCode::Char('X'));
    assert!(!app.tab.grid.vertical);
}

#[test]
fn the_row_view_steps_to_the_next_row_with_arrows() {
    let (mut app, _dir) = query_app();
    app.focus = Focus::Results;
    let press = |app: &mut App, code| app.handle_key(KeyEvent::new(code, KeyModifiers::NONE));
    press(&mut app, KeyCode::Char('x'));
    let screen = render(&mut app, 100, 30);
    assert_eq!(app.cell_view.as_ref().unwrap().title, "ROW 1 of 3");
    // Its keys are on a line of its own inside it, not the status bar.
    let lines: Vec<&str> = screen.lines().collect();
    let keys = lines.iter().position(|l| l.contains("↑↓ scroll, next row")).unwrap_or_else(|| panic!("{screen}"));
    assert!(lines[keys].contains("Esc close") && lines[keys + 1].contains('└'), "{screen}");
    assert!(!lines.last().unwrap().contains("next row"), "{screen}");
    // The row fits: ↓ is the next row, ↑ the one before.
    press(&mut app, KeyCode::Down);
    let screen = render(&mut app, 100, 30);
    assert!(screen.contains("ROW 2 of 3") && screen.contains("ben@example.com"), "{screen}");
    press(&mut app, KeyCode::Right);
    let _ = render(&mut app, 100, 30);
    press(&mut app, KeyCode::Down);
    assert_eq!(app.cell_view.as_ref().unwrap().title, "ROW 3 of 3", "stays on the last row");
    press(&mut app, KeyCode::Up);
    assert_eq!(app.cell_view.as_ref().unwrap().title, "ROW 2 of 3");
    // A row taller than the popup scrolls first.
    let _ = render(&mut app, 100, 9);
    assert!(app.cell_view.as_ref().unwrap().max_scroll > 0);
    press(&mut app, KeyCode::Down);
    assert_eq!(app.cell_view.as_ref().unwrap().title, "ROW 2 of 3");
    assert_eq!(app.cell_view.as_ref().unwrap().scroll, 1);
}

#[test]
fn a_tabular_plan_is_a_grid_with_its_notes() {
    let (mut app, _dir) = query_app();
    let cells = |r: &[&str]| r.iter().map(|c| Some((*c).to_string())).collect::<Vec<_>>();
    app.show_plan_for_tests(QueryOutcome {
        columns: ["id", "select_type", "table", "type", "rows", "Extra"].map(String::from).to_vec(),
        column_types: vec![
            "BIGINT".into(),
            "TEXT".into(),
            "TEXT".into(),
            "TEXT".into(),
            "BIGINT".into(),
            "TEXT".into(),
        ],
        rows: vec![
            cells(&["1", "PRIMARY", "orders", "ALL", "50000", "Using where"]),
            cells(&["2", "DEPENDENT SUBQUERY", "users", "eq_ref", "1", ""]),
        ],
        total_rows: 2,
        statements: 1,
        ..QueryOutcome::default()
    });
    assert_snapshot!(render(&mut app, 100, 30));
}

#[test]
fn the_filter_shows_only_with_its_grid() {
    let (mut app, _dir) = query_app();
    app.focus = Focus::Results;
    let press = |app: &mut App, code| app.handle_key(KeyEvent::new(code, KeyModifiers::NONE));
    for code in [KeyCode::Char('/'), KeyCode::Char('a'), KeyCode::Char('n'), KeyCode::Enter] {
        press(&mut app, code);
    }
    assert!(render(&mut app, 100, 30).contains("filter: an"));
    press(&mut app, KeyCode::Char('2'));
    assert!(!render(&mut app, 100, 30).contains("filter:"), "not over MESSAGES");
    press(&mut app, KeyCode::Char('1'));
    assert!(render(&mut app, 100, 30).contains("filter: an"), "back with the rows");
}

#[test]
fn a_plan_says_what_it_is_for_and_how_old_it_is() {
    let (mut app, _dir) = query_app();
    app.tab.plan = Some(crate::db::Plan { text: "Seq Scan on users".into(), notes: Vec::new() });
    let mut source = crate::app::PlanSource::new("from users\nwhere id < 4".into(), false, 1);
    source.at = chrono::Local::now() - chrono::TimeDelta::minutes(3);
    app.tab.plan_source = Some(source);
    app.tab.result_tab = crate::app::ResultTab::Plan;
    let screen = render(&mut app, 100, 30);
    assert!(
        screen.contains(" EXPLAIN  from users where id < 4  lines 2–3") && screen.contains("3 min ago"),
        "{screen}"
    );
    assert!(!screen.contains("edited since"), "{screen}");
    // Lines added above it: it's found where it is now, and a click there
    // puts the cursor on it.
    app.tab.editor.set_text("-- one\n-- two\nselect id, email, note\nfrom users\nwhere id < 4;");
    assert!(render(&mut app, 100, 30).contains("lines 4–5"));
    let at = app.hits.plan_source;
    app.handle_mouse(crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
        column: at.x + 2,
        row: at.y,
        modifiers: KeyModifiers::NONE,
    });
    assert_eq!((app.focus, app.tab.editor.cursor_row, app.tab.editor.cursor_col), (Focus::Editor, 3, 0));
    assert_eq!(app.tab.editor.selected_text().as_deref(), Some("from users\nwhere id < 4"), "the statement selected");
    // The statement is no longer in the editor.
    app.tab.editor.set_text("select 1");
    app.tab.result_tab = crate::app::ResultTab::Plan;
    let screen = render(&mut app, 100, 30);
    assert!(screen.contains("was lines 2–3 · edited since"), "{screen}");
}

#[test]
fn a_hint_says_its_line_and_a_click_goes_there() {
    let (mut app, _dir) = query_app();
    // "where" on line 3, while the cursor is on line 1.
    app.tab.syntax_hint = Some(SyntaxHint {
        message: "Unknown column".into(),
        offset: Some(34),
        len: Some(5),
        source: HintSource::Server,
        nth: None,
    });
    app.tab.editor.set_cursor((0, 0));
    let screen = render(&mut app, 100, 30);
    assert!(screen.contains("SERVER line 3  Unknown column"), "{screen}");
    let at = app.hits.syntax_hint;
    app.focus = Focus::Results;
    app.handle_mouse(crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
        column: at.x + 3,
        row: at.y,
        modifiers: KeyModifiers::NONE,
    });
    assert_eq!((app.focus, app.tab.editor.cursor_row, app.tab.editor.cursor_col), (Focus::Editor, 2, 0));
}

#[test]
fn stepping_through_definitions_keeps_the_popup_size() {
    let (mut app, _dir) = query_app();
    let size = |screen: &str| {
        let lines: Vec<&str> = screen.lines().collect();
        let top = lines.iter().position(|l| l.contains("┌ TABLE")).unwrap_or_else(|| panic!("{screen}"));
        let bottom = top + lines[top..].iter().position(|l| l.contains("Esc close")).unwrap();
        (bottom - top, lines[top].matches('─').count())
    };
    app.cell_view = Some(crate::app::CellView::definition("TABLE a".into(), "id int".into()));
    let small = size(&render(&mut app, 120, 40));
    let long = (0..20).map(|i| format!("column_with_a_long_name_{i}  varchar(255) not null")).collect::<Vec<_>>();
    app.cell_view = Some(crate::app::CellView::definition("TABLE b".into(), long.join("\n")));
    assert_eq!(size(&render(&mut app, 120, 40)), small);
}

#[test]
fn a_definition_sits_beside_the_navigator() {
    let (mut app, _dir) = query_app();
    app.cell_view = Some(crate::app::CellView::definition("TABLE users".into(), "id int".into()));
    let screen = render(&mut app, 120, 40);
    let nav = app.hits.schema.unwrap();
    let top = screen.lines().find(|l| l.contains("┌ TABLE users")).unwrap_or_else(|| panic!("{screen}"));
    // Rendered lines are quoted: the `"` comes first.
    let x = top[..top.find("┌ TABLE").unwrap()].chars().count() as u16 - 1;
    assert_eq!(x, nav.right(), "just right of the navigator:\n{screen}");
}

#[test]
fn hidden_columns_show_in_the_footer() {
    let (mut app, _dir) = query_app();
    app.schema_visible = false;
    let columns: Vec<String> = (0..30).map(|i| format!("column_{i}")).collect();
    let row: Vec<Option<String>> = (0..30).map(|i| Some(format!("value {i}"))).collect();
    let outcome = QueryOutcome {
        column_types: vec!["TEXT".into(); 30],
        columns,
        rows: vec![row],
        total_rows: 1,
        statements: 1,
        ..QueryOutcome::default()
    };
    app.show_results_for_tests(outcome);
    app.focus = Focus::Results;
    let screen = render(&mut app, 100, 20);
    assert!(screen.contains("cols 1–") && screen.contains("of 30 ▶"), "{screen}");
    app.handle_key(KeyEvent::new(KeyCode::Char('$'), KeyModifiers::NONE));
    let screen = render(&mut app, 100, 20);
    assert!(screen.contains("◀ cols") && screen.contains("–30 of 30") && screen.contains("column_29"), "{screen}");
}

#[test]
fn the_active_tabs_line_joins_the_panes_border() {
    let (mut app, _dir) = query_app();
    app.schema_visible = false;
    app.handle_key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL));
    let screen = render(&mut app, 70, 16);
    let lines: Vec<&str> = screen.lines().collect();
    // Second tab active: the separator before it is thick, joined above.
    let tabs = lines.iter().position(|l| l.contains('┃')).unwrap_or_else(|| panic!("{screen}"));
    let x = lines[tabs].chars().position(|c| c == '┃').unwrap();
    assert!(x > 1, "the second tab's line, not the pane's border: {screen}");
    assert_eq!(lines[tabs - 1].chars().nth(x), Some('┰'), "{screen}");
    // And the line on its right.
    let right = lines[tabs].chars().skip(x + 1).position(|c| c == '┃' || c == '│').unwrap() + x + 1;
    assert_eq!(lines[tabs].chars().nth(right), Some('┃'), "{screen}");
    assert_eq!(lines[tabs - 1].chars().nth(right), Some('┰'), "{screen}");
}

#[test]
fn a_scripts_result_sets() {
    let (mut app, _dir) = query_app();
    let set = |statement: usize, column: &str, rows: usize| QueryOutcome {
        columns: vec![column.into()],
        column_types: vec!["INT4".into()],
        rows: (0..rows).map(|i| vec![Some(i.to_string())]).collect(),
        total_rows: rows as u64,
        statements: 3,
        statement,
        ..QueryOutcome::default()
    };
    let outcome = QueryOutcome { earlier: vec![set(1, "a", 1), set(2, "b", 2)], ..set(3, "c", 3) };
    app.show_results_for_tests(outcome);
    app.focus = Focus::Results;
    app.handle_key(KeyEvent::new(KeyCode::Char('>'), KeyModifiers::NONE));
    assert_snapshot!(render(&mut app, 90, 16));
}

#[test]
fn system_tables_in_their_own_section() {
    let (mut app, _dir) = query_app();
    let panel = app.schema.as_mut().unwrap();
    for (schema, name, is_view) in [("pg_catalog", "pg_class", false), ("information_schema", "tables", true)] {
        let info =
            TableInfo { schema: schema.into(), name: name.into(), is_view, rows: None, bytes: None, system: true };
        panel.tables.push(SchemaTable {
            info,
            expanded: false,
            columns: vec![],
            details: None,
            details_loading: false,
        });
    }
    app.focus = Focus::Schema;
    assert_snapshot!(render(&mut app, 90, 16));
}

#[test]
fn lint_warning_names_its_rule() {
    let (mut app, _dir) = query_app();
    app.tab.editor.set_text("select * from users\nwhere note = null;");
    app.tab.syntax_hint =
        crate::lint::lints(&crate::parse::parse(&app.tab.editor.text(), app.engine()), "").into_iter().next();
    assert_snapshot!(render(&mut app, 160, 14));
}

/// The agent activity window: the sessions, then one session's calls; and
/// the status bar's note about what agents did since it was last open.
#[test]
fn agent_activity_window() {
    use crate::agents::{Entry, append, log_path};
    let (mut app, dir) = query_app();
    app.set_data_dir(dir.path().to_path_buf());
    let project = app.current_project.clone().unwrap();
    let mut shared = app.store.project(&project).unwrap().connections[0].clone();
    shared.agents = crate::db::AgentAccess::Read;
    let name = shared.name.clone();
    app.store.upsert(&project, Some(&name), shared).unwrap();
    let log = log_path(dir.path(), &project);
    let entry = |session: &str, at: &str, conn: &str, sql: &str| Entry {
        session: session.into(),
        at: format!("2026-10-13T{at}+03:00"),
        agent: "claude-code".into(),
        agent_version: Some("2.0.31".into()),
        dir: Some("/src/shop".into()),
        connection: conn.into(),
        tool: "query".into(),
        sql: sql.into(),
        ms: 3,
        ..Entry::default()
    };
    append(&log, Entry { rows: Some(42), ..entry("4f1c9a2e", "09:15:02", "shop", "select count(*) from orders") })
        .unwrap();
    append(
        &log,
        Entry {
            name: Some("billing".into()),
            rows: Some(7),
            ..entry("77ab01cd", "09:40:10", "shop", "select * from invoices")
        },
    )
    .unwrap();
    append(
        &log,
        Entry {
            refused: Some("“delete from users” is destructive".into()),
            ..entry("4f1c9a2e", "09:16:30", "prod", "delete from users")
        },
    )
    .unwrap();
    append(
        &log,
        Entry {
            dry_run: true,
            changed: Some(2),
            ..entry("4f1c9a2e", "09:18:44", "prod", "update users set note = 'x' where id < 3")
        },
    )
    .unwrap();
    app.update_agent_alerts();
    let screen = render(&mut app, 120, 14);
    assert!(screen.contains("agents: 1 refused · 1 dry run"), "{screen}");

    app.open_agents();
    assert_snapshot!("agent_sessions", render(&mut app, 120, 14));
    app.handle_agents_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    app.handle_agents_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert_snapshot!("agent_session_calls", render(&mut app, 120, 14));
}

#[test]
fn the_footer_says_which_statement_f6_runs() {
    let (mut app, _dir) = query_app();
    app.tab.editor.set_text("select 1;\nselect ';' as semi;\n\nselect 3");
    app.tab.editor.set_cursor((1, 3));
    assert_snapshot!(render(&mut app, 100, 24));
    // A blank line goes with the statement before it, as F6 picks it.
    app.tab.editor.set_cursor((2, 0));
    assert!(render(&mut app, 100, 24).contains("stmt 2/3"));
    // F6 runs a selection instead, and one statement needs no count.
    app.tab.editor.select_all();
    assert!(!render(&mut app, 100, 24).contains("stmt "));
    app.tab.editor.set_text("select 1;");
    assert!(!render(&mut app, 100, 24).contains("stmt "));
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
fn completion_popup() {
    let (mut app, _dir) = query_app();
    app.tab.editor.set_text("select em");
    let cand = |label: &str, kind, detail: &str| Candidate {
        label: label.into(),
        insert: label.into(),
        kind,
        detail: detail.into(),
    };
    app.completion = Some(Completion {
        items: vec![
            cand("email", CandKind::Column, "text · users"),
            cand("employee_id", CandKind::Column, "integer · orders"),
            cand("employees", CandKind::Table, "table"),
        ],
        selected: 1,
        prefix_len: 2,
    });
    assert_snapshot!(render(&mut app, 80, 20));
}

#[test]
fn cell_view_popup() {
    let (mut app, _dir) = query_app();
    app.cell_view = Some(CellView::new("note".into(), "{\n  \"vip\": true\n}".into()));
    assert_snapshot!(render(&mut app, 70, 16));
}

/// Every screen and popup must draw without panicking at any size,
/// including absurdly small terminals.
#[test]
fn renders_at_any_size() {
    let sizes = [(1, 1), (2, 2), (10, 4), (20, 6), (40, 10), (120, 40)];
    for (w, h) in sizes {
        for screen in 0..4 {
            let (mut app, _dir) = query_app();
            app.screen = match screen {
                0 => Screen::Projects,
                1 => Screen::Connections,
                2 => Screen::Connect,
                _ => Screen::Query,
            };
            app.schema_visible = true;
            app.help_visible = screen == 1;
            app.cell_view = Some(CellView { scroll: 3, ..CellView::new("x".into(), "y\n".repeat(50)) });
            app.confirm = Some(Confirm {
                message: "Sure?".into(),
                details: vec!["a long detail line ".repeat(10)],
                danger: false,
                action: ConfirmAction::Quit,
                offer_transaction: false,
            });
            let _ = render(&mut app, w, h);
        }
    }
}

#[test]
fn large_results_render_only_the_visible_window() {
    let (mut app, _dir) = query_app();
    let rows: Vec<Vec<Option<String>>> =
        (1..=5000).map(|n| vec![Some(n.to_string()), Some(format!("user{n}@example.com")), None]).collect();
    let mut outcome = app.tab.grid.results.take().unwrap();
    outcome.total_rows = rows.len() as u64;
    outcome.rows = rows;
    app.show_results_for_tests(outcome);
    app.focus = Focus::Results;
    app.tab.grid.table_state.select(Some(3999));
    let screen = render(&mut app, 90, 24);
    assert!(screen.contains("user4000@example.com"), "{screen}");
    assert!(!screen.contains("user1@example.com"));
    // Moving back up scrolls the window back.
    app.tab.grid.table_state.select(Some(0));
    let screen = render(&mut app, 90, 24);
    assert!(screen.contains("user1@example.com") && !screen.contains("user4000@"));
}

#[test]
fn hidden_schema_pane_cannot_keep_focus() {
    let (mut app, _dir) = query_app();
    app.focus = Focus::Schema;
    let _ = render(&mut app, 60, 20); // too narrow for the navigator
    assert!(!app.schema_shown);
    assert_eq!(app.focus, Focus::Editor);
    app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
    app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
    assert_eq!(app.focus, Focus::Editor, "Tab skips the hidden pane");
}

#[test]
fn command_palette_and_find_bar() {
    let (mut app, _dir) = query_app();
    app.handle_key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL));
    for c in "id".chars() {
        app.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
    }
    app.handle_key(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::CONTROL));
    for c in "tab".chars() {
        app.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
    }
    assert_snapshot!(render(&mut app, 100, 28));
}

#[test]
fn mouse_clicks_follow_the_layout() {
    use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    let (mut app, _dir) = query_app();
    let _ = render(&mut app, 100, 30);
    let click = |app: &mut App, x: u16, y: u16| {
        for kind in [MouseEventKind::Down(MouseButton::Left), MouseEventKind::Up(MouseButton::Left)] {
            app.handle_mouse(MouseEvent { kind, column: x, row: y, modifiers: KeyModifiers::NONE });
        }
    };
    // A result row, then the second column of it.
    let rows = app.hits.result_rows;
    let (x0, _, col) = app.hits.result_cols[1];
    click(&mut app, x0, rows.y + 2);
    assert_eq!(
        (app.focus, app.tab.grid.table_state.selected(), app.tab.grid.selected_col),
        (Focus::Results, Some(2), col)
    );
    // The editor: line 2, column 3.
    let text = app.hits.editor_text;
    click(&mut app, text.x + 3, text.y + 1);
    assert_eq!((app.focus, app.tab.editor.cursor_row, app.tab.editor.cursor_col), (Focus::Editor, 1, 3));
    // Drag selects.
    let drag = MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: text.x,
        row: text.y,
        modifiers: KeyModifiers::NONE,
    };
    app.handle_mouse(drag);
    app.handle_mouse(MouseEvent { kind: MouseEventKind::Drag(MouseButton::Left), column: text.x + 6, ..drag });
    assert_eq!(app.tab.editor.selected_text().as_deref(), Some("select"));
    // The `+` opens a tab; clicking the first tab goes back.
    let (plus, _) = app.hits.new_tab.unwrap();
    let _ = render(&mut app, 100, 30);
    let y = app.hits.tabs_y;
    click(&mut app, plus, y);
    assert_eq!(app.tabs.len(), 2);
    let _ = render(&mut app, 100, 30);
    let (tab0, _, _) = app.hits.tabs[0];
    let y = app.hits.tabs_y;
    click(&mut app, tab0, y);
    assert_eq!(app.active_tab, 0);
    // The wheel over the schema moves its selection.
    let schema = app.hits.schema.unwrap();
    let before = app.schema.as_ref().unwrap().selected;
    app.handle_mouse(MouseEvent {
        kind: MouseEventKind::ScrollDown,
        column: schema.x + 2,
        row: schema.y + 4,
        modifiers: KeyModifiers::NONE,
    });
    assert_ne!(app.schema.as_ref().unwrap().selected, before);
    // Clicking the filter line starts typing in it.
    let filter = app.hits.schema_filter;
    click(&mut app, filter.x + 3, filter.y);
    assert_eq!(app.focus, Focus::Schema);
    assert!(app.schema.as_ref().unwrap().filter_editing);
    app.handle_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::NONE));
    assert_eq!(app.schema.as_ref().unwrap().filter.text(), "u");
}

#[test]
fn ctrl_arrows_resize_the_panes() {
    let (mut app, _dir) = query_app();
    let _ = render(&mut app, 120, 30);
    let (editor, schema) = (app.hits.editor.height, app.hits.schema.unwrap().width);
    let ctrl = |app: &mut App, code| app.handle_key(KeyEvent::new(code, KeyModifiers::CONTROL));
    ctrl(&mut app, KeyCode::Down);
    ctrl(&mut app, KeyCode::Down);
    let _ = render(&mut app, 120, 30);
    assert!(app.hits.editor.height > editor, "the editor grows");
    // In the editor Ctrl+→ moves by word; from the navigator it resizes.
    ctrl(&mut app, KeyCode::Right);
    let _ = render(&mut app, 120, 30);
    assert_eq!(app.hits.schema.unwrap().width, schema);
    app.focus = Focus::Schema;
    ctrl(&mut app, KeyCode::Right);
    let _ = render(&mut app, 120, 30);
    assert_eq!(app.hits.schema.unwrap().width, schema + 2);
    for _ in 0..40 {
        ctrl(&mut app, KeyCode::Left);
        let _ = render(&mut app, 120, 30);
    }
    assert_eq!(app.hits.schema.unwrap().width, 15, "down to a minimum");
    for _ in 0..40 {
        ctrl(&mut app, KeyCode::Up);
    }
    assert_eq!(app.editor_percent, 10);
}

#[test]
fn navigator_pages_reveals_expanded_columns_and_sorts() {
    let (mut app, _dir) = query_app();
    let col =
        |i: usize| ColumnInfo { name: format!("c{i}"), data_type: "int".into(), nullable: true, primary_key: false };
    let tables = (0..40)
        .map(|i| SchemaTable {
            info: TableInfo {
                schema: "public".into(),
                name: format!("t{i:02}"),
                is_view: false,
                rows: Some(i * 10),
                bytes: Some(1000 - i),
                system: false,
            },
            expanded: false,
            columns: (0..8).map(col).collect(),
            details: None,
            details_loading: false,
        })
        .collect();
    app.schema = Some(SchemaPanel { tables, selected: 1, ..SchemaPanel::default() });
    app.focus = Focus::Schema;
    let _ = render(&mut app, 120, 30);
    let page = app.schema.as_ref().unwrap().page;
    assert!(page > 5 && page < 40);
    let press = |app: &mut App, code| app.handle_key(KeyEvent::new(code, KeyModifiers::NONE));
    press(&mut app, KeyCode::PageDown);
    assert_eq!(app.schema.as_ref().unwrap().selected, 1 + page);
    press(&mut app, KeyCode::PageUp);
    assert_eq!(app.schema.as_ref().unwrap().selected, 1);
    // The last table on screen, expanded: its columns scroll into view.
    let _ = render(&mut app, 120, 30);
    for _ in 2..page {
        press(&mut app, KeyCode::Down);
    }
    let _ = render(&mut app, 120, 30);
    let selected = app.schema.as_ref().unwrap().selected;
    assert_eq!(app.hits.schema_offset, 0);
    press(&mut app, KeyCode::Enter);
    let screen = render(&mut app, 120, 30);
    let panel = app.schema.as_ref().unwrap();
    assert!(panel.scroll <= selected && selected + 8 < panel.scroll + panel.page, "columns shown:\n{screen}");
    assert!(screen.contains("c7"));
    // Ordered by rows (most first), then by size.
    press(&mut app, KeyCode::Char('o'));
    let nodes = app.schema.as_ref().unwrap().nodes();
    assert_eq!(nodes[1], crate::app::SchemaNode::Table(39));
    assert!(render(&mut app, 120, 30).contains("by rows"));
    press(&mut app, KeyCode::Char('o'));
    let nodes = app.schema.as_ref().unwrap().nodes();
    assert_eq!(nodes[1], crate::app::SchemaNode::Table(0));
    // The expanded table stays selected (and on screen), showing its size.
    let screen = render(&mut app, 120, 30);
    let t = app.schema.as_ref().unwrap().nodes()[app.schema.as_ref().unwrap().selected];
    let crate::app::SchemaNode::Table(t) = t else { panic!("{t:?}") };
    assert!(screen.contains("by size") && screen.contains(&format!("{} B", 1000 - t)), "{screen}");
}

#[test]
fn right_click_opens_a_menu_at_the_pointer() {
    use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    let (mut app, _dir) = query_app();
    let _ = render(&mut app, 100, 30);
    let rows = app.hits.result_rows;
    let click = |kind, x, y| MouseEvent { kind, column: x, row: y, modifiers: KeyModifiers::NONE };
    app.handle_mouse(click(MouseEventKind::Down(MouseButton::Right), rows.x + 10, rows.y + 1));
    assert_eq!((app.focus, app.tab.grid.table_state.selected()), (Focus::Results, Some(1)));
    assert_eq!(app.menu.as_ref().unwrap().title, "RESULTS");
    assert_snapshot!(render(&mut app, 100, 30));
    // Clicking an item does it: "Copy the row" (the 9th).
    let menu = app.hits.menu;
    app.handle_mouse(click(MouseEventKind::Down(MouseButton::Left), menu.x + 3, menu.y + 9));
    assert!(app.menu.is_none());
    assert_eq!(app.take_clipboard().as_deref(), Some("2\tben@example.com\tline one line two"));
}

#[test]
fn asking_in_plain_language_and_sharing_a_schema() {
    let (mut app, _dir) = query_app();
    app.connect_form.name = "prod".into();
    app.ask_prompt = Some(crate::app::LineInput::new("revenue per country last month"));
    let screen = render(&mut app, 110, 24);
    assert!(screen.contains("ASK ABOUT PROD"), "{screen}");
    assert!(screen.contains("? revenue per country last month"), "{screen}");
    assert!(screen.contains("it lands in the editor and doesn't run"), "{screen}");
    app.ask_prompt = None;
    app.confirm = Some(Confirm {
        message: "Send shop's table and column names to the model?".into(),
        details: vec!["project 'shop' shares its schemas only with models on this machine".into()],
        danger: false,
        action: ConfirmAction::ShareSchema {
            question: crate::app::ask::Question { text: "x".into(), tab: 0, connection: "prod".into() },
            project_id: None,
        },
        offer_transaction: false,
    });
    let screen = render(&mut app, 110, 24);
    assert!(screen.contains(" y  this once") && screen.contains(" a  always, for this project"), "{screen}");
}

#[test]
fn the_memory_window_marks_what_waits_for_the_user() {
    let (mut app, _dir) = query_app();
    let fact = |id: i64, subject: &str, value: &str, confirmed: bool, source: &str| serde_json::json!({ "id": id, "subject": subject, "relation": "means", "value": value, "confirmed": confirmed, "source": source });
    app.memory_view = Some(crate::app::memory_view::MemoryView {
        facts: vec![
            fact(1, "orders.status", "3 = cancelled", true, "user"),
            fact(2, "orders.total", "VAT included", false, "model:claude"),
        ],
        ..Default::default()
    });
    let screen = render(&mut app, 110, 24);
    assert!(screen.contains("MEMORY · SHOP"), "{screen}");
    assert!(screen.contains("orders.status means 3 = cancelled  · user"), "{screen}");
    assert!(screen.contains("orders.total means VAT included  · model:claude, not confirmed: c"), "{screen}");
    assert!(screen.contains("a add · c confirm · d forget · Esc close"), "{screen}");
}

#[test]
fn kept_results_and_the_statement_above_a_result() {
    use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    let (mut app, _dir) = query_app();
    let one = || QueryOutcome {
        columns: vec!["id".into()],
        column_types: vec!["INT4".into()],
        rows: vec![vec![Some("1".into())]],
        total_rows: 1,
        statements: 1,
        ..QueryOutcome::default()
    };
    app.tab.editor.set_text("select 1;\nselect id, email, note\nfrom users\nwhere id < 4;");
    app.open_run_for_tests(0..9);
    app.show_results_for_tests(one());
    app.open_run_for_tests(10..app.tab.editor.text().len());
    app.show_results_for_tests(one());
    app.tab.runs[1].source.as_mut().unwrap().at = chrono::Local::now();
    app.focus = Focus::Results;
    let screen = render(&mut app, 140, 22);
    assert!(screen.contains(" #1  #2  select id, email, note from users where id < 4;  lines 2–4"), "{screen}");
    // A click on #1 shows it; on the lines, its statement in the editor.
    let (x0, _, i) = app.hits.run_tabs[0];
    let click = |x, y| MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: x,
        row: y,
        modifiers: KeyModifiers::NONE,
    };
    app.handle_mouse(click(x0 + 1, app.hits.run_source.y));
    assert_eq!((i, app.tab.run_at), (0, 0));
    let screen = render(&mut app, 140, 22);
    assert!(screen.contains("select 1;  line 1"), "{screen}");
    let source = app.hits.run_source;
    app.handle_mouse(click(source.x + source.width - 30, source.y));
    assert_eq!((app.focus, app.tab.editor.selected_text().as_deref()), (Focus::Editor, Some("select 1;")));
    // A run that failed says so where its result would be.
    app.open_run_for_tests(10..app.tab.editor.text().len());
    app.tab.run_error = Some("relation \"users\" does not exist".into());
    let screen = render(&mut app, 140, 22);
    assert!(screen.contains("The query failed:") && screen.contains("relation \"users\" does not exist"), "{screen}");
}

#[test]
fn clicking_a_table_while_filtering_the_navigator_gives_it_the_keys() {
    use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    let (mut app, _dir) = query_app();
    app.focus = Focus::Schema;
    app.handle_key(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE));
    app.handle_key(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::NONE));
    let _ = render(&mut app, 100, 30);
    let list = app.hits.schema_list;
    let click = MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: list.x + 2,
        row: list.y + 1,
        modifiers: KeyModifiers::NONE,
    };
    app.handle_mouse(click);
    let panel = app.schema.as_ref().unwrap();
    assert!(!panel.filter_editing, "the click ends typing the filter");
    assert_eq!(panel.filter.text(), "o", "the filter stays");
    // `d` is for the clicked table now, not the filter.
    app.handle_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE));
    assert_eq!(app.schema.as_ref().unwrap().filter.text(), "o");
}

#[test]
fn zoom_a_pane_and_click_result_tabs() {
    use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    let (mut app, _dir) = query_app();
    app.focus = Focus::Results;
    app.handle_key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::NONE));
    assert!(app.zoomed);
    let screen = render(&mut app, 90, 16);
    assert!(screen.contains("ZOOMED") && !screen.contains("┌ SQL ─"), "only the results pane:\n{screen}");
    assert_snapshot!(screen);
    // Clicking the QUERY PLAN tab, then RESULT.
    let click = |x, y| MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: x,
        row: y,
        modifiers: KeyModifiers::NONE,
    };
    let (plan_x, _, _) = app.hits.result_tabs[2];
    app.handle_mouse(click(plan_x + 1, app.hits.result_tabs_y));
    assert_eq!(app.tab.result_tab, ResultTab::Plan);
    let _ = render(&mut app, 90, 16);
    let (rows_x, _, _) = app.hits.result_tabs[0];
    app.handle_mouse(click(rows_x + 1, app.hits.result_tabs_y));
    assert_eq!(app.tab.result_tab, ResultTab::Rows);
    // Esc returns to the normal layout; F12 zooms the editor.
    app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert!(!app.zoomed);
    app.focus = Focus::Editor;
    app.handle_key(KeyEvent::new(KeyCode::F(12), KeyModifiers::NONE));
    assert!(app.zoomed);
    assert!(!render(&mut app, 90, 16).contains("QUERY RESULT"));
    // Double-clicking a pane title toggles the zoom.
    app.handle_key(KeyEvent::new(KeyCode::F(12), KeyModifiers::NONE));
    let _ = render(&mut app, 90, 16);
    let title = app.hits.results;
    let press = click(title.x + 3, title.y);
    app.handle_mouse(press);
    app.handle_mouse(press);
    assert!(app.zoomed && app.focus == Focus::Results);
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
fn ctrl_k_menu_shows_letters_and_the_panes_keys() {
    let (mut app, _dir) = query_app();
    app.schema_visible = true;
    app.focus = Focus::Schema;
    app.status.clear();
    let screen = render(&mut app, 90, 30);
    assert!(screen.contains("^K  keys") && !screen.contains("REFRESH"), "minimal hints, no pane footer:\n{screen}");
    // `?` outside the editor opens the same menu as Ctrl+K.
    app.handle_key(KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE));
    let screen = render(&mut app, 90, 30);
    assert!(screen.contains("IN THE NAVIGATOR") && screen.contains("zoom pane"), "{screen}");
    // The editor's own keys only from the editor.
    assert!(
        !screen.contains("format SQL")
            && !screen.contains("explain analyze")
            && !screen.contains("rename tab")
            && !screen.contains("transaction mode"),
        "{screen}"
    );
    assert_snapshot!(screen);
    // A letter runs its action.
    app.handle_key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::NONE));
    assert!(app.zoomed && app.palette.is_none());
    // Anything else (or `/`) searches every command.
    app.handle_key(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::CONTROL));
    for c in "/export".chars() {
        app.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
    }
    let screen = render(&mut app, 90, 30);
    assert!(screen.contains("COMMANDS") && screen.contains("Export rows as CSV"), "{screen}");
    // From the editor, its own keys are there.
    app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    app.zoomed = false;
    app.focus = Focus::Editor;
    app.handle_key(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::CONTROL));
    if let Some(palette) = &mut app.palette {
        palette.menu = true;
    }
    let screen = render(&mut app, 90, 30);
    assert!(
        screen.contains("format SQL")
            && screen.contains("explain analyze")
            && screen.contains("rename tab")
            && screen.contains("transaction mode"),
        "{screen}"
    );
    // Commit and roll back only with a transaction open.
    assert!(!screen.contains("commit") && !screen.contains("roll back"), "{screen}");
    app.txn = TxnState::Open;
    let screen = render(&mut app, 90, 30);
    assert!(screen.contains("c commit") && screen.contains("r roll back"), "{screen}");
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
    app.settings.editor_mode = crate::settings::EditorMode::Vim;
    app.focus = Focus::Editor;
    assert!(render(&mut app, 120, 12).lines().last().unwrap().contains(" NORMAL "));
}

#[test]
fn run_buttons_sit_right_of_the_tabs_and_click() {
    use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    let (mut app, _dir) = query_app();
    app.focus = Focus::Results;
    app.status.clear();
    let screen = render(&mut app, 120, 20);
    assert!(screen.contains("▶ F5 Run") && screen.contains("F7 Explain"), "{screen}");
    let &(x, _, action) = app.hits.run_buttons.first().unwrap();
    assert_eq!(action, crate::keys::Action::Run);
    app.handle_mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: x + 1,
        row: app.hits.tabs_y,
        modifiers: KeyModifiers::NONE,
    });
    // It ran (here: there's no database behind the test connection).
    assert_eq!(app.focus, Focus::Editor);
    let note = app.editor_note.as_deref().unwrap_or_default();
    assert!(note.contains("connect"), "in the editor's footer: {note}");
}

#[test]
fn a_runs_outcome_is_shown_in_the_results_pane() {
    let (mut app, _dir) = query_app();
    app.status = "relation \"nope\" does not exist (line 1, col 15)".into();
    app.status_is_error = true;
    app.status_in_results = true;
    let screen = render(&mut app, 100, 24);
    let lines: Vec<&str> = screen.lines().collect();
    let last = lines.last().unwrap();
    assert!(!last.contains("does not exist") && last.contains("keys"), "not in the status bar: {last}");
    // The results pane's footer, just above its bottom border.
    assert!(lines[lines.len() - 3].contains("does not exist"), "{screen}");
    assert!(
        !screen.contains("ran in") && !screen.contains("EXEC"),
        "the time is in the footer, not the title: {screen}"
    );
}

#[test]
fn ctrl_k_lists_the_tabs_and_finds_them_by_name() {
    let (mut app, _dir) = query_app();
    app.focus = Focus::Results;
    app.handle_key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL));
    assert_eq!(app.active_tab, 1);
    app.focus = Focus::Results;
    let first = app.tabs[0].name.clone();
    app.handle_key(KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE));
    let screen = render(&mut app, 100, 40);
    assert!(screen.contains("TABS") && screen.contains("(this one)") && screen.contains(&first), "{screen}");
    assert_snapshot!(screen);
    // The number switches.
    app.handle_key(KeyEvent::new(KeyCode::Char('1'), KeyModifiers::NONE));
    assert!(app.active_tab == 0 && app.palette.is_none());
    // Searching finds the other tab by name.
    app.handle_key(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::CONTROL));
    app.handle_key(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE));
    for c in format!("switch to tab: {}", app.tabs[1].name).chars() {
        app.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
    }
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert_eq!(app.active_tab, 1);
}

#[test]
fn welcome_on_first_run_and_the_reopening_card() {
    let dir = tempfile::tempdir().unwrap();
    let (store, _) = crate::store::ConnectionStore::load_from(dir.path().join("c.toml"));
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
fn clicking_the_project_in_the_top_bar_switches_project() {
    use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    let (mut app, _dir) = query_app();
    app.focus = Focus::Results;
    let screen = render(&mut app, 100, 20);
    assert!(screen.lines().next().unwrap().contains("shop ▼"), "{screen}");
    // The editor's menu is about the editor: no project switching there.
    app.focus = Focus::Editor;
    app.handle_key(KeyEvent::new(KeyCode::F(10), KeyModifiers::NONE));
    assert!(app.menu.as_ref().is_some_and(|m| m.items.iter().all(|i| !i.label.contains("project"))));
    app.menu = None;
    let r = app.hits.project;
    app.handle_mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: r.x + 1,
        row: r.y,
        modifiers: KeyModifiers::NONE,
    });
    // The projects pane, beside the project's connections and settings.
    assert!(matches!(app.screen, Screen::Projects));
    assert_snapshot!(render(&mut app, 120, 20));
    // A click on a connection selects it and moves to that pane.
    let (list, _) = app.hits.manager_connections;
    app.handle_mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: list.x + 2,
        row: list.y + 1,
        modifiers: KeyModifiers::NONE,
    });
    assert!(matches!(app.screen, Screen::Connections));
    assert_eq!(app.connections_selected, 1);
    assert!(render(&mut app, 120, 20).contains("Host  db.internal"));
}

#[test]
fn clicks_wait_for_a_connection_attempt_and_miss_the_borders() {
    use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    let click = |app: &mut App, column, row| {
        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column,
            row,
            modifiers: KeyModifiers::NONE,
        });
    };
    let (mut app, _dir) = app();
    app.screen = Screen::Connect;
    app.connect_form.focus = ConnectField::Host;
    let _ = render(&mut app, 120, 24);
    // The settings pane's top border isn't a field.
    let settings = app.hits.manager_settings;
    click(&mut app, settings.x + 4, settings.y);
    assert_eq!(app.connect_form.focus, ConnectField::Host);
    click(&mut app, settings.x + 4, settings.y + 1);
    assert_eq!(app.connect_form.focus, ConnectField::Name);
    // While connecting, the project's name in the top bar waits.
    app.connect_form.connecting = true;
    let project = app.hits.project;
    click(&mut app, project.x + 1, project.y);
    assert!(matches!(app.screen, Screen::Connect));
    app.connect_form.connecting = false;
    click(&mut app, project.x + 1, project.y);
    assert!(matches!(app.screen, Screen::Projects));
}

#[test]
fn a_long_script_draws_about_as_fast_as_a_short_one() {
    // Only the visible rows are styled, and the highlighting is reused
    // while the text doesn't change, so frames stay cheap anywhere in a
    // long script. Compared with a short script rather than a fixed time,
    // so a slow machine doesn't fail it.
    let frame = |text: &str, row_from_end: usize| {
        let (mut app, _dir) = query_app();
        app.tab.editor.set_text(text);
        app.focus = Focus::Editor;
        app.tab.editor.set_cursor((app.tab.editor.lines.len().saturating_sub(1 + row_from_end), 0));
        let _ = render(&mut app, 120, 40);
        let started = std::time::Instant::now();
        for _ in 0..5 {
            let _ = render(&mut app, 120, 40);
        }
        started.elapsed()
    };
    let row = "insert into t (id, name) values (1, 'name'), (2, 'other'); -- a comment\n";
    let short = frame(&row.repeat(20), 0);
    let long_script = row.repeat(20_000);
    for (place, from_end) in [("top", 19_999), ("bottom", 0)] {
        let long = frame(&long_script, from_end);
        assert!(long < short * 20 + std::time::Duration::from_millis(50), "{place}: {long:?} vs {short:?}");
    }
}

/// Random keys on every pane, popup and screen they lead to, drawing after
/// each at a random size: nothing may panic. The sandbox keeps it inside the
/// app (no connecting, `ssh` or shell commands), and results mix numbers,
/// text, NULLs and JSON so sorting and filtering get exercised. (It's a
/// general net: specific crashes keep their own tests, since a bad sort
/// comparison, for one, only shows on some inputs.)
/// `PODELIS_MONKEY_ROUNDS=500` runs longer.
#[tokio::test(flavor = "multi_thread")]
async fn random_keys_never_crash_the_app() {
    const CHARS: &[char] = &[
        'a', 'z', 'Q', '1', '9', ' ', '/', 's', 'd', 'f', 'e', 'c', 'm', 'k', 'j', 'h', 'l', 'n', 'p', 'r', 't', '.',
        ',', '\'', '"', '(', ')', '*', ';', '=', '<', '>', '-', '?', ':', '$', 'ž', '日', '[', ']', '{', '}', 'M', 'J',
        'C', 'i', 'g', 'G', 'x', 'w', 'u', 'v', 'y', 'b', 'q', '!', '#', '%', '&', '+', '@', '^', '~', '\\', '|',
    ];
    const KEYS: &[KeyCode] = &[
        KeyCode::Enter,
        KeyCode::Esc,
        KeyCode::Tab,
        KeyCode::BackTab,
        KeyCode::Up,
        KeyCode::Down,
        KeyCode::Left,
        KeyCode::Right,
        KeyCode::Home,
        KeyCode::End,
        KeyCode::PageUp,
        KeyCode::PageDown,
        KeyCode::Backspace,
        KeyCode::Delete,
        KeyCode::F(1),
        KeyCode::F(2),
        KeyCode::F(3),
        KeyCode::F(4),
        KeyCode::F(5),
        KeyCode::F(6),
        KeyCode::F(7),
        KeyCode::F(8),
        KeyCode::F(9),
        KeyCode::F(10),
        KeyCode::F(11),
        KeyCode::F(12),
    ];
    const MODS: &[KeyModifiers] = &[
        KeyModifiers::NONE,
        KeyModifiers::NONE,
        KeyModifiers::NONE,
        KeyModifiers::CONTROL,
        KeyModifiers::SHIFT,
        KeyModifiers::ALT,
    ];
    let rounds: u64 = std::env::var("PODELIS_MONKEY_ROUNDS").ok().and_then(|s| s.parse().ok()).unwrap_or(8);
    for round in 0..rounds {
        let mut seed = round.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
        let mut next = move |n: usize| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed % n as u64) as usize
        };
        let (mut app, dir) = query_app();
        app.sandbox = true;
        app.settings.export_dir = Some(dir.path().to_string_lossy().into_owned());
        app.show_results_for_tests(monkey_results(round));
        let (width, height) = (20 + next(140) as u16, 6 + next(50) as u16);
        let mut keys = Vec::new();
        for _ in 0..250 {
            let code = if next(2) == 0 { KeyCode::Char(CHARS[next(CHARS.len())]) } else { KEYS[next(KEYS.len())] };
            let key = KeyEvent::new(code, MODS[next(MODS.len())]);
            keys.push(key);
            let step = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                app.handle_key(key);
                if next(5) == 0 {
                    app.tick();
                }
                let _ = render(&mut app, width, height);
            }));
            assert!(
                step.is_ok(),
                "round {round} at {width}x{height} panicked after {:?}",
                &keys[keys.len().saturating_sub(15)..]
            );
        }
    }
}

/// Results for the random-keys test: numbers and text mixed in no
/// particular order, NULLs, JSON, long and non-ASCII text. Different rows
/// (and how many) each round: a bad sort comparison is only noticed on some
/// inputs.
fn monkey_results(round: u64) -> QueryOutcome {
    let rows = 20 + (round as usize * 7919) % 2000;
    let cell = |i: usize, c: usize| -> Option<String> {
        match (c, i % 9) {
            (_, 0) => None,
            (0, _) => Some(i.to_string()),
            (1, _) => {
                let h = (i as u64 ^ round << 20).wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 32;
                let v = h % 200;
                Some(match h % 4 {
                    0 => format!("{v}"),
                    1 => format!("{v}a"),
                    2 => format!("{v}.5"),
                    _ => format!("1{v}x"),
                })
            }
            (2, _) => Some(format!("{{\"k\": {i}, \"v\": [1, 2]}}")),
            (3, _) => Some(format!("žąsis {i} with a long note that needs more room than a column has\nsecond line")),
            _ => Some(format!("x{i}")),
        }
    };
    QueryOutcome {
        columns: ["id", "mixed", "doc", "note", "x"].iter().map(|s| (*s).to_string()).collect(),
        rows: (0..rows).map(|i| (0..5).map(|c| cell(i, c)).collect()).collect(),
        total_rows: rows as u64,
        statements: 1,
        ..QueryOutcome::default()
    }
}

#[test]
fn clicking_the_editor_tabs_focuses_the_editor() {
    use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    let click = |kind, x, y| MouseEvent { kind, column: x, row: y, modifiers: KeyModifiers::NONE };
    let (mut app, _dir) = query_app();
    // Even while a query runs (the tab can't be switched then).
    app.executing = true;
    let _ = render(&mut app, 110, 26);
    let (tab, y) = (app.hits.tabs[0].0 + 2, app.hits.tabs_y);
    app.focus = Focus::Results;
    app.handle_mouse(click(MouseEventKind::Down(MouseButton::Left), tab, y));
    assert_eq!(app.focus, Focus::Editor);
    // A right-click there opens the editor's menu, not the results'.
    app.focus = Focus::Results;
    app.handle_mouse(click(MouseEventKind::Down(MouseButton::Right), tab, y));
    assert_eq!(app.menu.as_ref().map(|m| m.title), Some("EDITOR"));
}
