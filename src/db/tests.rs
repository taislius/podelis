use super::*;

#[test]
fn quote_ident() {
    assert_eq!(Engine::Postgres.quote_ident("users"), "users");
    assert_eq!(Engine::Postgres.quote_ident("Users"), "\"Users\"");
    assert_eq!(Engine::Postgres.quote_ident("a\"b"), "\"a\"\"b\"");
    assert_eq!(Engine::MySql.quote_ident("order items"), "`order items`");
    assert_eq!(Engine::MySql.quote_ident("1abc"), "`1abc`");
}

/// Live-database tests. Skipped unless e.g. `PODELIS_TEST_MYSQL_PORT` is
/// set; also reads `_HOST`, `_USER`, `_PASSWORD` and `_DB` (defaults:
/// 127.0.0.1, root/postgres, empty, empty).
fn live_params(engine: Engine) -> Option<ConnectParams> {
    let prefix = match engine {
        Engine::Postgres => "PODELIS_TEST_PG",
        Engine::MySql => "PODELIS_TEST_MYSQL",
        Engine::Sqlite => return None,
    };
    let var = |name: &str| std::env::var(format!("{prefix}_{name}")).ok();
    Some(ConnectParams {
        engine,
        port: var("PORT")?.parse().ok()?,
        host: var("HOST").unwrap_or_else(|| "127.0.0.1".into()),
        user: var("USER").unwrap_or_else(|| match engine {
            Engine::Postgres => "postgres".into(),
            _ => "root".into(),
        }),
        password: var("PASSWORD").unwrap_or_default(),
        database: var("DB").unwrap_or_default(),
        ssl_mode: SslMode::Prefer,
        read_only: false,
        ssh: None,
        password_command: Vec::new(),
    })
}

pub(crate) async fn live_db(engine: Engine) -> Option<Database> {
    let params = live_params(engine)?;
    Some(Database::connect(params).await.expect("connect"))
}

#[test]
fn tls_failures_are_told_from_other_connect_errors() {
    let io = |text: &str| -> anyhow::Error {
        anyhow::Error::from(sqlx::Error::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, text.to_string())))
            .context("failed to connect to database")
    };
    assert_eq!(
        super::tls_failure(&io("received fatal alert: HandshakeFailure")).as_deref(),
        Some("received fatal alert: HandshakeFailure")
    );
    assert!(super::tls_failure(&io("Connection refused (os error 111)")).is_none());
    let denied = anyhow::Error::from(sqlx::Error::Protocol("Access denied for user".into()));
    assert!(super::tls_failure(&denied).is_none());
}

fn cells(o: &QueryOutcome) -> Vec<Vec<Option<&str>>> {
    o.rows.iter().map(|r| r.iter().map(|c| c.as_deref()).collect()).collect()
}

async fn live_common(db: &Database) {
    // Leading comment, multiple statements, session state persisting.
    let o = db
        .run(
            "-- setup\nCREATE TEMPORARY TABLE t (id INT, name VARCHAR(20)); \
              INSERT INTO t VALUES (1, 'a;b'), (2, NULL);",
            DEFAULT_ROW_LIMIT,
        )
        .await
        .unwrap();
    assert_eq!(o.statements, 2);
    assert_eq!(o.rows_affected, Some(2));
    let o = db.run("/* temp table survives */ SELECT id, name FROM t ORDER BY id", DEFAULT_ROW_LIMIT).await.unwrap();
    assert_eq!(o.columns, ["id", "name"]);
    assert_eq!(cells(&o), vec![vec![Some("1"), Some("a;b")], vec![Some("2"), None]]);

    // Literals built for edits survive the trip through each server.
    let tricky = r"it's a \ test";
    let o = db.run(&format!("SELECT {}", sql_literal(tricky, db.engine())), DEFAULT_ROW_LIMIT).await.unwrap();
    assert_eq!(cells(&o), vec![vec![Some(tricky)]]);

    // EXPLAIN output formats as a plan on every engine.
    let o = db
        .run(&db.engine().dialect().explain("SELECT id FROM t WHERE id = 1", db.version()), DEFAULT_ROW_LIMIT)
        .await
        .unwrap();
    let plan = db.engine().dialect().format_plan(&o.columns, &o.rows).unwrap().text;
    let expected = match db.engine() {
        Engine::Postgres => "Scan on t",
        // The classic table on MySQL before 8.0.16 and MariaDB.
        Engine::MySql if db.engine().dialect().explain("", db.version()) == "EXPLAIN " => "select_type",
        Engine::MySql => "-> ",
        Engine::Sqlite => "SCAN t",
    };
    assert!(plan.contains(expected), "{plan}");

    // Zero-row result still has column headers.
    let o = db.run("SELECT id, name FROM t WHERE id > 100", DEFAULT_ROW_LIMIT).await.unwrap();
    assert_eq!(o.columns, ["id", "name"]);
    assert_eq!(o.rows, Vec::<Vec<Option<String>>>::new());

    // A script ending in an empty SELECT still gets its headers.
    let o = db.run("UPDATE t SET id = id; SELECT name FROM t WHERE id < 0", DEFAULT_ROW_LIMIT).await.unwrap();
    assert_eq!(o.columns, ["name"]);
    assert_eq!(o.rows, Vec::<Vec<Option<String>>>::new());

    // The last row-producing statement is shown.
    let o = db.run("SELECT 1 AS a; UPDATE t SET id = id + 10", DEFAULT_ROW_LIMIT).await.unwrap();
    assert_eq!(o.columns, ["a"]);
    assert_eq!(o.statements, 2);

    // Row cap: everything is counted, only the row limit kept.
    let big = match db.engine() {
        Engine::Postgres => "SELECT g FROM generate_series(1, 2500) g",
        Engine::Sqlite => {
            "WITH RECURSIVE s(g) AS (SELECT 1 UNION ALL SELECT g + 1 FROM s WHERE g < 2500) SELECT g FROM s"
        }
        Engine::MySql => {
            "SET SESSION cte_max_recursion_depth = 5000; WITH RECURSIVE s(g) AS (SELECT 1 UNION ALL SELECT g + 1 FROM s WHERE g < 2500) SELECT g FROM s"
        }
    };
    let o = db.run(big, DEFAULT_ROW_LIMIT).await.unwrap();
    assert_eq!((o.rows.len(), o.total_rows), (DEFAULT_ROW_LIMIT, 2500));
    // Streaming: first page now, the rest on demand.
    let single = big.rsplit("; ").next().unwrap();
    let (o, stream) = db.run_streaming(single, 1000).await.unwrap();
    assert_eq!((o.rows.len(), o.columns.len()), (1000, 1));
    let stream = stream.expect("more rows to come");
    let p = stream.next_page(1000).await.unwrap();
    assert_eq!((p.rows.len(), p.done), (1000, false));
    let p = stream.next_page(1000).await.unwrap();
    assert_eq!((p.rows.len(), p.done), (500, true));
    drop(stream);
    // Dropping a stream part-way cancels it; the session frees up quickly.
    let (_, stream) = db.run_streaming(single, 10).await.unwrap();
    drop(stream);
    let after = tokio::time::timeout(Duration::from_secs(10), db.run("SELECT 5", DEFAULT_ROW_LIMIT))
        .await
        .expect("session freed after dropping the stream")
        .unwrap();
    assert_eq!(cells(&after), vec![vec![Some("5")]]);
    live_common_rest(db).await;
}

/// The part of [`live_common`] after streaming.
async fn live_common_rest(db: &Database) {
    // Empty results keep their columns and have no stream.
    let (o, stream) = db.run_streaming("SELECT name FROM t WHERE id < 0", 1000).await.unwrap();
    assert_eq!((o.columns, o.rows.len(), stream.is_none()), (vec!["name".to_string()], 0, true));

    // The session is still usable afterwards.
    assert_eq!(cells(&db.run("SELECT 42", DEFAULT_ROW_LIMIT).await.unwrap()), vec![vec![Some("42")]]);

    assert!(db.run("SELEC nonsense", DEFAULT_ROW_LIMIT).await.is_err());
    assert_eq!(cells(&db.run("SELECT 1", DEFAULT_ROW_LIMIT).await.unwrap()), vec![vec![Some("1")]]);
}

async fn live_cancel(db: &Database, slow: &str) {
    let runner = db.clone();
    let slow = slow.to_string();
    let handle = tokio::spawn(async move { runner.run(&slow, DEFAULT_ROW_LIMIT).await });
    tokio::time::sleep(Duration::from_millis(500)).await;
    db.cancel().await.unwrap();
    let result = tokio::time::timeout(Duration::from_secs(5), handle).await.expect("cancel took too long");
    match db.engine() {
        Engine::Postgres => assert!(result.unwrap().is_err()),
        // MySQL's SLEEP() returns 1 when interrupted rather than erroring.
        Engine::MySql => {
            if let Ok(o) = result.unwrap() {
                assert_eq!(cells(&o), vec![vec![Some("1")]]);
            }
        }
        Engine::Sqlite => {
            let err = result.unwrap().unwrap_err();
            assert!(format!("{err:#}").contains("interrupted"), "{err:#}");
        }
    }
    assert_eq!(cells(&db.run("SELECT 7", DEFAULT_ROW_LIMIT).await.unwrap()), vec![vec![Some("7")]]);
}

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
    live_cancel(&db, "SELECT SLEEP(10)").await;
    db.run("DROP DATABASE podelis_test", DEFAULT_ROW_LIMIT).await.unwrap();
}

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
    live_cancel(&db, "SELECT pg_sleep(10)").await;
}

fn sqlite_params(path: &std::path::Path, read_only: bool) -> ConnectParams {
    ConnectParams {
        engine: Engine::Sqlite,
        host: path.to_string_lossy().into_owned(),
        port: 0,
        user: String::new(),
        password: String::new(),
        database: String::new(),
        ssl_mode: SslMode::Disable,
        read_only,
        ssh: None,
        password_command: Vec::new(),
    }
}

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
async fn live_postgres_transaction_state_and_read_only() {
    let Some(mut params) = live_params(Engine::Postgres) else { return };
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
async fn live_mysql_error_line_and_read_only() {
    let Some(mut params) = live_params(Engine::MySql) else { return };
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

/// Opt-in: `PODELIS_TEST_SSH_DEST` names an ssh destination that can
/// reach the test database (by the same host/port as the direct test).
#[tokio::test]
async fn live_ssh_tunnel() {
    let Ok(dest) = std::env::var("PODELIS_TEST_SSH_DEST") else { return };
    let Some(mut params) = live_params(Engine::MySql).or_else(|| live_params(Engine::Postgres)) else {
        return;
    };
    params.ssh = Some(dest);
    let db = Database::connect(params.clone()).await.expect("connect through tunnel");
    assert_eq!(cells(&db.run("SELECT 1", DEFAULT_ROW_LIMIT).await.unwrap()), vec![vec![Some("1")]]);
    db.close().await;

    params.ssh = Some("podelis-no-such-host.invalid".into());
    let err = format!("{:#}", Database::connect(params).await.err().expect("bad host fails"));
    assert!(err.contains("ssh tunnel failed"), "{err}");
}

/// Checks that a statement is judged by the server without running it.
async fn live_check(db: &Database) {
    assert_eq!(db.check_statement("SELECT 1").await.unwrap(), CheckOutcome::Ok);
    let CheckOutcome::Problem { syntax, position, .. } = db.check_statement("SELECT * FRO t").await.unwrap() else {
        panic!("syntax error not reported");
    };
    assert!(syntax);
    assert!(position.is_some(), "position for syntax errors");
    let CheckOutcome::Problem { syntax, .. } = db.check_statement("SELECT * FROM podelis_no_such_table").await.unwrap()
    else {
        panic!("unknown table not reported");
    };
    assert!(!syntax);
    // Preparing doesn't execute.
    db.run("CREATE TABLE IF NOT EXISTS podelis_chk (id INT PRIMARY KEY, note TEXT)", DEFAULT_ROW_LIMIT).await.unwrap();
    let catalog = db.catalog(false).await.unwrap();
    let (_, cols) = catalog.iter().find(|(t, _)| t.name == "podelis_chk").expect("in catalog");
    let keys: Vec<_> = cols.iter().map(|c| (c.name.as_str(), c.primary_key)).collect();
    assert_eq!(keys, [("id", true), ("note", false)]);
    assert_eq!(db.check_statement("DELETE FROM podelis_chk").await.unwrap(), CheckOutcome::Ok);
    db.run("INSERT INTO podelis_chk (id) VALUES (1)", DEFAULT_ROW_LIMIT).await.unwrap();
    assert_eq!(db.check_statement("DELETE FROM podelis_chk").await.unwrap(), CheckOutcome::Ok);
    let o = db.run("SELECT count(*) FROM podelis_chk", DEFAULT_ROW_LIMIT).await.unwrap();
    assert_eq!(cells(&o), vec![vec![Some("1")]]);
    db.run("DROP TABLE podelis_chk", DEFAULT_ROW_LIMIT).await.unwrap();
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
async fn live_check_statement_postgres() {
    let Some(db) = live_db(Engine::Postgres).await else { return };
    live_check(&db).await;
}

#[tokio::test]
async fn live_check_statement_mysql() {
    let Some(db) = live_db(Engine::MySql).await else { return };
    db.run("CREATE DATABASE IF NOT EXISTS podelis_chk_db; USE podelis_chk_db", DEFAULT_ROW_LIMIT).await.unwrap();
    live_check(&db).await;
    db.run("DROP DATABASE podelis_chk_db", DEFAULT_ROW_LIMIT).await.unwrap();
}

#[test]
fn ssl_mode_cycles() {
    assert_eq!(SslMode::Prefer.cycle(true), SslMode::Require);
    assert_eq!(SslMode::Disable.cycle(false), SslMode::VerifyFull);
    assert_eq!(SslMode::VerifyFull.cycle(true), SslMode::Disable);
}

/// Indexes, foreign keys and definitions, after `setup` created the tables.
async fn live_details(db: &Database, schema: &str, setup: &str) {
    db.run(setup, DEFAULT_ROW_LIMIT).await.unwrap();
    let child = db.table_details(schema, "det_child").await.unwrap();
    let mut indexes: Vec<(bool, bool, &str)> =
        child.indexes.iter().map(|i| (i.primary, i.unique, i.columns.as_str())).collect();
    indexes.sort_unstable();
    // SQLite's INTEGER PRIMARY KEY is the rowid itself: no index.
    if db.engine() != Engine::Sqlite {
        assert!(indexes.contains(&(true, true, "id")), "{indexes:?}");
    }
    assert!(indexes.contains(&(false, true, "email")), "{indexes:?}");
    assert!(indexes.contains(&(false, false, "parent_id")), "{indexes:?}");
    let fks: Vec<(&str, &str)> = child.foreign_keys.iter().map(|f| (f.column.as_str(), f.target.as_str())).collect();
    assert_eq!(fks, [("parent_id", "det_parent.id")]);
    let view = db.table_details(schema, "det_v").await.unwrap();
    let def = view.definition.unwrap().to_lowercase();
    assert!(def.contains("det_parent"), "{def}");
}

// A table-level FOREIGN KEY: MySQL before 9.0 parses a column's inline
// REFERENCES and ignores it.
const DETAILS_TABLES: &str = "CREATE TABLE det_parent (id INTEGER PRIMARY KEY);
     CREATE TABLE det_child (id INTEGER PRIMARY KEY, parent_id INTEGER, email VARCHAR(100) UNIQUE,
                             FOREIGN KEY (parent_id) REFERENCES det_parent (id));
     CREATE INDEX det_child_parent ON det_child (parent_id);
     CREATE VIEW det_v AS SELECT id FROM det_parent;";

#[tokio::test]
async fn live_details_sqlite() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::connect(sqlite_params(&dir.path().join("d.db"), false)).await.unwrap();
    live_details(&db, "main", DETAILS_TABLES).await;
    assert!(db.routines().await.unwrap().is_empty());
}

#[tokio::test]
async fn live_details_postgres() {
    let Some(db) = live_db(Engine::Postgres).await else { return };
    let setup = format!(
        "DROP SCHEMA IF EXISTS podelis_det CASCADE; CREATE SCHEMA podelis_det; SET search_path = podelis_det;
         {DETAILS_TABLES}
         CREATE FUNCTION det_fn(x integer) RETURNS integer LANGUAGE sql AS 'SELECT x + 1';"
    );
    live_details(&db, "podelis_det", &setup).await;
    let routines = db.routines().await.unwrap();
    let f = routines.iter().find(|r| r.schema == "podelis_det").expect("det_fn listed");
    assert_eq!((f.name.as_str(), f.kind.as_str()), ("det_fn(x integer)", "function"));
    assert!(db.routine_definition(f).await.unwrap().contains("x + 1"));
    db.run("DROP SCHEMA podelis_det CASCADE", DEFAULT_ROW_LIMIT).await.unwrap();
}

#[tokio::test]
async fn live_details_mysql() {
    let Some(db) = live_db(Engine::MySql).await else { return };
    let setup = format!(
        "DROP DATABASE IF EXISTS podelis_det; CREATE DATABASE podelis_det; USE podelis_det;
         {DETAILS_TABLES}
         CREATE FUNCTION det_fn(x INT) RETURNS INT DETERMINISTIC RETURN x + 1;"
    );
    live_details(&db, "podelis_det", &setup).await;
    let routines = db.routines().await.unwrap();
    let f = routines.iter().find(|r| r.name == "det_fn").expect("det_fn listed");
    assert_eq!(f.kind, "function");
    assert!(db.routine_definition(f).await.unwrap().contains("x + 1"));
    db.run("DROP DATABASE podelis_det", DEFAULT_ROW_LIMIT).await.unwrap();
}

/// What the review flow relies on: `BEGIN;` before the writes in one run
/// leaves the transaction open, and ROLLBACK undoes them.
async fn live_begin_in_script(db: &Database, table: &str) {
    db.run(&format!("DROP TABLE IF EXISTS {table}; CREATE TABLE {table} (x INT); INSERT INTO {table} VALUES (1)"), 10)
        .await
        .unwrap();
    let o = db.run(&format!("BEGIN;\nUPDATE {table} SET x = 2 WHERE x = 1"), 10).await.unwrap();
    assert_eq!(o.rows_affected, Some(1));
    if db.engine() == Engine::Postgres {
        assert_eq!(db.transaction_state().await, Some(TxnState::Open));
    }
    db.run("ROLLBACK", 10).await.unwrap();
    let o = db.run(&format!("SELECT x FROM {table}"), 10).await.unwrap();
    assert_eq!(cells(&o), vec![vec![Some("1")]]);
    db.run(&format!("DROP TABLE {table}"), 10).await.unwrap();
}

#[tokio::test]
async fn live_begin_in_script_postgres() {
    let Some(db) = live_db(Engine::Postgres).await else { return };
    live_begin_in_script(&db, "podelis_review").await;
}

#[tokio::test]
async fn live_begin_in_script_mysql() {
    let Some(db) = live_db(Engine::MySql).await else { return };
    // Its own database: live_mysql drops podelis_test when it's done.
    db.run("CREATE DATABASE IF NOT EXISTS podelis_script; USE podelis_script", 10).await.unwrap();
    live_begin_in_script(&db, "podelis_review").await;
}

/// Killing the session's server process: a read reconnects and runs again,
/// a write reports the reconnect instead of retrying.
async fn live_reconnect(db: &Database, other: &Database, pid_sql: &str, kill: &str) {
    let pid = |o: &QueryOutcome| o.rows[0][0].clone().unwrap();
    let before = pid(&db.run(pid_sql, 10).await.unwrap());
    other.run(&kill.replace("{pid}", &before), 10).await.unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    let o = db.run(pid_sql, 10).await.expect("a read is retried after reconnecting");
    assert!(o.reconnected);
    assert_ne!(pid(&o), before);

    let now = pid(&o);
    other.run(&kill.replace("{pid}", &now), 10).await.unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    let e = db.run("CREATE TEMPORARY TABLE podelis_rc (x INT)", 10).await.unwrap_err();
    assert!(crate::db::is_reconnected(&e), "{e:#}");
    assert!(db.run(pid_sql, 10).await.is_ok());
}

#[tokio::test]
async fn live_reconnect_postgres() {
    let Some(db) = live_db(Engine::Postgres).await else { return };
    let other = live_db(Engine::Postgres).await.unwrap();
    live_reconnect(&db, &other, "SELECT pg_backend_pid()::text", "SELECT pg_terminate_backend({pid})").await;
}

#[tokio::test]
async fn live_reconnect_mysql() {
    let Some(db) = live_db(Engine::MySql).await else { return };
    let other = live_db(Engine::MySql).await.unwrap();
    live_reconnect(&db, &other, "SELECT CAST(CONNECTION_ID() AS CHAR)", "KILL {pid}").await;
}

/// The activity view sees another session's running query, and can cancel
/// it; EXPLAIN ANALYZE shows actual numbers.
async fn live_activity(engine: Engine, slow: &str) {
    let Some(db) = live_db(engine).await else { return };
    let other = live_db(engine).await.unwrap();
    let runner = other.clone();
    let slow_sql = slow.to_string();
    let handle = tokio::spawn(async move { runner.run(&slow_sql, 10).await });
    tokio::time::sleep(Duration::from_millis(600)).await;
    let sessions = db.activity().await.unwrap();
    // Only this test's query (others may be sleeping in parallel).
    let busy = sessions.iter().find(|a| a.query.contains("podelis_activity_test")).expect("sees the slow query");
    assert!(!busy.own);
    db.stop_session(&busy.id, false).await.unwrap();
    let result = tokio::time::timeout(Duration::from_secs(5), handle).await.expect("cancelled quickly").unwrap();
    if engine == Engine::Postgres {
        assert!(result.is_err());
    }
    // MySQL before 8.0.18 has none.
    let Some(explain) = engine.dialect().explain_analyze("SELECT 1", db.version()) else { return };
    let o = db.run(&explain, 10).await.unwrap();
    let plan = engine.dialect().format_plan(&o.columns, &o.rows).unwrap().text;
    // MariaDB's ANALYZE: the actual rows in a column of their own.
    assert!(plan.contains("actual time") || plan.contains("r_rows"), "{plan}");
}

#[tokio::test]
async fn live_activity_postgres() {
    live_activity(Engine::Postgres, "SELECT pg_sleep(10) AS podelis_activity_test").await;
}

#[tokio::test]
async fn live_activity_mysql() {
    live_activity(Engine::MySql, "SELECT SLEEP(10) AS podelis_activity_test").await;
}

/// Binary values show in each engine's hex notation, and that text goes
/// back into SQL as a literal equal to the stored value (for editing cells
/// and `INSERT` exports).
#[tokio::test]
async fn binary_values_show_in_hex_and_go_back_as_literals() {
    let dir = tempfile::tempdir().unwrap();
    for engine in [Engine::Postgres, Engine::MySql, Engine::Sqlite] {
        let db = if engine == Engine::Sqlite {
            Database::connect(sqlite_params(&dir.path().join("b.db"), false)).await.unwrap()
        } else {
            let Some(db) = live_db(engine).await else { continue };
            db
        };
        // A NUL, 0xFF, a quote and a backslash.
        let (create, insert, shown) = match engine {
            Engine::Postgres => ("CREATE TEMP TABLE bin_t (b bytea)", r"'\x00ff275c'", r"\x00ff275c"),
            Engine::MySql => ("CREATE TEMPORARY TABLE bin_t (b blob)", "X'00FF275C'", "0x00FF275C"),
            Engine::Sqlite => ("CREATE TEMP TABLE bin_t (b blob)", "X'00FF275C'", "X'00FF275C'"),
        };
        if engine == Engine::MySql {
            db.run("CREATE DATABASE IF NOT EXISTS podelis_bin; USE podelis_bin", 1).await.unwrap();
        }
        db.run(&format!("{create}; INSERT INTO bin_t VALUES ({insert}), ({insert})"), 1).await.unwrap();
        let o = db.run("SELECT b FROM bin_t", DEFAULT_ROW_LIMIT).await.unwrap();
        assert_eq!(o.rows[0][0].as_deref(), Some(shown), "{engine:?}");
        let literal = value_literal(shown, &o.column_types[0], engine);
        let o = db.run(&format!("SELECT count(*) FROM bin_t WHERE b = {literal}"), 1).await.unwrap();
        assert_eq!(o.rows[0][0].as_deref(), Some("2"), "{engine:?}: {literal}");
        db.run("DROP TABLE bin_t", 1).await.unwrap();
    }
}

#[test]
fn binary_literals_only_for_binary_columns_in_hex() {
    assert_eq!(value_literal("0xDEADBEEF", "BLOB", Engine::MySql), "0xDEADBEEF");
    assert_eq!(value_literal("0x", "VARBINARY", Engine::MySql), "X''");
    assert_eq!(value_literal("0xDEADBEEF", "TEXT", Engine::MySql), "'0xDEADBEEF'", "text that looks like hex");
    assert_eq!(value_literal("hello", "BLOB", Engine::MySql), "'hello'", "typed in by hand");
    assert_eq!(value_literal("X'00FF'", "BLOB", Engine::Sqlite), "X'00FF'");
    assert_eq!(value_literal("X'0FF'", "BLOB", Engine::Sqlite), "'X''0FF'''", "odd digits");
    assert_eq!(value_literal(r"\x00ff", "BYTEA", Engine::Postgres), r"'\x00ff'");
}

/// Every foreign key of the database, on each engine: what related tables
/// are made from.
#[tokio::test]
async fn links_on_each_engine() {
    let dir = tempfile::tempdir().unwrap();
    for engine in [Engine::Sqlite, Engine::Postgres, Engine::MySql] {
        let db = if engine == Engine::Sqlite {
            Database::connect(sqlite_params(&dir.path().join("l.db"), false)).await.unwrap()
        } else {
            let Some(db) = live_db(engine).await else { continue };
            db
        };
        if engine == Engine::MySql {
            db.run("CREATE DATABASE IF NOT EXISTS podelis_links; USE podelis_links", 1).await.unwrap();
        }
        db.run("DROP TABLE IF EXISTS link_child", 1).await.unwrap();
        db.run("DROP TABLE IF EXISTS link_parent", 1).await.unwrap();
        db.run("CREATE TABLE link_parent (id INT PRIMARY KEY)", 1).await.unwrap();
        db.run(
            "CREATE TABLE link_child (id INT PRIMARY KEY, parent_id INT, FOREIGN KEY (parent_id) REFERENCES link_parent (id))",
            1,
        )
        .await
        .unwrap();
        let links = db.links().await.unwrap();
        let link = links.iter().find(|l| l.table == "link_child").unwrap_or_else(|| panic!("{engine:?}: {links:?}"));
        assert_eq!(
            (link.column.as_str(), link.target_table.as_str(), link.target_column.as_deref()),
            ("parent_id", "link_parent", Some("id")),
            "{engine:?}"
        );
        assert!(crate::related::describe(&links, "", "link_parent").contains("← link_child (parent_id → id)"));
        db.run("DROP TABLE link_child", 1).await.unwrap();
        db.run("DROP TABLE link_parent", 1).await.unwrap();
    }
}

/// Comments on tables and columns, which the gateway shows models: none
/// on SQLite, `COMMENT ON` on Postgres, `COMMENT '…'` on MySQL.
#[tokio::test]
async fn comments_on_each_engine() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::connect(sqlite_params(&dir.path().join("c.db"), false)).await.unwrap();
    db.run("CREATE TABLE noted (id INT PRIMARY KEY)", 1).await.unwrap();
    assert!(db.comments().await.unwrap().is_empty());
    for engine in [Engine::Postgres, Engine::MySql] {
        let Some(db) = live_db(engine).await else { continue };
        if engine == Engine::MySql {
            db.run("CREATE DATABASE IF NOT EXISTS podelis_comments; USE podelis_comments", 1).await.unwrap();
        }
        db.run("DROP TABLE IF EXISTS noted", 1).await.unwrap();
        db.run("DROP VIEW IF EXISTS noted_view", 1).await.unwrap();
        if engine == Engine::Postgres {
            db.run("CREATE TABLE noted (id INT PRIMARY KEY, total NUMERIC, plain INT)", 1).await.unwrap();
            db.run("COMMENT ON TABLE noted IS 'One row per checkout'", 1).await.unwrap();
            db.run("COMMENT ON COLUMN noted.total IS 'VAT included'", 1).await.unwrap();
        } else {
            db.run(
                "CREATE TABLE noted (id INT PRIMARY KEY, total DECIMAL(10,2) COMMENT 'VAT included', plain INT) \
                 COMMENT 'One row per checkout'",
                1,
            )
            .await
            .unwrap();
        }
        db.run("CREATE VIEW noted_view AS SELECT id FROM noted", 1).await.unwrap();
        let comments = db.comments().await.unwrap();
        let mine: Vec<(Option<&str>, &str)> = comments
            .iter()
            .filter(|c| c.table.starts_with("noted"))
            .map(|c| (c.column.as_deref(), c.text.as_str()))
            .collect();
        assert_eq!(mine, [(None, "One row per checkout"), (Some("total"), "VAT included")], "{engine:?}");
        db.run("DROP VIEW noted_view", 1).await.unwrap();
        db.run("DROP TABLE noted", 1).await.unwrap();
        if engine == Engine::MySql {
            db.run("DROP DATABASE podelis_comments", 1).await.unwrap();
        }
    }
}

/// What a heavy-query guard asks about: the tables a read would read in
/// full, with about how many rows (the plan's estimate, else the catalog's).
#[tokio::test]
async fn full_scans_on_live_servers() {
    for engine in [Engine::Postgres, Engine::MySql] {
        let Some(db) = live_db(engine).await else { continue };
        if engine == Engine::MySql {
            db.run("CREATE DATABASE IF NOT EXISTS podelis_scans; USE podelis_scans", 1).await.unwrap();
        }
        db.run("DROP TABLE IF EXISTS scan_big", 1).await.unwrap();
        db.run("CREATE TABLE scan_big (id INT PRIMARY KEY, note VARCHAR(20))", 1).await.unwrap();
        let rows: Vec<String> = (1..=2000).map(|i| format!("({i}, 'n{i}')")).collect();
        db.run(&format!("INSERT INTO scan_big VALUES {}", rows.join(", ")), 1).await.unwrap();
        db.run(if engine == Engine::Postgres { "ANALYZE scan_big" } else { "ANALYZE TABLE scan_big" }, 1)
            .await
            .unwrap();
        let scans = db.full_scans("SELECT * FROM scan_big WHERE note = 'x'").await.unwrap();
        let (table, rows) = scans.first().unwrap_or_else(|| panic!("{engine:?}: no full scan found"));
        assert!(table == "scan_big" || table == "public.scan_big", "{engine:?}: {table}");
        assert!(*rows > 1000.0, "{engine:?}: {rows}");
        // By its key: no full scan; writes aren't planned.
        assert!(db.full_scans("SELECT * FROM scan_big WHERE id = 5").await.unwrap().is_empty(), "{engine:?}");
        assert!(db.full_scans("DELETE FROM scan_big WHERE note = 'x'").await.unwrap().is_empty(), "{engine:?}");
        assert!(db.full_scans("SELECT * FROM scan_big LIMIT 5").await.unwrap().is_empty(), "{engine:?}: limited");
        db.run("DROP TABLE scan_big", 1).await.unwrap();
    }
}

#[test]
fn full_scans_in_plans() {
    use crate::db::dialect::Scan;
    let rows = |text: &str| vec![vec![Some(text.to_string())]];
    let pg = |plan: &str| Engine::Postgres.dialect().full_scans(&["QUERY PLAN".into()], &rows(plan));
    let scan =
        |schema: Option<&str>, table: &str, rows| Scan { schema: schema.map(str::to_string), ..Scan::new(table, rows) };
    let join = r#"[{"Plan": {"Node Type": "Hash Join", "Plans": [
        {"Node Type": "Seq Scan", "Relation Name": "orders", "Schema": "shop", "Plan Rows": 5000},
        {"Node Type": "Index Scan", "Relation Name": "customers"}]}}]"#;
    assert_eq!(pg(join), [scan(Some("shop"), "orders", Some(5000.0))]);
    // A LIMIT stops a plain scan early; not one with a filter, nor one
    // under a sort, nor the inner side of a nested loop.
    let limited = r#"[{"Plan": {"Node Type": "Limit", "Plans": [{"Node Type": "Seq Scan", "Relation Name": "t", "Plan Rows": 9}]}}]"#;
    assert!(pg(limited).is_empty());
    let filtered = r#"[{"Plan": {"Node Type": "Limit", "Plans": [{"Node Type": "Seq Scan", "Relation Name": "t", "Filter": "(x = 1)", "Plan Rows": 9}]}}]"#;
    assert_eq!(pg(filtered), [scan(None, "t", None)], "rows to be looked up: the filter's aren't the table's");
    let sorted = r#"[{"Plan": {"Node Type": "Limit", "Plans": [{"Node Type": "Sort", "Plans": [{"Node Type": "Seq Scan", "Relation Name": "t", "Plan Rows": 9}]}]}}]"#;
    assert_eq!(pg(sorted), [scan(None, "t", Some(9.0))]);
    let nested = r#"[{"Plan": {"Node Type": "Limit", "Plans": [{"Node Type": "Nested Loop", "Plans": [
        {"Node Type": "Seq Scan", "Relation Name": "a", "Plan Rows": 9},
        {"Node Type": "Seq Scan", "Relation Name": "b", "Plan Rows": 9}]}]}}]"#;
    assert_eq!(pg(nested), [scan(None, "b", Some(9.0))]);

    let my = |plan: &str| Engine::MySql.dialect().full_scans(&["EXPLAIN".into()], &rows(plan));
    let tree = "-> Filter: (t.x = 1)  (cost=10.5 rows=2)\n    -> Table scan on t  (cost=10.5 rows=5200)";
    assert_eq!(my(tree), [scan(None, "t", Some(5200.0))]);
    assert!(my("-> Limit: 5 row(s)  (cost=1 rows=5)\n    -> Table scan on t  (cost=1 rows=5200)").is_empty());
    let limited_filter = "-> Limit: 5 row(s)\n    -> Filter: (t.x = 1)\n        -> Table scan on t  (cost=1 rows=5200)";
    assert_eq!(my(limited_filter), [scan(None, "t", Some(5200.0))]);
    let limited_sort = "-> Limit: 5 row(s)\n    -> Sort: t.x, limit input to 5 row(s) per chunk\n        -> Table scan on t  (cost=1 rows=5200)";
    assert_eq!(my(limited_sort), [scan(None, "t", Some(5200.0))]);
    let cols: Vec<String> = ["id", "select_type", "table", "type", "rows"].map(String::from).to_vec();
    let classic =
        vec![vec![Some("1".into()), Some("SIMPLE".into()), Some("t".into()), Some("ALL".into()), Some("99".into())]];
    assert_eq!(Engine::MySql.dialect().full_scans(&cols, &classic), [scan(None, "t", Some(99.0))]);
    let sqlite = vec![
        vec![Some("2".into()), Some("0".into()), Some("0".into()), Some("SCAN t".into())],
        vec![Some("3".into()), Some("0".into()), Some("0".into()), Some("SEARCH u USING INDEX i (x=?)".into())],
        vec![Some("4".into()), Some("0".into()), Some("0".into()), Some("SCAN v USING COVERING INDEX j".into())],
    ];
    assert_eq!(Engine::Sqlite.dialect().full_scans(&[], &sqlite), [scan(None, "t", None)]);
}

/// A plan while the session is busy: on a connection of its own, until the
/// session's settings change (or for SQLite, never).
#[tokio::test]
async fn live_plans_aside_while_the_session_is_busy() {
    for engine in [Engine::Postgres, Engine::MySql] {
        let Some(db) = live_db(engine).await else { continue };
        let explain = engine.dialect().explain("SELECT 1", db.version());
        // The session busy with a slow query meanwhile.
        let sleep = if engine == Engine::Postgres { "SELECT pg_sleep(1)" } else { "SELECT SLEEP(1)" };
        let busy = {
            let db = db.clone();
            tokio::spawn(async move { db.run(sleep, DEFAULT_ROW_LIMIT).await })
        };
        tokio::time::sleep(Duration::from_millis(100)).await;
        let started = Instant::now();
        let plan = db.run_aside(&explain, DEFAULT_ROW_LIMIT).await.unwrap().expect("made aside");
        assert!(started.elapsed() < Duration::from_millis(900), "didn't wait for the session");
        assert!(!plan.rows.is_empty(), "{engine:?}: {plan:?}");
        busy.await.unwrap().unwrap();
        db.mark_session_changed();
        assert!(db.run_aside(&explain, DEFAULT_ROW_LIMIT).await.unwrap().is_none());
    }
}

#[tokio::test]
async fn a_session_given_up_on_is_opened_again() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.db");
    let db = Database::connect(sqlite_params(&path, false)).await.unwrap();
    db.run("create temp table only_here (x int)", 10).await.unwrap();
    db.answers_within(std::time::Duration::from_secs(1)).await.unwrap();
    // Given up on mid-call: the next run has a session of its own (the
    // temporary table, the old session's, is gone with it).
    db.abandon_session();
    let err = db.run("select * from only_here", 10).await.unwrap_err();
    assert!(crate::db::error_message(&err).contains("no such table"), "{err:#}");
    db.run("select 1", 10).await.unwrap();
    // A write isn't run on the new session: the old one's transaction may
    // have been open, so it's the user's to run again, told why.
    db.abandon_session();
    let err = db.run("create table w (x int)", 10).await.unwrap_err();
    assert!(crate::db::is_reconnected(&err), "{err:#}");
    assert!(crate::db::error_message(&err).contains("wasn't run"), "{err:#}");
    assert!(db.run("select * from w", 10).await.is_err(), "not created");
    // A read is run again, and says it reconnected.
    db.abandon_session();
    assert!(db.run("select 1", 10).await.unwrap().reconnected);
}

#[tokio::test]
async fn an_address_nothing_answers_at_says_why_at_once() {
    // A port nothing listens on: refused, at once (not the pool's timeout).
    // Port 1: nothing listens there, and nobody can bind it without root
    // (a port freed for the test could be taken by another test meanwhile).
    let port = 1;
    let started = std::time::Instant::now();
    let err = super::reach("127.0.0.1", port).await.unwrap_err();
    assert!(started.elapsed() < std::time::Duration::from_secs(1));
    assert_eq!(err.to_string(), format!("can't reach 127.0.0.1:{port}: connection refused"));
    assert!(crate::db::is_unreachable(&err));
    let err = super::reach("db.nowhere.invalid", 5432).await.unwrap_err();
    assert_eq!(err.to_string(), "can't reach db.nowhere.invalid:5432: no such host");
    // Something listening: fine.
    let open = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    super::reach("127.0.0.1", open.local_addr().unwrap().port()).await.unwrap();
}

#[tokio::test]
async fn a_mysql_server_that_is_down_says_why_at_once() {
    // Port 1: nothing listens there, and nobody can bind it without root
    // (a port freed for the test could be taken by another test meanwhile).
    let port = 1;
    let opts = sqlx::mysql::MySqlConnectOptions::new().host("127.0.0.1").port(port).username("root");
    let started = std::time::Instant::now();
    let err = super::mysql::connect::try_login(&opts).await.unwrap_err();
    assert!(started.elapsed() < std::time::Duration::from_secs(1), "not the pool's 10 s");
    assert_eq!(err.to_string(), format!("can't reach 127.0.0.1:{port}: connection refused"));
    assert!(crate::db::is_unreachable(&err));
}
