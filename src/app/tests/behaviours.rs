//! Every database behaviour, drawn: each event the database side sends
//! the app (a connection's results, its supervisor's reports, a run's
//! answers, the background's), fed to it in a scenario, then the screen
//! drawn at every size in every language against the rules
//! (`ui::fit_rules`).
//!
//! The lists below name every event and the scenario that shows it; each
//! makes a `match` with no `_`, so an event added without a scenario
//! doesn't build. A test checks that each scenario does feed the events
//! it's named for.
//!
//! Real servers' words come in later (recorded from the live tests); for
//! now each says what a server would.

use std::cell::RefCell;
use std::collections::HashSet;

use crate::app::events::{Background, ConnEvent, DbEventKind, Ping, RunEvent, Swap};
use crate::db::{Database, Health, HealthState, QueryOutcome};
use crate::ui::fit_rules::{Known, Scenario, check, start, wait};

use super::*;

/// An event's kind as `Enum::Variant`, and the scenario named for it: from
/// one list, as a `match` with every variant.
macro_rules! behaviours {
    ($list:ident, $of:ident, $enum:ident { $($variant:ident => $scenario:literal),* $(,)? }) => {
        const $list: &[(&str, &str)] = &[$((concat!(stringify!($enum), "::", stringify!($variant)), $scenario)),*];
        fn $of(event: &$enum) -> &'static str {
            match event {
                $($enum::$variant { .. } => concat!(stringify!($enum), "::", stringify!($variant))),*
            }
        }
    };
}

behaviours!(CONNECTION, connection_kind, DbEventKind {
    PasswordLoaded => "a saved connection without its password",
    Connected => "connected",
    ConnectFailed => "connecting failed",
    DatabasesListed => "the server's databases",
    DatabaseOpened => "another database opened for the tab",
    DatabaseFailed => "another database failed to open",
    TableDetails => "a table's details",
    ActivityLoaded => "the server's sessions not readable",
    SessionStopped => "stopping a session refused",
    Diagnosed => "a connection test's report",
    PipeDone => "piping rows failed",
    SandboxCopied => "a file's sandbox copy failed",
    Definition => "a view's definition",
    RoutineDefinition => "a function's definition",
    TableDdl => "a table's CREATE TABLE",
    Opened => "rows not opened in an app",
    Followed => "a foreign key's row not found",
    SyntaxChecked => "a syntax problem",
    Guarded => "a big read on production",
    PlanAside => "a plan not made beside a run",
    SnapshotSaved => "a schema snapshot saved",
    SnapshotCompared => "a snapshot not compared",
    SchemaCompared => "schemas not compared",
    RowsCompared => "rows not compared",
    Background => "the sandbox's locks",
});

behaviours!(CONN, conn_kind, ConnEvent {
    Catalog => "the catalog loaded",
    Routines => "no functions",
    Unrollable => "tables a rollback can't undo not readable",
    Pinged => "a check with no answer",
    Health => "the connection lost",
    Followed => "a followed report",
});

behaviours!(HEALTH, health_kind, HealthState {
    Live => "the connection back",
    Lost => "the connection lost",
    Paused => "checks paused",
    Reconnected => "reconnected, the session's settings gone",
    Closed => "the connection closed",
});

behaviours!(RUN, run_kind, RunEvent {
    QueryDone => "a query's rows",
    MoreRows => "more rows loaded",
    MoreFailed => "more rows failed",
    QueryFailed => "a query failed",
    CancelFailed => "a cancel the server can't be told",
    SessionSeen => "a running query's session silent",
    SessionReplaced => "a running query's session replaced",
});

behaviours!(BACKGROUND, background_kind, Background {
    ExportProgress => "export progress with no export",
    ExportEstimated => "an export estimate nobody waits for",
    ExportAsk => "an export question with no export",
    ExportDone => "an export's end with no export",
    ExportShownDone => "a shown rows' export end with no export",
    Restored => "a restore nobody waits for",
    ContainersFound => "containers nobody asked for",
    AskProgress => "a model's progress nobody waits for",
    AskWaitingKey => "a model's key nobody waits for",
    SandboxLocks => "the sandbox's locks",
    AskDone => "a model's answer nobody waits for",
    GatewayDone => "a gateway answer for the memory window",
});

thread_local! {
    /// The events fed on this thread, as `Enum::Variant`.
    static FED: RefCell<HashSet<&'static str>> = RefCell::new(HashSet::new());
}

fn fed(kind: &'static str) {
    FED.with(|f| f.borrow_mut().insert(kind));
}

/// Feeds the app an event about its connection.
fn feed(app: &mut App, kind: DbEventKind) {
    fed(connection_kind(&kind));
    if let DbEventKind::Background(event) = &kind {
        fed(background_kind(event));
    }
    let generation = app.generation;
    app.handle_db_event(DbEvent::connection(generation, kind));
}

/// …one about the connection shown, by its id.
fn feed_conn(app: &mut App, kind: ConnEvent) {
    fed(conn_kind(&kind));
    if let ConnEvent::Health(health) | ConnEvent::Followed { health, .. } = &kind {
        fed(health_kind(&health.state));
    }
    let id = app.connection.slots.active.expect("a connection shown");
    app.handle_db_event(DbEvent::conn(id, kind));
}

/// …one about the tab's run.
fn feed_run(app: &mut App, kind: RunEvent) {
    fed(run_kind(&kind));
    let id = app.tab.run.id;
    app.handle_db_event(DbEvent::run(id, kind));
}

/// A database of its own (SQLite in memory), for the events that bring one.
#[expect(clippy::field_reassign_with_default, reason = "the form's fields are its draft's, set through it")]
fn memory_db() -> Database {
    let mut form = ConnectForm::default();
    form.engine = file_engine();
    form.host = ":memory:".into();
    wait(Database::connect(form.params().unwrap())).unwrap()
}

/// What the app says now, its start: a question it asks, else the status.
fn said_now(app: &App) -> Vec<String> {
    if let Some(confirm) = &app.popups.confirm {
        return vec![start(&confirm.message, 14)];
    }
    let status = &app.notes.status;
    if status.is_empty() { Vec::new() } else { vec![start(status, 14)] }
}

fn nothing(_: &App, _: u16, _: u16) -> Vec<String> {
    Vec::new()
}

/// What's said, where there's room: below 60 columns a lost connection's
/// state, or a run's, has the status bar instead (that's what matters
/// most then).
fn said(app: &App, width: u16, _: u16) -> Vec<String> {
    if width < 60 && (app.connection.state.is_lost() || app.tab.run.executing) {
        return Vec::new();
    }
    said_now(app)
}

/// A run going on in the tab.
fn running(app: &mut App) {
    app.tab.run.executing = true;
}

fn report(state: HealthState) -> Health {
    Health::of(state)
}

fn this_swap(app: &App) -> Swap {
    Swap { tab: app.tabs[app.active_tab].id, attempt: 1 }
}

fn other(app: &App) -> crate::app::query_screen::compare::Other {
    crate::app::query_screen::compare::Other {
        id: app.connection.slots.active.unwrap(),
        name: "prod".into(),
        engine: Engine::MySql,
    }
}

fn outcome_of(columns: &[&str], rows: &[&[&str]]) -> QueryOutcome {
    QueryOutcome {
        columns: columns.iter().map(|c| (*c).to_string()).collect(),
        column_types: columns.iter().map(|_| "TEXT".to_string()).collect(),
        rows: rows.iter().map(|r| r.iter().map(|c| Some((*c).to_string())).collect()).collect(),
        total_rows: rows.len() as u64,
        statements: 1,
        ..QueryOutcome::default()
    }
}

#[expect(clippy::too_many_lines, reason = "one scenario a behaviour, each short")]
fn scenarios() -> Vec<Scenario> {
    let base = crate::ui::tests::query_app;
    let s = |name: &str, setup: fn(&mut App, u16, u16), must_show: fn(&App, u16, u16) -> Vec<String>| {
        Scenario::new(name, base, setup, must_show)
    };
    vec![
        // The connection's own events.
        s(
            "a saved connection without its password",
            |app, _, _| {
                let profile = app.connection.form.to_profile();
                let note = Some("the keychain is locked".to_string());
                feed(app, DbEventKind::PasswordLoaded { profile, password: String::new(), note });
            },
            |app, _, _| app.connection.form.error.iter().map(|e| start(e, 14)).collect(),
        ),
        s(
            "connected",
            |app, _, _| {
                app.connection.form.engine = file_engine();
                app.connection.form.host = ":memory:".into();
                feed(app, DbEventKind::Connected(memory_db()));
            },
            |_, _, _| vec![format!("▶ F5 {}", t!("editor.run"))],
        ),
        s(
            "connecting failed",
            |app, _, _| {
                app.screen = Screen::Connect;
                app.connection.form.connecting = true;
                feed(app, DbEventKind::ConnectFailed("could not connect: connection refused".into()));
            },
            |app, _, _| app.connection.form.error.iter().map(|e| start(e, 14)).collect(),
        ),
        s(
            "the server's databases",
            |app, _, _| {
                feed(app, DbEventKind::DatabasesListed(Ok(Some(vec!["shop".into(), "shop_archive".into()]))));
            },
            |_, _, _| vec!["shop_archive".into()],
        ),
        s(
            "another database opened for the tab",
            |app, _, _| {
                let swap = this_swap(app);
                app.latest_swap = Some(swap);
                let mut form = app.connection.form.duplicate();
                form.engine = file_engine();
                form.host = ":memory:".into();
                feed(app, DbEventKind::DatabaseOpened(memory_db(), Box::new(form), swap));
            },
            |_, _, _| vec![format!("▶ F5 {}", t!("editor.run"))],
        ),
        s(
            "another database failed to open",
            |app, _, _| {
                let swap = this_swap(app);
                app.latest_swap = Some(swap);
                let error = "database \"archive\" does not exist".to_string();
                feed(app, DbEventKind::DatabaseFailed { name: "archive".into(), error, swap });
            },
            said,
        ),
        s(
            "a table's details",
            |app, _, _| {
                let details = crate::db::TableDetails {
                    indexes: vec![crate::db::IndexInfo {
                        name: "users_email_key".into(),
                        columns: "email".into(),
                        unique: true,
                        primary: false,
                        definition: None,
                    }],
                    ..crate::db::TableDetails::default()
                };
                let (schema, table) = ("public".to_string(), "users".to_string());
                feed(app, DbEventKind::TableDetails { schema, table, result: Ok(details) });
            },
            // The navigator's there from 90 columns: the index's name (its
            // start, where it gives way) and that it's unique.
            |_, width, _| {
                if width >= 90 { vec!["users_em".into(), t!("navigator.unique").to_string()] } else { Vec::new() }
            },
        ),
        s(
            "the server's sessions not readable",
            |app, _, _| {
                app.popups.activity = Some(crate::app::windows::activity::ActivityView::waiting());
                feed(app, DbEventKind::ActivityLoaded(Err("permission denied for pg_stat_activity".into())));
            },
            |_, _, _| vec!["permission denied".into()],
        ),
        s(
            "stopping a session refused",
            |app, _, _| feed(app, DbEventKind::SessionStopped(Err("must be a superuser to terminate".into()))),
            said,
        ),
        s(
            "a connection test's report",
            |app, _, _| {
                let report = "DNS  ok\nTCP  ok\nTLS  failed: the certificate has expired".to_string();
                feed(app, DbEventKind::Diagnosed { title: "Connection test".into(), report });
            },
            |_, _, _| vec!["Connection test".into(), format!("Esc {}", t!("cell_view.close"))],
        ),
        s(
            "piping rows failed",
            |app, _, _| feed(app, DbEventKind::PipeDone("jq .".into(), Err("jq: command not found".into()))),
            said,
        ),
        s(
            "a file's sandbox copy failed",
            |app, _, _| feed(app, DbEventKind::SandboxCopied(Err("no space left on device".into()))),
            said,
        ),
        s(
            "a view's definition",
            |app, _, _| {
                let text = Ok("CREATE VIEW public.active_users AS\n SELECT id, email FROM users".to_string());
                feed(app, DbEventKind::Definition { title: "VIEW active_users".into(), text });
            },
            |_, _, _| vec!["VIEW active_users".into()],
        ),
        s(
            "a function's definition",
            |app, _, _| {
                app.routine_wanted = Some(1);
                let text = Ok("CREATE FUNCTION public.touch() RETURNS trigger".to_string());
                feed(app, DbEventKind::RoutineDefinition { id: 1, title: "FUNCTION touch()".into(), text });
            },
            |_, _, _| vec!["FUNCTION touch()".into()],
        ),
        s(
            "a table's CREATE TABLE",
            |app, _, _| {
                let mut view = CellView::definition("TABLE orders".into(), t!("navigator.loading_tables").into());
                view.create_table = Some(("public".into(), "orders".into()));
                view.overview = Some("-- public.orders".into());
                view.awaiting = Some(1);
                app.popups.cell_view = Some(view);
                let ddl = "CREATE TABLE public.orders (\n  id bigint NOT NULL\n);".to_string();
                feed(app, DbEventKind::TableDdl { id: 1, table: "orders".into(), result: Ok(Some(ddl)) });
            },
            |_, _, _| vec!["CREATE TABLE public.orders".into(), format!("c {}", t!("cell_view.overview"))],
        ),
        s(
            "rows not opened in an app",
            |app, _, _| {
                let (what, command) = ("3 rows".to_string(), "open -a Numbers".to_string());
                feed(app, DbEventKind::Opened { what, command, result: Err("no such application".into()) });
            },
            said,
        ),
        s(
            "a foreign key's row not found",
            |app, _, _| {
                let found = crate::app::query_screen::results::follow_fk::Followed {
                    title: "customers · id = 7".into(),
                    tab_name: "customers 7".into(),
                    sql: "select * from customers where id = 7".into(),
                    asked_from: (app.tabs[app.active_tab].id, app.connection.slots.active),
                };
                let result = Err("relation \"customers\" does not exist".to_string());
                feed(app, DbEventKind::Followed(Box::new((found, result))));
            },
            said,
        ),
        s(
            "a syntax problem",
            |app, _, _| {
                app.focus = Focus::Editor;
                let hint = crate::sql::analyze::SyntaxHint {
                    message: "syntax error at or near \"form\"".into(),
                    offset: Some(19),
                    len: Some(4),
                    source: crate::sql::analyze::HintSource::Server,
                    nth: None,
                };
                let text = app.tab.editor.text();
                feed(app, DbEventKind::SyntaxChecked { text, hint: Some(hint) });
            },
            |_, width, _| if width >= 60 { vec!["syntax error".into()] } else { Vec::new() },
        ),
        s(
            "a big read on production",
            |app, _, _| {
                let sql = "select * from orders".to_string();
                let scans = Ok(vec![("orders".to_string(), 942_000.0)]);
                feed(app, DbEventKind::Guarded { sql, offset: 0, infos: Vec::new(), filled: None, scans });
            },
            said,
        ),
        s(
            "a plan not made beside a run",
            |app, _, _| {
                let tab = app.tabs[app.active_tab].id;
                feed(app, DbEventKind::PlanAside { tab, stmt: "select 1".into(), line: 0, plan: None });
            },
            said,
        ),
        s(
            "a schema snapshot saved",
            |app, _, _| {
                feed(app, DbEventKind::SnapshotSaved(Ok((std::path::PathBuf::from("/tmp/shop-2026-11-25.json"), 12))));
            },
            said,
        ),
        s(
            "a snapshot not compared",
            |app, _, _| feed(app, DbEventKind::SnapshotCompared(Err("the snapshot can't be read".into()))),
            said,
        ),
        s(
            "schemas not compared",
            |app, _, _| {
                let other = other(app);
                feed(app, DbEventKind::SchemaCompared { other, result: Err("access denied for user 'app'".into()) });
            },
            said,
        ),
        s(
            "rows not compared",
            |app, _, _| {
                let other = other(app);
                let result = Box::new(Err("table \"orders\" doesn't exist there".to_string()));
                feed(app, DbEventKind::RowsCompared { other, result });
            },
            said,
        ),
        // What's learnt about the connection, shown or not.
        s(
            "the catalog loaded",
            |app, _, _| {
                let load = app.connection.schema.as_ref().map_or(0, |p| p.load);
                let info = crate::db::TableInfo {
                    schema: "public".into(),
                    name: "invoices".into(),
                    is_view: false,
                    rows: Some(12),
                    bytes: None,
                    system: false,
                    partition_of: None,
                    materialized: false,
                };
                feed_conn(app, ConnEvent::Catalog { load, result: Ok(vec![(info, Vec::new())]) });
            },
            |_, width, _| if width >= 90 { vec!["invoices".into()] } else { Vec::new() },
        ),
        s(
            "no functions",
            |app, _, _| {
                let load = app.connection.schema.as_ref().map_or(0, |p| p.load);
                feed_conn(app, ConnEvent::Routines { load, routines: Vec::new() });
            },
            nothing,
        ),
        s(
            "tables a rollback can't undo not readable",
            |app, _, _| {
                let load = app.connection.schema.as_ref().map_or(0, |p| p.load);
                feed_conn(app, ConnEvent::Unrollable { load, tables: Err("information_schema not readable".into()) });
            },
            nothing,
        ),
        s(
            "a check with no answer",
            |app, _, _| feed_conn(app, ConnEvent::Pinged(Err(("no answer in 5 s".into(), Ping::NoAnswer)))),
            said,
        ),
        s(
            "the connection lost",
            |app, _, _| {
                let lost = report(HealthState::Lost("can't reach db.internal:3306: name not known".into()));
                feed_conn(app, ConnEvent::Health(lost));
            },
            nothing,
        ),
        s(
            "the connection back",
            |app, _, _| {
                feed_conn(app, ConnEvent::Health(report(HealthState::Lost("no answer".into()))));
                feed_conn(app, ConnEvent::Health(report(HealthState::Live)));
            },
            said,
        ),
        s(
            "checks paused",
            |app, _, _| feed_conn(app, ConnEvent::Health(report(HealthState::Paused("no answer in 5 s".into())))),
            said,
        ),
        s(
            "reconnected, the session's settings gone",
            |app, _, _| {
                let health = Health {
                    reconnects: 1,
                    lost: vec!["database shop".into(), "sql_mode".into(), "temporary tables".into()],
                    ..report(HealthState::Reconnected)
                };
                feed_conn(app, ConnEvent::Health(health));
            },
            said,
        ),
        s("the connection closed", |app, _, _| feed_conn(app, ConnEvent::Health(report(HealthState::Closed))), nothing),
        s(
            "a followed report",
            |app, _, _| {
                let follower =
                    app.connection.slots.active.and_then(|id| app.connection.slot_mut(id)).map_or(0, |s| s.follower);
                let health = report(HealthState::Paused("no answer in 5 s".into()));
                feed_conn(app, ConnEvent::Followed { follower, health });
            },
            said,
        ),
        // A run's answers.
        s(
            "a query's rows",
            // Read where they are: the results have the keys (on a short
            // screen, the pane with them gets the room).
            |app, _, _| {
                running(app);
                app.focus = Focus::Results;
                feed_run(app, RunEvent::QueryDone(outcome_of(&["no"], &[&["INV-7"], &["INV-8"]]), None, None));
            },
            |_, _, _| vec!["INV-7".into()],
        ),
        s(
            "more rows loaded",
            |app, _, _| {
                app.tab.run.fetching_more = true;
                let page = crate::db::Page {
                    rows: vec![vec![Some("4".into()), Some("dan@example.com".into()), None]],
                    done: true,
                };
                feed_run(app, RunEvent::MoreRows { page, shown: true });
            },
            said,
        ),
        s(
            "more rows failed",
            |app, _, _| {
                app.tab.run.fetching_more = true;
                feed_run(app, RunEvent::MoreFailed("connection reset by peer".into()));
            },
            said,
        ),
        s(
            "a query failed",
            |app, _, _| {
                running(app);
                let message = "ERROR: relation \"ordrs\" does not exist".to_string();
                let failed =
                    RunEvent::QueryFailed { message, position: None, txn: None, missing_feature: false, link: false };
                feed_run(app, failed);
            },
            |_, _, _| vec!["relation \"ordrs\"".into()],
        ),
        s(
            "a cancel the server can't be told",
            |app, _, _| {
                running(app);
                app.tab.run.cancelling = true;
                let message = "pool timed out while waiting for an open connection".to_string();
                feed_run(app, RunEvent::CancelFailed { message, unreachable: true });
            },
            said,
        ),
        s(
            "a running query's session silent",
            |app, _, _| {
                running(app);
                feed_run(app, RunEvent::SessionSeen(crate::app::tick::SessionSeen::NoAnswer));
            },
            said,
        ),
        s(
            "a running query's session replaced",
            |app, _, _| {
                running(app);
                feed_run(app, RunEvent::SessionReplaced("the connection dropped while it ran".into()));
            },
            said,
        ),
        // The background's: each finds what it's for, or nothing waits.
        s(
            "export progress with no export",
            |app, _, _| feed(app, Background::ExportProgress { rows: 10, size: 4096, free: None }.into()),
            nothing,
        ),
        s(
            "an export estimate nobody waits for",
            |app, _, _| {
                let request = crate::app::query_screen::results::export::ExportRequestId::default();
                let format = crate::app::query_screen::results::export::Format::Csv;
                feed(app, Background::ExportEstimated { request, format, rows: Some(1e6), row_bytes: 80.0 }.into());
            },
            nothing,
        ),
        s(
            "an export question with no export",
            |app, _, _| feed(app, Background::ExportAsk { rows: 10, size: 1 << 30, free: Some(1 << 20) }.into()),
            nothing,
        ),
        s(
            "an export's end with no export",
            |app, _, _| feed(app, Background::ExportDone("/tmp/orders.csv".into(), Ok(10)).into()),
            nothing,
        ),
        s(
            "a shown rows' export end with no export",
            |app, _, _| {
                let done = Background::ExportShownDone("/tmp/orders.parquet".into(), Ok(()), String::new());
                feed(app, done.into());
            },
            nothing,
        ),
        s(
            "a restore nobody waits for",
            |app, _, _| {
                let restored = Background::Restored {
                    project_id: project_id(app),
                    id: crate::app::ConnId::from_test_id(5),
                    attempt: 0,
                    form: Box::new(app.connection.form.duplicate()),
                    result: Err("timed out".into()),
                };
                feed(app, restored.into());
            },
            nothing,
        ),
        s(
            "containers nobody asked for",
            |app, _, _| {
                let request = crate::app::ContainerRequestId::default();
                feed(app, Background::ContainersFound(request, Ok(Vec::new())).into());
            },
            nothing,
        ),
        s(
            "a model's progress nobody waits for",
            |app, _, _| feed(app, Background::AskProgress(crate::app::query_screen::ask::AskId::default(), 120).into()),
            nothing,
        ),
        s(
            "a model's key nobody waits for",
            |app, _, _| feed(app, Background::AskWaitingKey(crate::app::query_screen::ask::AskId::default()).into()),
            nothing,
        ),
        s(
            "the sandbox's locks",
            |app, _, _| {
                app.connection.sandbox = Some(crate::app::Sandbox::for_tests(Vec::new()));
                app.popups.sandbox_view = Some(crate::app::windows::sandbox::SandboxView { scroll: 0, locks: None });
                feed(app, Background::SandboxLocks("row locks on orders (3)".into()).into());
            },
            // Below its steps and what they changed: in sight from 24 rows,
            // scrolled to below that.
            |_, _, height| if height >= 24 { vec!["row locks on orders".into()] } else { Vec::new() },
        ),
        s(
            "a model's answer nobody waits for",
            |app, _, _| {
                let failure =
                    crate::app::query_screen::ask::AskFailure { code: None, message: "no model".into(), data: None };
                let id = crate::app::query_screen::ask::AskId::default();
                feed(app, Background::AskDone(id, Box::new(Err(failure))).into());
            },
            nothing,
        ),
        s(
            "a gateway answer for the memory window",
            |app, _, _| {
                let failure = crate::app::query_screen::ask::AskFailure {
                    code: None,
                    message: "the gateway isn't running".into(),
                    data: None,
                };
                let purpose = crate::app::windows::memory::GatewayPurpose::MemoryList(project_id(app));
                feed(app, Background::GatewayDone(purpose, Box::new(Err(failure))).into());
            },
            nothing,
        ),
    ]
}

/// The scenarios drawn with what real servers said (`server_words`), word
/// for word: each server's errors for a query and for connecting, its
/// syntax error in the editor's footer, its CREATE TABLE; the app said to
/// be on that server's engine.
fn recorded_scenarios() -> Vec<Scenario> {
    use super::server_words::{CONNECT_SITUATIONS, QUERY_SITUATIONS, recorded};
    let base = crate::ui::tests::query_app;
    let mut out = Vec::new();
    for (server, said) in recorded() {
        let Some(engine) = said.get("engine").and_then(|label| Engine::ALL.into_iter().find(|e| e.label() == label))
        else {
            continue;
        };
        for situation in QUERY_SITUATIONS {
            let Some(message) = said.get(situation).cloned() else { continue };
            out.push(
                Scenario::new(
                    format!("a query failed · {server}: {situation}"),
                    base,
                    move |app, _, _| {
                        running(app);
                        let failed = RunEvent::QueryFailed {
                            message: message.clone(),
                            position: None,
                            txn: None,
                            missing_feature: false,
                            link: false,
                        };
                        feed_run(app, failed);
                    },
                    // What the results say of it, its start.
                    |app, _, _| app.tab.run_error.iter().map(|e| start(&one_line(e), 20)).collect(),
                )
                .on(engine),
            );
        }
        for situation in CONNECT_SITUATIONS {
            let Some(message) = said.get(situation).cloned() else { continue };
            out.push(
                Scenario::new(
                    format!("connecting failed · {server}: {situation}"),
                    base,
                    move |app, _, _| {
                        app.screen = Screen::Connect;
                        app.connection.form.connecting = true;
                        feed(app, DbEventKind::ConnectFailed(message.clone()));
                    },
                    |app, _, _| app.connection.form.error.iter().map(|e| start(e, 14)).collect(),
                )
                .on(engine),
            );
        }
        if let Some(message) = said.get("syntax_error").cloned() {
            out.push(
                Scenario::new(
                    format!("a syntax problem · {server}"),
                    base,
                    move |app, _, _| {
                        app.focus = Focus::Editor;
                        let hint = crate::sql::analyze::SyntaxHint {
                            message: message.clone(),
                            offset: Some(0),
                            len: Some(5),
                            source: crate::sql::analyze::HintSource::Server,
                            nth: None,
                        };
                        let text = app.tab.editor.text();
                        feed(app, DbEventKind::SyntaxChecked { text, hint: Some(hint) });
                    },
                    // Beside the write mode from 60 columns: its start.
                    |app, width, _| {
                        let hint = app.tab.syntax_hint.as_ref().map(|h| start(&h.message, 12));
                        if width >= 60 { hint.into_iter().collect() } else { Vec::new() }
                    },
                )
                .on(engine),
            );
        }
        if let Some(ddl) = said.get("create_table").cloned() {
            out.push(
                Scenario::new(
                    format!("a table's CREATE TABLE · {server}"),
                    base,
                    move |app, _, _| {
                        let mut view =
                            CellView::definition("TABLE words_orders".into(), t!("navigator.loading_tables").into());
                        view.create_table = Some(("public".into(), "words_orders".into()));
                        view.overview = Some("-- words_orders".into());
                        view.awaiting = Some(1);
                        app.popups.cell_view = Some(view);
                        let result = Ok(Some(ddl.clone()));
                        feed(app, DbEventKind::TableDdl { id: 1, table: "words_orders".into(), result });
                    },
                    // Its first words (a long first line wraps, as SQL may), and
                    // how to go back.
                    |app, _, _| {
                        let first =
                            app.popups.cell_view.as_ref().and_then(|v| v.text.lines().next().map(|l| start(l, 12)));
                        first.into_iter().chain([format!("c {}", t!("cell_view.overview"))]).collect()
                    },
                )
                .on(engine),
            );
        }
    }
    out
}

/// Scenarios' frames known to break a rule, and why; taken off as they're
/// fixed.
const KNOWN: &[Known] = &[];

#[test]
fn every_behaviour_keeps_the_rules_at_every_size() {
    check(&scenarios(), KNOWN);
}

/// …and with each real server's words.
#[test]
fn every_servers_words_keep_the_rules_at_every_size() {
    let scenarios = recorded_scenarios();
    assert!(scenarios.len() >= 30, "the recorded words are there: {}", scenarios.len());
    check(&scenarios, KNOWN);
}

/// Each scenario feeds the events it's named for: an event's scenario
/// does draw it.
#[test]
fn each_behaviours_scenario_feeds_it() {
    crate::ui::fit_rules::with_runtime(|| {
        let scenarios = scenarios();
        let mut missing = Vec::new();
        for list in [CONNECTION, CONN, HEALTH, RUN, BACKGROUND] {
            for (event, name) in list {
                let Some(scenario) = scenarios.iter().find(|s| s.name == *name) else {
                    missing.push(format!("{event}: no scenario \"{name}\""));
                    continue;
                };
                FED.with(|f| f.borrow_mut().clear());
                let (mut app, _dir) = (scenario.base)();
                (scenario.setup)(&mut app, 90, 24);
                if !FED.with(|f| f.borrow().contains(event)) {
                    missing.push(format!("{event}: \"{name}\" doesn't feed it"));
                }
            }
        }
        assert!(missing.is_empty(), "{}", missing.join("\n"));
    });
}
