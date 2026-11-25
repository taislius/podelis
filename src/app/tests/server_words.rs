//! What real servers say, recorded, for the scenarios to draw as they are
//! (`behaviours`): each server's errors for the same few situations (a
//! table that isn't there, a syntax error, a duplicate key, a wrong
//! password, a refused connection, a database that isn't there) and its
//! CREATE TABLE, word for word.
//!
//! Recorded by running the live tests with `PODELIS_RECORD=1` (and each
//! server's `PODELIS_TEST_*` variables, as for any live test): what each
//! server says goes into `testdata/server_words.json`, under its name and
//! version (`PostgreSQL 17`, `MySQL 8.4`, `MariaDB 11.4`, `SQLite`). Without
//! the variable the test does nothing; the file is what's read.

use std::collections::BTreeMap;

use crate::db::{ConnectParams, Database, Engine};

/// Each server's words: situation → what it said, and `engine` → its
/// engine's name.
pub(super) type Words = BTreeMap<String, BTreeMap<String, String>>;

const FILE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/testdata/server_words.json");

/// What's recorded.
pub(super) fn recorded() -> Words {
    serde_json::from_str(include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/testdata/server_words.json")))
        .expect("testdata/server_words.json reads")
}

/// The situations a query's answer is recorded for.
#[cfg_attr(not(feature = "sqlite"), expect(dead_code, reason = "the behaviours' scenarios are SQLite's"))]
pub(super) const QUERY_SITUATIONS: [&str; 3] = ["missing_table", "syntax_error", "duplicate_key"];
/// …and connecting's.
#[cfg_attr(not(feature = "sqlite"), expect(dead_code, reason = "the behaviours' scenarios are SQLite's"))]
pub(super) const CONNECT_SITUATIONS: [&str; 3] = ["wrong_password", "refused", "unknown_database"];

/// A server's name as its words are kept: its engine and version, MariaDB
/// apart from MySQL; a file engine's by its name alone.
fn server_name(engine: Engine, version: &str) -> String {
    let numbers: Vec<&str> = version.split(|c: char| !c.is_ascii_digit()).filter(|n| !n.is_empty()).collect();
    let major_minor = numbers.iter().take(2).copied().collect::<Vec<_>>().join(".");
    match engine {
        Engine::MySql if version.contains("MariaDB") => format!("MariaDB {major_minor}"),
        Engine::MySql => format!("MySQL {major_minor}"),
        Engine::Postgres => format!("PostgreSQL {}", numbers.first().copied().unwrap_or_default()),
        other => other.label().to_string(),
    }
}

/// What the server says to `sql`, if it refuses it.
async fn refused(db: &Database, sql: &str) -> Option<String> {
    db.run(sql, 1).await.err().map(|e| crate::db::error_message(&e))
}

/// What connecting with `params` is told, if it fails.
async fn connect_refused(params: ConnectParams) -> Option<String> {
    Database::connect(params).await.err().map(|e| crate::db::error_message(&e))
}

#[tokio::test]
async fn record_what_servers_say() {
    if std::env::var_os("PODELIS_RECORD").is_none() {
        return;
    }
    let mut words = recorded();
    for engine in Engine::BUILT {
        let Some((db, params)) = crate::db::tests::own_live_db(engine, "podelis_words").await else { continue };
        let name = server_name(engine, db.version());
        let mut said = BTreeMap::from([("engine".to_string(), engine.label().to_string())]);
        let mut say = |situation: &str, words: Option<String>| {
            if let Some(words) = words {
                said.insert(situation.to_string(), words);
            }
        };
        say("missing_table", refused(&db, "SELECT * FROM podelis_no_such_table").await);
        say("syntax_error", refused(&db, "SELEC id FROM orders").await);
        let _ = db.run("DROP TABLE IF EXISTS words_orders", 1).await;
        db.run("CREATE TABLE words_orders (id INT PRIMARY KEY, note VARCHAR(40) NOT NULL DEFAULT 'none')", 1)
            .await
            .expect("a table to say things of");
        db.run("INSERT INTO words_orders (id) VALUES (1)", 1).await.expect("a row");
        say("duplicate_key", refused(&db, "INSERT INTO words_orders (id) VALUES (1)").await);
        // Its CREATE TABLE as `d` then `c` shows it (a file's: as stored).
        let schema = match engine {
            Engine::Postgres => "public".to_string(),
            Engine::MySql => params.database.clone(),
            _ => "main".to_string(),
        };
        let ddl = match db.table_ddl(&schema, "words_orders").await.ok().flatten() {
            Some(ddl) => Some(ddl),
            None => db.table_details(&schema, "words_orders").await.ok().and_then(|d| d.definition),
        };
        say("create_table", ddl);
        if !engine.is_file_based() {
            let wrong = ConnectParams { password: "podelis-wrong-password".into(), ..params.clone() };
            say("wrong_password", connect_refused(wrong).await);
            let closed = ConnectParams { host: "127.0.0.1".into(), port: 1, ..params.clone() };
            say("refused", connect_refused(closed).await);
            let missing = ConnectParams { database: "podelis_no_such_database".into(), ..params.clone() };
            say("unknown_database", connect_refused(missing).await);
        }
        let _ = db.run("DROP TABLE words_orders", 1).await;
        words.insert(name, said);
    }
    let text = serde_json::to_string_pretty(&words).expect("words as JSON");
    std::fs::write(FILE, format!("{text}\n")).expect("testdata/server_words.json written");
}

#[test]
fn servers_are_named_by_engine_and_version() {
    assert_eq!(server_name(Engine::Postgres, "17.6"), "PostgreSQL 17");
    assert_eq!(server_name(Engine::MySql, "8.4.6"), "MySQL 8.4");
    assert_eq!(server_name(Engine::MySql, "11.4.8-MariaDB-ubu2404"), "MariaDB 11.4");
    assert_eq!(server_name(Engine::Sqlite, "3.50.4"), "SQLite");
}
