//! SQLite's own `Database` tests (the shared ones and their helpers
//! are `db::tests`); live ones run with the `PODELIS_TEST_*` server set.

use crate::db::tests::*;
use crate::db::*;

#[tokio::test]
async fn a_sqlite_path_keeps_its_brackets() {
    // Brackets are stripped from IPv6 hosts, not from file names.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("[data].db");
    let params = sqlite_params(&path, false);
    assert_eq!(params.file_to_create(), Some(path.to_string_lossy().as_ref()));
    let db = Database::connect(params).await.unwrap();
    db.close().await;
    assert!(path.exists() && !dir.path().join("data].db").exists());
}

/// SQLite needs no server, so this always runs.
#[tokio::test]
async fn live_sqlite() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.db");
    let db = Database::connect(sqlite_params(&path, false)).await.unwrap();
    live_common(&db).await;
    // A query that never ends stops when cancelled; a cancel with nothing
    // running doesn't stop the next one.
    live_cancel(&db, "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n) SELECT count(*) FROM n").await;
    db.cancel().await.unwrap();
    assert_eq!(cells(&db.run("SELECT 8", DEFAULT_ROW_LIMIT).await.unwrap()), vec![vec![Some("8")]]);
    let o = db.run("SELECT 1, 2.5, 'žąsis', X'00FF', NULL", DEFAULT_ROW_LIMIT).await.unwrap();
    assert_eq!(cells(&o), vec![vec![Some("1"), Some("2.5"), Some("žąsis"), Some("X'00FF'"), None]]);
    db.run("CREATE TABLE people (id INTEGER NOT NULL, nick TEXT); CREATE VIEW v AS SELECT 1", DEFAULT_ROW_LIMIT)
        .await
        .unwrap();
    let tables = db.tables(false).await.unwrap();
    let names: Vec<_> = tables.iter().map(|t| (t.name.as_str(), t.is_view)).collect();
    assert_eq!(names, [("people", false), ("v", true)]);
    let catalog = db.catalog(false).await.unwrap();
    let people = catalog.iter().find(|(t, _)| t.name == "people").unwrap();
    assert_eq!(people.1.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["id", "nick"]);
    assert!(catalog.iter().all(|(t, _)| !t.system));
    // System tables: sqlite_schema (never listed in itself) with its columns.
    let catalog = db.catalog(true).await.unwrap();
    let schema = catalog.iter().find(|(t, _)| t.name == "sqlite_schema").unwrap();
    assert!(schema.0.system && schema.1.iter().any(|c| c.name == "sql"));
    assert!(catalog.iter().any(|(t, _)| t.name == "people" && !t.system));
    let cols = db.columns("main", "people").await.unwrap();
    assert_eq!(
        cols.iter().map(|c| (c.name.as_str(), c.data_type.as_str(), c.nullable)).collect::<Vec<_>>(),
        [("id", "INTEGER", false), ("nick", "TEXT", true)]
    );
    assert_eq!(db.transaction_state().await, None);
    assert!(db.version().starts_with('3'), "SQLite 3.x: {}", db.version());
    db.close().await;

    // Read-only mode is enforced by SQLite itself.
    let ro = Database::connect(sqlite_params(&path, true)).await.unwrap();
    assert!(ro.run("SELECT count(*) FROM people", DEFAULT_ROW_LIMIT).await.is_ok());
    assert!(ro.run("INSERT INTO people VALUES (1, 'x')", DEFAULT_ROW_LIMIT).await.is_err());
}

#[tokio::test]
async fn live_check_statement_sqlite() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::connect(sqlite_params(&dir.path().join("c.db"), false)).await.unwrap();
    live_check(&db).await;
    let CheckOutcome::Problem { position, .. } = db.check_statement("SELECT * FRO t").await.unwrap() else { panic!() };
    assert_eq!(position, Some(ErrorPos::Token("FRO".into())));
}

#[tokio::test]
async fn live_details_sqlite() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::connect(sqlite_params(&dir.path().join("d.db"), false)).await.unwrap();
    live_details(&db, "main", DETAILS_TABLES).await;
    assert!(db.routines().await.unwrap().is_empty());
}
