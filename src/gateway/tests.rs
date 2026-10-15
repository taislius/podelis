use std::path::Path;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

use super::client::{CallError, Client, connect_or_start};
use super::protocol::code;
use super::server::{Ended, Options, serve};

/// A gateway for `dir` in this process, stopping after `idle`.
fn start(dir: &Path, idle: Option<Duration>) -> tokio::task::JoinHandle<std::io::Result<Ended>> {
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
}
