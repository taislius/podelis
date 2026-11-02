use rmcp::model::{CallToolRequestParams, ClientCapabilities, ClientConfig};
use rmcp::service::RunningService;
use rmcp::{ClientHandler, RoleClient};

use super::*;
use crate::db::Engine;

pub(crate) struct TestAgent;

impl ClientHandler for TestAgent {
    fn get_info(&self) -> ClientConfig {
        ClientConfig::new(ClientCapabilities::default(), Implementation::new("test-agent", "1"))
    }
}

/// A shared file connection (a quick query timeout).
#[cfg(feature = "sqlite")]
fn file_connection(name: &str, path: &std::path::Path, agents: AgentAccess, mode: ConnectionMode) -> ConnectionProfile {
    ConnectionProfile { agents, mode, timeout: "1".into(), ..crate::config::store::file_profile(name, path) }
}

/// A shared connection `name` to a server of `engine` at `host` (its port,
/// user and database left for the caller), with a quick query timeout.
fn server_connection(
    name: &str,
    engine: crate::db::Engine,
    host: &str,
    agents: AgentAccess,
    mode: ConnectionMode,
) -> ConnectionProfile {
    ConnectionProfile {
        unknown: crate::config::store::Unknown::default(),
        name: name.into(),
        engine,
        host: host.into(),
        port: String::new(),
        user: String::new(),
        database: String::new(),
        ssl_mode: crate::db::SslMode::default(),
        mode,
        transaction: None,
        ssh: String::new(),
        timeout: "1".into(),
        agents,
        password_command: Vec::new(),
        legacy_keychain: false,
    }
}

async fn connect(server: Server) -> RunningService<RoleClient, TestAgent> {
    let (client_write, server_read) = tokio::io::duplex(1 << 16);
    let (server_write, client_read) = tokio::io::duplex(1 << 16);
    tokio::spawn(async move {
        if let Ok(running) = server.serve((server_read, server_write)).await {
            let _ = running.waiting().await;
        }
    });
    TestAgent.serve((client_read, client_write)).await.unwrap()
}

/// The tool's text, with "ERROR " before a failure's.
pub(crate) async fn call(
    client: &RunningService<RoleClient, TestAgent>,
    tool: &str,
    args: serde_json::Value,
) -> String {
    let params = CallToolRequestParams::new(tool.to_string()).with_arguments(args.as_object().unwrap().clone());
    let result = client.call_tool(params).await.unwrap();
    let text: String = result.content.iter().filter_map(|c| c.as_text().map(|t| t.text.clone())).collect();
    if result.is_error == Some(true) { format!("ERROR {text}") } else { text }
}

/// An agent through the real protocol: what it sees, what runs, what's
/// refused, the dry run on production, and the log of it all.
#[cfg(feature = "sqlite")]
#[tokio::test]
async fn a_relative_sqlite_file_is_the_agents() {
    let dir = tempfile::tempdir().unwrap();
    make_file_db(&dir.path().join("shop.db"), "CREATE TABLE here (id INT); INSERT INTO here VALUES (7)");
    let mut shop = file_connection("shop", std::path::Path::new("shop.db"), AgentAccess::Read, ConnectionMode::Normal);
    shop.host = "shop.db".into();
    let server = Server::new("p1".into(), "shop".into(), vec![shop], dir.path().join("agents.jsonl"))
        .in_dir(Some(dir.path().display().to_string()));
    let client = connect(server).await;
    let rows = call(&client, "query", serde_json::json!({ "connection": "shop", "sql": "select id from here" })).await;
    assert!(rows.contains("\"7\""), "{rows}");
}

/// A sandbox: the agent's writes build on each other and its reads see
/// them, a failed call doesn't spoil it, nobody else sees anything, and
/// idle it's rolled back.
#[cfg(feature = "sqlite")]
#[tokio::test]
async fn an_agents_sandbox_keeps_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s.db");
    make_file_db(&path, "CREATE TABLE t (id INTEGER PRIMARY KEY, x INT); INSERT INTO t (x) VALUES (1), (2), (3)");
    let log = dir.path().join("agents.jsonl");
    let sandbox = file_connection("box", &path, AgentAccess::Sandbox, ConnectionMode::Production);
    let server = Server::new("p1".into(), "shop".into(), vec![sandbox], log.clone()).idling(Duration::from_secs(1));
    let client = connect(server).await;
    let q = |sql: &str| serde_json::json!({ "connection": "box", "sql": sql });
    let listed = call(&client, "connections", serde_json::json!({})).await;
    assert!(listed.contains("write in a sandbox"), "{listed}");
    assert!(!call(&client, "query", q("insert into t (x) values (4)")).await.starts_with("ERROR"));
    assert!(call(&client, "query", q("select count(*) from t")).await.contains("\"4\""), "sees its write");
    // A failed call: back to before it, the sandbox goes on.
    assert!(call(&client, "query", q("insert into nope values (1)")).await.starts_with("ERROR"));
    assert!(call(&client, "query", q("select count(*) from t")).await.contains("\"4\""));
    assert!(!call(&client, "explain", q("select * from t where x = 4")).await.starts_with("ERROR"));
    // Never anything destructive, sandbox or not.
    assert!(call(&client, "query", q("delete from t")).await.contains("destructive"));
    // Nobody else sees it.
    let look = file_connection("look", &path, AgentAccess::Read, ConnectionMode::Normal);
    let reader = connect(Server::new("p1".into(), "shop".into(), vec![look], dir.path().join("r.jsonl"))).await;
    let count = |text: String| text.contains("\"3\"");
    let look_q = serde_json::json!({ "connection": "look", "sql": "select count(*) from t" });
    assert!(count(call(&reader, "query", look_q.clone()).await), "not committed");
    // Idle, it's rolled back; the next call starts a new one.
    tokio::time::sleep(Duration::from_millis(2500)).await;
    assert!(count(call(&client, "query", q("select count(*) from t")).await), "rolled back");
    assert!(count(call(&reader, "query", look_q).await));
    let text = std::fs::read_to_string(&log).unwrap();
    let entries: Vec<serde_json::Value> = text.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert!(entries.iter().filter(|e| e["sandbox"] == serde_json::json!(1)).count() >= 4, "{text}");
    // The one after the idle rollback is a new sandbox.
    assert_eq!(entries.last().unwrap()["sandbox"], serde_json::json!(2), "{text}");
}

/// On the servers: a sandbox whose session is killed is never written
/// into after it (the next call says it was lost, and nothing from the
/// sandbox, before or after, is committed).
#[tokio::test]
async fn a_killed_sandbox_session_commits_nothing() {
    for engine in [crate::db::Engine::Postgres, crate::db::Engine::MySql] {
        let Some((admin, params)) = crate::db::tests::own_live_db(engine, "podelis_agent_sandbox").await else {
            continue;
        };
        // PostgreSQL's is the test server's own database: a run before
        // may have left the table.
        admin.run("DROP TABLE IF EXISTS sb", 1).await.unwrap();
        admin.run("CREATE TABLE sb (id INT PRIMARY KEY)", 1).await.unwrap();
        let dir = tempfile::tempdir().unwrap();
        let profile = ConnectionProfile {
            unknown: crate::config::store::Unknown::default(),
            name: "box".into(),
            engine,
            host: params.host.clone(),
            port: params.port.to_string(),
            user: params.user.clone(),
            database: params.database.clone(),
            ssl_mode: crate::db::SslMode::Disable,
            mode: ConnectionMode::Normal,
            transaction: None,
            ssh: String::new(),
            timeout: String::new(),
            agents: AgentAccess::Sandbox,
            password_command: vec!["echo".into(), params.password.clone()],
            legacy_keychain: false,
        };
        let server = Server::new("p1".into(), "shop".into(), vec![profile], dir.path().join("a.jsonl"));
        let client = connect(server).await;
        let q = |sql: &str| serde_json::json!({ "connection": "box", "sql": sql });
        assert!(!call(&client, "query", q("insert into sb values (1)")).await.starts_with("ERROR"), "{engine:?}");
        // Its session, from the agent's own view, then killed from outside.
        let who =
            if engine == crate::db::Engine::Postgres { "select pg_backend_pid()" } else { "select connection_id()" };
        let id_text = call(&client, "query", q(who)).await;
        let id: String = id_text.chars().filter(char::is_ascii_digit).collect();
        let kill = if engine == crate::db::Engine::Postgres {
            format!("select pg_terminate_backend({id})")
        } else {
            format!("kill {id}")
        };
        admin.run(&kill, 1).await.unwrap();
        let after = call(&client, "query", q("insert into sb values (2)")).await;
        assert!(after.starts_with("ERROR") && after.contains("lost"), "{engine:?}: {after}");
        // Nothing committed: not the write before, not the one after.
        let count = admin.run("select count(*) from sb", 1).await.unwrap();
        assert_eq!(count.rows[0][0].as_deref(), Some("0"), "{engine:?}");
    }
}

#[cfg(feature = "sqlite")]
#[tokio::test]
async fn agents_get_shared_connections_safely() {
    let dir = tempfile::tempdir().unwrap();
    let file = |name: &str| {
        let path = dir.path().join(name);
        make_file_db(&path, "CREATE TABLE t (id INTEGER PRIMARY KEY, x INT); INSERT INTO t (x) VALUES (1), (2), (3)");
        path
    };
    let (reads, writes, prod, hidden) = (file("r.db"), file("w.db"), file("p.db"), file("h.db"));
    let log = dir.path().join("agents.jsonl");
    let server = Server::new(
        "p1".into(),
        "shop".into(),
        vec![
            file_connection("reads", &reads, AgentAccess::Read, ConnectionMode::Normal),
            file_connection("writes", &writes, AgentAccess::Write, ConnectionMode::Normal),
            file_connection("prod", &prod, AgentAccess::Write, ConnectionMode::Production),
            file_connection("hidden", &hidden, AgentAccess::None, ConnectionMode::Normal),
        ],
        log.clone(),
    )
    .named(Some(" tester ".into()));
    let client = connect(server).await;
    let q = |conn: &str, sql: &str| serde_json::json!({ "connection": conn, "sql": sql });

    let listed = call(&client, "connections", serde_json::json!({})).await;
    assert!(
        listed.contains("reads") && listed.contains("prod (SQLite, production)") && !listed.contains("hidden"),
        "{listed}"
    );
    let rows = call(
        &client,
        "query",
        serde_json::json!({ "connection": "reads", "sql": "SELECT x FROM t ORDER BY id", "limit": 2 }),
    )
    .await;
    assert_eq!(rows, r#"{"columns":["x"],"more_rows":true,"rows":[["1"],["2"]]}"#);
    assert!(
        call(&client, "schema", serde_json::json!({ "connection": "reads" })).await.contains("t: id INTEGER PK, x INT")
    );

    // Read-only: refused by Podėlis, and by SQLite for what it misses.
    assert!(call(&client, "query", q("reads", "UPDATE t SET x = 0 WHERE id = 1")).await.starts_with("ERROR"));
    let sneaky = call(&client, "query", q("reads", "PRAGMA user_version = 7")).await;
    assert!(sneaky.starts_with("ERROR") && sneaky.contains("not PRAGMA"), "{sneaky}");
    assert!(call(&client, "query", q("hidden", "SELECT 1")).await.starts_with("ERROR No connection 'hidden'"));

    // Writes where allowed, never destructive ones.
    assert_eq!(call(&client, "query", q("writes", "UPDATE t SET x = 9 WHERE id = 1")).await, r#"{"rows_affected":1}"#);
    assert!(call(&client, "query", q("writes", "DELETE FROM t")).await.contains("destructive"));

    // Production: a dry run that changes nothing.
    let dry = call(&client, "query", q("prod", "UPDATE t SET x = 0 WHERE x > 1")).await;
    assert!(dry.contains("dry run") && dry.contains("would change 2 row(s)"), "{dry}");
    let after = call(&client, "query", q("prod", "SELECT count(*) FROM t WHERE x = 0")).await;
    assert!(after.contains(r#"[["0"]]"#), "{after}");

    // The time limit stops a query that runs too long.
    let forever = "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n) SELECT count(*) FROM n";
    assert!(call(&client, "query", q("reads", forever)).await.contains("stopped after 1 s"));

    let plan = call(&client, "explain", q("reads", "SELECT * FROM t WHERE id = 2")).await;
    assert!(plan.contains("USING INTEGER PRIMARY KEY"), "{plan}");

    let entries = agents::read(&log);
    assert!(entries.iter().all(|e| e.agent == "test-agent"));
    // Who it was: the client as it reported itself, this run, its
    // folder, and the user's name for it.
    let first = &entries[0];
    assert_eq!(first.session.len(), 8);
    assert!(entries.iter().all(|e| e.session == first.session));
    assert_eq!((first.agent_version.as_deref(), first.name.as_deref()), (Some("1"), Some("tester")));
    assert_eq!(first.dir, std::env::current_dir().ok().map(|d| d.display().to_string()));
    let kinds: Vec<(&str, bool, bool, bool)> =
        entries.iter().map(|e| (e.connection.as_str(), e.refused.is_some(), e.error.is_some(), e.dry_run)).collect();
    assert_eq!(
        kinds,
        [
            ("reads", false, false, false),
            ("reads", true, false, false),
            ("reads", true, false, false),
            ("hidden", true, false, false),
            ("writes", false, false, false),
            ("writes", true, false, false),
            ("prod", false, false, true),
            ("prod", false, false, false),
            ("reads", false, true, false),
            ("reads", false, false, false),
        ]
    );
    client.cancel().await.unwrap();
}

/// Sharing changed in Podėlis applies at once to a server that's running.
#[cfg(feature = "sqlite")]
#[tokio::test]
async fn sharing_changes_apply_at_once() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("s.db");
    make_file_db(&db, "CREATE TABLE t (x INT)");
    let store = dir.path().join("connections.toml");
    let save = |agents: &str| {
        let toml = format!(
            "[[projects]]\nid = \"p1\"\nname = \"shop\"\n[[projects.connections]]\nname = \"s\"\nengine = \"Sqlite\"\n\
             host = \"{}\"\nport = \"\"\nuser = \"\"\ndatabase = \"\"\n{agents}",
            db.display()
        );
        std::fs::write(&store, toml).unwrap();
    };
    save("agents = \"read\"\n");
    let server =
        Server::new("p1".into(), "shop".into(), Vec::new(), dir.path().join("log.jsonl")).reading(store.clone());
    let client = connect(server).await;
    let q = serde_json::json!({ "connection": "s", "sql": "SELECT count(*) FROM t" });
    assert!(!call(&client, "query", q.clone()).await.starts_with("ERROR"));
    save("");
    assert!(call(&client, "query", q.clone()).await.starts_with("ERROR No connection 's'"));
    assert!(call(&client, "connections", serde_json::json!({})).await.contains("No connection"));
    client.cancel().await.unwrap();
}

/// Where the user approves writes: each one waits for their answer in
/// Podėlis, which sees its dry run; yes runs it, no or no answer don't.
#[cfg(feature = "sqlite")]
#[tokio::test]
async fn approved_writes_wait_for_the_user() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.db");
    make_file_db(&path, "CREATE TABLE t (id INTEGER PRIMARY KEY, x INT); INSERT INTO t (x) VALUES (1), (2)");
    let log = dir.path().join("agents").join("p1.jsonl");
    let server = Server::new(
        "p1".into(),
        "shop".into(),
        vec![file_connection("prod", &path, AgentAccess::Approve, ConnectionMode::Production)],
        log.clone(),
    )
    .waiting(Duration::from_secs(2));
    // The user: yes to x = 5, no to x = 6, and x = 7 left unanswered.
    let pending = crate::agents::pending_dir(dir.path(), "p1");
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let user = tokio::spawn({
        let (pending, seen) = (pending.clone(), Arc::clone(&seen));
        async move {
            loop {
                for request in crate::agents::pending(&pending) {
                    if request.sql.contains("x = 7") {
                        continue;
                    }
                    seen.lock().unwrap().push(request.dry_run.clone());
                    crate::agents::answer(&pending, &request.id, request.sql.contains("x = 5"));
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
    });
    let client = connect(server).await;
    let q = |sql: &str| serde_json::json!({ "connection": "prod", "sql": sql });
    assert!(call(&client, "connections", serde_json::json!({})).await.contains("once the user approves"));
    assert_eq!(call(&client, "query", q("UPDATE t SET x = 5 WHERE id = 1")).await, r#"{"rows_affected":1}"#);
    let no = call(&client, "query", q("UPDATE t SET x = 6 WHERE id = 2")).await;
    assert!(no.starts_with("ERROR") && no.contains("said no"), "{no}");
    let late = call(&client, "query", q("UPDATE t SET x = 7 WHERE id = 2")).await;
    assert!(late.starts_with("ERROR") && late.contains("no answer"), "{late}");
    assert!(call(&client, "query", q("DELETE FROM t")).await.contains("destructive"), "never asked");
    let rows = call(&client, "query", q("SELECT x FROM t ORDER BY id")).await;
    assert!(rows.contains(r#"[["5"],["2"]]"#), "{rows}");
    user.abort();
    client.cancel().await.unwrap();
    assert_eq!(*seen.lock().unwrap(), ["dry run: would change 1 row(s)", "dry run: would change 1 row(s)"]);
    assert!(crate::agents::pending(&pending).is_empty() && std::fs::read_dir(&pending).unwrap().count() == 0);
    let entries = agents::read(&log);
    assert!(entries[0].approved && entries[0].changed == Some(1), "{:?}", entries[0]);
    assert!(!entries[1].approved && entries[1].refused.as_deref().is_some_and(|r| r.contains("said no")));
}

#[test]
fn answers_say_when_rows_were_left_out() {
    let wide = "x".repeat(1500);
    let outcome = QueryOutcome {
        columns: vec!["a".into(), "b".into()],
        rows: vec![vec![Some(wide.clone()), Some(wide)]; 1000],
        total_rows: 1000,
        ..QueryOutcome::default()
    };
    let json: serde_json::Value = serde_json::from_str(&answer(&outcome, false)).unwrap();
    assert_eq!(json["more_rows"], true);
    assert!(json["rows"].as_array().unwrap().len() < 1000);
}

/// On live servers: what an agent might try to get around read-only
/// (SET, BEGIN … COMMIT, a setting switched from a query, a statement
/// riding along an explain) is refused or rolled back, and a production
/// dry run changes nothing.
#[tokio::test]
async fn live_agents_on_servers() {
    for engine in Engine::BUILT {
        let Some((setup, params)) = crate::db::tests::own_live_db(engine, "podelis_agents").await else { continue };
        let sql = crate::db::tests::test_sql(engine);
        setup.run("DROP TABLE IF EXISTS agent_t", 1).await.unwrap();
        setup.run("CREATE TABLE agent_t (id int PRIMARY KEY, x int)", 1).await.unwrap();
        setup.run("INSERT INTO agent_t VALUES (1, 1), (2, 2), (3, 3)", 1).await.unwrap();
        setup.run(&sql.analyze.replace("{table}", "agent_t"), 1).await.unwrap();
        if let Some([create, _, _]) = sql.sequence {
            setup.run(create, 1).await.unwrap();
        }
        let profile = |name: &str, agents, mode| ConnectionProfile {
            port: if params.port == 0 { String::new() } else { params.port.to_string() },
            user: params.user.clone(),
            database: params.database.clone(),
            password_command: vec!["printf".into(), "%s".into(), params.password.clone()],
            ..server_connection(name, engine, &params.host, agents, mode)
        };
        let dir = tempfile::tempdir().unwrap();
        let server = Server::new(
            "p1".into(),
            "live".into(),
            vec![
                profile("reads", AgentAccess::Read, ConnectionMode::Normal),
                profile("writes", AgentAccess::Write, ConnectionMode::Normal),
                profile("prod", AgentAccess::Write, ConnectionMode::Production),
            ],
            dir.path().join("log.jsonl"),
        )
        .guarding(2);
        let client = connect(server).await;
        let q = |conn: &str, sql: &str| serde_json::json!({ "connection": conn, "sql": sql });
        let refused = |answer: String| assert!(answer.starts_with("ERROR"), "{engine:?}: {answer}");
        // Nothing that changes the session or ends Podėlis's transaction.
        refused(call(&client, "query", q("reads", "SET default_transaction_read_only = off")).await);
        refused(call(&client, "query", q("reads", "SET SESSION TRANSACTION READ WRITE")).await);
        refused(call(&client, "query", q("prod", "UPDATE agent_t SET x = 0 WHERE id = 1; COMMIT")).await);
        // MySQL runs an executable comment's code: refused, not hidden
        // from the checks (a read-only agent wrote a file this way).
        if engine == crate::db::Engine::MySql {
            let hidden = "SELECT 1 /*! INTO OUTFILE '/var/lib/mysql-files/podelis_agent.txt' */";
            let answer = call(&client, "query", q("reads", hidden)).await;
            assert!(answer.starts_with("ERROR") && answer.contains("executable comment"), "{answer}");
        }
        // A statement riding along after the one to plan.
        refused(call(&client, "explain", q("writes", "SELECT 1; DELETE FROM agent_t WHERE id = 1")).await);
        // Switching the session's read-only setting from inside a query is
        // refused (it'd change the session; `standard_conforming_strings`
        // the same way would change where strings end); each call is its
        // own read-only transaction anyway.
        if let Some(switched) = sql.read_only_off {
            let answer = call(&client, "query", q("reads", switched)).await;
            assert!(answer.starts_with("ERROR") && answer.contains("changes the session's settings"), "{answer}");
        }
        if let Some([_, moves, moved]) = sql.sequence {
            let sneaky = call(&client, "query", q("reads", moves)).await;
            assert!(sneaky.starts_with("ERROR") && sneaky.contains("read-only"), "{engine:?}: {sneaky}");
            // On production a read is rolled back too.
            refused(call(&client, "query", q("prod", moves)).await);
            let seq = setup.run(moved, 1).await.unwrap();
            assert_eq!(seq.rows[0][0].as_deref(), Some("false"), "{engine:?}: the sequence moved");
        }
        // A whole "big" table (2 rows is big here) on production: refused
        // where the engine can tell how big (not SQLite); elsewhere it runs.
        let heavy = call(&client, "query", q("prod", "SELECT * FROM agent_t WHERE x = 2")).await;
        let tables = setup.tables(false).await.unwrap();
        if tables.iter().find(|t| t.name == "agent_t").is_some_and(|t| t.rows.is_some()) {
            assert!(
                heavy.starts_with("ERROR") && heavy.contains("read all of") && heavy.contains("agent_t"),
                "{engine:?}: {heavy}"
            );
        } else {
            assert!(!heavy.starts_with("ERROR"), "{engine:?}: {heavy}");
        }
        assert!(!call(&client, "query", q("reads", "SELECT * FROM agent_t WHERE x = 2")).await.starts_with("ERROR"));
        let all = setup.run("SELECT count(*) FROM agent_t", 1).await.unwrap();
        assert_eq!(all.rows[0][0].as_deref(), Some("3"), "{engine:?}: a row went");
        if let Some(create) = sql.without_rollback {
            // A table a rollback can't undo: no dry runs in this database.
            setup.run("DROP TABLE IF EXISTS agent_m", 1).await.unwrap();
            setup.run(create, 1).await.unwrap();
            let dry = call(&client, "query", q("prod", "UPDATE agent_t SET x = 0 WHERE x > 1")).await;
            assert!(dry.starts_with("ERROR") && dry.contains("agent_m"), "{dry}");
            setup.run("DROP TABLE agent_m", 1).await.unwrap();
        }
        let dry = call(&client, "query", q("prod", "UPDATE agent_t SET x = 0 WHERE x > 1")).await;
        assert!(dry.contains("would change 2 row(s)"), "{engine:?}: {dry}");
        let left = setup.run("SELECT count(*) FROM agent_t WHERE x = 0", 1).await.unwrap();
        assert_eq!(left.rows[0][0].as_deref(), Some("0"), "{engine:?}: the dry run changed rows");
        client.cancel().await.unwrap();
        setup.run("DROP TABLE agent_t", 1).await.unwrap();
    }
}

/// Makes a file database with `sql` in it, through Podėlis's own driver
/// (on its own runtime: the test's is busy with the protocol).
#[cfg(feature = "sqlite")]
fn make_file_db(path: &std::path::Path, sql: &str) {
    let target = path.to_path_buf();
    let sql = sql.to_string();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        rt.block_on(async {
            let params = crate::db::tests::file_params(&target, false);
            let db = Database::connect(params).await.unwrap();
            db.run(&sql, 1).await.unwrap();
            db.close().await;
        });
    })
    .join()
    .unwrap();
}
