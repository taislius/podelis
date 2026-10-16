use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use futures_util::TryStreamExt;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use sqlx::pool::PoolConnection;
use sqlx::{AssertSqlSafe, Column, Either, Executor, Row, SqlSafeStr, Statement, TypeInfo, ValueRef};
use tokio::io::AsyncReadExt;
use tokio::process::{Child, Command};
use tokio::sync::{MappedMutexGuard, Mutex, MutexGuard, mpsc, oneshot};
use uuid::Uuid;

use crate::sql;

// The engines (`Engine`, `Engine::ALL`, `Pool`, `Session`, their modules
// and `on_engine!` / `on_pool!` / `on_session!`), found by build.rs.
include!(concat!(env!("OUT_DIR"), "/engines.rs"));

mod activity;
pub mod agent_constraints;
mod catalog;
mod details;
mod diagnose;
mod dialect;
mod errors;
mod info;
mod options;
pub mod password_command;
pub mod passwords;
mod run;
#[cfg(test)]
pub(crate) mod tests;
mod tunnel;
mod types;

pub use activity::Activity;
pub use details::{Comment, ForeignKey, IndexInfo, Link, RoutineInfo, TableDetails};
pub use diagnose::{Step, StepState, diagnose, is_auth_failure, report, with_hint};
#[cfg(test)]
pub use dialect::PlanNote;
pub use dialect::{Dialect, LexerRules, Plan, human_rows};
pub use errors::{
    CheckOutcome, ErrorPos, error_message, error_position, is_broken, is_reconnected, is_unreachable, sql_literal,
    value_literal,
};
pub use options::{AgentAccess, ConnectParams, ConnectionMode, PasswordSource, SslMode, TxMode};
pub use run::RunError;
use tunnel::{Tunnel, open_tunnel};
pub use types::{TypeKind, same_type, translate_type};

/// Default for the rows kept in memory per result set. Rows beyond the limit
/// are still read from the server (so multi-statement scripts run to
/// completion) but only counted.
pub const DEFAULT_ROW_LIMIT: usize = 1000;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone)]
pub struct Database(Arc<Inner>);

struct Inner {
    engine: Engine,
    pool: Pool,
    /// The connection user queries run on. Holding one connection means
    /// session state (`SET`, `BEGIN`, temp tables) survives between runs.
    session: Arc<Mutex<Session>>,
    /// Server-side id of the session connection, used to cancel its query.
    backend_id: AtomicI64,
    /// SQLite has no server to ask: cancelling sets this, and the
    /// connection's progress handler stops the query when it sees it.
    interrupt: Arc<AtomicBool>,
    /// Keeps the SSH tunnel process alive; killed when dropped.
    _tunnel: Option<Tunnel>,
    /// The server's version string, e.g. "16.2".
    version: String,
    /// The session's `sql_mode` (MySQL), refreshed with the catalog.
    sql_mode: std::sync::Mutex<String>,
    /// Why TLS failed when SSL mode `prefer` fell back to no encryption.
    tls_failed: Option<String>,
    /// The session's settings were changed (`USE`, `SET`…): another
    /// connection wouldn't see the same, so nothing runs aside.
    session_changed: AtomicBool,
    /// The session was given up on mid-call (no answer: a dropped VPN, a
    /// dead link): its connection is in an unknown state, so the next run
    /// opens a new one first.
    stale: AtomicBool,
    /// The task reading the last streamed result, which holds the session
    /// while it runs: stopped when the session is given up on.
    stream_task: std::sync::Mutex<Option<tokio::task::AbortHandle>>,
    /// The server's TCP address, for its engine to check it answers before
    /// a new connection is made for a session or a cancel
    /// (`connect::check_reach`). None for a file, a socket path, or an SSH
    /// tunnel (its end is this machine).
    address: Option<(String, u16)>,
}

/// Transaction state of the session connection.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TxnState {
    #[default]
    Idle,
    Open,
    /// An error happened inside the transaction; only ROLLBACK works now.
    Aborted,
}

#[derive(Debug, Default)]
pub struct QueryOutcome {
    pub columns: Vec<String>,
    /// The server's type name for each column (may be empty if unknown).
    pub column_types: Vec<String>,
    /// `None` is SQL `NULL`.
    pub rows: Vec<Vec<Option<String>>>,
    /// Rows the statement returned, including any beyond the row limit.
    pub total_rows: u64,
    pub rows_affected: Option<u64>,
    pub statements: usize,
    pub elapsed: Duration,
    /// The connection had dropped and was reopened before this result.
    pub reconnected: bool,
    /// Earlier statements of the script that also returned rows, in order
    /// (this result is the last statement that did).
    pub earlier: Vec<QueryOutcome>,
    /// Which statement of the script produced this result (1-based; 0 when
    /// unknown).
    pub statement: usize,
}

impl QueryOutcome {
    pub fn truncated(&self) -> bool {
        self.total_rows > self.rows.len() as u64
    }
}

/// One statement's result while a script is being read.
#[derive(Default)]
struct StatementResult {
    /// (name, type name) per column.
    columns: Option<Vec<(String, String)>>,
    rows: Vec<Vec<Option<String>>>,
    total_rows: u64,
    rows_affected: u64,
}

/// More rows of a streamed result.
pub struct Page {
    pub rows: Vec<Vec<Option<String>>>,
    /// No rows remain.
    pub done: bool,
}

type PageRequest = (usize, oneshot::Sender<Result<Page, String>>);

/// Fetches further pages of a [`Database::run_streaming`] result. Cheap to
/// clone; the query is cancelled once every clone is dropped.
#[derive(Clone)]
pub struct RowStream {
    tx: mpsc::Sender<PageRequest>,
}

impl std::fmt::Debug for RowStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RowStream")
    }
}

impl RowStream {
    pub async fn next_page(&self, rows: usize) -> Result<Page, String> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx.send((rows, reply_tx)).await.map_err(|_| "the query was closed".to_string())?;
        reply_rx.await.map_err(|_| "the query was closed".to_string())?
    }
}

type FirstPage = (Option<Vec<(String, String)>>, Vec<Vec<Option<String>>>, bool);

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TableInfo {
    pub schema: String,
    pub name: String,
    pub is_view: bool,
    /// The server's row-count estimate, if it keeps one.
    pub rows: Option<i64>,
    /// Its size on disk with its indexes, in bytes: an estimate (Postgres
    /// as of the last VACUUM/ANALYZE), and none for SQLite.
    pub bytes: Option<i64>,
    /// Part of the database's own catalog (`pg_catalog`, `sqlite_schema`…).
    pub system: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ColumnInfo {
    pub name: String,
    pub data_type: String,
    pub nullable: bool,
    pub primary_key: bool,
}

/// What went wrong with TLS, if that's why connecting failed: the TLS
/// library's error, or the handshake on the socket (rustls reports a
/// rejected protocol version or cipher as an I/O error).
fn tls_failure(e: &anyhow::Error) -> Option<String> {
    let looks_tls = |text: &str| {
        let text = text.to_ascii_lowercase();
        ["handshake", "alert", "tls", "ssl", "protocol version"].iter().any(|w| text.contains(w))
    };
    e.chain().find_map(|cause| match cause.downcast_ref::<sqlx::Error>()? {
        sqlx::Error::Tls(tls) => Some(tls.to_string()),
        sqlx::Error::Io(io) if looks_tls(&io.to_string()) => Some(io.to_string()),
        _ => None,
    })
}

/// Why a connection to a server failed, in a user's words.
fn io_reason(e: &std::io::Error) -> String {
    match e.kind() {
        std::io::ErrorKind::ConnectionRefused => "connection refused".into(),
        std::io::ErrorKind::HostUnreachable | std::io::ErrorKind::NetworkUnreachable => "no route to the host".into(),
        _ => e.to_string(),
    }
}

/// `host:port` as written in messages (an IPv6 address in brackets).
fn address_text(host: &str, port: u16) -> String {
    if host.contains(':') { format!("[{host}]:{port}") } else { format!("{host}:{port}") }
}

/// How long a server's address has to answer `reach`.
const REACH_TIMEOUT: Duration = Duration::from_secs(3);

/// Whether something answers at `host:port` (a TCP connection, closed at
/// once): the reason in words when not (no such host, refused, no route,
/// no answer), which sqlx's pool would only report as a timeout.
pub(super) async fn reach(host: &str, port: u16) -> Result<()> {
    let at = address_text(host, port);
    let unreachable = |why: String| anyhow::Error::new(errors::Unreachable(format!("can't reach {at}: {why}")));
    let addrs: Vec<std::net::SocketAddr> =
        match tokio::time::timeout(REACH_TIMEOUT, tokio::net::lookup_host((host, port))).await {
            Ok(Ok(addrs)) => addrs.collect(),
            // (`diagnose::hint` adds what to do about each.)
            Ok(Err(_)) => return Err(unreachable("no such host".into())),
            Err(_) => return Err(unreachable("its name didn't resolve in time (DNS)".into())),
        };
    match tokio::time::timeout(REACH_TIMEOUT, tokio::net::TcpStream::connect(&addrs[..])).await {
        Ok(Ok(_)) => Ok(()),
        Ok(Err(e)) => Err(unreachable(io_reason(&e))),
        Err(_) => Err(unreachable(format!("no answer: timed out after {} s", REACH_TIMEOUT.as_secs()))),
    }
}

impl Database {
    /// Checks the server's address answers before a new connection is
    /// made for it after something went wrong: reopening the session,
    /// cancelling. There a server that's down fails at once, with why.
    async fn check_reach(&self) -> Result<()> {
        on_engine!(self.0.engine, engine::connect::check_reach(self).await)
    }

    pub async fn connect(params: ConnectParams) -> Result<Self> {
        let params = params.with_password().await?;
        // An IPv6 address may come in brackets; a file path is used as given.
        let mut host = if params.engine.is_file_based() {
            params.host.trim().to_string()
        } else {
            params.host.trim().trim_start_matches('[').trim_end_matches(']').to_string()
        };
        let mut port = params.port;
        let mut tunnel = None;
        if let Some(dest) = params.ssh.as_deref().filter(|_| !params.engine.is_file_based()) {
            if host.starts_with('/') {
                anyhow::bail!("SSH tunnels need a TCP host, not a socket path");
            }
            let (t, local) = open_tunnel(dest, &host, port).await?;
            tunnel = Some(t);
            host = "127.0.0.1".to_string();
            port = local;
        }
        // The server's TCP address (not a socket, not a tunnel's end on
        // this machine): each engine checks it answers its own way first
        // (`check_before_connect`), so a server that's down says so at once
        // rather than as the pool's "timed out" 10 s later.
        let address = (!params.engine.is_file_based() && !host.starts_with('/') && tunnel.is_none())
            .then(|| (host.clone(), port));
        if let Some((host, port)) = &address {
            on_engine!(params.engine, engine::connect::check_before_connect(host, *port).await)?;
        }
        let host = host.as_str();
        let open = |params: ConnectParams| async move {
            on_engine!(params.engine, engine::connect::connect(&params, host, port).await)
        };
        // `prefer` means TLS if the server can: when it offers TLS the
        // client can't speak (an old MySQL's TLS 1.0/1.1), try without.
        let (pool, mut session, tls_failed) = match open(params.clone()).await {
            Err(e) if params.ssl_mode == SslMode::Prefer && !host.starts_with('/') && tls_failure(&e).is_some() => {
                let why = tls_failure(&e).unwrap_or_default();
                tracing::warn!(why, "TLS failed; connecting without it (SSL mode prefer)");
                let (pool, session) = open(ConnectParams { ssl_mode: SslMode::Disable, ..params.clone() }).await?;
                (pool, session, Some(why))
            }
            result => {
                let (pool, session) = result?;
                (pool, session, None)
            }
        };
        let interrupt = Arc::new(AtomicBool::new(false));
        session.watch_interrupt(&interrupt).await?;
        let backend_id = session.backend_id().await?;
        let version = session.server_version().await;
        let sql_mode = session.sql_mode().await.unwrap_or_default();
        Ok(Database(Arc::new(Inner {
            engine: params.engine,
            pool,
            session: Arc::new(Mutex::new(session)),
            backend_id: AtomicI64::new(backend_id),
            interrupt,
            _tunnel: tunnel,
            version,
            sql_mode: std::sync::Mutex::new(sql_mode),
            tls_failed,
            session_changed: AtomicBool::new(false),
            stale: AtomicBool::new(false),
            stream_task: std::sync::Mutex::new(None),
            address,
        })))
    }

    /// Why TLS failed, if SSL mode `prefer` connected without it.
    pub fn tls_failed(&self) -> Option<&str> {
        self.0.tls_failed.as_deref()
    }

    pub fn engine(&self) -> Engine {
        self.0.engine
    }

    /// The server version, e.g. "16.2" (empty if it couldn't be read).
    pub fn version(&self) -> &str {
        &self.0.version
    }

    /// The session's `sql_mode` when last read (empty for other engines).
    pub fn sql_mode(&self) -> String {
        self.0.sql_mode.lock().map(|m| m.clone()).unwrap_or_default()
    }

    /// Forgets a cancel that came when nothing was running (SQLite), so it
    /// doesn't stop the next query. Called once a query has the session.
    fn clear_interrupt(&self) {
        self.0.interrupt.store(false, Ordering::SeqCst);
    }

    /// Re-reads the session's `sql_mode`, unless a query is running on it.
    pub async fn refresh_sql_mode(&self) {
        let Ok(mut session) = self.0.session.try_lock() else { return };
        if let Some(mode) = session.sql_mode().await
            && let Ok(mut current) = self.0.sql_mode.lock()
        {
            *current = mode;
        }
    }
}
