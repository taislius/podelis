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
async fn live_postgres_materialized_views_are_listed_with_their_columns() {
    let Some(db) = live_db(Engine::Postgres).await else { return };
    db.run(
        "DROP MATERIALIZED VIEW IF EXISTS mv_totals; \
         CREATE MATERIALIZED VIEW mv_totals AS \
           SELECT 3::int AS z, 'a'::varchar(12) AS name, 1.5::numeric(6,2) AS amount",
        DEFAULT_ROW_LIMIT,
    )
    .await
    .unwrap();
    let catalog = db.catalog(false).await.unwrap();
    let (view, columns) = catalog.iter().find(|(t, _)| t.name == "mv_totals").expect("mv_totals listed");
    assert!(view.is_view && view.materialized && !view.system);
    let columns: Vec<(&str, &str)> = columns.iter().map(|c| (c.name.as_str(), c.data_type.as_str())).collect();
    assert_eq!(columns, [("z", "int4"), ("name", "varchar(12)"), ("amount", "numeric(6,2)")], "in their order");
    let details = db.table_details("public", "mv_totals").await.unwrap();
    assert!(details.definition.is_some_and(|d| d.contains("amount")));
    db.run("DROP MATERIALIZED VIEW mv_totals", DEFAULT_ROW_LIMIT).await.unwrap();
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

#[tokio::test]
async fn live_postgres_details_have_check_constraints() {
    let Some(db) = live_db(Engine::Postgres).await else { return };
    db.run("DROP TABLE IF EXISTS checked_orders; CREATE TABLE checked_orders (total int CONSTRAINT positive CHECK (total > 0))", DEFAULT_ROW_LIMIT)
        .await
        .unwrap();
    let details = db.table_details("public", "checked_orders").await.unwrap();
    assert_eq!(details.checks.len(), 1, "{:?}", details.checks);
    assert_eq!(details.checks[0].name, "positive");
    assert!(details.checks[0].definition.starts_with("CHECK") && details.checks[0].definition.contains("total > 0"));
    db.run("DROP TABLE checked_orders", DEFAULT_ROW_LIMIT).await.unwrap();
}

/// One value of each kind of type there is, as `psql` shows it: a new
/// server's new way of sending one fails here, not in someone's grid.
#[tokio::test]
async fn live_postgres_shows_every_kind_of_type_as_text() {
    let Some(db) = live_db(Engine::Postgres).await else { return };
    let values: &[(&str, &str)] = &[
        ("1::int2", "1"),
        ("9007199254740993::int8", "9007199254740993"),
        ("1.10::numeric(5,2)", "1.10"),
        ("'NaN'::numeric", "NaN"),
        ("1.5::float8", "1.5"),
        ("'$1.50'::money", "$1.50"),
        ("'ž'::char(2)", "ž "),
        ("'2024-02-29'::date", "2024-02-29"),
        ("'infinity'::timestamp", "infinity"),
        ("'2024-01-01 12:00+00'::timestamptz", "2024-01-01 14:00:00+02"),
        ("'12:30+00'::timetz", "12:30:00+00"),
        ("'1 year 2 days'::interval", "1 year 2 days"),
        ("'a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11'::uuid", "a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11"),
        ("'192.168.0.1/24'::inet", "192.168.0.1/24"),
        ("'08:00:2b:01:02:03'::macaddr", "08:00:2b:01:02:03"),
        ("B'101'::bit(3)", "101"),
        ("'[1,5)'::int4range", "[1,5)"),
        ("'(1,2)'::point", "(1,2)"),
        ("'<a/>'::xml", "<a/>"),
        ("'a fat cat'::tsvector", "'a' 'cat' 'fat'"),
        ("ARRAY[[1,2],[3,4]]", "{{1,2},{3,4}}"),
        ("ROW(1, 'x')", "(1,x)"),
        ("'{\"a\": [1, null]}'::json", "{\"a\": [1, null]}"),
        ("'\\x00ff'::bytea", "\\x00ff"),
    ];
    // The session's zone, for timestamptz.
    db.run("SET TIME ZONE 'Europe/Vilnius'", DEFAULT_ROW_LIMIT).await.unwrap();
    for (sql, shown) in values {
        let o = db.run(&format!("SELECT {sql}"), DEFAULT_ROW_LIMIT).await.unwrap_or_else(|e| panic!("{sql}: {e:#}"));
        assert_eq!(cells(&o), vec![vec![Some(*shown)]], "{sql}");
    }
    db.run("RESET TIME ZONE", DEFAULT_ROW_LIMIT).await.unwrap();
}

/// Sample texts a value of a type may be made from: the first a type
/// takes is its value.
#[cfg(test)]
const SAMPLES: &[&str] = &[
    "1", "t", "a", "2024-01-02", "12:30", "12:30+02", "2024-01-02 12:30", "2024-01-02 12:30+02", "1 day",
    "(1,2)", "((0,0),(1,1))", "[(0,0),(1,1)]", "<(0,0),1>", "{1,2,3}", "[1,5)", "{[1,5)}",
    "[2024-01-01,2024-02-01)", "{[2024-01-01,2024-02-01)}", "[2024-01-01 00:00,2024-02-01 00:00)",
    "{[2024-01-01 00:00,2024-02-01 00:00)}", "127.0.0.1", "192.168.0.0/24", "08:00:2b:01:02:03",
    "08:00:2b:01:02:03:04:05", "101", "{\"a\": [1, null]}", "<a/>", "a fat cat", "fat & cat", "0/16B3748",
    "a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11", "\\x00ff", "$.a", "10:20:10,14,15", "1 2", "0",
];

/// PostgreSQL's base types no sample fits, and why: internal ones, made by
/// the server only, or needing a value of a kind no sample is.
#[cfg(test)]
const NO_SAMPLE: &[(&str, &str)] = &[
    ("aclitem", "a privilege, naming a role that must exist"),
    ("gtsvector", "a text search index's own, no input"),
    ("pg_brin_bloom_summary", "a BRIN index's summary, made by the server"),
    ("pg_brin_minmax_multi_summary", "a BRIN index's summary, made by the server"),
    ("pg_dependencies", "extended statistics, made by ANALYZE"),
    ("pg_mcv_list", "extended statistics, made by ANALYZE"),
    ("pg_ndistinct", "extended statistics, made by ANALYZE"),
    ("pg_node_tree", "a stored expression's tree, made by the server"),
];

/// Where Podėlis shows a type otherwise than the server writes it, on
/// purpose: the type, the server's text, what's shown, why.
#[cfg(test)]
const SHOWN_OTHERWISE: &[(&str, &str, &str, &str)] =
    &[("bool", "t", "true", "true and false read at a glance, as MySQL's and SQLite's are shown")];

/// Every base, range and multirange type the server has (`pg_type`), not a
/// list written by hand: each made from the first sample it takes, then
/// shown by Podėlis as the server itself writes it (its output function,
/// as psql shows it), or as `SHOWN_OTHERWISE` says on purpose. A type no
/// sample fits is in `NO_SAMPLE`, with why: a new version's new type fails
/// here until it's looked at.
#[tokio::test]
async fn live_postgres_shows_every_base_type_as_the_server_writes_it() {
    let Some(db) = live_db(Engine::Postgres).await else { return };
    let types = db
        .run(
            "SELECT t.typname::text FROM pg_type t JOIN pg_namespace n ON n.oid = t.typnamespace
             WHERE n.nspname = 'pg_catalog' AND t.typtype IN ('b', 'r', 'm') AND t.typisdefined
               AND t.typcategory <> 'A' ORDER BY 1",
            10_000,
        )
        .await
        .unwrap();
    let mut differ = Vec::new();
    let mut unsampled = Vec::new();
    for row in cells(&types) {
        let name = row[0].unwrap().to_string();
        let ty = format!("pg_catalog.\"{name}\"");
        let mut found = None;
        for sample in SAMPLES {
            let value = format!("CAST({} AS {ty})", crate::db::sql_literal(sample, Engine::Postgres));
            // The type's own output (as psql shows it), not a cast to text
            // (inet's adds its netmask).
            if let Ok(o) = db.run(&format!("SELECT format('%s', {value})"), 1).await {
                found = Some((value, cells(&o)[0][0].map(str::to_string)));
                break;
            }
        }
        let Some((value, server)) = found else {
            if !NO_SAMPLE.iter().any(|(t, _)| *t == name) {
                unsampled.push(name);
            }
            continue;
        };
        let shown = db.run(&format!("SELECT {value}"), 1).await.map(|o| cells(&o)[0][0].map(str::to_string));
        let chosen = SHOWN_OTHERWISE
            .iter()
            .find(|(t, written, _, _)| *t == name && server.as_deref() == Some(*written))
            .map(|(_, _, shown, _)| (*shown).to_string());
        match shown {
            Ok(shown) if shown == server || (chosen.is_some() && shown == chosen) => {}
            Ok(shown) => differ.push(format!("{name}: the server writes {server:?}, Podėlis shows {shown:?}")),
            Err(e) => differ.push(format!("{name}: {e:#}")),
        }
    }
    assert!(unsampled.is_empty(), "no sample fits these types (add one, or put them in NO_SAMPLE with why): {unsampled:?}");
    assert!(differ.is_empty(), "shown otherwise than the server writes them:\n  {}", differ.join("\n  "));
}

/// Every kind of object PostgreSQL has, as the navigator accounts for it:
/// `relkind`s (tables listed, views, materialized views, foreign and
/// partitioned tables; sequences and composite types with the functions;
/// indexes under their table; TOAST tables internal), `prokind`s
/// (functions, procedures, aggregates, window functions) and `typtype`s
/// (base and pseudo types the server's; composite, domain, enum, range
/// listed; a multirange with its range).
#[cfg(test)]
const PG_RELKINDS: &[char] = &['r', 'p', 'v', 'm', 'f', 'S', 'c', 'i', 'I', 't'];
#[cfg(test)]
const PG_PROKINDS: &[char] = &['f', 'p', 'a', 'w'];
#[cfg(test)]
const PG_TYPTYPES: &[char] = &['b', 'p', 'c', 'd', 'e', 'r', 'm'];

/// One of each kind of object a user makes, found in the navigator with
/// its kind and its definition; and every kind the server's catalogs have
/// accounted for (`PG_RELKINDS`…): a new version's new kind fails here
/// until the navigator knows what to do with it.
#[tokio::test]
async fn live_postgres_lists_every_kind_of_object() {
    let Some(db) = live_db(Engine::Postgres).await else { return };
    db.run(
        "DROP SCHEMA IF EXISTS podelis_kinds CASCADE; CREATE SCHEMA podelis_kinds; SET search_path = podelis_kinds;
         CREATE TABLE t (id int PRIMARY KEY);
         CREATE TABLE p (id int) PARTITION BY RANGE (id);
         CREATE TABLE p1 PARTITION OF p FOR VALUES FROM (0) TO (10);
         CREATE INDEX p_id ON p (id);
         CREATE VIEW v AS SELECT 1 AS x;
         CREATE MATERIALIZED VIEW m AS SELECT 1 AS x;
         CREATE SEQUENCE s;
         CREATE TYPE c AS (a int);
         CREATE TYPE e AS ENUM ('a', 'b');
         CREATE DOMAIN d AS int CHECK (VALUE > 0);
         CREATE TYPE r AS RANGE (SUBTYPE = int8);
         CREATE FUNCTION f() RETURNS int LANGUAGE sql AS 'SELECT 1';
         CREATE PROCEDURE pr() LANGUAGE sql AS 'SELECT 1';
         CREATE AGGREGATE ag(int) (SFUNC = int4pl, STYPE = int, INITCOND = '0');
         CREATE EXTENSION IF NOT EXISTS postgres_fdw SCHEMA public;
         DROP SERVER IF EXISTS podelis_kinds_far CASCADE;
         CREATE SERVER podelis_kinds_far FOREIGN DATA WRAPPER postgres_fdw OPTIONS (host 'localhost');
         CREATE FOREIGN TABLE ft (a int) SERVER podelis_kinds_far;
         RESET search_path",
        DEFAULT_ROW_LIMIT,
    )
    .await
    .unwrap();
    // The tables, views and the rest of their kind, as the navigator lists them.
    let tables = db.tables(false).await.unwrap();
    let ours: Vec<_> = tables.iter().filter(|t| t.schema == "podelis_kinds").collect();
    let kind = |name: &str| {
        let t = ours.iter().find(|t| t.name == name).unwrap_or_else(|| panic!("{name} listed: {ours:?}"));
        (t.is_view, t.materialized, t.partition_of.is_some())
    };
    assert_eq!(kind("t"), (false, false, false));
    assert_eq!(kind("p"), (false, false, false));
    assert_eq!(kind("p1"), (false, false, true), "a partition names its parent");
    assert_eq!(kind("v"), (true, false, false));
    assert_eq!(kind("m"), (true, true, false));
    assert_eq!(kind("ft"), (false, false, false), "a foreign table is listed with the tables");
    // The rest with the functions, each with its kind and a definition.
    let routines = db.routines().await.unwrap();
    for (name, wanted) in [
        ("s", "sequence"),
        ("c", "type"),
        ("e", "enum"),
        ("d", "domain"),
        ("r", "range"),
        ("f()", "function"),
        ("pr()", "procedure"),
        ("ag(integer)", "aggregate"),
    ] {
        let routine = routines
            .iter()
            .find(|r| r.schema == "podelis_kinds" && r.name == name)
            .unwrap_or_else(|| panic!("{name} listed with the functions"));
        assert_eq!(routine.kind, wanted, "{name}");
        let definition = db.routine_definition(routine).await.unwrap_or_else(|e| panic!("{name}: {e:#}"));
        assert!(definition.contains("CREATE"), "{name}: {definition}");
    }
    // Every kind the server's catalogs have, accounted for.
    for (sql, known) in [
        ("SELECT DISTINCT relkind::text FROM pg_class", PG_RELKINDS),
        ("SELECT DISTINCT prokind::text FROM pg_proc", PG_PROKINDS),
        ("SELECT DISTINCT typtype::text FROM pg_type", PG_TYPTYPES),
    ] {
        let o = db.run(sql, 1000).await.unwrap();
        let new: Vec<String> = cells(&o)
            .into_iter()
            .filter_map(|row| row[0].map(str::to_string))
            .filter(|k| !k.chars().next().is_some_and(|c| known.contains(&c)))
            .collect();
        assert!(new.is_empty(), "{sql}: kinds the navigator doesn't account for: {new:?}");
    }
    db.run("DROP SCHEMA podelis_kinds CASCADE; DROP SERVER podelis_kinds_far CASCADE", 1).await.unwrap();
}

#[tokio::test]
async fn live_postgres_lists_triggers_sequences_and_types_with_their_definitions() {
    let Some(db) = live_db(Engine::Postgres).await else { return };
    db.run(
        "DROP SCHEMA IF EXISTS podelis_obj CASCADE; CREATE SCHEMA podelis_obj; SET search_path = podelis_obj;
         CREATE TYPE mood AS ENUM ('sad', 'ok', 'happy');
         CREATE DOMAIN positive AS int NOT NULL CHECK (VALUE > 0);
         CREATE TYPE pair AS (a int, b text);
         CREATE SEQUENCE tickets START 100;
         CREATE TABLE logged (id serial, m mood);
         CREATE FUNCTION touch() RETURNS trigger LANGUAGE plpgsql AS 'BEGIN RETURN NEW; END';
         CREATE TRIGGER logged_touch BEFORE INSERT ON logged FOR EACH ROW EXECUTE FUNCTION touch();
         CREATE TABLE parted (id int) PARTITION BY RANGE (id);
         CREATE TABLE parted_a PARTITION OF parted FOR VALUES FROM (0) TO (10);
         CREATE TABLE parted_b PARTITION OF parted FOR VALUES FROM (10) TO (20);
         CREATE TRIGGER parted_touch BEFORE INSERT ON parted FOR EACH ROW EXECUTE FUNCTION touch();",
        DEFAULT_ROW_LIMIT,
    )
    .await
    .unwrap();
    let objects = db.routines().await.unwrap();
    let ours: Vec<(&str, &str)> = objects
        .iter()
        .filter(|r| r.schema == "podelis_obj")
        .map(|r| (r.name.as_str(), r.kind.as_str()))
        .collect();
    for wanted in [
        ("mood", "enum"),
        ("positive", "domain"),
        ("pair", "type"),
        ("tickets", "sequence"),
        ("logged_touch (logged)", "trigger"),
        ("touch()", "function"),
    ] {
        assert!(ours.contains(&wanted), "{wanted:?} in {ours:?}");
    }
    assert!(!ours.iter().any(|(n, _)| n.starts_with("logged_id_seq")), "a serial's own sequence isn't listed");
    let parted: Vec<_> = ours.iter().filter(|(n, _)| n.starts_with("parted_touch")).collect();
    assert_eq!(parted, [&("parted_touch (parted)", "trigger")], "the parent's, not each partition's copy");
    let def = |name: &str| {
        let r = objects.iter().find(|r| r.schema == "podelis_obj" && r.name == name).unwrap().clone();
        let db = db.clone();
        async move { db.routine_definition(&r).await.unwrap() }
    };
    assert!(def("mood").await.contains("ENUM ('sad', 'ok', 'happy')"));
    let positive = def("positive").await;
    assert!(positive.contains("NOT NULL") && positive.contains("CHECK"), "{positive}");
    assert_eq!(positive.matches("NOT NULL").count(), 1, "said once: {positive}");
    assert!(def("pair").await.contains("b text"));
    assert!(def("tickets").await.contains("START WITH 100"));
    // (The table's schema is left off while it's on the search path.)
    let trigger = def("logged_touch (logged)").await;
    assert!(trigger.contains("BEFORE INSERT ON") && trigger.contains("logged"), "{trigger}");
    db.run("DROP SCHEMA podelis_obj CASCADE; RESET search_path", DEFAULT_ROW_LIMIT).await.unwrap();
}

#[tokio::test]
async fn live_postgres_notices_are_kept_for_the_app() {
    let Some(db) = live_db(Engine::Postgres).await else { return };
    let _ = db.take_notices();
    let o = db
        .run("DO $$ BEGIN RAISE NOTICE 'counted %', 3; RAISE WARNING 'careful'; END $$", DEFAULT_ROW_LIMIT)
        .await
        .unwrap();
    assert_eq!(o.notices, ["NOTICE: counted 3", "WARNING: careful"], "with the run's own outcome");
    assert!(db.take_notices().is_empty(), "not left waiting too");
    // What Podėlis's own statements left on the session isn't the next run's.
    db.with_session(|session| {
        Box::pin(async move {
            let _ = session.run("DO $$ BEGIN RAISE NOTICE 'podelis own'; END $$", 1).await;
        })
    })
    .await
    .unwrap();
    let o = db.run("SELECT 1", DEFAULT_ROW_LIMIT).await.unwrap();
    assert!(o.notices.is_empty(), "{:?}", o.notices);
    // A streamed read's, with its first page.
    db.run("CREATE OR REPLACE FUNCTION podelis_says() RETURNS int LANGUAGE plpgsql AS \
            'BEGIN RAISE NOTICE ''said''; RETURN 1; END'", DEFAULT_ROW_LIMIT)
        .await
        .unwrap();
    let (o, _) = db.run_streaming("SELECT podelis_says()", 10).await.unwrap();
    assert_eq!(o.notices, ["NOTICE: said"]);
    db.run("DROP FUNCTION podelis_says()", DEFAULT_ROW_LIMIT).await.unwrap();
    // A failing run's notices before it failed are kept too.
    let failed = db.run("DO $$ BEGIN RAISE NOTICE 'before'; RAISE EXCEPTION 'no'; END $$", DEFAULT_ROW_LIMIT).await;
    assert!(failed.is_err());
    assert_eq!(db.take_notices(), ["NOTICE: before"]);
}

/// A session advisory lock outlives a ROLLBACK: remembered all the same.
#[tokio::test]
async fn live_an_advisory_lock_in_a_rolled_back_transaction_is_remembered() {
    let Some(db) = live_db(Engine::Postgres).await else { return };
    db.run("BEGIN; SELECT pg_advisory_lock(4242); ROLLBACK", DEFAULT_ROW_LIMIT).await.unwrap();
    assert!(db.remembered().iter().any(|s| s == "advisory locks"), "{:?}", db.remembered());
    db.run("SELECT pg_advisory_unlock_all()", DEFAULT_ROW_LIMIT).await.unwrap();
    assert!(!db.remembered().iter().any(|s| s == "advisory locks"));
    // In a run that fails after it, too.
    assert!(db.run("SELECT pg_advisory_lock(4243); SELECT 1/0", DEFAULT_ROW_LIMIT).await.is_err());
    assert!(db.remembered().iter().any(|s| s == "advisory locks"), "{:?}", db.remembered());
    db.run("SELECT pg_advisory_unlock_all()", DEFAULT_ROW_LIMIT).await.unwrap();
    // A try that another session's lock turns down: not held, not said.
    let Some(other) = live_db(Engine::Postgres).await else { return };
    other.run("SELECT pg_advisory_lock(4244)", DEFAULT_ROW_LIMIT).await.unwrap();
    let o = db.run("SELECT pg_try_advisory_lock(4244)", DEFAULT_ROW_LIMIT).await.unwrap();
    assert_eq!(cells(&o), vec![vec![Some("false")]]);
    assert!(!db.remembered().iter().any(|s| s == "advisory locks"), "{:?}", db.remembered());
    other.run("SELECT pg_advisory_unlock_all()", DEFAULT_ROW_LIMIT).await.unwrap();
    // A statement that fails after taking one: held all the same, by a
    // run and streamed. (Divided by what's known only as it runs: `1/0`
    // fails while being planned, before the lock is taken.)
    assert!(db.run("SELECT pg_advisory_lock(4247), 1 / (pg_backend_pid() * 0)", DEFAULT_ROW_LIMIT).await.is_err());
    let held = db.run("SELECT count(*)::text FROM pg_locks WHERE locktype = 'advisory' AND pid = pg_backend_pid()", 1);
    assert_eq!(cells(&held.await.unwrap()), vec![vec![Some("1")]], "the server holds it");
    assert!(db.remembered().iter().any(|s| s == "advisory locks"), "{:?}", db.remembered());
    db.run("SELECT pg_advisory_unlock_all()", DEFAULT_ROW_LIMIT).await.unwrap();
    assert!(db.run_streaming("SELECT pg_advisory_lock(4248), 1 / (pg_backend_pid() * 0)", 10).await.is_err());
    assert!(db.remembered().iter().any(|s| s == "advisory locks"), "{:?}", db.remembered());
    db.run("SELECT pg_advisory_unlock_all()", DEFAULT_ROW_LIMIT).await.unwrap();
    // Streamed, as the app runs a lone read.
    let (_, _) = db.run_streaming("SELECT pg_advisory_lock(4245)", 10).await.unwrap();
    assert!(db.remembered().iter().any(|s| s == "advisory locks"), "{:?}", db.remembered());
    db.run("SELECT pg_advisory_unlock_all()", DEFAULT_ROW_LIMIT).await.unwrap();
    // Several rows, or the call not the only column: what came back says
    // nothing, and the locks stay noted.
    db.run("SELECT 1 AS id, pg_try_advisory_lock(4246)", DEFAULT_ROW_LIMIT).await.unwrap();
    assert!(db.remembered().iter().any(|s| s == "advisory locks"), "{:?}", db.remembered());
    db.run("SELECT pg_advisory_unlock_all()", DEFAULT_ROW_LIMIT).await.unwrap();
}

/// A check that finds another server session than the connection's, with
/// no reconnect, says a pooler moved it.
#[tokio::test]
async fn live_a_server_session_that_changes_by_itself_is_said() {
    let Some(db) = live_db(Engine::Postgres).await else { return };
    db.check_now().await.unwrap();
    assert!(!db.health().borrow().pooled, "the same session: no pooler");
    // As a pooler would show it: the session's pid isn't the one it had.
    db.0.backend_id.store(-1, std::sync::atomic::Ordering::SeqCst);
    db.check_now().await.unwrap();
    assert!(db.health().borrow().pooled);
}

/// `d` on a table: the CREATE TABLE as the server keeps it, every detail.
#[tokio::test]
async fn live_postgres_table_ddl_has_every_detail() {
    let Some(db) = live_db(Engine::Postgres).await else { return };
    db.run(
        "DROP SCHEMA IF EXISTS podelis_ddl CASCADE; CREATE SCHEMA podelis_ddl; SET search_path = podelis_ddl;
         CREATE TABLE users (id int GENERATED ALWAYS AS IDENTITY PRIMARY KEY, email text UNIQUE NOT NULL);
         CREATE TABLE orders (
             id bigint GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY,
             user_id int REFERENCES users (id) ON DELETE CASCADE ON UPDATE SET NULL DEFERRABLE,
             total numeric(10,2) NOT NULL DEFAULT 0 CHECK (total >= 0),
             cents bigint GENERATED ALWAYS AS ((total * 100)::bigint) STORED,
             note text COLLATE \"C\");
         CREATE INDEX orders_note ON orders (note) WHERE note IS NOT NULL;
         CREATE FUNCTION touch() RETURNS trigger LANGUAGE plpgsql AS 'BEGIN RETURN NEW; END';
         CREATE TRIGGER orders_touch BEFORE UPDATE ON orders FOR EACH ROW EXECUTE FUNCTION touch();
         ALTER TABLE orders ENABLE ROW LEVEL SECURITY;
         CREATE POLICY own ON orders USING (user_id = 1);
         COMMENT ON TABLE orders IS 'One row per order';
         COMMENT ON COLUMN orders.total IS 'VAT''s in';
         RESET search_path",
        DEFAULT_ROW_LIMIT,
    )
    .await
    .unwrap();
    let details = db.table_details("podelis_ddl", "orders").await.unwrap();
    let fk = details.foreign_keys.first().expect("the foreign key");
    assert_eq!((fk.on_delete.as_deref(), fk.on_update.as_deref()), (Some("CASCADE"), Some("SET NULL")));
    let ddl = db.table_ddl("podelis_ddl", "orders").await.unwrap().expect("a CREATE TABLE");
    // Each part as the server writes it (its brackets differ by version).
    for part in [
        "CREATE TABLE podelis_ddl.orders (",
        "id bigint GENERATED BY DEFAULT AS IDENTITY NOT NULL",
        "total numeric(10,2) DEFAULT 0 NOT NULL",
        "cents bigint GENERATED ALWAYS AS (",
        ") STORED",
        "note text COLLATE \"C\"",
        "FOREIGN KEY (user_id) REFERENCES podelis_ddl.users(id)",
        "ON DELETE CASCADE",
        "ON UPDATE SET NULL",
        "DEFERRABLE",
        "CHECK (total >= 0::numeric)",
        "CREATE INDEX orders_note ON podelis_ddl.orders USING btree (note) WHERE",
        "CREATE TRIGGER orders_touch BEFORE UPDATE ON podelis_ddl.orders",
        "ALTER TABLE podelis_ddl.orders ENABLE ROW LEVEL SECURITY;",
        "CREATE POLICY own ON podelis_ddl.orders AS PERMISSIVE FOR ALL TO public USING ((user_id = 1));",
        "COMMENT ON TABLE podelis_ddl.orders IS 'One row per order';",
        "COMMENT ON COLUMN podelis_ddl.orders.total IS 'VAT''s in';",
    ] {
        assert!(ddl.contains(part), "{part}\n\nin\n\n{ddl}");
    }
    // The key's index isn't said twice (its constraint says it).
    assert!(!ddl.contains("CREATE UNIQUE INDEX orders_pkey"), "{ddl}");
    // A view has none: its query is its definition.
    db.run("CREATE VIEW podelis_ddl.v AS SELECT 1 AS x", DEFAULT_ROW_LIMIT).await.unwrap();
    assert!(db.table_ddl("podelis_ddl", "v").await.unwrap().is_none());
    db.run("DROP SCHEMA podelis_ddl CASCADE", DEFAULT_ROW_LIMIT).await.unwrap();
}

/// Tables made from another: a partition, a child, a typed table, each
/// with only what's its own; quoted roles, a collation from another schema,
/// and (18 on) a virtual generated column.
#[tokio::test]
async fn live_postgres_ddl_of_partitions_children_and_typed_tables() {
    let Some(db) = live_db(Engine::Postgres).await else { return };
    db.run(
        "DROP SCHEMA IF EXISTS podelis_ddl2 CASCADE; CREATE SCHEMA podelis_ddl2; SET search_path = podelis_ddl2;
         DROP ROLE IF EXISTS \"App Users\"; CREATE ROLE \"App Users\";
         CREATE COLLATION podelis_ddl2.mine FROM \"C\";
         CREATE TABLE m (id int, x int CHECK (x > 0)) PARTITION BY RANGE (id);
         CREATE INDEX m_x ON m (x);
         CREATE FUNCTION touch() RETURNS trigger LANGUAGE plpgsql AS 'BEGIN RETURN NEW; END';
         CREATE TRIGGER m_touch BEFORE UPDATE ON m FOR EACH ROW EXECUTE FUNCTION touch();
         CREATE TABLE m_1 PARTITION OF m FOR VALUES FROM (0) TO (10);
         ALTER TABLE m_1 ADD CONSTRAINT m_1_small CHECK (id < 5);
         ALTER TABLE m_1 ALTER COLUMN x SET DEFAULT 5;
         CREATE TABLE parent (id int, note text COLLATE podelis_ddl2.mine);
         CREATE TABLE child (extra int) INHERITS (parent);
         CREATE TYPE pair AS (a int, b text);
         CREATE TABLE typed OF pair (PRIMARY KEY (a), b WITH OPTIONS NOT NULL);
         ALTER TABLE parent ENABLE ROW LEVEL SECURITY;
         CREATE POLICY p ON parent TO \"App Users\" USING (true);
         RESET search_path",
        DEFAULT_ROW_LIMIT,
    )
    .await
    .unwrap();
    let ddl = |table: &'static str| {
        let db = db.clone();
        async move { db.table_ddl("podelis_ddl2", table).await.unwrap().expect(table) }
    };
    let part = ddl("m_1").await;
    assert!(part.starts_with("CREATE TABLE podelis_ddl2.m_1 PARTITION OF podelis_ddl2.m ("), "{part}");
    assert!(part.contains("CONSTRAINT m_1_small CHECK") && part.contains("FOR VALUES FROM (0) TO (10)"), "{part}");
    assert!(!part.contains("x > 0"), "the parent's CHECK is the parent's: {part}");
    assert!(!part.contains("m_touch") && !part.contains("CREATE INDEX"), "the parent's copies aren't its: {part}");
    assert!(part.contains("ALTER TABLE podelis_ddl2.m_1 ALTER COLUMN x SET DEFAULT 5;"), "{part}");
    let child = ddl("child").await;
    assert!(child.contains("extra integer") && child.contains("INHERITS (podelis_ddl2.parent)"), "{child}");
    assert!(!child.contains("note text"), "its parent's columns aren't its own: {child}");
    let typed = ddl("typed").await;
    assert!(typed.starts_with("CREATE TABLE podelis_ddl2.typed OF podelis_ddl2.pair ("), "{typed}");
    assert!(!typed.contains("a integer"), "its type's columns: {typed}");
    assert!(typed.contains("ALTER TABLE podelis_ddl2.typed ALTER COLUMN b SET NOT NULL;"), "{typed}");
    let parent = ddl("parent").await;
    assert!(parent.contains("COLLATE podelis_ddl2.mine"), "{parent}");
    assert!(parent.contains("TO \"App Users\" USING (true)"), "{parent}");
    // A temporary table: as it's made, the session's.
    let o = db.run("CREATE TEMPORARY TABLE scratch (n int); SELECT pg_my_temp_schema()::regnamespace::text", 1);
    let temp_schema = cells(&o.await.unwrap())[0][0].unwrap().to_string();
    db.run("CREATE INDEX scratch_n ON scratch (n); COMMENT ON TABLE scratch IS 'mine'", 1).await.unwrap();
    let scratch = db.table_ddl(&temp_schema, "scratch").await.unwrap().expect("scratch");
    assert!(scratch.starts_with("CREATE TEMPORARY TABLE scratch ("), "{scratch}");
    // After it, as any session's own (`pg_temp`), not this one's number.
    assert!(scratch.contains("CREATE INDEX scratch_n ON pg_temp.scratch USING btree (n);"), "{scratch}");
    assert!(scratch.contains("COMMENT ON TABLE pg_temp.scratch IS 'mine';"), "{scratch}");
    assert!(!scratch.contains(&format!("{temp_schema}.")), "{scratch}");
    // A string with the session's schema in it stays as it is.
    db.run(&format!("COMMENT ON COLUMN scratch.n IS 'was {temp_schema}.scratch'"), 1).await.unwrap();
    let scratch = db.table_ddl(&temp_schema, "scratch").await.unwrap().expect("scratch");
    assert!(scratch.contains(&format!("IS 'was {temp_schema}.scratch';")), "{scratch}");
    // Its sequence named in the CREATE too.
    let o = db.run("CREATE TEMPORARY TABLE scratch2 (id int GENERATED ALWAYS AS IDENTITY (SEQUENCE NAME pg_temp.s2))", 1);
    o.await.unwrap();
    let scratch2 = db.table_ddl(&temp_schema, "scratch2").await.unwrap().expect("scratch2");
    assert!(scratch2.contains("(SEQUENCE NAME pg_temp.s2)"), "{scratch2}");
    // PostgreSQL 18's virtual generated columns.
    if crate::db::server::parse_version(db.version()).is_some_and(|v| v.0 >= 18) {
        db.run("CREATE TABLE podelis_ddl2.v (a int, b int GENERATED ALWAYS AS (a * 2) VIRTUAL)", 1).await.unwrap();
        let v = ddl("v").await;
        assert!(v.contains("b integer GENERATED ALWAYS AS ((a * 2)) VIRTUAL"), "{v}");
    }
    db.run("DROP SCHEMA podelis_ddl2 CASCADE; DROP ROLE \"App Users\"", DEFAULT_ROW_LIMIT).await.unwrap();
}

/// What a table keeps beside its columns: identity sequences' options,
/// storage parameters, replica identity, comments on its constraints,
/// indexes and triggers; a foreign table with its server and options;
/// 18's NOT NULL constraints. The DDL runs again elsewhere as it is.
#[tokio::test]
async fn live_postgres_ddl_keeps_storage_identity_and_foreign_tables() {
    let Some(db) = live_db(Engine::Postgres).await else { return };
    db.run(
        "DROP SCHEMA IF EXISTS podelis_ddl3 CASCADE; DROP SCHEMA IF EXISTS podelis_ddl3b CASCADE;
         CREATE SCHEMA podelis_ddl3; CREATE SCHEMA podelis_ddl3b; SET search_path = podelis_ddl3;
         CREATE TABLE t (
             id int GENERATED ALWAYS AS IDENTITY (START WITH 100 INCREMENT BY 5 CACHE 10),
             b bigint GENERATED BY DEFAULT AS IDENTITY (SEQUENCE NAME podelis_ddl3.b_numbers),
             w bigint GENERATED BY DEFAULT AS IDENTITY,
             u int NOT NULL,
             note text,
             CONSTRAINT t_pk PRIMARY KEY (id)
         ) WITH (fillfactor = 70, toast.autovacuum_enabled = false);
         ALTER SEQUENCE t_w_seq AS integer;
         CREATE UNIQUE INDEX t_u ON t (u);
         ALTER TABLE t REPLICA IDENTITY USING INDEX t_u;
         COMMENT ON CONSTRAINT t_pk ON t IS 'the key';
         COMMENT ON INDEX t_u IS 'unique u';
         CREATE FUNCTION touch() RETURNS trigger LANGUAGE plpgsql AS 'BEGIN RETURN NEW; END';
         CREATE FUNCTION podelis_ddl3b.touch() RETURNS trigger LANGUAGE plpgsql AS 'BEGIN RETURN NEW; END';
         CREATE TRIGGER t_touch BEFORE UPDATE ON t FOR EACH ROW EXECUTE FUNCTION touch();
         COMMENT ON TRIGGER t_touch ON t IS 'touches';
         CREATE TABLE f (a int);
         ALTER TABLE f REPLICA IDENTITY FULL;
         CREATE EXTENSION IF NOT EXISTS postgres_fdw SCHEMA public;
         DROP SERVER IF EXISTS podelis_far CASCADE;
         CREATE SERVER podelis_far FOREIGN DATA WRAPPER postgres_fdw OPTIONS (host 'localhost', dbname 'x');
         CREATE FOREIGN TABLE ft (a int OPTIONS (column_name 'b'), c text NOT NULL)
             SERVER podelis_far OPTIONS (table_name 'far''s');
         COMMENT ON FOREIGN TABLE ft IS 'far';
         CREATE TABLE a_table_whose_name_is_long_enough_that_its_names_are_shortened (id int GENERATED ALWAYS AS IDENTITY NOT NULL);
         RESET search_path",
        DEFAULT_ROW_LIMIT,
    )
    .await
    .unwrap();
    let ddl = |table: &'static str| {
        let db = db.clone();
        async move { db.table_ddl("podelis_ddl3", table).await.unwrap().expect(table) }
    };
    let t = ddl("t").await;
    for part in [
        "id integer GENERATED ALWAYS AS IDENTITY (START WITH 100 INCREMENT BY 5 CACHE 10) NOT NULL",
        "b bigint GENERATED BY DEFAULT AS IDENTITY (SEQUENCE NAME podelis_ddl3.b_numbers) NOT NULL",
        "w bigint GENERATED BY DEFAULT AS IDENTITY (MAXVALUE 2147483647) NOT NULL",
        "ALTER SEQUENCE podelis_ddl3.t_w_seq AS integer;",
        ") WITH (fillfactor=70, toast.autovacuum_enabled=false);",
        "ALTER TABLE podelis_ddl3.t REPLICA IDENTITY USING INDEX t_u;",
        "COMMENT ON CONSTRAINT t_pk ON podelis_ddl3.t IS 'the key';",
        "COMMENT ON INDEX podelis_ddl3.t_u IS 'unique u';",
        "COMMENT ON TRIGGER t_touch ON podelis_ddl3.t IS 'touches';",
    ] {
        assert!(t.contains(part), "{part}\n\nin\n\n{t}");
    }
    let f = ddl("f").await;
    assert!(f.contains("ALTER TABLE podelis_ddl3.f REPLICA IDENTITY FULL;"), "{f}");
    let ft = ddl("ft").await;
    assert!(ft.starts_with("CREATE FOREIGN TABLE podelis_ddl3.ft ("), "{ft}");
    assert!(ft.contains("a integer OPTIONS (column_name 'b')") && ft.contains("c text NOT NULL"), "{ft}");
    assert!(ft.contains(") SERVER podelis_far OPTIONS (table_name 'far''s');"), "{ft}");
    assert!(ft.contains("COMMENT ON FOREIGN TABLE podelis_ddl3.ft IS 'far';"), "{ft}");
    // Its sequence's (and 18's NOT NULL's) name shortened as the server
    // shortens it: the server's own, not said.
    let long = ddl("a_table_whose_name_is_long_enough_that_its_names_are_shortened").await;
    assert!(long.contains("id integer GENERATED ALWAYS AS IDENTITY NOT NULL"), "{long}");
    let again = vec![t, f, ft, long];
    // Each runs again as it's written, in another schema.
    for ddl in again {
        let elsewhere = ddl.replace("podelis_ddl3.", "podelis_ddl3b.");
        db.run(&elsewhere, DEFAULT_ROW_LIMIT).await.unwrap_or_else(|e| panic!("{e}\n\n{elsewhere}"));
    }
    db.run("DROP SERVER podelis_far CASCADE; DROP SCHEMA podelis_ddl3 CASCADE; DROP SCHEMA podelis_ddl3b CASCADE", 1)
        .await
        .unwrap();
}

/// PostgreSQL 18's NOT NULL constraints: named, NO INHERIT, NOT VALID (its
/// own added as it was; a parent's not again, nor checked by the child's).
/// The DDL runs again elsewhere as it is.
#[tokio::test]
async fn live_postgres_18_ddl_keeps_not_null_constraints() {
    let Some(db) = live_db(Engine::Postgres).await else { return };
    if crate::db::server::parse_version(db.version()).is_none_or(|v| v.0 < 18) {
        return;
    }
    db.run(
        "DROP SCHEMA IF EXISTS podelis_ddl4 CASCADE; DROP SCHEMA IF EXISTS podelis_ddl4b CASCADE;
         CREATE SCHEMA podelis_ddl4; CREATE SCHEMA podelis_ddl4b",
        DEFAULT_ROW_LIMIT,
    )
    .await
    .unwrap();
    let ddl = |table: &'static str| {
        let db = db.clone();
        async move { db.table_ddl("podelis_ddl4", table).await.unwrap().expect(table) }
    };
    let mut again = Vec::new();
    db.run(
        "CREATE TABLE podelis_ddl4.nn (a int, b int CONSTRAINT my_b NOT NULL, c int NOT NULL NO INHERIT);
         INSERT INTO podelis_ddl4.nn VALUES (NULL, 1, 1);
         ALTER TABLE podelis_ddl4.nn ADD CONSTRAINT a_nn NOT NULL a NOT VALID",
        DEFAULT_ROW_LIMIT,
    )
    .await
    .unwrap();
    let nn = ddl("nn").await;
    for part in [
        "a integer,",
        "b integer CONSTRAINT my_b NOT NULL",
        "c integer NOT NULL NO INHERIT",
        "ALTER TABLE podelis_ddl4.nn ADD CONSTRAINT a_nn NOT NULL a NOT VALID;",
    ] {
        assert!(nn.contains(part), "{part}\n\nin\n\n{nn}");
    }
    again.push(nn);
    // A child's NOT VALID NOT NULL is its parent's: not added again,
    // nor checked (SET NOT NULL) by the child's.
    db.run(
        "CREATE TABLE podelis_ddl4.ip (a int); CREATE TABLE podelis_ddl4.ic (b int) INHERITS (podelis_ddl4.ip);
         INSERT INTO podelis_ddl4.ic VALUES (NULL, 1);
         ALTER TABLE podelis_ddl4.ip ADD CONSTRAINT ip_a NOT NULL a NOT VALID",
        DEFAULT_ROW_LIMIT,
    )
    .await
    .unwrap();
    let (ip, ic) = (ddl("ip").await, ddl("ic").await);
    assert!(ip.contains("ADD CONSTRAINT ip_a NOT NULL a NOT VALID;"), "{ip}");
    assert!(!ic.contains("NOT VALID") && !ic.contains("SET NOT NULL"), "{ic}");
    again.extend([ip, ic]);
    // A partition's and a typed table's own NOT NULLs (their columns aren't
    // written): by their names, NO INHERIT kept.
    db.run(
        "CREATE TABLE podelis_ddl4.m (id int, x int) PARTITION BY RANGE (id);
         CREATE TABLE podelis_ddl4.m1 PARTITION OF podelis_ddl4.m FOR VALUES FROM (0) TO (10);
         ALTER TABLE podelis_ddl4.m1 ADD CONSTRAINT nn_x NOT NULL x NO INHERIT;
         CREATE TYPE podelis_ddl4.pair AS (a int, b text);
         CREATE TABLE podelis_ddl4.typed OF podelis_ddl4.pair;
         ALTER TABLE podelis_ddl4.typed ADD CONSTRAINT typed_b_nn NOT NULL b;
         CREATE TYPE podelis_ddl4b.pair AS (a int, b text)",
        DEFAULT_ROW_LIMIT,
    )
    .await
    .unwrap();
    let (m, m1, typed) = (ddl("m").await, ddl("m1").await, ddl("typed").await);
    assert!(m1.contains("ALTER TABLE podelis_ddl4.m1 ADD CONSTRAINT nn_x NOT NULL x NO INHERIT;"), "{m1}");
    assert!(typed.contains("ALTER TABLE podelis_ddl4.typed ADD CONSTRAINT typed_b_nn NOT NULL b;"), "{typed}");
    again.extend([m, m1, typed]);
    for ddl in again {
        let elsewhere = ddl.replace("podelis_ddl4.", "podelis_ddl4b.");
        db.run(&elsewhere, DEFAULT_ROW_LIMIT).await.unwrap_or_else(|e| panic!("{e}\n\n{elsewhere}"));
    }
    db.run("DROP SCHEMA podelis_ddl4 CASCADE; DROP SCHEMA podelis_ddl4b CASCADE", 1).await.unwrap();
}
