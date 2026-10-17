//! MySQL's own `Database` tests (the shared ones and their helpers
//! are `db::tests`); live ones run with the `PODELIS_TEST_*` server set.

use crate::db::tests::*;
use crate::db::*;

/// Its test server: `PODELIS_TEST_MYSQL_PORT` (and `_HOST`, `_USER`…).
pub(in crate::db) fn live_params() -> Option<ConnectParams> {
    server_params(Engine::MySql, "PODELIS_TEST_MYSQL", "root")
}

/// A database `name` of the test's own on the test server: tests run at
/// once, and `live_mysql` drops `podelis_test` when it's done.
pub(in crate::db) async fn own_live_db(name: &str) -> Option<(Database, ConnectParams)> {
    let mut params = live_params()?;
    let server = Database::connect(params.clone()).await.expect("connect");
    server.run(&format!("CREATE DATABASE IF NOT EXISTS {name}"), 1).await.unwrap();
    server.close().await;
    params.database = name.to_string();
    Some((Database::connect(params.clone()).await.expect("connect"), params))
}

pub(crate) static TEST_SQL: TestSql = TestSql {
    sleep: Some("SELECT SLEEP({s})"),
    count_to_2500: "SET SESSION cte_max_recursion_depth = 5000; \
        WITH RECURSIVE s(g) AS (SELECT 1 UNION ALL SELECT g + 1 FROM s WHERE g < 2500) SELECT g FROM s",
    // The classic table before 8.0.16 and on MariaDB; the tree after.
    plan_shows: &["select_type", "-> "],
    analyze: "ANALYZE TABLE {table}",
    binary: ["CREATE TEMPORARY TABLE bin_t (b blob)", "X'00FF275C'", "0x00FF275C"],
    comments: Some(&["CREATE TABLE noted (id INT PRIMARY KEY, total DECIMAL(10,2) COMMENT 'VAT included', plain INT) \
         COMMENT 'One row per checkout'"]),
    sequence: None,
    read_only_off: None,
    without_rollback: Some("CREATE TABLE agent_m (x int) ENGINE=MyISAM"),
};

#[tokio::test]
async fn live_mysql() {
    let Some(db) = live_db(Engine::MySql).await else { return };
    db.run("CREATE DATABASE IF NOT EXISTS podelis_test; USE podelis_test", DEFAULT_ROW_LIMIT).await.unwrap();
    live_common(&db).await;
    let o = db
        .run(
            "SELECT CAST(1.50 AS DECIMAL(5,2)) d, DATE '2024-02-29' dt, \
              X'00FF' bin, JSON_OBJECT('k', 1) j, _utf8mb4'žąsis' u",
            DEFAULT_ROW_LIMIT,
        )
        .await
        .unwrap();
    assert_eq!(
        cells(&o),
        vec![vec![Some("1.50"), Some("2024-02-29"), Some("0x00FF"), Some("{\"k\": 1}"), Some("žąsis")]]
    );
    let o = db
        .run(
            "CREATE TEMPORARY TABLE bits (b BIT(8)); INSERT INTO bits VALUES (b'101'); SELECT b FROM bits",
            DEFAULT_ROW_LIMIT,
        )
        .await
        .unwrap();
    assert_eq!(cells(&o), vec![vec![Some("5")]]);
    db.run("CREATE TABLE IF NOT EXISTS podelis_test.people (id INT NOT NULL, nick TEXT)", DEFAULT_ROW_LIMIT)
        .await
        .unwrap();
    let tables = db.tables(false).await.unwrap();
    assert!(tables.iter().any(|t| t.name == "people"));
    let cols = db.columns("podelis_test", "people").await.unwrap();
    assert_eq!(cols.iter().map(|c| (c.name.as_str(), c.nullable)).collect::<Vec<_>>(), [("id", false), ("nick", true)]);
    let catalog = db.catalog(false).await.unwrap();
    let people = catalog.iter().find(|(t, _)| t.name == "people").unwrap();
    assert_eq!(people.1.len(), 2);
    assert!(people.0.bytes.is_some(), "MySQL keeps data_length + index_length");
    assert!(catalog.iter().all(|(t, _)| !t.system));
    let catalog = db.catalog(true).await.unwrap();
    let tables = catalog
        .iter()
        .find(|(t, _)| t.schema.eq_ignore_ascii_case("information_schema") && t.name.eq_ignore_ascii_case("tables"));
    assert!(tables.is_some_and(|(t, cols)| t.system && !cols.is_empty()));
    // The server's own sql_mode, read at connect and again on refresh.
    assert!(!db.sql_mode().contains("PIPES_AS_CONCAT"), "{}", db.sql_mode());
    let o = db.run("SELECT 'a' || 'b'", DEFAULT_ROW_LIMIT).await.unwrap();
    assert_eq!(cells(&o), vec![vec![Some("0")]], "|| is OR, as in the mysql client");
    db.run("SET SESSION sql_mode = 'ANSI'", DEFAULT_ROW_LIMIT).await.unwrap();
    db.refresh_sql_mode().await;
    assert!(db.sql_mode().contains("PIPES_AS_CONCAT"), "{}", db.sql_mode());
    db.run("SET SESSION sql_mode = DEFAULT", DEFAULT_ROW_LIMIT).await.unwrap();
    // SLEEP() returns 1 when interrupted rather than failing.
    live_cancel(&db, "SELECT SLEEP(10)", |r| {
        if let Ok(o) = r {
            assert_eq!(cells(&o), vec![vec![Some("1")]]);
        }
    })
    .await;
    db.run("DROP DATABASE podelis_test", DEFAULT_ROW_LIMIT).await.unwrap();
}

#[tokio::test]
async fn live_mysql_error_line_and_read_only() {
    let Some(mut params) = live_params() else { return };
    let db = Database::connect(params.clone()).await.unwrap();
    // The check before a run is a ping, not a statement: what the last
    // statement left (its warnings, its row count) is still there after it.
    db.run("SELECT CAST('x' AS SIGNED)", DEFAULT_ROW_LIMIT).await.unwrap();
    db.answers_within(std::time::Duration::from_secs(5)).await.unwrap();
    let warnings = db.run("SHOW WARNINGS", DEFAULT_ROW_LIMIT).await.unwrap();
    assert_eq!(warnings.rows.len(), 1, "{:?}", warnings.rows);
    let err = db.run("SELECT 1,\n2 FROM WHERE", DEFAULT_ROW_LIMIT).await.unwrap_err();
    assert_eq!(error_position(&err), Some(ErrorPos::Near { line: 2, text: "WHERE".into() }));
    assert!(error_message(&err).starts_with("syntax error near 'WHERE'"), "{}", error_message(&err));
    db.run(
        "CREATE DATABASE IF NOT EXISTS podelis_ro; CREATE TABLE IF NOT EXISTS podelis_ro.t (id int)",
        DEFAULT_ROW_LIMIT,
    )
    .await
    .unwrap();
    params.read_only = true;
    let ro = Database::connect(params).await.unwrap();
    assert!(ro.run("INSERT INTO podelis_ro.t VALUES (1)", DEFAULT_ROW_LIMIT).await.is_err());
    db.run("DROP DATABASE podelis_ro", DEFAULT_ROW_LIMIT).await.unwrap();
}

#[tokio::test]
async fn live_check_statement_mysql() {
    let Some(db) = live_db(Engine::MySql).await else { return };
    db.run("CREATE DATABASE IF NOT EXISTS podelis_chk_db; USE podelis_chk_db", DEFAULT_ROW_LIMIT).await.unwrap();
    live_check(&db).await;
    db.run("DROP DATABASE podelis_chk_db", DEFAULT_ROW_LIMIT).await.unwrap();
}

#[tokio::test]
async fn live_details_mysql() {
    let Some(db) = live_db(Engine::MySql).await else { return };
    let setup = format!(
        "DROP DATABASE IF EXISTS podelis_det; CREATE DATABASE podelis_det; USE podelis_det;
         {DETAILS_TABLES}
         CREATE FUNCTION det_fn(x INT) RETURNS INT DETERMINISTIC RETURN x + 1;"
    );
    live_details(&db, "podelis_det", &setup, true).await;
    let routines = db.routines().await.unwrap();
    let f = routines.iter().find(|r| r.name == "det_fn").expect("det_fn listed");
    assert_eq!(f.kind, "function");
    assert!(db.routine_definition(f).await.unwrap().contains("x + 1"));
    db.run("DROP DATABASE podelis_det", DEFAULT_ROW_LIMIT).await.unwrap();
}

#[tokio::test]
async fn live_begin_in_script_mysql() {
    let Some(db) = live_db(Engine::MySql).await else { return };
    // Its own database: live_mysql drops podelis_test when it's done.
    db.run("CREATE DATABASE IF NOT EXISTS podelis_script; USE podelis_script", 10).await.unwrap();
    live_begin_in_script(&db, "podelis_review").await;
}

#[tokio::test]
async fn live_reconnect_mysql() {
    let Some(db) = live_db(Engine::MySql).await else { return };
    let other = live_db(Engine::MySql).await.unwrap();
    live_reconnect(&db, &other, "SELECT CAST(CONNECTION_ID() AS CHAR)", "KILL {pid}").await;
}

#[tokio::test]
async fn live_activity_mysql() {
    // KILL QUERY ends SLEEP early, with a result rather than an error.
    live_activity(Engine::MySql, "SELECT SLEEP(10) AS podelis_activity_test", false).await;
}

#[tokio::test]
async fn a_mysql_server_that_is_down_says_why_at_once() {
    // Port 1: nothing listens there, and nobody can bind it without root
    // (a port freed for the test could be taken by another test meanwhile).
    let port = 1;
    let opts = sqlx::mysql::MySqlConnectOptions::new().host("127.0.0.1").port(port).username("root");
    let started = std::time::Instant::now();
    let err = super::connect::try_login(&opts).await.unwrap_err();
    assert!(started.elapsed() < std::time::Duration::from_secs(1), "not the pool's 10 s");
    assert_eq!(err.to_string(), format!("can't reach 127.0.0.1:{port}: connection refused"));
    assert!(crate::db::is_unreachable(&err));
}

/// How MySQL's types took the sample file (`import::tests`): booleans are
/// TINYINT(1), timestamps keep their microseconds, a time with a zone is
/// kept as written; a bad value puts nothing in.
#[tokio::test]
async fn live_import_types() {
    use crate::import::tests::{import_failing, import_sample};
    let Some((db, _)) = own_live_db("podelis_import_types").await else { return };
    let o = import_sample(&db, "pets_types").await;
    assert_eq!(o.rows[1][2].as_deref(), Some("0"), "BOOLEAN is TINYINT(1)");
    assert_eq!(o.rows[0][4].as_deref(), Some("2026-10-13 10:00:00.123456"));
    assert_eq!(o.rows[1][4].as_deref(), Some("2026-10-13 11:30:00.000000"));
    assert_eq!(o.rows[0][6].as_deref(), Some("2026-10-13T10:00:00Z"), "kept as written");
    import_failing(&db, "id,born\n4,2020-01-01\n5,not a date\n", "pets_types").await;
    let count = db.run("SELECT count(*) FROM pets_types", 1).await.unwrap();
    assert_eq!(count.rows[0][0].as_deref(), Some("3"));
    db.run("DROP TABLE pets_types", 1).await.unwrap();
}
