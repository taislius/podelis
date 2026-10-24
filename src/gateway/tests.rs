use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

use super::client::{CallError, Client, connect_or_start};
use super::protocol::code;
use super::server::{Ended, Options, serve};

/// A gateway for `dir` in this process, stopping after `idle`.
pub(crate) fn start(dir: &Path, idle: Option<Duration>) -> tokio::task::JoinHandle<std::io::Result<Ended>> {
    let dir = dir.to_path_buf();
    tokio::spawn(async move { serve(&dir, Options::new(idle), std::future::pending()).await })
}

async fn connect(dir: &Path) -> Client {
    for _ in 0..200 {
        if let Ok(client) = Client::connect(dir, "test").await {
            return client;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the gateway didn't start");
}

/// Sends raw lines and reads one line back.
async fn raw(dir: &Path, lines: &str) -> (Value, UnixStream) {
    let mut stream = UnixStream::connect(super::socket_path(dir)).await.unwrap();
    stream.write_all(lines.as_bytes()).await.unwrap();
    let mut reader = BufReader::new(&mut stream);
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();
    (serde_json::from_str(&line).unwrap(), stream)
}

#[tokio::test]
async fn hello_then_status_and_shutdown() {
    let dir = tempfile::tempdir().unwrap();
    let server = start(dir.path(), None);
    let mut client = connect(dir.path()).await;
    assert_eq!(client.hello["protocol"], json!(super::PROTOCOL));
    let status = client.call("status", json!({})).await.unwrap();
    assert_eq!((status["clients"].clone(), status["pid"].clone()), (json!(1), json!(std::process::id())));
    assert!(status["idle_stop_secs"].is_null());
    let err = client.call("nope", json!({})).await.unwrap_err();
    assert!(matches!(err, CallError::Rpc(e) if e.code == code::METHOD_NOT_FOUND));
    // Only for this user; the socket goes with the gateway.
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(super::dir(dir.path())).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o700);
    client.call("shutdown", json!({})).await.unwrap();
    assert_eq!(server.await.unwrap().unwrap(), Ended::Stopped("asked"));
    assert!(!super::socket_path(dir.path()).exists());
}

#[tokio::test]
async fn the_first_request_must_be_hello_in_this_protocol() {
    let dir = tempfile::tempdir().unwrap();
    let _server = start(dir.path(), None);
    drop(connect(dir.path()).await);
    let (reply, _) = raw(dir.path(), "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"status\"}\n").await;
    assert_eq!(reply["error"]["code"], json!(code::HELLO_FIRST));
    let hello = "{\"jsonrpc\":\"2.0\",\"id\":7,\"method\":\"hello\",\"params\":{\"client\":\"old\",\"protocol\":99}}\n";
    let (reply, mut stream) = raw(dir.path(), hello).await;
    assert_eq!((reply["id"].clone(), reply["error"]["code"].clone()), (json!(7), json!(code::VERSION)));
    assert!(reply["error"]["message"].as_str().unwrap().contains("podelis gateway stop"));
    // And then hung up on.
    let mut rest = String::new();
    assert_eq!(BufReader::new(&mut stream).read_line(&mut rest).await.unwrap(), 0);
    let (reply, _) = raw(dir.path(), "not json\n").await;
    assert_eq!(reply["error"]["code"], json!(code::PARSE));
}

#[tokio::test]
async fn one_gateway_per_folder_and_a_crashed_ones_socket_is_replaced() {
    let dir = tempfile::tempdir().unwrap();
    // A socket file left behind, with nothing listening.
    std::fs::create_dir_all(super::dir(dir.path())).unwrap();
    drop(std::os::unix::net::UnixListener::bind(super::socket_path(dir.path())).unwrap());
    let _server = start(dir.path(), None);
    let mut client = connect(dir.path()).await;
    let second = serve(dir.path(), Options::new(None), std::future::pending()).await.unwrap();
    assert_eq!(second, Ended::AlreadyRunning);
    // The first one is untouched.
    client.call("status", json!({})).await.unwrap();
}

#[tokio::test]
async fn it_stops_when_idle_and_tells_clients_when_it_stops() {
    let dir = tempfile::tempdir().unwrap();
    let server = start(dir.path(), Some(Duration::from_millis(150)));
    let client = connect(dir.path()).await;
    // Not while a client is connected.
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!server.is_finished());
    drop(client);
    let ended = tokio::time::timeout(Duration::from_secs(5), server).await.unwrap().unwrap().unwrap();
    assert_eq!(ended, Ended::Stopped("idle"));

    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let path = dir.path().to_path_buf();
    let server = tokio::spawn(async move {
        serve(&path, Options::new(None), async {
            let _ = rx.await;
        })
        .await
    });
    let mut client = connect(dir.path()).await;
    tx.send(()).unwrap();
    assert_eq!(server.await.unwrap().unwrap(), Ended::Stopped("interrupted"));
    let said = client.next_notification().await.unwrap();
    assert_eq!(said, ("stopping".to_string(), json!({ "reason": "interrupted" })));
    let err = client.call("status", json!({})).await.unwrap_err();
    assert!(matches!(err, CallError::Io(_)));
}

#[tokio::test]
async fn a_client_starts_the_gateway_when_none_runs() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().to_path_buf();
    let mut started = None;
    let mut client = connect_or_start(dir.path(), "tui", || {
        started = Some(start(&path, None));
        Ok(())
    })
    .await
    .unwrap();
    assert!(started.is_some());
    client.call("status", json!({})).await.unwrap();
    // Running: it's used, not started again.
    let again = connect_or_start(dir.path(), "tui", || panic!("started twice")).await;
    assert!(again.is_ok());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn calls_at_the_same_moment_keep_the_call_log_whole() {
    let dir = tempfile::tempdir().unwrap();
    let _server = start(dir.path(), None);
    connect(dir.path()).await;
    let calls = (0..20).map(|_| {
        let dir = dir.path().to_path_buf();
        tokio::spawn(async move {
            let mut client = connect(&dir).await;
            client.call("status", json!({})).await.unwrap();
        })
    });
    for call in calls.collect::<Vec<_>>() {
        call.await.unwrap();
    }
    let log = std::fs::read_to_string(super::log_path(dir.path())).unwrap();
    // A hello and a status each, and the first hello.
    assert_eq!(log.lines().count(), 41);
    for line in log.lines() {
        assert!(serde_json::from_str::<Value>(line).is_ok(), "a broken line: {line}");
    }
}

#[tokio::test]
async fn the_call_log_names_methods_never_parameters() {
    let dir = tempfile::tempdir().unwrap();
    let _server = start(dir.path(), None);
    let mut client = connect(dir.path()).await;
    let _ = client.call("nope", json!({ "secret": "hunter2" })).await;
    client.call("status", json!({})).await.unwrap();
    let log = std::fs::read_to_string(super::log_path(dir.path())).unwrap();
    let lines: Vec<Value> = log.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    let methods: Vec<&str> = lines.iter().filter_map(|l| l["method"].as_str()).collect();
    assert_eq!(methods, ["hello", "nope", "status"]);
    assert_eq!(lines[1]["error"], json!(code::METHOD_NOT_FOUND));
    assert_eq!(lines[2]["client"], json!("test"));
    assert!(!log.contains("hunter2"));
}

#[tokio::test]
async fn a_socket_path_too_long_for_the_os_says_so() {
    let dir = tempfile::tempdir().unwrap();
    let deep = dir.path().join("x".repeat(120));
    let err = serve(&deep, Options::new(None), std::future::pending()).await.unwrap_err();
    assert!(err.to_string().contains("PODELIS_CONFIG_DIR"), "{err}");
    // A client says so at once, not after waiting for a gateway that
    // can't listen.
    let asked = Instant::now();
    let err = connect_or_start(&deep, "tui", || panic!("not started")).await.err().unwrap();
    assert!(err.to_string().contains("PODELIS_CONFIG_DIR"), "{err}");
    assert!(asked.elapsed() < Duration::from_secs(1));
}

/// A model server on this machine: answers each connection with the next
/// of `replies` (an HTTP status, then the body), and keeps the requests.
pub(crate) async fn fake_model(replies: Vec<(u16, String)>) -> (String, tokio::task::JoinHandle<Vec<String>>) {
    use tokio::io::AsyncReadExt;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let mut seen = Vec::new();
        for (status, body) in replies {
            let (mut stream, _) = listener.accept().await.unwrap();
            // The request: headers, then as many bytes as they say.
            let mut request = Vec::new();
            let mut chunk = [0u8; 4096];
            loop {
                let n = stream.read(&mut chunk).await.unwrap();
                request.extend_from_slice(&chunk[..n]);
                let text = String::from_utf8_lossy(&request).to_string();
                if let Some(end) = text.find("\r\n\r\n") {
                    let length = text
                        .lines()
                        .find_map(|l| {
                            l.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .map(|v| v.trim().parse::<usize>().unwrap())
                        })
                        .unwrap_or(0);
                    if request.len() >= end + 4 + length {
                        break;
                    }
                }
            }
            seen.push(String::from_utf8_lossy(&request).to_string());
            let kind = if status == 200 { "text/event-stream" } else { "application/json" };
            let head =
                format!("HTTP/1.1 {status} X\r\ncontent-type: {kind}\r\nretry-after: 0\r\nconnection: close\r\n\r\n");
            stream.write_all(head.as_bytes()).await.unwrap();
            stream.write_all(body.as_bytes()).await.unwrap();
            stream.shutdown().await.unwrap();
        }
        seen
    });
    (url, server)
}

fn sse(events: &[Value]) -> String {
    use std::fmt::Write as _;
    events.iter().fold(String::new(), |mut out, e| {
        let _ = write!(out, "event: {}\ndata: {e}\n\n", e["type"].as_str().unwrap());
        out
    })
}

#[tokio::test]
async fn complete_asks_the_model_streams_its_answer_and_tries_again_when_busy() {
    let busy = json!({"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}).to_string();
    let answer = sse(&[
        json!({"type":"message_start","message":{"model":"claude-opus-5-5","usage":{"input_tokens":9}}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"select "}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"1"}}),
        json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":3}}),
        json!({"type":"message_stop"}),
    ]);
    let (url, server) = fake_model(vec![(529, busy), (200, answer)]).await;
    let dir = tempfile::tempdir().unwrap();
    let config =
        format!("[[models]]\nname = \"fake\"\napi = \"anthropic\"\nurl = \"{url}\"\nmodel = \"claude-opus-5-5\"\n");
    std::fs::write(dir.path().join("config.toml"), config).unwrap();
    crate::config::store::set_password("model:fake", "sk-test-key").unwrap();
    let _gateway = start(dir.path(), None);
    let mut client = connect(dir.path()).await;

    let listed = client.call("models", json!({})).await.unwrap();
    assert_eq!(listed["models"][0]["key"], json!("keychain"));
    assert!(!listed.to_string().contains("sk-test-key"), "the key never goes back");

    let params = json!({ "system": "Write SQL.", "messages": [{ "role": "user", "content": "one" }] });
    let reply = client.call("complete", params).await.unwrap();
    assert_eq!(reply["text"], json!("select 1"));
    assert_eq!((reply["model"].clone(), reply["stop_reason"].clone()), (json!("fake"), json!("end_turn")));
    assert_eq!(reply["usage"], json!({ "input_tokens": 9, "output_tokens": 3 }));
    // The pieces came first, as they were written.
    let pieces: Vec<&str> = client
        .notifications
        .iter()
        .filter(|(m, _)| m == "complete.delta")
        .map(|(_, p)| p["text"].as_str().unwrap())
        .collect();
    assert_eq!(pieces, ["select ", "1"]);

    let requests = server.await.unwrap();
    assert_eq!(requests.len(), 2, "tried again after 529");
    let request = requests[1].to_ascii_lowercase();
    assert!(request.starts_with("post /v1/messages "), "{request}");
    assert!(request.contains("x-api-key: sk-test-key") && request.contains("anthropic-version: 2023-06-01"));
    assert!(requests[1].contains("\"system\":\"Write SQL.\""));
    // The log has the model and its tokens, not what was asked or answered.
    let log = std::fs::read_to_string(super::log_path(dir.path())).unwrap();
    let complete: Value =
        log.lines().map(|l| serde_json::from_str::<Value>(l).unwrap()).find(|l| l["method"] == "complete").unwrap();
    assert_eq!((complete["model"].clone(), complete["output_tokens"].clone()), (json!("fake"), json!(3)));
    assert!(!log.contains("Write SQL") && !log.contains("select 1") && !log.contains("sk-test-key"));
    crate::config::store::delete_password("model:fake").unwrap();
}

#[tokio::test]
async fn complete_says_why_a_model_cant_answer() {
    let dir = tempfile::tempdir().unwrap();
    let _gateway = start(dir.path(), None);
    let mut client = connect(dir.path()).await;
    let ask = json!({ "messages": [{ "role": "user", "content": "hi" }] });
    let err = client.call("complete", ask.clone()).await.unwrap_err();
    assert!(matches!(&err, CallError::Rpc(e) if e.code == code::MODEL && e.message.contains("[[models]]")), "{err}");
    // (A name of its own: the test keychain is shared by tests running at once.)
    let config = "[[models]]\nname = \"unkeyed\"\napi = \"openai\"\nurl = \"https://gpu.example/v1\"\nmodel = \"x\"\n";
    std::fs::write(dir.path().join("config.toml"), config).unwrap();
    let err = client.call("complete", ask).await.unwrap_err();
    assert!(matches!(&err, CallError::Rpc(e) if e.message.contains("podelis gateway key unkeyed")), "{err}");
    let err = client.call("complete", json!({ "messages": [] })).await.unwrap_err();
    assert!(matches!(&err, CallError::Rpc(e) if e.code == code::INVALID_PARAMS), "{err}");
}

/// A saved file connection (SQLite) `shop` in project `p`, in `dir`'s config.
pub(crate) async fn saved_shop(dir: &Path) {
    use crate::db::{AgentAccess, ConnectionMode};
    let file = dir.join("shop.db");
    let params = crate::db::tests::file_params(&file, false);
    let db = crate::db::Database::connect(params).await.unwrap();
    let sql = "create table customers (id integer primary key, email text, country text);
               create table orders (id integer primary key, customer_id integer references customers(id), total numeric);
               create table products (id integer primary key, name text);
               create table order_items (id integer primary key, order_id integer references orders(id),
                                         product_id integer references products(id), qty integer);
               create table audit_log (id integer primary key, payload text);
               insert into customers values (1, 'secret@example.com', 'LT');";
    db.run(sql, 1).await.unwrap();
    db.close().await;
    let (mut store, _) = crate::config::store::ConnectionStore::load_from(dir.join("connections.toml"));
    let project = store.add_project("p").unwrap();
    let profile = crate::config::store::ConnectionProfile {
        unknown: crate::config::store::Unknown::default(),
        name: "shop".into(),
        engine: crate::db::tests::file_engine(),
        host: file.to_string_lossy().into_owned(),
        port: String::new(),
        user: String::new(),
        database: String::new(),
        ssl_mode: crate::db::SslMode::default(),
        mode: ConnectionMode::Normal,
        transaction: None,
        ssh: String::new(),
        timeout: String::new(),
        agents: AgentAccess::None,
        password_command: Vec::new(),
        legacy_keychain: false,
    };
    store.upsert(&project, None, profile).unwrap();
}

#[tokio::test]
async fn context_shows_the_tables_a_question_needs_from_a_saved_connection() {
    let dir = tempfile::tempdir().unwrap();
    saved_shop(dir.path()).await;
    let _gateway = start(dir.path(), None);
    let mut client = connect(dir.path()).await;
    let ask = json!({ "project": "p", "connection": "SHOP", "question": "products ordered by customers" });
    let built = client.call("context", ask).await.unwrap();
    let names: Vec<&str> = built["tables"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["main.customers", "main.products", "main.order_items", "main.orders"], "{built}");
    let text = built["text"].as_str().unwrap();
    assert!(text.starts_with("Database: SQLite."), "{text}");
    assert!(text.contains("product_id INTEGER → products.id"), "{text}");
    // The schema only: no rows, no unrelated tables.
    assert!(!text.contains("secret@example.com") && !text.contains("audit_log"), "{text}");
    // The log keeps the connection and the count, not the question.
    let log = std::fs::read_to_string(super::log_path(dir.path())).unwrap();
    assert!(log.contains("\"connection\":\"SHOP\"") && log.contains("\"tables\":4"), "{log}");
    assert!(!log.contains("ordered by"), "{log}");

    let err =
        client.call("context", json!({ "project": "p", "connection": "nope", "question": "x" })).await.unwrap_err();
    assert!(
        matches!(&err, CallError::Rpc(e) if e.code == code::DATABASE && e.message.contains("there: shop")),
        "{err}"
    );
}

#[tokio::test]
async fn context_never_moves_an_unreadable_connections_file() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("connections.toml");
    std::fs::write(&file, "[[projects]]\nid = 1\n").unwrap();
    let _gateway = start(dir.path(), None);
    let mut client = connect(dir.path()).await;
    let err = client.call("context", json!({ "connection": "x", "question": "y" })).await.unwrap_err();
    assert!(matches!(&err, CallError::Rpc(e) if e.message.contains("connections.toml is invalid")), "{err}");
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "[[projects]]\nid = 1\n", "left as it was");
}

pub(crate) fn claude_answer(text: &str) -> String {
    sse(&[
        json!({"type":"message_start","message":{"model":"claude-opus-5-5","usage":{"input_tokens":100}}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":text}}),
        json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":20}}),
        json!({"type":"message_stop"}),
    ])
}

#[tokio::test]
async fn ask_checks_the_models_sql_against_the_schema_and_never_runs_it() {
    let first = claude_answer(
        "```sql\nSELECT p.name, sum(oi.price) FROM products p JOIN order_items oi ON oi.product_id = p.id GROUP BY p.name\n```\nRevenue per product.",
    );
    let second = claude_answer(
        "```sql\nSELECT p.name, sum(oi.qty) AS sold FROM products p JOIN order_items oi ON oi.product_id = p.id GROUP BY p.name\n```\nThere's no price: this counts items sold.",
    );
    let (url, server) = fake_model(vec![(200, first), (200, second)]).await;
    let dir = tempfile::tempdir().unwrap();
    saved_shop(dir.path()).await;
    let config =
        format!("[[models]]\nname = \"near\"\napi = \"anthropic\"\nurl = \"{url}\"\nmodel = \"claude-opus-5-5\"\n");
    std::fs::write(dir.path().join("config.toml"), config).unwrap();
    let _gateway = start(dir.path(), None);
    let mut client = connect(dir.path()).await;

    let ask = json!({ "project": "p", "connection": "shop", "question": "products sold per product name" });
    let answer = client.call("ask", ask).await.unwrap();
    assert_eq!(answer["attempts"], json!(2));
    assert!(answer["sql"].as_str().unwrap().contains("sum(oi.qty) AS sold"), "{answer}");
    assert_eq!(answer["problems"], json!([]));
    assert_eq!(answer["said"], json!("There's no price: this counts items sold."));
    assert_eq!(answer["usage"], json!({ "input_tokens": 200, "output_tokens": 40 }));
    let streamed = client.notifications.iter().filter(|(m, _)| m == "ask.delta").count();
    assert_eq!(streamed, 2);

    let requests = server.await.unwrap();
    // The model saw the schema it needs, not the data; the second time,
    // what was wrong with its first SQL.
    assert!(
        requests[0].contains("products")
            && requests[0].contains("order_items")
            && !requests[0].contains("secret@example.com")
    );
    assert!(!requests[0].contains("audit_log"));
    // A model on this machine isn't offered lookups unless it's set to.
    assert!(!requests[0].contains("find_tables"));
    assert!(requests[1].contains("no table it reads has a column `price`"), "{}", requests[1]);
    // Nothing ran: the orders table is as it was (empty).
    let log = std::fs::read_to_string(super::log_path(dir.path())).unwrap();
    let entry: Value =
        log.lines().map(|l| serde_json::from_str::<Value>(l).unwrap()).find(|l| l["method"] == "ask").unwrap();
    assert_eq!((entry["attempts"].clone(), entry["model"].clone()), (json!(2), json!("near")));
    assert!(!log.contains("products sold") && !log.contains("SELECT"), "{log}");
}

/// A model that may look for itself asks for a table it wasn't shown;
/// it gets the table's columns (never rows), and says what it looked at.
#[tokio::test]
async fn ask_lets_the_model_look_up_a_table_it_was_not_shown() {
    let lookup = sse(&[
        json!({"type":"message_start","message":{"model":"claude-opus-5-5","usage":{"input_tokens":100}}}),
        json!({"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"toolu_1","name":"describe_table","input":{}}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"name\": "}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"\"audit_log\"}"}}),
        json!({"type":"content_block_stop","index":0}),
        json!({"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":10}}),
        json!({"type":"message_stop"}),
    ]);
    let answer = claude_answer("```sql\nSELECT count(*) FROM audit_log\n```\nCounts the audit entries.");
    let (url, server) = fake_model(vec![(200, lookup), (200, answer)]).await;
    let dir = tempfile::tempdir().unwrap();
    saved_shop(dir.path()).await;
    let config = format!(
        "[[models]]\nname = \"near\"\napi = \"anthropic\"\nurl = \"{url}\"\nmodel = \"claude-opus-5-5\"\ntools = true\n"
    );
    std::fs::write(dir.path().join("config.toml"), config).unwrap();
    let _gateway = start(dir.path(), None);
    let mut client = connect(dir.path()).await;
    let ask = json!({ "project": "p", "connection": "shop", "question": "products sold per product name" });
    let answer = client.call("ask", ask).await.unwrap();
    assert!(answer["sql"].as_str().unwrap().contains("FROM audit_log"), "{answer}");
    assert_eq!(answer["problems"], json!([]), "its SQL is checked against the whole schema");
    assert!(
        answer["looked_at"].as_array().unwrap().iter().any(|t| t.as_str().unwrap().ends_with("audit_log")),
        "{answer}"
    );
    let requests = server.await.unwrap();
    assert!(requests[0].contains("find_tables") && requests[0].contains("describe_table"));
    // The second request answers the call with the table, not its rows.
    assert!(requests[1].contains("tool_result") && requests[1].contains("toolu_1"), "{}", requests[1]);
    assert!(requests[1].contains("audit_log") && !requests[1].contains("secret@example.com"));
    let log = std::fs::read_to_string(super::log_path(dir.path())).unwrap();
    let entry: Value =
        log.lines().map(|l| serde_json::from_str::<Value>(l).unwrap()).find(|l| l["method"] == "ask").unwrap();
    assert_eq!(entry["looked_up"], json!(1));
}

#[tokio::test]
async fn ask_shows_a_schema_to_a_far_model_only_if_the_project_shares_it() {
    let dir = tempfile::tempdir().unwrap();
    saved_shop(dir.path()).await;
    let config = "[[models]]\nname = \"far\"\napi = \"anthropic\"\nurl = \"https://models.example\"\nmodel = \"claude-opus-5-5\"\n";
    std::fs::write(dir.path().join("config.toml"), config).unwrap();
    crate::config::store::set_password("model:far", "sk-far").unwrap();
    let _gateway = start(dir.path(), None);
    let mut client = connect(dir.path()).await;
    let ask = json!({ "project": "p", "connection": "shop", "question": "orders per customer" });
    let err = client.call("ask", ask).await.unwrap_err();
    let CallError::Rpc(e) = err else { panic!("{err}") };
    assert_eq!(e.code, code::NOT_SHARED);
    assert!(
        e.message.contains("only with models on this machine") && e.message.contains("https://models.example"),
        "{}",
        e.message
    );
    assert_eq!(e.data.as_ref().unwrap()["project"], json!("p"));
    crate::config::store::delete_password("model:far").unwrap();
}

#[tokio::test]
async fn the_teams_facts_go_to_the_model_with_the_tables_they_are_about() {
    let answer = claude_answer("```sql\nSELECT count(*) FROM orders\n```\nCounts orders.");
    let (url, server) = fake_model(vec![(200, answer)]).await;
    let dir = tempfile::tempdir().unwrap();
    saved_shop(dir.path()).await;
    let config =
        format!("[[models]]\nname = \"near\"\napi = \"anthropic\"\nurl = \"{url}\"\nmodel = \"claude-opus-5-5\"\n");
    std::fs::write(dir.path().join("config.toml"), config).unwrap();
    let _gateway = start(dir.path(), None);
    let mut client = connect(dir.path()).await;
    let add = |subject: &str, relation: &str, value: &str| json!({ "project": "p", "subject": subject, "relation": relation, "value": value });
    client.call("memory.add", add("orders.total", "means", "VAT included")).await.unwrap();
    client.call("memory.add", add("cancelled orders", "are", "those with a refund row")).await.unwrap();
    client.call("memory.add", add("audit_log", "is", "never for reports")).await.unwrap();
    let listed = client.call("memory.list", json!({ "project": "p" })).await.unwrap();
    assert_eq!(listed["facts"].as_array().unwrap().len(), 3);

    // `context` shows them: about orders (a table it chose), and matching
    // "cancelled"; not audit_log's.
    let ask = json!({ "project": "p", "connection": "shop", "question": "orders not cancelled per customer" });
    let built = client.call("context", ask.clone()).await.unwrap();
    assert_eq!(
        built["facts"],
        json!(["orders.total means VAT included", "cancelled orders are those with a refund row"])
    );
    let answer = client.call("ask", ask).await.unwrap();
    assert_eq!(answer["facts"].as_array().unwrap().len(), 2);
    let requests = server.await.unwrap();
    assert!(
        requests[0].contains("What this team has said about its data") && requests[0].contains("VAT included"),
        "{}",
        requests[0]
    );
    assert!(!requests[0].contains("never for reports"));

    let exported = client.call("memory.export", json!({ "project": "p" })).await.unwrap();
    assert!(exported["text"].as_str().unwrap().contains("audit_log | is | never for reports | user"));
    let forgotten = client.call("memory.forget", json!({ "project": "p", "id": 3 })).await.unwrap();
    assert_eq!(forgotten["id"], json!(3));
    let err = client.call("memory.forget", json!({ "project": "p", "id": 3 })).await.unwrap_err();
    assert!(matches!(&err, CallError::Rpc(e) if e.code == code::MEMORY), "{err}");
    // The log knows the project, never a fact.
    let log = std::fs::read_to_string(super::log_path(dir.path())).unwrap();
    assert!(log.contains("\"method\":\"memory.add\"") && !log.contains("VAT"), "{log}");
}

#[tokio::test]
async fn migrations_teach_the_memory_and_a_models_guesses_wait_for_the_user() {
    let first = claude_answer(
        "```sql\nSELECT count(*) FROM orders WHERE total > 0\n```\nCounts paid orders.\nFACT: orders.total | means | 0 for free orders\n- FACT: bad line",
    );
    let (url, server) = fake_model(vec![(200, first.clone()), (200, first)]).await;
    let dir = tempfile::tempdir().unwrap();
    saved_shop(dir.path()).await;
    let config =
        format!("[[models]]\nname = \"near\"\napi = \"anthropic\"\nurl = \"{url}\"\nmodel = \"claude-opus-5-5\"\n");
    std::fs::write(dir.path().join("config.toml"), config).unwrap();
    let migrations = dir.path().join("migrations");
    std::fs::create_dir(&migrations).unwrap();
    std::fs::write(
        migrations.join("20240301_orders.sql"),
        "-- Totals include VAT.\nALTER TABLE orders ADD COLUMN total numeric;\n",
    )
    .unwrap();
    let _gateway = start(dir.path(), None);
    let mut client = connect(dir.path()).await;
    let folder = migrations.to_string_lossy().into_owned();
    let learned = client.call("memory.learn", json!({ "project": "p", "folder": folder })).await.unwrap();
    assert_eq!((learned["files"].clone(), learned["facts"].clone()), (json!(1), json!(2)));
    // Learning again replaces, not doubles.
    client.call("memory.learn", json!({ "project": "p", "folder": folder })).await.unwrap();
    let listed = client.call("memory.list", json!({ "project": "p" })).await.unwrap();
    assert_eq!(listed["facts"].as_array().unwrap().len(), 2);

    let ask = json!({ "project": "p", "connection": "shop", "question": "paid orders" });
    let answer = client.call("ask", ask.clone()).await.unwrap();
    assert_eq!(
        answer["facts"],
        json!([
            "orders.total note Totals include VAT.",
            "orders history total added (2024-03-01, 20240301_orders.sql)"
        ])
    );
    // FACT lines are taken out of its note (a malformed one is dropped).
    assert_eq!(answer["said"], json!("Counts paid orders."));
    let suggested = answer["suggested"].as_array().unwrap();
    assert_eq!(suggested.len(), 1);
    assert_eq!(
        (suggested[0]["confirmed"].clone(), suggested[0]["source"].clone()),
        (json!(false), json!("model:near"))
    );
    // The same guess again: still said (the answer rests on it), not kept twice.
    let again = client.call("ask", ask.clone()).await.unwrap();
    assert_eq!(again["suggested"][0]["id"], suggested[0]["id"]);
    let listed = client.call("memory.list", json!({ "project": "p" })).await.unwrap();
    assert_eq!(listed["facts"].as_array().unwrap().len(), 3);
    // Not shown to a model until the user confirms it.
    let built = client.call("context", ask.clone()).await.unwrap();
    assert!(!built["facts"].to_string().contains("free orders"));
    client.call("memory.confirm", json!({ "project": "p", "id": suggested[0]["id"] })).await.unwrap();
    let built = client.call("context", ask.clone()).await.unwrap();
    assert!(built["facts"].to_string().contains("orders.total means 0 for free orders"), "{built}");
    let requests = server.await.unwrap();
    assert!(requests[0].contains("FACT: subject | relation | value"));
}

#[tokio::test]
async fn an_agent_gets_its_tools_from_the_gateway_through_mcp_open() {
    use rmcp::ServiceExt as _;
    let dir = tempfile::tempdir().unwrap();
    saved_shop(dir.path()).await;
    // Shared with agents, to read.
    let (mut store, _) = crate::config::store::ConnectionStore::load_from(dir.path().join("connections.toml"));
    let project_id = store.project_by_name("p").unwrap().id.clone();
    let mut shop = store.project_by_name("p").unwrap().connections[0].clone();
    shop.agents = crate::db::AgentAccess::Read;
    store.upsert(&project_id, Some("shop"), shop).unwrap();
    let _gateway = start(dir.path(), None);
    let mut client = connect(dir.path()).await;
    let opened = client.call("mcp.open", json!({ "project": "p", "name": "my agent", "dir": "/src/shop" })).await;
    assert_eq!(opened.unwrap(), json!({ "mcp": true }));
    // The connection is MCP now: a real MCP client on it.
    let agent = crate::agents::mcp::tests::TestAgent.serve(client.into_stream()).await.unwrap();
    let call = crate::agents::mcp::tests::call;
    let listed = call(&agent, "connections", json!({})).await;
    assert!(listed.contains("shop (SQLite): agents may read"), "{listed}");
    let rows = call(&agent, "query", json!({ "connection": "shop", "sql": "select email from customers" })).await;
    assert!(rows.contains("secret@example.com"), "{rows}");
    let refused = call(&agent, "query", json!({ "connection": "shop", "sql": "delete from customers" })).await;
    assert!(refused.starts_with("ERROR"), "{refused}");
    agent.cancel().await.unwrap();
    // Logged as before: the agent's name and folder, from the bridge.
    let log = std::fs::read_to_string(crate::agents::log_path(dir.path(), &project_id)).unwrap();
    assert!(log.contains("\"name\":\"my agent\"") && log.contains("/src/shop"), "{log}");
    // A project that isn't there: refused before any MCP.
    let mut other = connect(dir.path()).await;
    let err = other.call("mcp.open", json!({ "project": "nope" })).await.unwrap_err();
    assert!(matches!(&err, CallError::Rpc(e) if e.message.contains("no project named 'nope'")), "{err}");
}

#[tokio::test]
async fn a_key_command_runs_once_for_many_questions() {
    let reply = || claude_answer("OK");
    let (url, server) = fake_model(vec![(200, reply()), (200, reply())]).await;
    let dir = tempfile::tempdir().unwrap();
    let runs = dir.path().join("runs");
    let command = format!("sh -c 'echo run >> {}; echo sk-from-command'", runs.display());
    let config = format!(
        "[[models]]\nname = \"cmdkey\"\napi = \"anthropic\"\nurl = \"{url}\"\nmodel = \"claude-opus-5-5\"\nkey_command = \"{}\"\n",
        command.replace('\\', "\\\\").replace('"', "\\\"")
    );
    std::fs::write(dir.path().join("config.toml"), config).unwrap();
    let _gateway = start(dir.path(), None);
    let mut client = connect(dir.path()).await;
    let ask = json!({ "messages": [{ "role": "user", "content": "hi" }] });
    client.call("complete", ask.clone()).await.unwrap();
    client.call("complete", ask).await.unwrap();
    assert_eq!(std::fs::read_to_string(&runs).unwrap().lines().count(), 1, "the password manager was asked once");
    let requests = server.await.unwrap();
    assert!(requests.iter().all(|r| r.to_lowercase().contains("x-api-key: sk-from-command")));
}

/// A key slow to come (a keychain dialog, a password manager) is said to
/// the client while it waits; a quick one isn't.
#[tokio::test]
async fn a_slow_key_is_said_while_waiting() {
    let (url, server) = fake_model(vec![(200, claude_answer("OK")), (200, claude_answer("OK"))]).await;
    let dir = tempfile::tempdir().unwrap();
    let config = format!(
        "[[models]]\nname = \"slow\"\napi = \"anthropic\"\nurl = \"{url}\"\nmodel = \"claude-opus-5-5\"\nkey_command = \"sh -c 'sleep 3; echo sk-slow'\"\n"
    );
    std::fs::write(dir.path().join("config.toml"), config).unwrap();
    let _gateway = start(dir.path(), None);
    let mut client = connect(dir.path()).await;
    let ask = json!({ "messages": [{ "role": "user", "content": "hi" }] });
    let mut notes = Vec::new();
    client.call_streaming("complete", ask.clone(), |m, p| notes.push((m.to_string(), p.clone()))).await.unwrap();
    assert!(notes.iter().any(|(m, p)| m == "key.waiting" && p["model"] == json!("slow")), "{notes:?}");
    // Kept for a while: the next one doesn't wait, and says nothing.
    notes.clear();
    client.call_streaming("complete", ask, |m, p| notes.push((m.to_string(), p.clone()))).await.unwrap();
    assert!(!notes.iter().any(|(m, _)| m == "key.waiting"), "{notes:?}");
    drop(server);
}

/// A gateway as version 1 was: refuses other protocols with its message
/// (no `data`), shuts down when asked in its own.
fn old_gateway(dir: &Path) -> tokio::task::JoinHandle<()> {
    std::fs::create_dir_all(super::dir(dir)).unwrap();
    let socket = super::socket_path(dir);
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let (read, mut write) = stream.into_split();
            let mut lines = BufReader::new(read).lines();
            let Ok(Some(hello)) = lines.next_line().await else { continue };
            let hello: Value = serde_json::from_str(&hello).unwrap();
            if hello["params"]["protocol"] != json!(1) {
                let refusal = json!({ "jsonrpc": "2.0", "id": hello["id"], "error": { "code": code::VERSION,
                    "message": "this gateway speaks protocol 1, the client 2: it's from another Podėlis version" } });
                write.write_all(format!("{refusal}\n").as_bytes()).await.unwrap();
                continue;
            }
            let ok = json!({ "jsonrpc": "2.0", "id": hello["id"], "result": {} });
            write.write_all(format!("{ok}\n").as_bytes()).await.unwrap();
            let Ok(Some(shutdown)) = lines.next_line().await else { continue };
            let shutdown: Value = serde_json::from_str(&shutdown).unwrap();
            assert_eq!(shutdown["method"], json!("shutdown"));
            let ok = json!({ "jsonrpc": "2.0", "id": shutdown["id"], "result": { "stopping": true } });
            write.write_all(format!("{ok}\n").as_bytes()).await.unwrap();
            std::fs::remove_file(&socket).unwrap();
            return;
        }
    })
}

#[tokio::test]
async fn an_older_versions_gateway_is_replaced() {
    let dir = tempfile::tempdir().unwrap();
    let old = old_gateway(dir.path());
    let path = dir.path().to_path_buf();
    let mut started = false;
    let mut client = connect_or_start(dir.path(), "tui", || {
        started = true;
        drop(start(&path, None));
        Ok(())
    })
    .await
    .unwrap();
    assert!(started, "this version's gateway was started");
    old.await.unwrap();
    assert_eq!(client.hello["protocol"], json!(super::PROTOCOL));
    client.call("status", json!({})).await.unwrap();
}

#[tokio::test]
async fn a_client_that_goes_stops_its_model_call() {
    use tokio::io::AsyncReadExt;
    // A model that never answers: it reports when its connection closes.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let (closed_tx, closed_rx) = tokio::sync::oneshot::channel();
    let (asked_tx, asked_rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let _ = asked_tx.send(());
        let mut buf = [0u8; 4096];
        while stream.read(&mut buf).await.is_ok_and(|n| n > 0) {}
        let _ = closed_tx.send(());
    });
    let dir = tempfile::tempdir().unwrap();
    let config =
        format!("[[models]]\nname = \"slow\"\napi = \"anthropic\"\nurl = \"{url}\"\nmodel = \"claude-opus-5-5\"\n");
    std::fs::write(dir.path().join("config.toml"), config).unwrap();
    let _gateway = start(dir.path(), None);
    let mut client = connect(dir.path()).await;
    let ask = json!({ "messages": [{ "role": "user", "content": "hi" }] });
    // The client gives up (Esc in the app) once the model has been
    // asked (not before: then there'd be no call to stop): its
    // connection closes.
    tokio::select! {
        _ = client.call("complete", ask) => panic!("the model never answers"),
        asked = tokio::time::timeout(Duration::from_secs(10), asked_rx) => asked.unwrap().unwrap(),
    }
    drop(client);
    tokio::time::timeout(Duration::from_secs(3), closed_rx).await.expect("the model call was dropped").unwrap();
}

#[tokio::test]
async fn a_newer_versions_gateway_is_left_alone() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(super::dir(dir.path())).unwrap();
    let listener = tokio::net::UnixListener::bind(super::socket_path(dir.path())).unwrap();
    // A gateway of a later version: refuses this one, with its protocol.
    tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let (read, mut write) = stream.into_split();
            let mut lines = BufReader::new(read).lines();
            let Ok(Some(hello)) = lines.next_line().await else { continue };
            let hello: Value = serde_json::from_str(&hello).unwrap();
            assert_ne!(hello["method"], json!("shutdown"), "never stopped");
            let refusal = json!({ "jsonrpc": "2.0", "id": hello["id"], "error": { "code": code::VERSION,
                "message": "this gateway speaks protocol 99", "data": { "protocol": 99 } } });
            write.write_all(format!("{refusal}\n").as_bytes()).await.unwrap();
        }
    });
    let err = connect_or_start(dir.path(), "tui", || panic!("not started")).await.err().unwrap();
    assert!(matches!(&err, CallError::Rpc(e) if e.message.contains("a newer Podėlis")), "{err}");
    assert!(super::socket_path(dir.path()).exists());
}

#[tokio::test]
async fn replacing_a_gateway_another_client_already_stopped_is_fine() {
    let dir = tempfile::tempdir().unwrap();
    let refused = super::protocol::RpcError::new(code::VERSION, "this gateway speaks protocol 1, the client 2");
    // No socket any more: the other client got there first.
    super::client::replace_other(dir.path(), &refused).await.unwrap();
}

#[tokio::test]
async fn an_openai_answer_without_done_still_counts() {
    let chunks = [
        json!({"model":"m","choices":[{"delta":{"content":"OK"},"finish_reason":"stop"}]}),
        json!({"choices":[],"usage":{"prompt_tokens":3,"completion_tokens":1}}),
    ];
    let body = chunks.iter().fold(String::new(), |body, c| body + &format!("data: {c}\n\n"));
    let (url, _server) = fake_model(vec![(200, body)]).await;
    let dir = tempfile::tempdir().unwrap();
    let config = format!("[[models]]\nname = \"near\"\napi = \"openai\"\nurl = \"{url}/v1\"\nmodel = \"m\"\n");
    std::fs::write(dir.path().join("config.toml"), config).unwrap();
    let _gateway = start(dir.path(), None);
    let mut client = connect(dir.path()).await;
    let reply = client.call("complete", json!({ "messages": [{ "role": "user", "content": "hi" }] })).await.unwrap();
    assert_eq!((reply["text"].clone(), reply["stop_reason"].clone()), (json!("OK"), json!("stop")));
}

#[tokio::test]
async fn projects_differing_only_in_case_are_told_apart_by_id() {
    let dir = tempfile::tempdir().unwrap();
    let (mut store, _) = crate::config::store::ConnectionStore::load_from(dir.path().join("connections.toml"));
    let _ = store.add_project("Shop");
    let second = store.add_project("shop");
    // (If the store refuses names differing in case, there's nothing to tell apart.)
    if let Ok(second) = second {
        let found = super::ai::find_project(dir.path(), Some(&format!("id:{second}"))).unwrap();
        assert_eq!(found.id, second);
    }
    let first = store.project_by_name("Shop").unwrap().id.clone();
    assert_eq!(super::ai::find_project(dir.path(), Some(&format!("id:{first}"))).unwrap().id, first);
}

#[tokio::test]
async fn a_relative_sqlite_file_is_the_askers() {
    let dir = tempfile::tempdir().unwrap();
    saved_shop(dir.path()).await;
    // Saved as `shop.db`, from the folder it's in.
    let (mut store, _) = crate::config::store::ConnectionStore::load_from(dir.path().join("connections.toml"));
    let id = store.project_by_name("p").unwrap().id.clone();
    let mut shop = store.project_by_name("p").unwrap().connections[0].clone();
    shop.host = "shop.db".into();
    store.upsert(&id, Some("shop"), shop).unwrap();
    let _gateway = start(dir.path(), None);
    let mut client = connect(dir.path()).await;
    let here = dir.path().display().to_string();
    let built = client
        .call("context", json!({ "project": "p", "connection": "shop", "question": "customers", "dir": here }))
        .await;
    assert!(built.unwrap()["text"].as_str().unwrap().contains("customers"));
}

#[tokio::test]
async fn a_client_done_sending_still_gets_its_answer() {
    let (url, _server) = fake_model(vec![(200, claude_answer("OK"))]).await;
    let dir = tempfile::tempdir().unwrap();
    let config =
        format!("[[models]]\nname = \"near\"\napi = \"anthropic\"\nurl = \"{url}\"\nmodel = \"claude-opus-5-5\"\n");
    std::fs::write(dir.path().join("config.toml"), config).unwrap();
    let _gateway = start(dir.path(), None);
    drop(connect(dir.path()).await);
    // As `printf … | socat`: both requests, then its sending side closed.
    let stream = UnixStream::connect(super::socket_path(dir.path())).await.unwrap();
    let (read, mut write) = stream.into_split();
    let hello = json!({ "jsonrpc": "2.0", "id": 1, "method": "hello", "params": { "client": "socat", "protocol": super::PROTOCOL } });
    let ask = json!({ "jsonrpc": "2.0", "id": 2, "method": "complete", "params": { "messages": [{ "role": "user", "content": "hi" }] } });
    write.write_all(format!("{hello}\n{ask}\n").as_bytes()).await.unwrap();
    write.shutdown().await.unwrap();
    let mut lines = BufReader::new(read).lines();
    let answer = tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(line) = lines.next_line().await.unwrap() {
            let message: Value = serde_json::from_str(&line).unwrap();
            if message["id"] == json!(2) {
                return message;
            }
        }
        panic!("the connection ended without an answer");
    })
    .await
    .unwrap();
    assert_eq!(answer["result"]["text"], json!("OK"), "{answer}");
}

#[tokio::test]
async fn a_config_that_doesnt_parse_says_why() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("config.toml"), "[[models]]\nname = \"x\"\napi = \"ollama\"\nmodel = \"y\"\n")
        .unwrap();
    let _gateway = start(dir.path(), None);
    let mut client = connect(dir.path()).await;
    let err = client.call("models", json!({})).await.unwrap_err();
    assert!(
        matches!(&err, CallError::Rpc(e) if e.message.contains("config.toml") && !e.message.contains("no models are set up")),
        "{err}"
    );
}

#[test]
fn a_piped_key_is_its_first_line_that_isnt_blank() {
    use super::command::key_from;
    assert_eq!(key_from("sk-ant-123\n".as_bytes()).as_deref(), Some("sk-ant-123"));
    assert_eq!(key_from("\n  sk-ant-123  \r\nignored\n".as_bytes()).as_deref(), Some("sk-ant-123"));
    assert_eq!(key_from("".as_bytes()), None);
    assert_eq!(key_from(" \n\n".as_bytes()), None);
}
