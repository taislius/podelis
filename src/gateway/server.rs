//! The gateway process: one per config folder, on a private socket,
//! until it's asked to stop or no client has been connected for the
//! idle time.
//!
//! One at a time: the lock file is held for the process's life (the OS
//! lets go of it when the process ends, however it ends), so a second
//! one started meanwhile sees it and leaves. A socket file left by a
//! gateway that crashed is replaced by the next one.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::watch;

use super::protocol::{self, RpcError, code};

/// The longest socket path the OS takes (`sun_path` is 104 bytes on
/// macOS, 108 on Linux; one for the final NUL).
const MAX_SOCKET_PATH: usize = 103;

/// The call log is cut to its newer half past this.
const MAX_LOG_BYTES: u64 = 1 << 20;

#[derive(Debug, Clone)]
pub struct Options {
    /// Stop after this long with no client connected; `None` never.
    pub idle: Option<Duration>,
    /// How often the idle time is checked.
    pub tick: Duration,
}

impl Options {
    pub fn new(idle: Option<Duration>) -> Self {
        // Often enough to stop within a few percent of the idle time.
        let tick = idle
            .map_or(Duration::from_secs(60), |d| (d / 20).clamp(Duration::from_millis(10), Duration::from_secs(15)));
        Options { idle, tick }
    }
}

/// How `serve` ended.
#[derive(Debug, PartialEq, Eq)]
pub enum Ended {
    /// Another gateway has this config folder.
    AlreadyRunning,
    /// It stopped, saying why: `idle`, `asked` (a client's `shutdown`) or
    /// `interrupted`.
    Stopped(&'static str),
}

/// What the connections share.
struct Shared {
    started: Instant,
    clients: AtomicUsize,
    /// When a client last came or went.
    last_change: Mutex<Instant>,
    idle: Option<Duration>,
    log: PathBuf,
    stop: watch::Sender<Option<&'static str>>,
}

impl Shared {
    fn touch(&self) {
        *self.last_change.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Instant::now();
    }

    fn idle_for(&self) -> Duration {
        self.last_change.lock().unwrap_or_else(std::sync::PoisonError::into_inner).elapsed()
    }
}

/// Runs the gateway for `config_dir` until it stops. `interrupt` ends it
/// from outside (Ctrl+C, a signal).
pub async fn serve(
    config_dir: &Path,
    options: Options,
    interrupt: impl std::future::Future<Output = ()>,
) -> std::io::Result<Ended> {
    let dir = super::dir(config_dir);
    std::fs::create_dir_all(&dir)?;
    restrict(&dir, 0o700);
    let lock =
        std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(super::lock_path(config_dir))?;
    match lock.try_lock() {
        Ok(()) => {}
        Err(std::fs::TryLockError::WouldBlock) => return Ok(Ended::AlreadyRunning),
        Err(std::fs::TryLockError::Error(e)) => return Err(e),
    }
    let socket = super::socket_path(config_dir);
    let listener = bind(&socket)?;
    let (stop, mut stopped) = watch::channel(None);
    let shared = Arc::new(Shared {
        started: Instant::now(),
        clients: AtomicUsize::new(0),
        last_change: Mutex::new(Instant::now()),
        idle: options.idle,
        log: super::log_path(config_dir),
        stop,
    });
    tracing::info!(socket = %socket.display(), idle = ?options.idle, "gateway started");
    tokio::pin!(interrupt);
    let mut tick = tokio::time::interval(options.tick);
    let reason = loop {
        tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) => {
                    tokio::spawn(connection(stream, Arc::clone(&shared)));
                }
                Err(e) => tracing::warn!(error = %e, "gateway: accept failed"),
            },
            _ = stopped.changed() => break (*stopped.borrow()).unwrap_or("asked"),
            () = &mut interrupt => break "interrupted",
            _ = tick.tick() => {
                let idle = shared.idle.is_some_and(|idle| {
                    shared.clients.load(Ordering::SeqCst) == 0 && shared.idle_for() >= idle
                });
                if idle {
                    break "idle";
                }
            }
        }
    };
    // Clients are told, then given a moment to read it.
    let _ = shared.stop.send(Some(reason));
    let deadline = Instant::now() + Duration::from_secs(1);
    while shared.clients.load(Ordering::SeqCst) > 0 && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let _ = std::fs::remove_file(&socket);
    tracing::info!(reason, "gateway stopped");
    drop(lock);
    Ok(Ended::Stopped(reason))
}

/// Listens on `socket`, replacing a file a crashed gateway left (the
/// caller holds the lock, so no live gateway owns it).
fn bind(socket: &Path) -> std::io::Result<UnixListener> {
    if socket.as_os_str().len() > MAX_SOCKET_PATH {
        return Err(std::io::Error::other(format!(
            "the gateway's socket path is too long for the OS ({} bytes, at most {MAX_SOCKET_PATH}): {}; \
             set PODELIS_CONFIG_DIR to a shorter folder",
            socket.as_os_str().len(),
            socket.display()
        )));
    }
    match std::fs::remove_file(socket) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e),
        _ => {}
    }
    let listener = UnixListener::bind(socket)?;
    // The folder is the user's only already; this is belt and braces.
    restrict(socket, 0o600);
    Ok(listener)
}

fn restrict(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode));
}

/// One client, until it goes or the gateway stops.
async fn connection(stream: UnixStream, shared: Arc<Shared>) {
    shared.clients.fetch_add(1, Ordering::SeqCst);
    shared.touch();
    let (read, mut write) = stream.into_split();
    let mut reader = BufReader::new(read);
    let mut stopping = shared.stop.subscribe();
    let mut client: Option<String> = None;
    let mut buf = Vec::new();
    loop {
        tokio::select! {
            line = read_line(&mut reader, &mut buf) => {
                let reply = match line {
                    Ok(true) => handle(&buf, &mut client, &shared),
                    Ok(false) | Err(LineError::Io(_)) => break,
                    Err(LineError::TooLong) => Reply::close(protocol::error(
                        &Value::Null,
                        &RpcError::new(code::INVALID_REQUEST, format!("a message is one line of at most {} bytes", protocol::MAX_LINE)),
                    )),
                };
                if let Some(text) = &reply.text
                    && write.write_all(text.as_bytes()).await.is_err()
                {
                    break;
                }
                if reply.close {
                    break;
                }
            }
            _ = stopping.changed() => {
                let reason = (*stopping.borrow()).unwrap_or("asked");
                let _ = write.write_all(protocol::notification("stopping", &json!({ "reason": reason })).as_bytes()).await;
                break;
            }
        }
    }
    let _ = write.shutdown().await;
    shared.clients.fetch_sub(1, Ordering::SeqCst);
    shared.touch();
}

pub(super) enum LineError {
    TooLong,
    Io(std::io::Error),
}

/// Reads one line into `buf` (without the `\n`). False at the end of the
/// stream.
pub(super) async fn read_line<R: AsyncBufRead + Unpin>(reader: &mut R, buf: &mut Vec<u8>) -> Result<bool, LineError> {
    buf.clear();
    let limit = protocol::MAX_LINE as u64 + 1;
    let read = (&mut *reader).take(limit).read_until(b'\n', buf).await.map_err(LineError::Io)?;
    if read == 0 {
        return Ok(false);
    }
    if buf.last() == Some(&b'\n') {
        buf.pop();
    } else if buf.len() as u64 >= limit {
        return Err(LineError::TooLong);
    }
    Ok(true)
}

/// What to send back, and whether to hang up after it.
struct Reply {
    text: Option<String>,
    close: bool,
}

impl Reply {
    fn send(text: String) -> Self {
        Reply { text: Some(text), close: false }
    }

    fn close(text: String) -> Self {
        Reply { text: Some(text), close: true }
    }
}

fn handle(line: &[u8], client: &mut Option<String>, shared: &Shared) -> Reply {
    if line.iter().all(u8::is_ascii_whitespace) {
        return Reply { text: None, close: false };
    }
    let incoming = match protocol::parse_incoming(line) {
        Ok(incoming) => incoming,
        Err(e) => return Reply::send(protocol::error(&Value::Null, &e)),
    };
    let started = Instant::now();
    let first = client.is_none();
    let outcome = if first && incoming.method != "hello" {
        Err(RpcError::new(code::HELLO_FIRST, "say hello first: the first request must be `hello`"))
    } else {
        call(&incoming.method, &incoming.params, client, shared)
    };
    log_call(shared, client.as_deref(), &incoming.method, &outcome, started.elapsed());
    // A client that didn't get through hello is hung up on.
    let close = client.is_none();
    let Some(id) = incoming.id else {
        return Reply { text: None, close };
    };
    let text = match &outcome {
        Ok(value) => protocol::result(&id, value),
        Err(e) => protocol::error(&id, e),
    };
    Reply { text: Some(text), close }
}

fn call(method: &str, params: &Value, client: &mut Option<String>, shared: &Shared) -> Result<Value, RpcError> {
    match method {
        "hello" => hello(params, client),
        "status" => Ok(json!({
            "version": env!("CARGO_PKG_VERSION"),
            "protocol": super::PROTOCOL,
            "pid": std::process::id(),
            "uptime_secs": shared.started.elapsed().as_secs(),
            "clients": shared.clients.load(Ordering::SeqCst),
            "idle_stop_secs": shared.idle.map(|d| d.as_secs()),
        })),
        "shutdown" => {
            let _ = shared.stop.send(Some("asked"));
            Ok(json!({ "stopping": true }))
        }
        _ => Err(RpcError::new(code::METHOD_NOT_FOUND, format!("no method `{method}`"))),
    }
}

fn hello(params: &Value, client: &mut Option<String>) -> Result<Value, RpcError> {
    if client.is_some() {
        return Err(RpcError::new(code::INVALID_REQUEST, "already said hello"));
    }
    let name = params.get("client").and_then(Value::as_str).unwrap_or_default();
    if name.is_empty() || name.len() > 64 {
        return Err(RpcError::new(code::INVALID_PARAMS, "hello needs `client`: who's asking, 1 to 64 bytes"));
    }
    let Some(version) = params.get("protocol").and_then(Value::as_u64) else {
        return Err(RpcError::new(code::INVALID_PARAMS, "hello needs `protocol`: the version the client speaks"));
    };
    if version != u64::from(super::PROTOCOL) {
        return Err(RpcError::new(
            code::VERSION,
            format!(
                "this gateway speaks protocol {}, the client {version}: it's from another Podėlis version \
                 (`podelis gateway stop`, and the next use starts the right one)",
                super::PROTOCOL
            ),
        ));
    }
    *client = Some(name.to_string());
    Ok(json!({ "gateway": "podelis", "version": env!("CARGO_PKG_VERSION"), "protocol": super::PROTOCOL }))
}

/// A line in the call log: when, who, what, the outcome and how long it
/// took. Never the parameters: they will carry questions, SQL and keys.
fn log_call(shared: &Shared, client: Option<&str>, method: &str, outcome: &Result<Value, RpcError>, took: Duration) {
    let entry = json!({
        "at": chrono::Local::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, false),
        "client": client,
        "method": method,
        "error": outcome.as_ref().err().map(|e| e.code),
        "ms": took.as_millis(),
    });
    if let Err(e) = append(&shared.log, &entry) {
        tracing::warn!(error = %e, "gateway: couldn't write the call log");
    }
}

fn append(path: &Path, entry: &Value) -> std::io::Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.create(true).append(true);
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options.open(path)?;
    writeln!(file, "{entry}")?;
    if file.metadata()?.len() > MAX_LOG_BYTES {
        // Keep the newer half, from a line's start.
        let text = std::fs::read(path)?;
        let half = text.len() / 2;
        let start = text[half..].iter().position(|&b| b == b'\n').map_or(text.len(), |i| half + i + 1);
        let tmp = path.with_extension("jsonl.tmp");
        std::fs::write(&tmp, &text[start..])?;
        restrict(&tmp, 0o600);
        std::fs::rename(tmp, path)?;
    }
    Ok(())
}
