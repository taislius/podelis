//! PostgreSQL's own `Database` tests (the shared ones and their helpers
//! are `db::tests`); live ones run with the `PODELIS_TEST_*` server set.

use crate::db::tests::*;
use crate::db::*;

/// Its test server: `PODELIS_TEST_PG_PORT` (and `_HOST`, `_USER`…).
pub(in crate::db) fn live_params() -> Option<ConnectParams> {
    server_params(Engine::Postgres, "PODELIS_TEST_PG", "postgres")
}

/// The test server's database: tests keep apart by their tables' names.
pub(in crate::db) async fn own_live_db(_name: &str) -> Option<(Database, ConnectParams)> {
    let params = live_params()?;
    Some((Database::connect(params.clone()).await.expect("connect"), params))
}

pub(crate) static TEST_SQL: TestSql = TestSql {
    sleep: Some("SELECT pg_sleep({s})"),
    count_to_2500: "SELECT g FROM generate_series(1, 2500) g",
    plan_shows: &["Scan on t"],
    analyze: "ANALYZE {table}",
    binary: ["CREATE TEMP TABLE bin_t (b bytea)", r"'\x00ff275c'", r"\x00ff275c"],
    comments: Some(&[
        "CREATE TABLE noted (id INT PRIMARY KEY, total NUMERIC, plain INT)",
        "COMMENT ON TABLE noted IS 'One row per checkout'",
        "COMMENT ON COLUMN noted.total IS 'VAT included'",
    ]),
    sequence: Some([
        "DROP SEQUENCE IF EXISTS agent_seq; CREATE SEQUENCE agent_seq",
        "SELECT nextval('agent_seq')",
        "SELECT is_called FROM agent_seq",
    ]),
    read_only_off: Some("SELECT set_config('default_transaction_read_only', 'off', false)"),
    without_rollback: None,
};

#[tokio::test]
async fn live_postgres() {
    let Some(db) = live_db(Engine::Postgres).await else { return };
    live_common(&db).await;
    db.run("CREATE TEMPORARY TABLE IF NOT EXISTS cat (a int, b text)", DEFAULT_ROW_LIMIT).await.unwrap();
    let catalog = db.catalog(false).await.unwrap();
    assert!(catalog.iter().all(|(t, _)| t.schema != "pg_catalog"));
    let catalog = db.catalog(true).await.unwrap();
    let pg_class = catalog.iter().find(|(t, _)| t.schema == "pg_catalog" && t.name == "pg_class").unwrap();
    assert!(pg_class.0.system && pg_class.1.iter().any(|c| c.name == "relname"));
    // Sizes from the catalog: pg_class has pages (and indexes).
    assert!(pg_class.0.bytes.is_some_and(|b| b > 0), "{:?}", pg_class.0.bytes);
    assert!(catalog.iter().all(|(t, _)| t.system == matches!(t.schema.as_str(), "pg_catalog" | "information_schema")));
    let o = db
        .run(
            "SELECT true b, '\\x00ff'::bytea bin, '1 day'::interval i, ARRAY[1,2] a, \
              1.50::numeric n, '{\"k\":1}'::jsonb j",
            DEFAULT_ROW_LIMIT,
        )
        .await
        .unwrap();
    assert_eq!(
        cells(&o),
        vec![vec![Some("true"), Some("\\x00ff"), Some("1 day"), Some("{1,2}"), Some("1.50"), Some("{\"k\": 1}")]]
    );
    let o = db.run("BEGIN; SET LOCAL search_path TO pg_catalog; SHOW search_path", DEFAULT_ROW_LIMIT).await.unwrap();
    assert_eq!(cells(&o), vec![vec![Some("pg_catalog")]]);
    db.run("ROLLBACK", DEFAULT_ROW_LIMIT).await.unwrap();
    live_cancel(&db, "SELECT pg_sleep(10)", |r| assert!(r.is_err())).await;
}

#[tokio::test]
async fn live_postgres_transaction_state_and_read_only() {
    let Some(mut params) = live_params() else { return };
    let db = Database::connect(params.clone()).await.unwrap();
    assert_eq!(db.transaction_state().await, Some(TxnState::Idle));
    db.run("BEGIN", DEFAULT_ROW_LIMIT).await.unwrap();
    assert_eq!(db.transaction_state().await, Some(TxnState::Open));
    assert!(db.run("SELECT 1/0", DEFAULT_ROW_LIMIT).await.is_err());
    assert_eq!(db.transaction_state().await, Some(TxnState::Aborted));
    // The check before a run: the server answers (with an error, in a
    // failed transaction), and that's an answer; nothing is reset by it.
    db.answers_within(std::time::Duration::from_secs(5)).await.unwrap();
    assert_eq!(db.transaction_state().await, Some(TxnState::Aborted));
    db.run("ROLLBACK", DEFAULT_ROW_LIMIT).await.unwrap();
    assert_eq!(db.transaction_state().await, Some(TxnState::Idle));

    let err = db.run("SELECT nope FROM pg_class", DEFAULT_ROW_LIMIT).await.unwrap_err();
    assert_eq!(error_message(&err), "column \"nope\" does not exist");
    let err = db.run("SELECT relnam FROM pg_class", DEFAULT_ROW_LIMIT).await.unwrap_err();
    assert!(error_message(&err).contains("(hint: Perhaps you meant"), "{}", error_message(&err));

    // Error positions point into the submitted text.
    let err = db.run("SELECT 1;\nSELECT * FRM t", DEFAULT_ROW_LIMIT).await.unwrap_err();
    assert_eq!(error_position(&err), Some(ErrorPos::Char("SELECT 1;\nSELECT * ".len())));

    params.read_only = true;
    let ro = Database::connect(params).await.unwrap();
    let err = ro.run("CREATE TEMPORARY TABLE x (id int); CREATE TABLE y (id int)", DEFAULT_ROW_LIMIT).await;
    assert!(format!("{:#}", err.unwrap_err()).contains("read-only"));
}

#[tokio::test]
async fn live_check_statement_postgres() {
    let Some(db) = live_db(Engine::Postgres).await else { return };
    live_check(&db).await;
}

#[tokio::test]
async fn live_details_postgres() {
    let Some(db) = live_db(Engine::Postgres).await else { return };
    let setup = format!(
        "DROP SCHEMA IF EXISTS podelis_det CASCADE; CREATE SCHEMA podelis_det; SET search_path = podelis_det;
         {DETAILS_TABLES}
         CREATE FUNCTION det_fn(x integer) RETURNS integer LANGUAGE sql AS 'SELECT x + 1';"
    );
    live_details(&db, "podelis_det", &setup, true).await;
    let routines = db.routines().await.unwrap();
    let f = routines.iter().find(|r| r.schema == "podelis_det").expect("det_fn listed");
    assert_eq!((f.name.as_str(), f.kind.as_str()), ("det_fn(x integer)", "function"));
    assert!(db.routine_definition(f).await.unwrap().contains("x + 1"));
    db.run("DROP SCHEMA podelis_det CASCADE", DEFAULT_ROW_LIMIT).await.unwrap();
}

#[tokio::test]
async fn live_begin_in_script_postgres() {
    let Some(db) = live_db(Engine::Postgres).await else { return };
    live_begin_in_script(&db, "podelis_review").await;
}

#[tokio::test]
async fn live_reconnect_postgres() {
    let Some(db) = live_db(Engine::Postgres).await else { return };
    let other = live_db(Engine::Postgres).await.unwrap();
    live_reconnect(&db, &other, "SELECT pg_backend_pid()::text", "SELECT pg_terminate_backend({pid})").await;
}

#[tokio::test]
async fn live_activity_postgres() {
    live_activity(Engine::Postgres, "SELECT pg_sleep(10) AS podelis_activity_test", true).await;
}

#[tokio::test]
async fn an_address_nothing_answers_at_says_why_at_once() {
    // A port nothing listens on: refused, at once (not the pool's timeout).
    // Port 1: nothing listens there, and nobody can bind it without root
    // (a port freed for the test could be taken by another test meanwhile).
    let port = 1;
    let started = std::time::Instant::now();
    let err = crate::db::reach("127.0.0.1", port).await.unwrap_err();
    assert!(started.elapsed() < std::time::Duration::from_secs(1));
    assert_eq!(err.to_string(), format!("can't reach 127.0.0.1:{port}: connection refused"));
    assert!(crate::db::is_unreachable(&err));
    let err = crate::db::reach("db.nowhere.invalid", 5432).await.unwrap_err();
    assert_eq!(err.to_string(), "can't reach db.nowhere.invalid:5432: no such host");
    // Something listening: fine.
    let open = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    crate::db::reach("127.0.0.1", open.local_addr().unwrap().port()).await.unwrap();
}

/// How PostgreSQL's types took the sample file (`import::tests`): each
/// guessed one, the same moment from two time zones, and a bad value
/// named by its line.
#[tokio::test]
async fn live_import_types() {
    use crate::import::tests::{import_failing, import_sample};
    let Some(db) = live_db(Engine::Postgres).await else { return };
    let o = import_sample(&db, "pets_types").await;
    let types: Vec<String> = o.column_types.iter().map(|t| t.to_lowercase()).collect();
    assert_eq!(types[1..], ["text", "bool", "date", "timestamp", "float8", "timestamptz"]);
    assert_eq!(o.rows[1][2].as_deref(), Some("false"));
    assert_eq!(o.rows[0][4].as_deref(), Some("2026-10-13 10:00:00.123456"));
    db.run("SET TIME ZONE 'UTC'", 1).await.unwrap();
    let at = db.run("SELECT at FROM pets_types ORDER BY id", 10).await.unwrap();
    assert_eq!(at.rows[0][0], at.rows[1][0], "the same moment: {:?}", at.rows);
    let err = import_failing(&db, "id,born\n4,2020-01-01\n5,not a date\n", "pets_types").await;
    assert!(format!("{err:#}").starts_with("line 3: "), "{err:#}");
    db.run("DROP TABLE pets_types", 1).await.unwrap();
}
