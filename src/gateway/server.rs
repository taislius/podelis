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
use tokio::net::unix::OwnedWriteHalf;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::watch;

use super::call::{Call, Notifier};
use super::methods::{self as m, Method, Note, Stopping};
use super::protocol::{self, RpcError, code};

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
    /// It stopped, saying why.
    Stopped(StopReason),
}

/// Why the gateway stops, as its `stopping` notification and log say it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopReason {
    /// No client for the idle time.
    Idle,
    /// A client's `shutdown`.
    Asked,
    /// Ctrl+C or a signal.
    Interrupted,
}

impl StopReason {
    pub fn as_str(self) -> &'static str {
        match self {
            StopReason::Idle => "idle",
            StopReason::Asked => "asked",
            StopReason::Interrupted => "interrupted",
        }
    }
}

impl std::fmt::Display for StopReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What the connections share.
struct Shared {
    started: Instant,
    clients: AtomicUsize,
    /// When a client last came or went.
    last_change: Mutex<Instant>,
    idle: Option<Duration>,
    log: PathBuf,
    /// Where `config.toml` is (models are read from it on each call).
    config_dir: PathBuf,
    /// What's kept between calls: schemas, memories, keys.
    caches: super::caches::Caches,
    stop: watch::Sender<Option<StopReason>>,
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
    let socket = super::usable_socket_path(config_dir)?;
    let listener = bind(&socket)?;
    let (stop, mut stopped) = watch::channel(None);
    let shared = Arc::new(Shared {
        started: Instant::now(),
        clients: AtomicUsize::new(0),
        last_change: Mutex::new(Instant::now()),
        idle: options.idle,
        log: super::log_path(config_dir),
        config_dir: config_dir.to_path_buf(),
        caches: super::caches::Caches::default(),
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
            _ = stopped.changed() => break (*stopped.borrow()).unwrap_or(StopReason::Asked),
            () = &mut interrupt => break StopReason::Interrupted,
            _ = tick.tick() => {
                let idle = shared.idle.is_some_and(|idle| {
                    shared.clients.load(Ordering::SeqCst) == 0 && shared.idle_for() >= idle
                });
                if idle {
                    break StopReason::Idle;
                }
                shared.caches.sweep();
            }
        }
    };
    // Clients are told, then given a moment to read it.
    let _ = shared.stop.send(Some(reason));
    let_clients_read(&shared).await;
    let _ = std::fs::remove_file(&socket);
    tracing::info!(reason = reason.as_str(), "gateway stopped");
    drop(lock);
    Ok(Ended::Stopped(reason))
}

/// Waits, at most a second, for connected clients to read that the gateway
/// stops and go, before the process ends under them.
async fn let_clients_read(shared: &Shared) {
    let deadline = Instant::now() + Duration::from_secs(1);
    while shared.clients.load(Ordering::SeqCst) > 0 && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// Listens on `socket`, replacing a file a crashed gateway left (the
/// caller holds the lock, so no live gateway owns it).
fn bind(socket: &Path) -> std::io::Result<UnixListener> {
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
    // After `mcp.open`, the connection carries MCP for an agent.
    let mut agent: Option<Agent> = None;
    loop {
        let line = tokio::select! {
            line = read_line(&mut reader, &mut buf) => line,
            _ = stopping.changed() => {
                let reason = (*stopping.borrow()).unwrap_or(StopReason::Asked);
                let note = Note::Stopping(Stopping { reason: reason.as_str().to_string() });
                Notifier::new(&mut write).send(&note).await;
                break;
            }
        };
        let reply = match line {
            // On the heap: a request's work (a model's conversation) is a
            // large future, kept off the connection task's stack.
            Ok(true) => {
                match Box::pin(watched(handle(&buf, &mut client, &shared, &mut write), &mut reader, &mut stopping))
                    .await
                {
                    Some(reply) => reply,
                    // Gone, or the gateway is stopping: the work (a model
                    // call) was dropped, so it stops costing.
                    None => break,
                }
            }
            Ok(false) | Err(LineError::Io(_)) => break,
            Err(LineError::TooLong) => Reply::close(protocol::error(
                &Value::Null,
                &RpcError::new(
                    code::INVALID_REQUEST,
                    format!("a message is one line of at most {} bytes", protocol::MAX_LINE),
                ),
            )),
        };
        if let Some(text) = &reply.text
            && write.write_all(text.as_bytes()).await.is_err()
        {
            break;
        }
        if reply.agent.is_some() {
            agent = reply.agent;
            break;
        }
        if reply.close {
            break;
        }
    }
    if let Some(agent) = agent {
        // The agent's session, until it goes or the gateway stops.
        let mut stopping = shared.stop.subscribe();
        let session =
            crate::agents::mcp::serve_on(&shared.config_dir, &agent.project, agent.name, agent.dir, reader, write);
        tokio::select! {
            ended = session => {
                if let Err(e) = ended {
                    tracing::warn!(error = %e, "gateway: an agent's MCP session failed");
                }
            }
            _ = stopping.changed() => {}
        }
    } else {
        let _ = write.shutdown().await;
    }
    shared.clients.fetch_sub(1, Ordering::SeqCst);
    shared.touch();
}

/// Runs a request's `work`, unless its client goes or the gateway stops
/// first: then it's dropped (`None`), and a model call stops costing.
///
/// A client that only closed its sending side (`printf … | socat`) still
/// gets its answer: the end of what it sends isn't taken for it having
/// gone. Once that end comes, the connection is checked a few times a
/// second for being closed for writing too, which is a client gone.
/// Clients wait for each response, so anything else that comes meanwhile
/// is left for the next read.
async fn watched(
    work: impl std::future::Future<Output = Reply>,
    reader: &mut BufReader<tokio::net::unix::OwnedReadHalf>,
    stopping: &mut watch::Receiver<Option<StopReason>>,
) -> Option<Reply> {
    #[derive(PartialEq)]
    enum Client {
        Reading,
        /// It sent more (the next request): nothing to watch.
        Busy,
        /// It closed its sending side: gone, or only done sending.
        DoneSending,
    }
    tokio::pin!(work);
    let mut client = Client::Reading;
    let mut probe = tokio::time::interval(Duration::from_millis(250));
    loop {
        tokio::select! {
            reply = &mut work => return Some(reply),
            seen = reader.fill_buf(), if client == Client::Reading => match seen {
                Ok(bytes) if !bytes.is_empty() => client = Client::Busy,
                Ok(_) => client = Client::DoneSending,
                Err(_) => return None,
            },
            _ = probe.tick(), if client == Client::DoneSending => {
                let ready = reader.get_ref().ready(tokio::io::Interest::WRITABLE).await;
                if ready.is_err() || ready.is_ok_and(tokio::io::Ready::is_write_closed) {
                    return None;
                }
            }
            _ = stopping.changed() => return None,
        }
    }
}

/// An agent whose `podelis mcp` asked for an MCP session.
struct Agent {
    project: crate::config::store::Project,
    name: Option<String>,
    dir: Option<String>,
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

/// What to send back, and whether to hang up after it (or hand the
/// connection to an agent's MCP session).
struct Reply {
    text: Option<String>,
    close: bool,
    agent: Option<Agent>,
}

impl Reply {
    fn send(text: String) -> Self {
        Reply { text: Some(text), close: false, agent: None }
    }

    fn close(text: String) -> Self {
        Reply { text: Some(text), close: true, agent: None }
    }
}

async fn handle(line: &[u8], client: &mut Option<String>, shared: &Shared, write: &mut OwnedWriteHalf) -> Reply {
    if line.iter().all(u8::is_ascii_whitespace) {
        return Reply { text: None, close: false, agent: None };
    }
    let incoming = match protocol::parse_incoming(line) {
        Ok(incoming) => incoming,
        Err(e) => return Reply::send(protocol::error(&Value::Null, &e)),
    };
    let started = Instant::now();
    let id = incoming.id.clone().unwrap_or(Value::Null);
    let mut call = Call::new(&shared.config_dir, &shared.caches, id, Notifier::new(write));
    let mut agent = None;
    let outcome = if client.is_none() && incoming.method != m::Hello::NAME {
        Err(RpcError::new(code::HELLO_FIRST, "say hello first: the first request must be `hello`"))
    } else {
        dispatch(&incoming, &mut call, client, shared, &mut agent).await
    };
    let about = call.into_log();
    log_call(shared, client.as_deref(), &incoming.method, &outcome, started.elapsed(), about);
    // A client that didn't get through hello is hung up on.
    let close = client.is_none();
    let Some(id) = incoming.id else {
        return Reply { text: None, close, agent: None };
    };
    let text = match &outcome {
        Ok(value) => protocol::result(&id, value),
        Err(e) => protocol::error(&id, e),
    };
    Reply { text: Some(text), close, agent }
}

/// `M`'s params, as the request has them (none: its defaults).
fn params<M: Method>(params: &Value) -> Result<M::Params, RpcError> {
    let params = if params.is_null() { json!({}) } else { params.clone() };
    serde_json::from_value(params).map_err(|e| RpcError::new(code::INVALID_PARAMS, format!("{}: {e}", M::NAME)))
}

/// `M`'s answer, as it goes.
fn answer<M: Method>(result: &M::Result) -> Result<Value, RpcError> {
    serde_json::to_value(result).map_err(|e| RpcError::new(code::INTERNAL, format!("{}: {e}", M::NAME)))
}

/// Runs the request's method.
async fn dispatch(
    incoming: &protocol::Incoming,
    call: &mut Call<'_>,
    client: &mut Option<String>,
    shared: &Shared,
    agent: &mut Option<Agent>,
) -> Result<Value, RpcError> {
    // A method's params read, its handler run on them, its answer written.
    macro_rules! run {
        ($method:ty, $handler:path) => {{
            let p = params::<$method>(&incoming.params)?;
            answer::<$method>(&$handler(call, p).await?)
        }};
    }
    match incoming.method.as_str() {
        m::Hello::NAME => answer::<m::Hello>(&hello(params::<m::Hello>(&incoming.params)?, client)?),
        m::Status::NAME => answer::<m::Status>(&status(shared)),
        m::Shutdown::NAME => {
            let _ = shared.stop.send(Some(StopReason::Asked));
            answer::<m::Shutdown>(&m::ShutdownResult { stopping: true })
        }
        m::Models::NAME => run!(m::Models, super::ai::models),
        m::Complete::NAME => run!(m::Complete, super::ai::complete),
        m::Context::NAME => run!(m::Context, super::ask::context),
        m::Ask::NAME => run!(m::Ask, super::ask::ask),
        m::Ontology::NAME => run!(m::Ontology, super::ontology::ontology),
        m::OntologyImport::NAME => run!(m::OntologyImport, super::ontology::import),
        m::MemoryAdd::NAME => run!(m::MemoryAdd, super::memory::add),
        m::MemoryUpdate::NAME => run!(m::MemoryUpdate, super::memory::update),
        m::MemoryList::NAME => run!(m::MemoryList, super::memory::list),
        m::MemoryForget::NAME => run!(m::MemoryForget, super::memory::forget),
        m::MemoryConfirm::NAME => run!(m::MemoryConfirm, super::memory::confirm),
        m::MemoryMove::NAME => run!(m::MemoryMove, super::memory::move_fact),
        m::MemoryLearn::NAME => run!(m::MemoryLearn, super::memory::learn),
        m::MemoryExport::NAME => run!(m::MemoryExport, super::memory::export),
        m::McpOpen::NAME => {
            let p = params::<m::McpOpen>(&incoming.params)?;
            let project = super::source::find_project(call.config_dir, m::given(p.project.as_deref()))
                .map_err(|e| RpcError::new(code::DATABASE, e))?;
            call.log("project", &project.name);
            *agent = Some(Agent { project, name: p.name, dir: p.dir });
            answer::<m::McpOpen>(&m::McpOpenResult { mcp: true })
        }
        method => Err(RpcError::new(code::METHOD_NOT_FOUND, format!("no method `{method}`"))),
    }
}

fn status(shared: &Shared) -> m::StatusResult {
    m::StatusResult {
        version: env!("CARGO_PKG_VERSION").to_string(),
        protocol: super::PROTOCOL,
        pid: std::process::id(),
        uptime_secs: shared.started.elapsed().as_secs(),
        clients: shared.clients.load(Ordering::SeqCst),
        idle_stop_secs: shared.idle.map(|d| d.as_secs()),
    }
}

fn hello(p: m::HelloParams, client: &mut Option<String>) -> Result<m::HelloResult, RpcError> {
    if client.is_some() {
        return Err(RpcError::new(code::INVALID_REQUEST, "already said hello"));
    }
    if p.client.is_empty() || p.client.len() > 64 {
        return Err(RpcError::new(code::INVALID_PARAMS, "hello needs `client`: who's asking, 1 to 64 bytes"));
    }
    let Some(version) = p.protocol else {
        return Err(RpcError::new(code::INVALID_PARAMS, "hello needs `protocol`: the version the client speaks"));
    };
    if version != u64::from(super::PROTOCOL) {
        // Which version, so the client can stop this gateway in it.
        return Err(RpcError::with_data(
            code::VERSION,
            format!(
                "this gateway speaks protocol {}, the client {version}: it's from another Podėlis version \
                 (`podelis gateway stop`, and the next use starts the right one)",
                super::PROTOCOL
            ),
            json!({ "protocol": super::PROTOCOL, "pid": std::process::id() }),
        ));
    }
    *client = Some(p.client);
    Ok(m::HelloResult {
        gateway: "podelis".into(),
        version: env!("CARGO_PKG_VERSION").into(),
        protocol: super::PROTOCOL,
    })
}

/// A line in the call log: when, who, what, the outcome and how long it
/// took, and `about` it (a model call: the model, its tokens). Never the
/// parameters or the answer: they carry questions, SQL and data.
fn log_call(
    shared: &Shared,
    client: Option<&str>,
    method: &str,
    outcome: &Result<Value, RpcError>,
    took: Duration,
    about: serde_json::Map<String, Value>,
) {
    let mut entry = json!({
        "at": chrono::Local::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, false),
        "client": client,
        "method": method,
        "error": outcome.as_ref().err().map(|e| e.code),
        "ms": took.as_millis(),
    });
    if let Some(entry) = entry.as_object_mut() {
        entry.extend(about);
    }
    if let Err(e) = append(&shared.log, &entry) {
        tracing::warn!(error = %e, "gateway: couldn't write the call log");
    }
}

/// Appends `entry` as one line. Calls end at the same moment: the line
/// goes in one write, under the file's lock, and the file is trimmed in
/// place under it too (a rename would lose another call's line).
fn append(path: &Path, entry: &Value) -> std::io::Result<()> {
    use std::io::{Read as _, Seek as _};
    let line = format!("{entry}\n");
    let mut options = std::fs::OpenOptions::new();
    options.create(true).read(true).append(true);
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options.open(path)?;
    file.lock()?;
    file.write_all(line.as_bytes())?;
    if file.metadata()?.len() > MAX_LOG_BYTES {
        // Keep the newer half, from a line's start.
        let mut text = Vec::new();
        file.rewind()?;
        file.read_to_end(&mut text)?;
        let half = text.len() / 2;
        let start = text[half..].iter().position(|&b| b == b'\n').map_or(text.len(), |i| half + i + 1);
        file.set_len(0)?;
        file.write_all(&text[start..])?;
    }
    Ok(())
}
