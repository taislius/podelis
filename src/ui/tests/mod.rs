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
use crate::app::{
    App, CellView, Completion, Confirm, ConfirmAction, ConnectField, Focus, ResultTab, SchemaPanel, SchemaTable, Screen,
};
use crate::config::store::ConnectionStore;
use crate::db::{ColumnInfo, ConnectionMode, Engine, QueryOutcome, TableInfo, TxnState};
use crate::sql::analyze::{HintSource, SyntaxHint};
use crate::sql::complete::{CandKind, Candidate};

mod agents;
mod editor;
mod layout;
mod navigator;
mod plans;
mod results;
mod screens;

pub(super) fn render(app: &mut App, width: u16, height: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|f| draw(f, app)).unwrap();
    terminal.backend().to_string()
}

/// An app with projects "shop" (two connections) and "analytics".
pub(super) fn app() -> (App, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let (mut store, _) = ConnectionStore::load_from(dir.path().join("c.toml"));
    let shop = store.add_project("shop").unwrap();
    store.add_project("analytics").unwrap();
    let mut app = App::new(store, None, None);
    app.open_project_by_name("shop").unwrap();
    for (name, engine, host) in [("local", Engine::Postgres, "localhost"), ("prod", Engine::MySql, "db.internal")] {
        app.connection.form.name = name.into();
        app.connection.form.engine = engine;
        app.connection.form.host = host.into();
        app.connection.form.port = engine.default_port().into();
        let mut profile = app.connection.form.to_profile_for_tests();
        profile.database = "shop".into();
        app.store.upsert(&shop, None, profile).unwrap();
    }
    (app, dir)
}

/// The query screen with a small result set.
pub(super) fn query_app() -> (App, tempfile::TempDir) {
    let (mut app, dir) = app();
    app.screen = Screen::Query;
    app.pretend_connected();
    app.panes.schema_visible = true;
    app.connection.summary = "[shop] PostgreSQL as me@localhost:5432/shop".into();
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
    app.connection.schema = Some(SchemaPanel {
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

/// Results for the random-keys test: numbers and text mixed in no
/// particular order, NULLs, JSON, long and non-ASCII text. Different rows
/// (and how many) each round: a bad sort comparison is only noticed on some
/// inputs.
pub(super) fn monkey_results(round: u64) -> QueryOutcome {
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
