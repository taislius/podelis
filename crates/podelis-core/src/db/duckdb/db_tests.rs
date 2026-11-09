//! DuckDB's own `Database` tests (the shared ones and their helpers are
//! `db::tests`). It needs no server: a file is its database, so these
//! always run.

use crate::db::tests::*;
use crate::db::*;

/// No server: a file is its database.
pub(in crate::db) fn live_params() -> Option<ConnectParams> {
    None
}

/// A DuckDB database at `path`.
fn params(path: &std::path::Path, read_only: bool) -> ConnectParams {
    ConnectParams {
        engine: Engine::DuckDb,
        host: path.to_string_lossy().into_owned(),
        port: 0,
        user: String::new(),
        password: String::new(),
        database: String::new(),
        ssl_mode: SslMode::Disable,
        read_only,
        ssh: None,
        password_command: Vec::new(),
        for_agents: false,
    }
}

/// A new file, `name.db`.
pub(in crate::db) async fn own_live_db(name: &str) -> Option<(Database, ConnectParams)> {
    let params = params(&scratch_file(&format!("{name}-duckdb")), false);
    Some((Database::connect(params.clone()).await.expect("open"), params))
}

pub static TEST_SQL: TestSql = TestSql {
    sleep: None,
    count_to_2500: "SELECT * FROM range(1, 2501) t(g)",
    plan_shows: &["SEQ_SCAN", "TABLE_SCAN", "FILTER"],
    analyzed_shows: "Total Time",
    analyze: "ANALYZE {table}",
    binary: ["CREATE TEMP TABLE bin_t (b BLOB)", "'\\x00\\xFF''\\x5C'::BLOB", "00ff275c"],
    comments: Some(&[
        "CREATE TABLE noted (id INTEGER, total DECIMAL(10,2), plain VARCHAR)",
        "COMMENT ON TABLE noted IS 'One row per checkout'",
        "COMMENT ON COLUMN noted.total IS 'VAT included'",
    ]),
    sequence: None,
    read_only_off: None,
    without_rollback: None,
};

#[cfg(test)]
async fn open(name: &str) -> (tempfile::TempDir, Database) {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::connect(params(&dir.path().join(format!("{name}.duckdb")), false)).await.unwrap();
    (dir, db)
}

#[tokio::test]
async fn live_duckdb() {
    let (dir, db) = open("t").await;
    live_common(&db).await;
    // A query that would take long stops when cancelled; a cancel with
    // nothing running doesn't stop the next one.
    live_cancel(&db, "SELECT count(*) FROM range(10000000000) a, range(1000) b", |r| {
        let err = r.unwrap_err();
        assert!(format!("{err:#}").contains("Interrupt"), "{err:#}");
    })
    .await;
    db.cancel().await.unwrap();
    assert_eq!(cells(&db.run("SELECT 8", DEFAULT_ROW_LIMIT).await.unwrap()), vec![vec![Some("8")]]);
    let o = db.run("SELECT 1, 2.5, 'žąsis', NULL, [1, 2], {'a': 1}", DEFAULT_ROW_LIMIT).await.unwrap();
    assert_eq!(cells(&o), vec![vec![Some("1"), Some("2.5"), Some("žąsis"), None, Some("[1, 2]"), Some("{a: 1}")]]);
    db.run("CREATE TABLE people (id INTEGER NOT NULL, nick VARCHAR); CREATE VIEW v AS SELECT 1 AS one", DEFAULT_ROW_LIMIT)
        .await
        .unwrap();
    let tables = db.tables(false).await.unwrap();
    let names: Vec<_> = tables.iter().map(|t| (t.schema.as_str(), t.name.as_str(), t.is_view)).collect();
    assert_eq!(names, [("main", "people", false), ("main", "v", true)]);
    let cols = db.columns("main", "people").await.unwrap();
    assert_eq!(
        cols.iter().map(|c| (c.name.as_str(), c.data_type.as_str(), c.nullable)).collect::<Vec<_>>(),
        [("id", "INTEGER", false), ("nick", "VARCHAR", true)]
    );
    let o = db.run("INSERT INTO people VALUES (1, 'a'), (2, 'b')", DEFAULT_ROW_LIMIT).await.unwrap();
    assert_eq!((o.rows_affected, o.columns.len()), (Some(2), 0));
    assert_eq!(db.transaction_state().await, None);
    assert!(db.version().starts_with('v'), "DuckDB's version: {}", db.version());
    let comments = test_sql(Engine::DuckDb).comments.unwrap();
    db.run(&comments.join("; "), DEFAULT_ROW_LIMIT).await.unwrap();
    let noted: Vec<_> = db.comments().await.unwrap().into_iter().map(|c| (c.table, c.column, c.text)).collect();
    assert!(noted.contains(&("noted".into(), Some("total".into()), "VAT included".into())), "{noted:?}");
    db.close().await;

    // Read-only mode is DuckDB's own.
    let ro = Database::connect(params(&dir.path().join("t.duckdb"), true)).await.unwrap();
    assert!(ro.run("SELECT count(*) FROM people", DEFAULT_ROW_LIMIT).await.is_ok());
    assert!(ro.run("INSERT INTO people VALUES (3, 'c')", DEFAULT_ROW_LIMIT).await.is_err());
}

#[tokio::test]
async fn live_check_statement_duckdb() {
    let (_dir, db) = open("c").await;
    live_check(&db).await;
    let CheckOutcome::Problem { position, .. } = db.check_statement("SELECT * FRO t").await.unwrap() else { panic!() };
    // DuckDB reads `FRO` as an alias: the error is at what follows it.
    assert_eq!(position, Some(ErrorPos::Token("t".into())));
}

#[tokio::test]
async fn live_details_duckdb() {
    let (_dir, db) = open("d").await;
    live_details(&db, "main", DETAILS_TABLES, true).await;
    assert!(db.routines().await.unwrap().is_empty());
}

/// A read of a whole table is seen in its plan, with the table's size from
/// the catalog: what the agents' guard on production goes by.
#[tokio::test]
async fn live_full_scans_duckdb() {
    let (_dir, db) = open("s").await;
    db.run("CREATE TABLE big_t (id INTEGER, x INTEGER); INSERT INTO big_t SELECT i, i FROM range(5) t(i)", DEFAULT_ROW_LIMIT)
        .await
        .unwrap();
    let scans = db.full_scans("SELECT * FROM big_t WHERE x = 2").await.unwrap();
    assert_eq!(scans, [("main.big_t".to_string(), 5.0)]);
}

/// Two connections to one file share its instance: what one writes the
/// other reads. A read-only one shares it too, and only reads; one that
/// writes can't share a read-only instance.
#[tokio::test]
async fn live_one_instance_per_file_duckdb() {
    let (dir, a) = open("shared").await;
    let path = dir.path().join("shared.duckdb");
    let b = Database::connect(params(&path, false)).await.unwrap();
    a.run("CREATE TABLE seen (x INTEGER); INSERT INTO seen VALUES (1), (2)", DEFAULT_ROW_LIMIT).await.unwrap();
    assert_eq!(cells(&b.run("SELECT count(*) FROM seen", DEFAULT_ROW_LIMIT).await.unwrap()), vec![vec![Some("2")]]);
    let ro = Database::connect(params(&path, true)).await.unwrap();
    assert_eq!(cells(&ro.run("SELECT count(*) FROM seen", DEFAULT_ROW_LIMIT).await.unwrap()), vec![vec![Some("2")]]);
    let refused = ro.run("INSERT INTO seen VALUES (3)", DEFAULT_ROW_LIMIT).await.unwrap_err();
    assert!(format!("{refused:#}").contains("read-only"), "{refused:#}");
    a.close().await;
    // The other connections keep the instance open.
    assert_eq!(cells(&b.run("SELECT count(*) FROM seen", DEFAULT_ROW_LIMIT).await.unwrap()), vec![vec![Some("2")]]);
    b.close().await;
    ro.close().await;
    // Read-only first: a writer can't share it.
    let first = Database::connect(params(&path, true)).await.unwrap();
    let writer = Database::connect(params(&path, false)).await.err().unwrap();
    assert!(format!("{writer:#}").contains("one way at a time"), "{writer:#}");
    first.close().await;
}
