use rmcp::model::{CallToolRequestParams, ClientCapabilities, ClientConfig};
use rmcp::service::RunningService;
use rmcp::{ClientHandler, RoleClient};

use super::*;
use crate::db::Engine;

pub(crate) struct TestAgent;

impl ClientHandler for TestAgent {
    fn get_info(&self) -> ClientConfig {
        ClientConfig::new(
            ClientCapabilities::default(),
            Implementation::new("test-agent", "1").with_title("Test Agent"),
        )
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

/// DuckDB through the protocol: reads, writes and production's dry run as
/// on any engine, and the file it is shut off from: what its SQL reads
/// by itself (`FROM 'file.csv'`) fails on the agents' connection, whatever
/// the words.
// (`file_profile`, a saved file connection, is the SQLite build's.)
#[cfg(all(feature = "duckdb", feature = "sqlite"))]
#[tokio::test]
async fn duckdb_for_agents_reaches_no_file() {
    let dir = tempfile::tempdir().unwrap();
    let (path, secret) = (dir.path().join("shop.duckdb"), dir.path().join("secret.csv"));
    std::fs::write(&secret, "token\nhunter2\n").unwrap();
    let target = path.clone();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        rt.block_on(async {
            let params = crate::db::ConnectParams {
                engine: Engine::DuckDb,
                host: target.to_string_lossy().into_owned(),
                port: 0,
                user: String::new(),
                password: String::new(),
                database: String::new(),
                ssl_mode: crate::db::SslMode::Disable,
                read_only: false,
                ssh: None,
                password_command: Vec::new(),
                for_agents: false,
            };
            let db = Database::connect(params).await.unwrap();
            db.run("create table t (id int primary key, x int); insert into t values (1, 1), (2, 2)", 1).await.unwrap();
            db.close().await;
        });
    })
    .join()
    .unwrap();
    let duck = |name: &str, agents, mode| ConnectionProfile {
        engine: Engine::DuckDb,
        agents,
        mode,
        timeout: "5".into(),
        ..crate::config::store::file_profile(name, &path)
    };
    // One instance of the file per process: the connections take turns.
    for (agents, mode) in
        [(AgentAccess::Read, ConnectionMode::Normal), (AgentAccess::Write, ConnectionMode::Production)]
    {
        let server = Server::new("p1".into(), "shop".into(), vec![duck("d", agents, mode)], dir.path().join("a.jsonl"));
        let client = connect(server).await;
        let q = |sql: &str| serde_json::json!({ "connection": "d", "sql": sql });
        assert!(call(&client, "query", q("select x from t order by id")).await.contains("\"2\""));
        let file = format!("select * from '{}'", secret.display());
        let read = call(&client, "query", q(&file)).await;
        assert!(read.starts_with("ERROR") && !read.contains("hunter2"), "{agents:?}: {read}");
        let copied = call(&client, "query", q(&format!("create table s as {file}"))).await;
        assert!(copied.starts_with("ERROR") && !copied.contains("hunter2"), "{agents:?}: {copied}");
        if agents == AgentAccess::Write {
            let dry = call(&client, "query", q("update t set x = 0 where x > 0")).await;
            assert!(dry.contains("dry run") && dry.contains("would change 2 row(s)"), "{dry}");
            assert!(call(&client, "query", q("select count(*) from t where x = 0")).await.contains("[[\"0\"]]"));
        }
        client.cancel().await.unwrap();
        // The server lets go of the file once its session has ended.
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// MongoDB through the protocol, by the same rules as SQL's engines:
/// reads, writes where allowed (one a call), production's dry run in a
/// transaction where the server has them (a replica set) and refused
/// where it hasn't, the guard against whole big collections, and plans.
/// Live with `PODELIS_TEST_MONGO_PORT` (and `_REPLICA_SET` for one).
#[cfg(feature = "mongodb")]
#[tokio::test]
async fn live_agents_on_mongodb() {
    let Some(port) = std::env::var("PODELIS_TEST_MONGO_PORT").ok() else { return };
    let host = std::env::var("PODELIS_TEST_MONGO_HOST").unwrap_or_else(|_| "127.0.0.1".into());
    let replica_set = std::env::var_os("PODELIS_TEST_MONGO_REPLICA_SET").is_some();
    let profile = |name: &str, agents, mode| ConnectionProfile {
        port: port.clone(),
        database: "podelis_mongo_agents".into(),
        timeout: "10".into(),
        ..server_connection(name, Engine::MongoDb, &host, agents, mode)
    };
    let setup = Database::connect(
        crate::config::draft::ConnectionDraft::from_profile(
            &profile("s", AgentAccess::Write, ConnectionMode::Normal),
            String::new(),
        )
        .params()
        .unwrap(),
    )
    .await
    .unwrap();
    setup.run("db.runCommand({dropDatabase: 1})", 1).await.unwrap();
    setup.run("db.agent_t.insertMany([{_id: 1, x: 1}, {_id: 2, x: 2}, {_id: 3, x: 3}])", 1).await.unwrap();
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
    let count = || async { setup.run("db.agent_t.countDocuments({x: 0})", 1).await.unwrap().rows[0][0].clone() };

    let schema = call(&client, "schema", serde_json::json!({ "connection": "reads" })).await;
    assert!(schema.contains("agent_t") && schema.contains("_id"), "{schema}");
    let rows = call(&client, "query", q("reads", "db.agent_t.find({x: {$gte: 2}}).sort({_id: 1})")).await;
    assert!(rows.contains("\"2\"") && rows.contains("\"3\"") && !rows.contains("\"1\""), "{rows}");
    let refused = |answer: String, why: &str| assert!(answer.starts_with("ERROR") && answer.contains(why), "{answer}");
    refused(call(&client, "query", q("reads", "db.agent_t.updateOne({_id: 1}, {$set: {x: 0}})")).await, "read only");
    refused(call(&client, "query", q("reads", "db.agent_t.find({\"$where\": \"sleep(10)\"})")).await, "JavaScript");
    refused(call(&client, "query", q("writes", "db.agent_t.deleteMany({})")).await, "destructive");
    refused(call(&client, "query", q("writes", "db.startTransaction()")).await, "startTransaction()");

    // Writes where allowed, one a call.
    let wrote = call(&client, "query", q("writes", "db.agent_t.updateOne({_id: 1}, {$set: {x: 0}})")).await;
    assert_eq!(wrote, r#"{"rows_affected":1}"#);
    assert_eq!(count().await.as_deref(), Some("1"));
    let two = "db.agent_t.updateOne({_id: 2}, {$set: {x: 0}}); db.agent_t.updateOne({_id: 3}, {$set: {x: 0}})";
    refused(call(&client, "query", q("writes", two)).await, "one statement a call");

    // Production: a dry run in a transaction, where there are any.
    let dry = call(&client, "query", q("prod", "db.agent_t.updateMany({x: {$gt: 1}}, {$set: {x: 0}})")).await;
    if replica_set {
        assert!(dry.contains("dry run") && dry.contains("would change 2 row(s)"), "{dry}");
    } else {
        refused(dry, "replica set");
    }
    assert_eq!(count().await.as_deref(), Some("1"), "nothing changed");

    // A whole "big" collection (3 documents is big here) on production.
    refused(call(&client, "query", q("prod", "db.agent_t.find({x: 2})")).await, "read all of agent_t");
    let by_key = call(&client, "query", q("prod", "db.agent_t.find({_id: 2})")).await;
    assert!(!by_key.starts_with("ERROR"), "{by_key}");
    let plan = call(&client, "explain", q("reads", "db.agent_t.find({x: 2})")).await;
    assert!(plan.contains("COLLSCAN") && plan.contains("reads all of agent_t"), "{plan}");
    refused(call(&client, "explain", q("reads", "db.agent_t.insertOne({})")).await, "can't be planned");
    client.cancel().await.unwrap();
    setup.run("db.runCommand({dropDatabase: 1})", 1).await.unwrap();
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
    let first = entries.iter().find(|e| e["sandbox"].is_number()).unwrap();
    assert_eq!(first["sandbox"], serde_json::json!(1), "the first sandbox is 1: {text}");
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
    // As values: the keys' order is serde_json's (a feature another
    // engine's driver turns on keeps them as inserted).
    let rows: serde_json::Value = serde_json::from_str(&rows).unwrap();
    assert_eq!(rows, serde_json::json!({"columns": ["x"], "more_rows": true, "rows": [["1"], ["2"]]}));
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
                    assert!(request.production, "a production connection: {request:?}");
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
        // A file open in this process is opened one way: as agents' (shut
        // off from other files), like the server's connections to it.
        let setup = if engine.is_file_based() {
            setup.close().await;
            Database::connect(crate::db::ConnectParams { for_agents: true, ..params.clone() }).await.unwrap()
        } else {
            setup
        };
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
        let rows = tables.iter().find(|t| t.name == "agent_t").and_then(|t| t.rows).filter(|r| *r >= 0);
        // An engine's row estimate, where it has one, goes with the table.
        let schema = call(&client, "schema", serde_json::json!({ "connection": "reads", "filter": "agent_t" })).await;
        assert_eq!(schema.contains(", ~"), rows.is_some(), "{engine:?}: {schema}");
        if let Some(rows) = rows {
            assert!(schema.contains(&format!(", ~{rows} rows")), "{engine:?}: {schema}");
        }
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

/// What agents learn of a table: its columns and keys, its indexes (not
/// the primary key's), its foreign keys, a view's definition, found in any
/// case; and its neighbours through foreign keys.
#[cfg(feature = "sqlite")]
#[tokio::test]
async fn agents_learn_a_tables_shape_and_its_neighbours() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s.db");
    make_file_db(
        &path,
        "CREATE TABLE parent (id INTEGER PRIMARY KEY, code TEXT NOT NULL); \
         CREATE UNIQUE INDEX parent_code ON parent (code); \
         CREATE TABLE child (id INTEGER PRIMARY KEY, parent_id INT REFERENCES parent (id), note TEXT); \
         CREATE VIEW kids AS SELECT id, note FROM child",
    );
    let shared = vec![file_connection("s", &path, AgentAccess::Read, ConnectionMode::Normal)];
    let client = connect(Server::new("p1".into(), "shop".into(), shared, dir.path().join("log.jsonl"))).await;
    let describe =
        |table: &str| call(&client, "describe_table", serde_json::json!({ "connection": "s", "table": table }));
    let parent = describe("PARENT").await;
    assert!(parent.starts_with("parent\n"), "{parent}");
    assert!(
        parent.contains("  id INTEGER NOT NULL PRIMARY KEY") || parent.contains("  id INTEGER PRIMARY KEY"),
        "{parent}"
    );
    assert!(parent.contains("  code TEXT NOT NULL"), "{parent}");
    assert!(parent.contains("unique index parent_code (code)"), "{parent}");
    assert!(!parent.contains("autoindex") && parent.matches("index").count() == 1, "only its own index: {parent}");
    let child = describe("child").await;
    assert!(child.contains("  note TEXT\n"), "nullable, no NOT NULL: {child}");
    assert!(child.contains("foreign key parent_id → parent"), "{child}");
    let view = describe("kids").await;
    assert!(view.starts_with("kids (view)\n") && view.contains("definition: CREATE VIEW kids"), "{view}");
    assert!(describe("nope").await.starts_with("ERROR No table 'nope'"));
    let related = call(&client, "related_tables", serde_json::json!({ "connection": "s", "table": "parent" })).await;
    assert!(related.contains("child") && related.contains("parent_id"), "{related}");
}

/// The schema of a big database is cut, with a note to filter, and a filter
/// that matches nothing says so.
#[cfg(feature = "sqlite")]
#[tokio::test]
async fn a_big_schema_is_cut_with_a_note() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("big.db");
    let columns: Vec<String> = (0..12).map(|c| format!("a_rather_long_column_name_{c} TEXT")).collect();
    let tables: Vec<String> =
        (0..700).map(|t| format!("CREATE TABLE table_number_{t} ({});", columns.join(", "))).collect();
    make_file_db(&path, &tables.join("\n"));
    let shared = vec![file_connection("big", &path, AgentAccess::Read, ConnectionMode::Normal)];
    let client = connect(Server::new("p1".into(), "shop".into(), shared, dir.path().join("log.jsonl"))).await;
    let all = call(&client, "schema", serde_json::json!({ "connection": "big" })).await;
    assert!(all.ends_with("… more tables left out: pass a filter\n"), "{}", &all[all.len().saturating_sub(200)..]);
    assert!(all.len() < 200_000 + 1_000, "{}", all.len());
    let one = call(&client, "schema", serde_json::json!({ "connection": "big", "filter": "NUMBER_699" })).await;
    assert_eq!(one.lines().count(), 1, "{one}");
    let none = call(&client, "schema", serde_json::json!({ "connection": "big", "filter": "nothing_like_it" })).await;
    assert_eq!(none, "No tables match.");
}

/// The log says who ran what, where: the agent as it calls itself (title
/// too), the engine, the tool, the SQL as sent but trimmed; a shared
/// connection that can't be opened is an error, an unshared one a refusal.
#[cfg(feature = "sqlite")]
#[tokio::test]
async fn the_log_says_who_ran_what_where() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s.db");
    make_file_db(&path, "CREATE TABLE t (x INT)");
    let log = dir.path().join("log.jsonl");
    let shared = vec![
        file_connection("s", &path, AgentAccess::Read, ConnectionMode::Normal),
        file_connection("w", &path, AgentAccess::Write, ConnectionMode::Normal),
        file_connection("gone", &dir.path().join("missing.db"), AgentAccess::Read, ConnectionMode::Normal),
    ];
    let client = connect(Server::new("p1".into(), "shop".into(), shared, log.clone())).await;
    let q = |connection: &str, sql: &str| serde_json::json!({ "connection": connection, "sql": sql });
    assert!(!call(&client, "query", q("s", "  select x from t  ")).await.starts_with("ERROR"));
    assert!(!call(&client, "explain", q("s", "select x from t")).await.starts_with("ERROR"));
    assert_eq!(call(&client, "query", q("w", "insert into t values (1)")).await, r#"{"rows_affected":1}"#);
    assert!(call(&client, "query", q("gone", "select 1")).await.starts_with("ERROR"));
    assert!(call(&client, "query", q("nope", "select 1")).await.starts_with("ERROR"));
    let text = std::fs::read_to_string(&log).unwrap();
    let entries: Vec<serde_json::Value> = text.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    let find = |connection: &str, tool: &str| {
        entries
            .iter()
            .find(|e| e["connection"] == connection && e["tool"] == tool)
            .unwrap_or_else(|| panic!("no {tool} on {connection}: {text}"))
    };
    let ran = find("s", "query");
    assert_eq!(ran["sql"], "select x from t", "{text}");
    assert_eq!(ran["engine"], serde_json::to_value(Engine::Sqlite).unwrap(), "{text}");
    assert_eq!(
        (ran["agent"].as_str(), ran["agent_title"].as_str()),
        (Some("test-agent"), Some("Test Agent")),
        "{text}"
    );
    assert!(ran["at"].as_str().is_some_and(|at| at.starts_with("20")), "{text}");
    assert!(ran["rows"] == 0 && ran["changed"].is_null(), "a read's rows: {text}");
    let wrote = find("w", "query");
    assert!(wrote["changed"] == 1 && wrote["rows"].is_null(), "a write's rows changed: {text}");
    assert_eq!(find("s", "explain")["sql"], "select x from t", "{text}");
    let gone = find("gone", "query");
    assert!(gone["error"].is_string() && gone["refused"].is_null(), "an error, not a refusal: {text}");
    let nope = find("nope", "query");
    assert!(nope["refused"].is_string() && nope["error"].is_null(), "a refusal: {text}");
}

/// A connection is opened for agents read-only on the server too, unless
/// they may write there and it isn't a read-only connection, so a write
/// that got past the SQL's checks still fails; changed sharing opens it
/// again with the new rights.
#[cfg(feature = "sqlite")]
#[tokio::test]
async fn agents_connections_are_read_only_unless_they_may_write() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s.db");
    make_file_db(&path, "CREATE TABLE t (x INT)");
    let shared = vec![
        file_connection("reads", &path, AgentAccess::Read, ConnectionMode::Normal),
        file_connection("writes", &path, AgentAccess::Write, ConnectionMode::Normal),
        file_connection("locked", &path, AgentAccess::Write, ConnectionMode::ReadOnly),
    ];
    let server = Server::new("p1".into(), "shop".into(), shared, dir.path().join("log.jsonl"));
    // Straight to the connection, past the SQL's checks.
    let writes = async |server: &Server, name: &str| {
        let (db, _, _) = server.database(name).await.unwrap();
        db.run("INSERT INTO t VALUES (1)", 1).await.is_ok()
    };
    assert!(!writes(&server, "reads").await, "read access: read-only on the server");
    assert!(writes(&server, "writes").await);
    assert!(!writes(&server, "locked").await, "a read-only connection stays read-only");
    // Sharing changed from write to read: opened again, read-only.
    let store = dir.path().join("connections.toml");
    let save = |agents: &str| {
        let toml = format!(
            "[[projects]]\nid = \"p1\"\nname = \"shop\"\n[[projects.connections]]\nname = \"s\"\nengine = \"Sqlite\"\n\
             host = \"{}\"\nport = \"\"\nuser = \"\"\ndatabase = \"\"\nagents = \"{agents}\"\n",
            path.display()
        );
        std::fs::write(&store, toml).unwrap();
    };
    let server =
        Server::new("p1".into(), "shop".into(), Vec::new(), dir.path().join("log.jsonl")).reading(store.clone());
    save("write");
    assert!(writes(&server, "s").await);
    save("read");
    assert!(!writes(&server, "s").await, "the new rights, not the old connection");
}

/// An answer exactly at the size limit isn't cut, and when every row came
/// it says none were left out.
#[test]
fn an_answer_at_the_limit_is_whole() {
    // Each cell counts its length and 3: 1,000 a row, 200 rows, the limit.
    let cell = "x".repeat(997);
    let outcome = QueryOutcome {
        columns: vec!["a".into()],
        rows: vec![vec![Some(cell)]; 200],
        total_rows: 200,
        ..QueryOutcome::default()
    };
    let json: serde_json::Value = serde_json::from_str(&answer(&outcome, false)).unwrap();
    assert_eq!(json["rows"].as_array().unwrap().len(), 200);
    assert_eq!(json["more_rows"], false);
    assert!(json["notes"].is_null(), "{}", json["notes"]);
}

/// A sandbox in use isn't rolled back as idle: idle counts from its last
/// call, not from its first.
#[cfg(feature = "sqlite")]
#[tokio::test]
async fn a_sandbox_in_use_isnt_idle() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s.db");
    make_file_db(&path, "CREATE TABLE t (x INT)");
    let log = dir.path().join("log.jsonl");
    let sandbox = vec![file_connection("box", &path, AgentAccess::Sandbox, ConnectionMode::Normal)];
    let server = Server::new("p1".into(), "shop".into(), sandbox, log.clone()).idling(Duration::from_secs(2));
    let client = connect(server).await;
    let q = serde_json::json!({ "connection": "box", "sql": "insert into t values (1)" });
    // A call every half second for 3.5 s: never 2 s apart.
    for _ in 0..8 {
        assert!(!call(&client, "query", q.clone()).await.starts_with("ERROR"));
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    let entries = agents::read(&log);
    assert!(entries.iter().all(|e| e.sandbox == Some(1)), "one sandbox throughout: {entries:?}");
}

/// A sandbox whose connection was opened again (its sharing changed) is
/// lost, said so, and never written into; the next call starts a new one.
#[cfg(feature = "sqlite")]
#[tokio::test]
async fn a_sandbox_on_a_reopened_connection_is_lost() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s.db");
    make_file_db(&path, "CREATE TABLE t (x INT)");
    let store = dir.path().join("connections.toml");
    let save = |timeout: &str| {
        let toml = format!(
            "[[projects]]\nid = \"p1\"\nname = \"shop\"\n[[projects.connections]]\nname = \"box\"\nengine = \"Sqlite\"\n\
             host = \"{}\"\nport = \"\"\nuser = \"\"\ndatabase = \"\"\nagents = \"sandbox\"\ntimeout = \"{timeout}\"\n",
            path.display()
        );
        std::fs::write(&store, toml).unwrap();
    };
    save("5");
    let log = dir.path().join("log.jsonl");
    let server = Server::new("p1".into(), "shop".into(), Vec::new(), log.clone()).reading(store.clone());
    let client = connect(server).await;
    let q = |sql: &str| serde_json::json!({ "connection": "box", "sql": sql });
    assert!(!call(&client, "query", q("insert into t values (1)")).await.starts_with("ERROR"));
    save("6");
    let lost = call(&client, "query", q("insert into t values (2)")).await;
    assert!(lost.starts_with("ERROR") && lost.contains("the sandbox was lost"), "{lost}");
    let next = call(&client, "query", q("select count(*) from t")).await;
    assert!(next.contains(r#"[["0"]]"#), "a new sandbox, with neither insert: {next}");
    let numbers: Vec<Option<u32>> = agents::read(&log).iter().map(|e| e.sandbox).collect();
    assert_eq!(numbers.first(), Some(&Some(1)));
    assert_eq!(numbers.last(), Some(&Some(2)), "{numbers:?}");
}

/// On the servers: a write's dry run for the user approving it (none for a
/// schema change where it commits at once), and a sandbox's end turning
/// MySQL's autocommit back on.
#[tokio::test]
async fn live_dry_run_notes_and_sandbox_ends() {
    for engine in [Engine::Postgres, Engine::MySql] {
        let Some((db, params)) = crate::db::tests::own_live_db(engine, "podelis_agent_dry").await else { continue };
        db.run("DROP TABLE IF EXISTS dry_t", 1).await.unwrap();
        db.run("DROP TABLE IF EXISTS dry_u", 1).await.unwrap();
        db.run("CREATE TABLE dry_t (id INT PRIMARY KEY, x INT)", 1).await.unwrap();
        db.run("INSERT INTO dry_t VALUES (1, 1), (2, 2)", 1).await.unwrap();
        let profile = ConnectionProfile {
            port: params.port.to_string(),
            user: params.user.clone(),
            database: params.database.clone(),
            ..server_connection("d", engine, &params.host, AgentAccess::Approve, ConnectionMode::Production)
        };
        let ddl = dry_run_note(&db, &profile, "CREATE TABLE dry_u (id INT)", &mut Turn::default()).await.unwrap();
        let write =
            dry_run_note(&db, &profile, "UPDATE dry_t SET x = 0 WHERE x > 0", &mut Turn::default()).await.unwrap();
        assert_eq!(write, "dry run: would change 2 row(s)", "{engine:?}");
        if engine.capabilities().ddl_commits {
            assert_eq!(ddl, format!("no dry run: {} commits schema changes at once", engine.label()));
        } else {
            assert_eq!(ddl, "dry run: would change 0 row(s)", "{engine:?}: a schema change rolls back here");
        }
        let changed = db.run("SELECT count(*) FROM dry_t WHERE x = 0", 1).await.unwrap();
        assert_eq!(changed.rows[0][0].as_deref(), Some("0"), "{engine:?}: dry runs change nothing");
        if engine == Engine::MySql {
            db.run_once("SET autocommit = 0", 1).await.unwrap();
            end_sandbox(&db).await;
            let on = db.run_once("SELECT @@autocommit", 1).await.unwrap();
            assert_eq!(on.rows[0][0].as_deref(), Some("1"), "autocommit back on");
        }
        db.run("DROP TABLE dry_t", 1).await.unwrap();
        db.run("DROP TABLE IF EXISTS dry_u", 1).await.unwrap();
    }
}

/// A plan's notes come first, under their heading, only when it has some.
#[cfg(feature = "sqlite")]
#[tokio::test]
async fn a_plans_notes_lead_only_when_there_are_some() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s.db");
    make_file_db(&path, "CREATE TABLE t (id INTEGER PRIMARY KEY, x INT)");
    let shared = vec![file_connection("s", &path, AgentAccess::Read, ConnectionMode::Normal)];
    let client = connect(Server::new("p1".into(), "shop".into(), shared, dir.path().join("log.jsonl"))).await;
    let explain = |sql: &str| call(&client, "explain", serde_json::json!({ "connection": "s", "sql": sql }));
    let sorted = explain("SELECT * FROM t ORDER BY x").await;
    assert!(sorted.starts_with("What stands out:\n- Sorts in a temporary B-tree"), "{sorted}");
    let looked_up = explain("SELECT * FROM t WHERE id = 2").await;
    assert!(!looked_up.contains("What stands out") && looked_up.contains("PRIMARY KEY"), "{looked_up}");
}

/// A call cut short between its transaction's begin and its end leaves it
/// open on the session: the next call rolls it back first, so its own
/// commit commits only itself (found in review, 2026-11-09).
#[cfg(feature = "sqlite")]
#[tokio::test]
async fn a_transaction_a_cancelled_call_left_open_is_rolled_back_first() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("c.db");
    make_file_db(&path, "CREATE TABLE t (x INT)");
    let db = Database::connect(crate::db::tests::file_params(&path, false)).await.unwrap();
    let profile = file_connection("c", &path, AgentAccess::Write, ConnectionMode::Normal);
    // As a dry run cancelled after its write: begun, written, never ended.
    let mut held = Turn { left_open: true };
    db.run_once("BEGIN", 1).await.unwrap();
    db.run_once("INSERT INTO t VALUES (1)", 1).await.unwrap();
    settle(&db, &mut held).await;
    assert!(!held.left_open);
    run_wrapped(&db, &profile, Wrap::Commit, "INSERT INTO t VALUES (2)", 1, &mut held).await.unwrap();
    assert!(!held.left_open, "ended");
    let rows = db.run_once("SELECT x FROM t", 10).await.unwrap().rows;
    assert_eq!(rows, vec![vec![Some("2".to_string())]], "the dry run's write wasn't committed with it");
    db.close().await;
}
