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
        info: TableInfo { schema: "public".into(), name: name.into(), is_view, rows, system: false },
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
        let info = TableInfo { schema: schema.into(), name: name.into(), is_view, rows: None, system: true };
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
    // Clicking an item does it: "Copy the row" (the 8th).
    let menu = app.hits.menu;
    app.handle_mouse(click(MouseEventKind::Down(MouseButton::Left), menu.x + 3, menu.y + 8));
    assert!(app.menu.is_none());
    assert_eq!(app.take_clipboard().as_deref(), Some("2\tben@example.com\tline one line two"));
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
    assert!(screen.contains("IN THE SCHEMA PANE") && screen.contains("zoom pane"), "{screen}");
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
    assert!(app.status.contains("connect"), "{}", app.status);
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
    assert!(screen.contains("ran in 4 ms") && !screen.contains("EXEC"), "{screen}");
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
