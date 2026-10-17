use super::*;

/// Live-database tests run where an engine's test server is set, by its
/// own variables (each engine's `db_tests::live_params`): e.g.
/// `PODELIS_TEST_MYSQL_PORT`, with `_HOST`, `_USER`, `_PASSWORD` and `_DB`
/// (defaults: 127.0.0.1, the engine's usual user, empty, empty).
pub(in crate::db) fn server_params(engine: Engine, prefix: &str, user: &str) -> Option<ConnectParams> {
    let var = |name: &str| std::env::var(format!("{prefix}_{name}")).ok();
    Some(ConnectParams {
        engine,
        port: var("PORT")?.parse().ok()?,
        host: var("HOST").unwrap_or_else(|| "127.0.0.1".into()),
        user: var("USER").unwrap_or_else(|| user.into()),
        password: var("PASSWORD").unwrap_or_default(),
        database: var("DB").unwrap_or_default(),
        ssl_mode: SslMode::Prefer,
        read_only: false,
        ssh: None,
        password_command: Vec::new(),
    })
}

/// The engine's test server, if one is set (none for a file database).
pub(in crate::db) fn live_params(engine: Engine) -> Option<ConnectParams> {
    on_engine!(engine, engine::db_tests::live_params())
}

pub(crate) async fn live_db(engine: Engine) -> Option<Database> {
    let params = live_params(engine)?;
    Some(Database::connect(params).await.expect("connect"))
}

/// A database of the test's own, `name`, on `engine`, and how to reach it
/// again: on its test server (none set: `None`), or a new file. For the
/// tests every engine shares, so each runs on each engine there is.
pub(crate) async fn own_live_db(engine: Engine, name: &str) -> Option<(Database, ConnectParams)> {
    on_engine!(engine, engine::db_tests::own_live_db(name).await)
}

/// A new SQLite file named after the test (each test its own name), in
/// one folder that later runs reuse.
pub(in crate::db) fn scratch_file(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join("podelis-tests");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{name}.db"));
    let _ = std::fs::remove_file(&path);
    path
}

/// What the tests every engine shares need said in the engine's own SQL
/// (each engine's `db_tests::TEST_SQL`).
pub(crate) struct TestSql {
    /// A query that waits `{s}` seconds; `None` where a query can't wait.
    pub sleep: Option<&'static str>,
    /// Statements giving the numbers 1 to 2500, one per row (the last one
    /// a query on its own).
    pub count_to_2500: &'static str,
    /// What the plan of `SELECT id FROM t WHERE id = 1` shows (any of).
    pub plan_shows: &'static [&'static str],
    /// Updates the planner's numbers for table `{table}`.
    pub analyze: &'static str,
    /// Makes table `bin_t` with a binary column `b`; a value with a NUL,
    /// 0xFF, a quote and a backslash, as a literal; and as it's shown.
    pub binary: [&'static str; 3],
    /// Makes table `noted` (`id`, `total`, `plain`) with the comments "One
    /// row per checkout" on it and "VAT included" on `total`; `None` where
    /// there are no comments (then it's made without).
    pub comments: Option<&'static [&'static str]>,
    /// Makes sequence `agent_seq`; a read that moves it; and a query that
    /// says whether it moved (`false`: not yet). `None` without sequences.
    pub sequence: Option<[&'static str; 3]>,
    /// A read that switches the session's read-only setting off.
    pub read_only_off: Option<&'static str>,
    /// Makes table `agent_m`, whose changes a rollback can't undo.
    pub without_rollback: Option<&'static str>,
}

pub(crate) fn test_sql(engine: Engine) -> &'static TestSql {
    on_engine!(engine, &engine::db_tests::TEST_SQL)
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

pub(in crate::db) fn cells(o: &QueryOutcome) -> Vec<Vec<Option<&str>>> {
    o.rows.iter().map(|r| r.iter().map(|c| c.as_deref()).collect()).collect()
}

pub(in crate::db) async fn live_common(db: &Database) {
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
    assert!(test_sql(db.engine()).plan_shows.iter().any(|s| plan.contains(s)), "{plan}");

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
    let big = test_sql(db.engine()).count_to_2500;
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
pub(in crate::db) async fn live_common_rest(db: &Database) {
    // Empty results keep their columns and have no stream.
    let (o, stream) = db.run_streaming("SELECT name FROM t WHERE id < 0", 1000).await.unwrap();
    assert_eq!((o.columns, o.rows.len(), stream.is_none()), (vec!["name".to_string()], 0, true));

    // The session is still usable afterwards.
    assert_eq!(cells(&db.run("SELECT 42", DEFAULT_ROW_LIMIT).await.unwrap()), vec![vec![Some("42")]]);

    assert!(db.run("SELEC nonsense", DEFAULT_ROW_LIMIT).await.is_err());
    assert_eq!(cells(&db.run("SELECT 1", DEFAULT_ROW_LIMIT).await.unwrap()), vec![vec![Some("1")]]);
}

/// `slow` cancelled part way; `stopped` checks what its run returned (each
/// engine stops a query its own way).
pub(in crate::db) async fn live_cancel(db: &Database, slow: &str, stopped: impl FnOnce(Result<QueryOutcome>)) {
    let runner = db.clone();
    let slow = slow.to_string();
    let handle = tokio::spawn(async move { runner.run(&slow, DEFAULT_ROW_LIMIT).await });
    tokio::time::sleep(Duration::from_millis(500)).await;
    db.cancel().await.unwrap();
    let result = tokio::time::timeout(Duration::from_secs(5), handle).await.expect("cancel took too long");
    stopped(result.unwrap());
    assert_eq!(cells(&db.run("SELECT 7", DEFAULT_ROW_LIMIT).await.unwrap()), vec![vec![Some("7")]]);
}

/// The engine whose database is a file, needing no server: for tests that
/// need a database that's always there.
pub(crate) fn file_engine() -> Engine {
    Engine::ALL.into_iter().find(|e| e.is_file_based()).expect("an engine with file databases")
}

/// A file database at `path`.
pub(crate) fn file_params(path: &std::path::Path, read_only: bool) -> ConnectParams {
    ConnectParams {
        engine: file_engine(),
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

/// A saved connection `name` to the file database at `path`.
pub(crate) fn file_profile(name: &str, path: &std::path::Path) -> crate::config::store::ConnectionProfile {
    crate::config::store::ConnectionProfile {
        name: name.into(),
        engine: file_engine(),
        host: path.to_string_lossy().into_owned(),
        port: String::new(),
        user: String::new(),
        database: String::new(),
        ssl_mode: SslMode::Disable,
        mode: ConnectionMode::Normal,
        transaction: None,
        ssh: String::new(),
        timeout: String::new(),
        password_command: Vec::new(),
        agents: AgentAccess::None,
        legacy_keychain: false,
    }
}

/// Opt-in: `PODELIS_TEST_SSH_DEST` names an ssh destination that can
/// reach the test database (by the same host/port as the direct test).
#[tokio::test]
async fn live_ssh_tunnel() {
    let Ok(dest) = std::env::var("PODELIS_TEST_SSH_DEST") else { return };
    let Some(mut params) = Engine::ALL.into_iter().find_map(live_params) else { return };
    params.ssh = Some(dest);
    let db = Database::connect(params.clone()).await.expect("connect through tunnel");
    assert_eq!(cells(&db.run("SELECT 1", DEFAULT_ROW_LIMIT).await.unwrap()), vec![vec![Some("1")]]);
    db.close().await;

    params.ssh = Some("podelis-no-such-host.invalid".into());
    let err = format!("{:#}", Database::connect(params).await.err().expect("bad host fails"));
    assert!(err.contains("ssh tunnel failed"), "{err}");
}

/// Checks that a statement is judged by the server without running it.
pub(in crate::db) async fn live_check(db: &Database) {
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

#[test]
fn ssl_mode_cycles() {
    assert_eq!(SslMode::Prefer.cycle(true), SslMode::Require);
    assert_eq!(SslMode::Disable.cycle(false), SslMode::VerifyFull);
    assert_eq!(SslMode::VerifyFull.cycle(true), SslMode::Disable);
}

/// Indexes, foreign keys and definitions, after `setup` created the tables;
/// `key_index`: the primary key has an index of its own (not SQLite's
/// INTEGER PRIMARY KEY, the rowid itself).
pub(in crate::db) async fn live_details(db: &Database, schema: &str, setup: &str, key_index: bool) {
    db.run(setup, DEFAULT_ROW_LIMIT).await.unwrap();
    let child = db.table_details(schema, "det_child").await.unwrap();
    let mut indexes: Vec<(bool, bool, &str)> =
        child.indexes.iter().map(|i| (i.primary, i.unique, i.columns.as_str())).collect();
    indexes.sort_unstable();
    if key_index {
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
pub(in crate::db) const DETAILS_TABLES: &str = "CREATE TABLE det_parent (id INTEGER PRIMARY KEY);
     CREATE TABLE det_child (id INTEGER PRIMARY KEY, parent_id INTEGER, email VARCHAR(100) UNIQUE,
                             FOREIGN KEY (parent_id) REFERENCES det_parent (id));
     CREATE INDEX det_child_parent ON det_child (parent_id);
     CREATE VIEW det_v AS SELECT id FROM det_parent;";

/// What the review flow relies on: `BEGIN;` before the writes in one run
/// leaves the transaction open, and ROLLBACK undoes them.
pub(in crate::db) async fn live_begin_in_script(db: &Database, table: &str) {
    db.run(&format!("DROP TABLE IF EXISTS {table}; CREATE TABLE {table} (x INT); INSERT INTO {table} VALUES (1)"), 10)
        .await
        .unwrap();
    let o = db.run(&format!("BEGIN;\nUPDATE {table} SET x = 2 WHERE x = 1"), 10).await.unwrap();
    assert_eq!(o.rows_affected, Some(1));
    // Where the engine can tell (PostgreSQL), it's open.
    if let Some(state) = db.transaction_state().await {
        assert_eq!(state, TxnState::Open);
    }
    db.run("ROLLBACK", 10).await.unwrap();
    let o = db.run(&format!("SELECT x FROM {table}"), 10).await.unwrap();
    assert_eq!(cells(&o), vec![vec![Some("1")]]);
    db.run(&format!("DROP TABLE {table}"), 10).await.unwrap();
}

/// Killing the session's server process: a read reconnects and runs again,
/// a write reports the reconnect instead of retrying.
pub(in crate::db) async fn live_reconnect(db: &Database, other: &Database, pid_sql: &str, kill: &str) {
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

/// The activity view sees another session's running query, and can cancel
/// it; EXPLAIN ANALYZE shows actual numbers.
pub(in crate::db) async fn live_activity(engine: Engine, slow: &str, cancelled_is_an_error: bool) {
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
    if cancelled_is_an_error {
        assert!(result.is_err());
    }
    // MySQL before 8.0.18 has none.
    let Some(explain) = engine.dialect().explain_analyze("SELECT 1", db.version()) else { return };
    let o = db.run(&explain, 10).await.unwrap();
    let plan = engine.dialect().format_plan(&o.columns, &o.rows).unwrap().text;
    // MariaDB's ANALYZE: the actual rows in a column of their own.
    assert!(plan.contains("actual time") || plan.contains("r_rows"), "{plan}");
}

/// Binary values show in each engine's hex notation, and that text goes
/// back into SQL as a literal equal to the stored value (for editing cells
/// and `INSERT` exports).
#[tokio::test]
async fn binary_values_show_in_hex_and_go_back_as_literals() {
    for engine in Engine::ALL {
        let Some((db, _)) = own_live_db(engine, "podelis_bin").await else { continue };
        let [create, insert, shown] = test_sql(engine).binary;
        db.run(&format!("{create}; INSERT INTO bin_t VALUES ({insert}), ({insert})"), 1).await.unwrap();
        let o = db.run("SELECT b FROM bin_t", DEFAULT_ROW_LIMIT).await.unwrap();
        assert_eq!(o.rows[0][0].as_deref(), Some(shown), "{engine:?}");
        let literal = value_literal(shown, &o.column_types[0], engine);
        let o = db.run(&format!("SELECT count(*) FROM bin_t WHERE b = {literal}"), 1).await.unwrap();
        assert_eq!(o.rows[0][0].as_deref(), Some("2"), "{engine:?}: {literal}");
        db.run("DROP TABLE bin_t", 1).await.unwrap();
    }
}

/// Every foreign key of the database, on each engine: what related tables
/// are made from.
#[tokio::test]
async fn links_on_each_engine() {
    for engine in Engine::ALL {
        let Some((db, _)) = own_live_db(engine, "podelis_links").await else { continue };
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

/// Comments on tables and columns, which the gateway shows models, on
/// each engine that has them (each says how: `TestSql::comments`).
#[tokio::test]
async fn comments_on_each_engine() {
    for engine in Engine::ALL {
        let Some((db, _)) = own_live_db(engine, "podelis_comments").await else { continue };
        db.run("DROP VIEW IF EXISTS noted_view", 1).await.unwrap();
        db.run("DROP TABLE IF EXISTS noted", 1).await.unwrap();
        let commented = test_sql(engine).comments;
        for sql in commented.unwrap_or(&["CREATE TABLE noted (id INT PRIMARY KEY, total NUMERIC, plain INT)"]) {
            db.run(sql, 1).await.unwrap();
        }
        db.run("CREATE VIEW noted_view AS SELECT id FROM noted", 1).await.unwrap();
        let comments = db.comments().await.unwrap();
        let mine: Vec<(Option<&str>, &str)> = comments
            .iter()
            .filter(|c| c.table.starts_with("noted"))
            .map(|c| (c.column.as_deref(), c.text.as_str()))
            .collect();
        let expected: &[(Option<&str>, &str)] =
            if commented.is_some() { &[(None, "One row per checkout"), (Some("total"), "VAT included")] } else { &[] };
        assert_eq!(mine, expected, "{engine:?}");
        db.run("DROP VIEW noted_view", 1).await.unwrap();
        db.run("DROP TABLE noted", 1).await.unwrap();
    }
}

/// What a heavy-query guard asks about: the tables a read would read in
/// full, with about how many rows (the plan's estimate, else the catalog's;
/// with neither, as SQLite, it can't tell, and says none).
#[tokio::test]
async fn full_scans_on_live_servers() {
    for engine in Engine::ALL {
        let Some((db, _)) = own_live_db(engine, "podelis_scans").await else { continue };
        db.run("DROP TABLE IF EXISTS scan_big", 1).await.unwrap();
        db.run("CREATE TABLE scan_big (id INT PRIMARY KEY, note VARCHAR(20))", 1).await.unwrap();
        let rows: Vec<String> = (1..=2000).map(|i| format!("({i}, 'n{i}')")).collect();
        db.run(&format!("INSERT INTO scan_big VALUES {}", rows.join(", ")), 1).await.unwrap();
        db.run(&test_sql(engine).analyze.replace("{table}", "scan_big"), 1).await.unwrap();
        let scans = db.full_scans("SELECT * FROM scan_big WHERE note = 'x'").await.unwrap();
        let tables = db.tables(false).await.unwrap();
        if tables.iter().find(|t| t.name == "scan_big").is_some_and(|t| t.rows.is_none()) {
            assert!(scans.is_empty(), "{engine:?}: {scans:?}");
            continue;
        }
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

/// A plan while the session is busy: on a connection of its own, until the
/// session's settings change (or for SQLite, never).
#[tokio::test]
async fn live_plans_aside_while_the_session_is_busy() {
    for engine in Engine::ALL {
        let Some((db, _)) = own_live_db(engine, "podelis_aside").await else { continue };
        let explain = engine.dialect().explain("SELECT 1", db.version());
        // Where a query can't wait, there's nothing to plan around.
        let Some(sleep) = test_sql(engine).sleep.map(|s| s.replace("{s}", "1")) else {
            assert!(db.run_aside(&explain, DEFAULT_ROW_LIMIT).await.unwrap().is_none(), "{engine:?}");
            continue;
        };
        // The session busy with a slow query meanwhile.
        let busy = {
            let db = db.clone();
            tokio::spawn(async move { db.run(&sleep, DEFAULT_ROW_LIMIT).await })
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
    let db = Database::connect(file_params(&path, false)).await.unwrap();
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
