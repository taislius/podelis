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
    // Fresh each time: a run before (one that failed) may have left it.
    server.run(&format!("DROP DATABASE IF EXISTS {name}; CREATE DATABASE {name}"), 1).await.expect("create the test's database");
    server.close().await;
    params.database = name.to_string();
    Some((Database::connect(params.clone()).await.expect("connect"), params))
}

pub static TEST_SQL: TestSql = TestSql {
    sleep: Some("SELECT SLEEP({s})"),
    count_to_2500: "SET SESSION cte_max_recursion_depth = 5000; \
        WITH RECURSIVE s(g) AS (SELECT 1 UNION ALL SELECT g + 1 FROM s WHERE g < 2500) SELECT g FROM s",
    // The classic table before 8.0.16 and on MariaDB; the tree after.
    plan_shows: &["select_type", "-> "],
    analyzed_shows: "actual time",
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
    // Tables a rollback can't undo: by schema, this database's marked.
    db.run("DROP TABLE IF EXISTS unrolled; CREATE TABLE unrolled (x int) ENGINE=MyISAM", DEFAULT_ROW_LIMIT).await.unwrap();
    let sql = db.engine().dialect().and_then(crate::db::SqlDialect::tables_without_rollback).unwrap();
    let o = db.run(sql, DEFAULT_ROW_LIMIT).await.unwrap();
    assert!(cells(&o).contains(&vec![Some("podelis_test"), Some("unrolled"), Some("1")]), "{:?}", cells(&o));
    db.run("DROP TABLE unrolled", DEFAULT_ROW_LIMIT).await.unwrap();
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

/// Behind ProxySQL (`PODELIS_TEST_PROXYSQL_PORT`, its user and password):
/// a cancel stops the query, by ProxySQL's session id, not
/// `CONNECTION_ID()`'s, which `KILL` through ProxySQL doesn't mean (the
/// cancel missed: "the server didn't take the cancel").
#[tokio::test]
async fn live_cancel_behind_proxysql() {
    let Some(params) = server_params(Engine::MySql, "PODELIS_TEST_PROXYSQL", "app") else { return };
    let db = Database::connect(params).await.expect("connect through ProxySQL");
    let databases = db.databases().await.unwrap().expect("a server has databases to pick");
    assert!(databases.iter().any(|d| d == "information_schema"), "{databases:?}");
    live_cancel(&db, "SELECT SLEEP(10)", |r| {
        assert_eq!(cells(&r.expect("SLEEP ends with a result")), vec![vec![Some("1")]], "interrupted");
    })
    .await;
}

/// Behind ProxySQL with reads sent to other servers than writes: `SET
/// @id` ties the session to the writers' group, and a SELECT after it
/// fails, said in words; the next run has a new session, not one that
/// fails every SELECT until the app is closed.
/// Run in one transaction, they work.
#[tokio::test]
async fn live_a_session_proxysql_tied_is_said_and_replaced() {
    let Some(params) = server_params(Engine::MySql, "PODELIS_TEST_PROXYSQL", "app") else { return };
    let db = Database::connect(params).await.expect("connect through ProxySQL");
    // One batch goes where its first statement goes: the SELECT after it
    // in a run of its own is what fails.
    db.run("SET @id = 1", DEFAULT_ROW_LIMIT).await.unwrap();
    let err = db.run("SELECT @id", DEFAULT_ROW_LIMIT).await.unwrap_err();
    let said = crate::db::error_message(&err);
    assert!(said.contains("locked to hostgroup") && said.contains("BEGIN … COMMIT"), "{said}");
    assert_eq!(cells(&db.run("SELECT 7", DEFAULT_ROW_LIMIT).await.unwrap()), vec![vec![Some("7")]], "a new session");
    let o = db.run("BEGIN; SET @id = 8; SELECT @id; COMMIT", DEFAULT_ROW_LIMIT).await.unwrap();
    assert_eq!(cells(&o), vec![vec![Some("8")]], "in one transaction");
}

#[test]
fn proxysqls_session_id_is_read_from_its_json() {
    let json = "{\n  \"age_ms\": 17,\n  \"client\": {\"encrypted\": true},\n  \"thread_session_id\": 15\n}";
    assert_eq!(super::connect::proxysql_thread_session_id(json), Some(15));
    assert_eq!(super::connect::proxysql_thread_session_id("not json"), None);
}

#[tokio::test]
async fn live_reconnect_mysql() {
    let Some(db) = live_db(Engine::MySql).await else { return };
    let other = live_db(Engine::MySql).await.unwrap();
    live_reconnect(&db, &other, "SELECT CAST(CONNECTION_ID() AS CHAR)", "KILL {pid}").await;
}

/// Stopping a session that's gone (closed, or reconnected under another
/// id) is done, not MySQL's "Unknown thread id".
#[tokio::test]
async fn live_stopping_a_gone_session_is_done_mysql() {
    let Some(db) = live_db(Engine::MySql).await else { return };
    let gone = i64::from(i32::MAX) - 7;
    super::queries::cancel(&db, gone).await.unwrap();
    super::queries::stop_session(&db, gone, true).await.unwrap();
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


#[tokio::test]
async fn live_own_session_mysql() {
    crate::db::tests::live_own_session(Engine::MySql, "SELECT SLEEP(2)", "KILL {pid}").await;
}

#[tokio::test]
async fn live_dead_link_mysql() {
    let big = "SELECT a.column_name FROM information_schema.columns a, information_schema.columns b";
    crate::db::tests::live_dead_link(Engine::MySql, big).await;
}

#[tokio::test]
async fn live_supervisor_keeps_the_connection_mysql() {
    crate::db::tests::live_supervisor_keeps_the_connection(Engine::MySql).await;
}

/// What the session had (its database, a setting, a user variable, a
/// temporary table) is said gone after a reconnect, and not run again: the
/// new session has the connection's own.
#[tokio::test]
async fn live_session_memory_mysql() {
    let Some((db, _)) = own_live_db("podelis_memory").await else { return };
    let Some(other) = live_db(Engine::MySql).await else { return };
    let own = db.run("SELECT DATABASE()", 1).await.unwrap().rows[0][0].clone();
    db.run("CREATE DATABASE IF NOT EXISTS podelis_memory_b", 1).await.unwrap();
    db.run("USE podelis_memory_b", 1).await.unwrap();
    db.run("SET SESSION sql_mode = 'ANSI_QUOTES'; SET @id = 42", 1).await.unwrap();
    db.run("CREATE TEMPORARY TABLE podelis_tmp (x int)", 1).await.unwrap();
    let pid = db.run("SELECT CAST(CONNECTION_ID() AS CHAR)", 1).await.unwrap().rows[0][0].clone().unwrap();
    other.run(&format!("KILL {pid}"), 1).await.unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    let o = crate::db::tests::rerun(&db, "SELECT DATABASE(), @@session.sql_mode, CAST(@id AS CHAR)", 1).await;
    assert_eq!(o.rows[0][0], own, "the connection's own database");
    assert_ne!(o.rows[0][1].as_deref(), Some("ANSI_QUOTES"));
    assert_eq!(o.rows[0][2], None, "no @id");
    let health = db.health().borrow().clone();
    assert_eq!(health.lost, ["database podelis_memory_b", "sql_mode", "@id", "temporary tables"]);
    // The lint reads the new session's sql_mode, not the old one's.
    assert!(!db.sql_mode().contains("ANSI_QUOTES"), "{}", db.sql_mode());
    other.run("DROP DATABASE podelis_memory_b", 1).await.unwrap();
}

/// Behind ProxySQL, a SELECT after `SET @id` fails (the session's tied to
/// one group of servers) and a new session opens: said as a reconnect, the
/// variable gone with the old one.
#[tokio::test]
async fn live_session_memory_behind_proxysql() {
    let Some(params) = server_params(Engine::MySql, "PODELIS_TEST_PROXYSQL", "app") else { return };
    let db = Database::connect(params).await.expect("connect through ProxySQL");
    db.run("SET @id = 1", 1).await.unwrap();
    let err = db.run("SELECT @id", 1).await.unwrap_err();
    assert!(crate::db::is_reconnected(&err), "{err:#}");
    let health = db.health().borrow().clone();
    assert!(health.lost.contains(&"@id".to_string()), "{health:?}");
}

#[tokio::test]
async fn live_a_run_after_a_killed_session_reconnects_mysql() {
    crate::db::tests::live_a_run_after_a_killed_session_reconnects(Engine::MySql, "SELECT CAST(CONNECTION_ID() AS CHAR)", "KILL {pid}").await;
}

#[tokio::test]
async fn live_a_check_reopens_a_killed_session_in_a_transaction_mysql() {
    crate::db::tests::live_a_check_reopens_a_killed_session_in_a_transaction(Engine::MySql, "SELECT CAST(CONNECTION_ID() AS CHAR)", "KILL {pid}").await;
}

#[tokio::test]
async fn live_a_check_cut_short_while_reopening_mysql() {
    crate::db::tests::live_a_check_cut_short_while_reopening(Engine::MySql).await;
}

#[tokio::test]
async fn live_a_lost_transaction_isnt_reopened_behind_its_back_mysql() {
    crate::db::tests::live_a_lost_transaction_isnt_reopened_behind_its_back(Engine::MySql).await;
}

/// What ran before a failing statement in the same run stands, and is
/// said gone after a reconnect: `USE shop; SELECT … missing` left the
/// session in shop.
#[tokio::test]
async fn live_a_failed_runs_earlier_statements_are_remembered_mysql() {
    let Some((db, _)) = own_live_db("podelis_memory_part").await else { return };
    let Some(other) = live_db(Engine::MySql).await else { return };
    db.run("CREATE DATABASE IF NOT EXISTS podelis_memory_part_b", 1).await.unwrap();
    db.run("USE podelis_memory_part_b; SELECT * FROM no_such_table", 1).await.expect_err("the second fails");
    let pid = db.run("SELECT CAST(CONNECTION_ID() AS CHAR)", 1).await.unwrap().rows[0][0].clone().unwrap();
    other.run(&format!("KILL {pid}"), 1).await.unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    let o = crate::db::tests::rerun(&db, "SELECT DATABASE()", 1).await;
    assert_ne!(cells(&o), vec![vec![Some("podelis_memory_part_b")]], "not put back");
    assert_eq!(db.health().borrow().lost, ["database podelis_memory_part_b"]);
    other.run("DROP DATABASE podelis_memory_part_b", 1).await.unwrap();
}

#[tokio::test]
async fn live_a_broken_stream_reopens_its_session_mysql() {
    // More than the link holds on its way: the kill is met mid-read.
    let big = "SELECT a.ORDINAL_POSITION FROM information_schema.COLUMNS a, information_schema.COLUMNS b";
    crate::db::tests::live_a_broken_stream_reopens_its_session(Engine::MySql, big, "SELECT CAST(CONNECTION_ID() AS CHAR)", "KILL {pid}").await;
}

/// A `CALL` gives several results, each counted as a statement: from the
/// first, which statements of a failed run ran isn't known, and none after
/// it is said lost (`USE other` after a failure never ran).
#[tokio::test]
async fn live_a_call_doesnt_make_a_failed_runs_count_wrong_mysql() {
    let Some((db, _)) = own_live_db("podelis_memory_call").await else { return };
    let Some(other) = live_db(Engine::MySql).await else { return };
    db.run("DROP PROCEDURE IF EXISTS podelis_two", 1).await.unwrap();
    db.run("CREATE PROCEDURE podelis_two() BEGIN SELECT 1; SELECT 2; END", 1).await.unwrap();
    db.run("CALL podelis_two(); SELECT * FROM no_such_table; USE mysql", 1).await.expect_err("the second fails");
    let pid = db.run("SELECT CAST(CONNECTION_ID() AS CHAR)", 1).await.unwrap().rows[0][0].clone().unwrap();
    other.run(&format!("KILL {pid}"), 1).await.unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    crate::db::tests::rerun(&db, "SELECT 1", 1).await;
    assert!(db.health().borrow().lost.is_empty(), "{:?}", db.health().borrow().lost);
}


/// After `USE`, the catalog is the session's database's even while the
/// session is busy: it waits for its turn rather than read a pool
/// connection, which is still in the connection's own.
#[tokio::test]
async fn live_the_catalog_follows_use_while_the_session_is_busy() {
    let Some((db, _)) = own_live_db("podelis_catalog_use").await else { return };
    db.run("CREATE DATABASE IF NOT EXISTS podelis_catalog_b", 1).await.unwrap();
    db.run("CREATE TABLE IF NOT EXISTS podelis_catalog_b.only_in_b (x int)", 1).await.unwrap();
    db.run("USE podelis_catalog_b", 1).await.unwrap();
    db.mark_session_changed();
    let busy = db.clone();
    let sleeping = tokio::spawn(async move { busy.run("SELECT SLEEP(1)", 1).await });
    tokio::time::sleep(Duration::from_millis(200)).await;
    let tables = db.tables(false).await.unwrap();
    assert!(tables.iter().any(|t| t.name == "only_in_b"), "{:?}", tables.iter().map(|t| &t.name).collect::<Vec<_>>());
    sleeping.await.unwrap().unwrap();
    db.run("DROP DATABASE podelis_catalog_b", 1).await.unwrap();
}

/// A batch whose `BEGIN` and insert ran before a statement failed leaves
/// its transaction open (MySQL doesn't say; what ran does), and a rollback
/// undoes the insert.
#[tokio::test]
async fn live_a_failed_batch_leaves_its_transaction_open_mysql() {
    let Some((db, _)) = own_live_db("podelis_failed_batch").await else { return };
    db.run("CREATE TABLE IF NOT EXISTS podelis_batch (x int) ENGINE = InnoDB", 1).await.unwrap();
    db.run("BEGIN; INSERT INTO podelis_batch VALUES (1); SELECT * FROM podelis_nowhere", 1).await.unwrap_err();
    assert!(db.in_transaction(), "the BEGIN ran");
    db.run("ROLLBACK", 1).await.unwrap();
    assert!(!db.in_transaction());
    let counted = db.run("SELECT CAST(count(*) AS CHAR) FROM podelis_batch", 1).await.unwrap();
    assert_eq!(cells(&counted), vec![vec![Some("0")]]);
    db.run("DROP TABLE podelis_batch", 1).await.unwrap();
}

/// A `CALL` that gives several results, then a failing statement, then a
/// `COMMIT` that never ran: the transaction is still open (the results
/// counted aren't statements).
#[tokio::test]
async fn live_a_call_doesnt_end_a_transaction_that_stays_open_mysql() {
    let Some((db, _)) = own_live_db("podelis_call_txn").await else { return };
    db.run("DROP PROCEDURE IF EXISTS podelis_two", 1).await.unwrap();
    db.run("CREATE PROCEDURE podelis_two() BEGIN SELECT 1; SELECT 2; END", 1).await.unwrap();
    db.run("BEGIN", 1).await.unwrap();
    db.run("CALL podelis_two(); SELECT * FROM podelis_nowhere; COMMIT", 1).await.unwrap_err();
    assert!(db.in_transaction(), "the COMMIT after the failure never ran");
    db.run("ROLLBACK; DROP PROCEDURE podelis_two", 1).await.unwrap();
}

/// After `USE`, a catalog load behind an open result (its rows hold the
/// session) doesn't wait for good: it says the session's busy, in seconds.
#[tokio::test]
async fn live_the_catalog_after_use_doesnt_wait_behind_an_open_result() {
    let Some((db, _)) = own_live_db("podelis_catalog_open").await else { return };
    db.run("USE podelis_catalog_open", 1).await.unwrap();
    db.mark_session_changed();
    let big = "SELECT a.COLUMN_NAME FROM information_schema.COLUMNS a CROSS JOIN information_schema.COLUMNS b";
    let (_, stream) = db.run_streaming(big, 10).await.unwrap();
    assert!(stream.is_some(), "more rows to come: the session's held");
    let tables = tokio::time::timeout(Duration::from_secs(15), db.tables(false)).await.expect("not for good");
    let err = tables.expect_err("busy");
    assert!(format!("{err:#}").contains("busy"), "{err:#}");
    drop(stream);
}

/// A connection check doesn't run a statement on the session: a run's
/// warnings (and `ROW_COUNT()`) are still there after it.
#[tokio::test]
async fn live_a_check_keeps_the_last_runs_warnings_mysql() {
    let Some(db) = live_db(Engine::MySql).await else { return };
    db.run("SELECT CAST('7x' AS UNSIGNED)", 1).await.unwrap();
    db.check_now().await.unwrap();
    let warnings = db.run("SHOW WARNINGS", 10).await.unwrap();
    assert!(!warnings.rows.is_empty(), "the warning kept");
}
