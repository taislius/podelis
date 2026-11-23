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

pub static TEST_SQL: TestSql = TestSql {
    sleep: Some("SELECT pg_sleep({s})"),
    count_to_2500: "SELECT g FROM generate_series(1, 2500) g",
    plan_shows: &["Scan on t"],
    analyzed_shows: "actual time",
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

#[tokio::test]
async fn live_postgres_partitions_name_their_parent() {
    let Some(db) = live_db(Engine::Postgres).await else { return };
    // Named like a view ("review"), and partitioned on two levels.
    db.run(
        "DROP TABLE IF EXISTS parted_reviews; \
         CREATE TABLE parted_reviews (id int, at date, region text) PARTITION BY RANGE (at); \
         CREATE TABLE parted_reviews_2022 PARTITION OF parted_reviews \
           FOR VALUES FROM ('2022-01-01') TO ('2023-01-01') PARTITION BY LIST (region); \
         CREATE TABLE parted_reviews_2022_eu PARTITION OF parted_reviews_2022 FOR VALUES IN ('eu')",
        DEFAULT_ROW_LIMIT,
    )
    .await
    .unwrap();
    let tables = db.tables(false).await.unwrap();
    let find = |name: &str| tables.iter().find(|t| t.name == name).unwrap_or_else(|| panic!("{name}"));
    let parent = |s: &str| Some(("public".to_string(), s.to_string()));
    assert_eq!(find("parted_reviews").partition_of, None);
    assert_eq!(find("parted_reviews_2022").partition_of, parent("parted_reviews"));
    assert_eq!(find("parted_reviews_2022_eu").partition_of, parent("parted_reviews_2022"));
    for name in ["parted_reviews", "parted_reviews_2022", "parted_reviews_2022_eu"] {
        assert!(!find(name).is_view, "{name}");
    }
    db.run("DROP TABLE parted_reviews", DEFAULT_ROW_LIMIT).await.unwrap();
}

#[tokio::test]
async fn live_own_session_postgres() {
    crate::db::tests::live_own_session(Engine::Postgres, "SELECT pg_sleep(2)", "SELECT pg_terminate_backend({pid})").await;
}


#[tokio::test]
async fn live_dead_link_postgres() {
    crate::db::tests::live_dead_link(Engine::Postgres, "SELECT generate_series(1, 1000000)").await;
}

#[tokio::test]
async fn live_supervisor_keeps_the_connection_postgres() {
    crate::db::tests::live_supervisor_keeps_the_connection(Engine::Postgres).await;
}

/// What the session had (`search_path`, a setting, a temporary table) is
/// said gone after a reconnect, and not run again; what was `RESET`, or
/// rolled back, isn't said.
#[tokio::test]
async fn live_session_memory_postgres() {
    let Some(db) = live_db(Engine::Postgres).await else { return };
    let Some(other) = live_db(Engine::Postgres).await else { return };
    db.run("SET search_path TO pg_catalog, public; SET application_name = 'podelis_memory'", 1).await.unwrap();
    db.run("SET work_mem = '8MB'; RESET work_mem", 1).await.unwrap();
    // Undone by a ROLLBACK: not kept. Committed: kept.
    db.run("BEGIN; SET statement_timeout = '5s'; ROLLBACK", 1).await.unwrap();
    db.run("BEGIN; SET lock_timeout = '3s'; COMMIT", 1).await.unwrap();
    db.run("CREATE TEMP TABLE podelis_tmp (x int)", 1).await.unwrap();
    let pid = db.run("SELECT pg_backend_pid()::text", 1).await.unwrap().rows[0][0].clone().unwrap();
    other.run(&format!("SELECT pg_terminate_backend({pid})"), 1).await.unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    // Nothing's run again: the new session has the connection's own
    // settings, and what the old one had is said, as gone.
    let o = crate::db::tests::rerun(&db, "SELECT current_setting('search_path'), current_setting('application_name')", 1)
        .await;
    assert_ne!(cells(&o), vec![vec![Some("pg_catalog, public"), Some("podelis_memory")]]);
    let health = db.health().borrow().clone();
    assert_eq!(health.lost, ["search_path", "application_name", "lock_timeout", "temporary tables"]);
}

#[tokio::test]
async fn live_a_run_after_a_killed_session_reconnects_postgres() {
    crate::db::tests::live_a_run_after_a_killed_session_reconnects(Engine::Postgres, "SELECT pg_backend_pid()::text", "SELECT pg_terminate_backend({pid})").await;
}

#[tokio::test]
async fn live_a_check_reopens_a_killed_session_in_a_transaction_postgres() {
    crate::db::tests::live_a_check_reopens_a_killed_session_in_a_transaction(Engine::Postgres, "SELECT pg_backend_pid()::text", "SELECT pg_terminate_backend({pid})").await;
}

#[tokio::test]
async fn live_a_check_cut_short_while_reopening_postgres() {
    crate::db::tests::live_a_check_cut_short_while_reopening(Engine::Postgres).await;
}

/// An import stopped half way (its task dropped) aborts its `COPY`: nothing
/// of it is kept, where finishing it would keep half the file.
#[tokio::test]
async fn live_a_stopped_copy_keeps_nothing() {
    let Some(db) = live_db(Engine::Postgres).await else { return };
    db.run("CREATE TEMP TABLE podelis_copied (x int)", 1).await.unwrap();
    let copying = db.clone();
    let import = tokio::spawn(async move {
        let rows = std::iter::repeat_with(|| Ok(b"1\n".to_vec()));
        copying.copy_in("COPY podelis_copied FROM STDIN", 1, rows).await
    });
    tokio::time::sleep(Duration::from_millis(300)).await;
    import.abort();
    let _ = import.await;
    let counted = db.run("SELECT count(*)::text FROM podelis_copied", 1).await.expect("the session free again");
    assert_eq!(cells(&counted), vec![vec![Some("0")]]);
}

#[tokio::test]
async fn live_an_unanswered_copy_abort_gives_the_session_up() {
    crate::db::tests::live_an_unanswered_copy_abort_gives_the_session_up().await;
}

#[tokio::test]
async fn live_a_lost_transaction_isnt_reopened_behind_its_back_postgres() {
    crate::db::tests::live_a_lost_transaction_isnt_reopened_behind_its_back(Engine::Postgres).await;
}

#[tokio::test]
async fn live_a_broken_stream_reopens_its_session_postgres() {
    // More than the link holds on its way: the kill is met mid-read.
    let big = "SELECT g FROM generate_series(1, 5000000) g";
    crate::db::tests::live_a_broken_stream_reopens_its_session(Engine::Postgres, big, "SELECT pg_backend_pid()::text", "SELECT pg_terminate_backend({pid})").await;
}


/// A close stops what the session runs on the server too: its backend is
/// gone soon after, not left running the query.
#[tokio::test]
async fn live_a_close_stops_what_the_session_runs() {
    let Some(db) = live_db(Engine::Postgres).await else { return };
    let Some(other) = live_db(Engine::Postgres).await else { return };
    let pid = db.run("SELECT pg_backend_pid()::text", 1).await.unwrap().rows[0][0].clone().unwrap();
    let sleeping = db.clone();
    let run = tokio::spawn(async move { sleeping.run("SELECT pg_sleep(30)", 1).await });
    tokio::time::sleep(Duration::from_millis(300)).await;
    tokio::time::timeout(Duration::from_secs(5), db.close()).await.expect("not behind the sleep");
    assert!(run.await.unwrap().is_err());
    let left = format!("SELECT count(*)::text FROM pg_stat_activity WHERE pid = {pid}");
    for _ in 0..20 {
        if cells(&other.run(&left, 1).await.unwrap()) == vec![vec![Some("0")]] {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("the session's backend still runs");
}

/// A COMMIT in a failed transaction ends it and keeps nothing (the server
/// rolls it back), and the transaction is said failed before it: what the
/// app goes by to refuse the commit key, and to say a typed COMMIT rolled
/// back.
#[tokio::test]
async fn live_a_commit_of_a_failed_transaction_keeps_nothing() {
    let Some(db) = live_db(Engine::Postgres).await else { return };
    db.run("CREATE TEMP TABLE podelis_kept (x int)", 1).await.unwrap();
    db.run("BEGIN; INSERT INTO podelis_kept VALUES (1)", 1).await.unwrap();
    assert!(db.run("SELECT 1/0", 1).await.is_err());
    assert_eq!(db.transaction_state().await, Some(TxnState::Aborted));
    db.run("COMMIT", 1).await.unwrap();
    assert_eq!(db.transaction_state().await, Some(TxnState::Idle));
    let counted = db.run("SELECT count(*)::text FROM podelis_kept", 1).await.unwrap();
    assert_eq!(cells(&counted), vec![vec![Some("0")]]);
}

/// A read of what only the session has (a temporary table) is planned on
/// the session when a pool connection can't: no "couldn't plan" for it.
#[tokio::test]
async fn live_a_temp_tables_read_is_planned_postgres() {
    let Some(db) = live_db(Engine::Postgres).await else { return };
    db.run("CREATE TEMP TABLE podelis_planned (x int)", 1).await.unwrap();
    db.full_scans("SELECT * FROM podelis_planned").await.expect("planned on the session");
    // In a transaction, a failing plan would abort it: not tried there (the
    // table a pool connection can't see, as if not there: not asked about).
    db.run("BEGIN", 1).await.unwrap();
    db.note_transaction(true);
    assert_eq!(db.full_scans("SELECT * FROM podelis_planned").await.unwrap(), []);
    assert_eq!(db.transaction_state().await, Some(TxnState::Open), "not aborted");
    db.run("ROLLBACK", 1).await.unwrap();
}

/// What a run changed on the session is read against the transaction as it
/// was before the run: a `SET` rolled back in the same run isn't kept, and
/// one made before the run's own `BEGIN` is.
#[tokio::test]
async fn live_a_runs_settings_are_read_against_the_transaction_before_it() {
    let Some(db) = live_db(Engine::Postgres).await else { return };
    let lost = |db: &Database| db.0.memory.lock().unwrap().lost();
    db.run("BEGIN", 1).await.unwrap();
    db.run("SET search_path = pg_catalog; ROLLBACK", 1).await.unwrap();
    assert!(!lost(&db).iter().any(|l| l.contains("search_path")), "{:?}", lost(&db));
    db.run("SET application_name = 'podelis_before'; BEGIN", 1).await.unwrap();
    assert!(lost(&db).iter().any(|l| l.contains("application_name")), "{:?}", lost(&db));
    db.run("ROLLBACK", 1).await.unwrap();
}
