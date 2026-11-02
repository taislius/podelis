//! Opening a DuckDB database: the file (or `:memory:`), its first
//! connection the pool's, and the session a clone of it.

#![expect(clippy::unused_async, reason = "every engine has these functions with one signature, the generated dispatch calling them alike (build.rs): some needn't wait for anything")]

use super::super::*;
use super::DriverConnection;
use super::driver::{self, DuckPool};

/// The databases open in this process, by file and whether read-only:
/// DuckDB wants one instance of a file per process (two would each cache
/// it and write over each other), so another connection to an open file
/// shares its instance, a clone of a pool's first connection.
static OPEN: std::sync::LazyLock<std::sync::Mutex<Vec<Opened>>> = std::sync::LazyLock::new(|| std::sync::Mutex::new(Vec::new()));

/// A file open in this process: its path, whether its instance is
/// read-only, and a pool's first connection (gone once that pool closes).
type Opened = (String, bool, std::sync::Weak<std::sync::Mutex<Option<::duckdb::Connection>>>);

/// A connection to `path`'s instance open in this process, if there is one.
/// A read-only connection shares an instance that writes (its driver runs
/// only reads on it); one that writes can't share a read-only instance.
fn shared(path: &str, read_only: bool) -> Result<Option<::duckdb::Connection>> {
    let mut open = OPEN.lock().map_err(|_| anyhow::anyhow!("the open databases' list broke"))?;
    open.retain(|(_, _, first)| first.upgrade().is_some_and(|f| f.lock().is_ok_and(|c| c.is_some())));
    for (file, mode, first) in open.iter() {
        if file != path {
            continue;
        }
        if *mode && !read_only {
            anyhow::bail!(
                "this file is open read-only in another connection: DuckDB opens a file one way at a time, so \
                 close that one to write"
            );
        }
        let Some(first) = first.upgrade() else { continue };
        let first = first.lock().map_err(|_| anyhow::anyhow!("the database broke"))?;
        if let Some(conn) = first.as_ref() {
            return Ok(Some(conn.try_clone()?));
        }
    }
    Ok(None)
}

/// `path` with its folder resolved (links, `..`), so two ways of naming a
/// file are one.
fn resolved(path: &str) -> String {
    let path = std::path::Path::new(path);
    let (Some(folder), Some(name)) = (path.parent(), path.file_name()) else { return path.to_string_lossy().into_owned() };
    let folder = if folder.as_os_str().is_empty() { std::path::Path::new(".") } else { folder };
    std::fs::canonicalize(folder).map_or_else(|_| path.to_path_buf(), |f| f.join(name)).to_string_lossy().into_owned()
}

pub(in crate::db) async fn connect(params: &ConnectParams, host: &str, _port: u16) -> Result<(Pool, Session)> {
    let memory = host.is_empty() || host == ":memory:";
    // The same file by any path to it (its folder resolved: the file may
    // not be there yet).
    let path = if memory { String::new() } else { resolved(host) };
    let read_only = params.read_only;
    let reused = if memory { None } else { shared(&path, read_only)? };
    let pool = if let Some(conn) = reused {
        DuckPool::sharing(conn, read_only)
    } else {
        let file = path.clone();
        let first = tokio::task::spawn_blocking(move || -> Result<::duckdb::Connection> {
            let mode = if read_only { ::duckdb::AccessMode::ReadOnly } else { ::duckdb::AccessMode::ReadWrite };
            let config = ::duckdb::Config::default().access_mode(mode)?;
            let opened = if memory {
                ::duckdb::Connection::open_in_memory_with_flags(config)
            } else {
                ::duckdb::Connection::open_with_flags(&file, config)
            };
            opened.context("failed to open database file")
        })
        .await??;
        DuckPool::new(first)
    };
    if !memory && let Ok(mut open) = OPEN.lock() {
        open.push((path, read_only, std::sync::Arc::downgrade(pool.first())));
    }
    let session = driver::acquire(&pool).await.context("failed to open session")?;
    Ok((Pool::DuckDb(pool), Session::DuckDb(session)))
}

/// No server to tell about a session: a cancel interrupts it (`cancel`).
pub(in crate::db) async fn backend_id(_c: &mut DriverConnection) -> Result<i64> {
    Ok(0)
}

pub(in crate::db) async fn watch_interrupt(_c: &mut DriverConnection, _interrupt: &Arc<AtomicBool>) -> Result<()> {
    Ok(())
}

/// A file: nothing to reach first.
pub(in crate::db) async fn check_before_connect(_host: &str, _port: u16) -> Result<()> {
    Ok(())
}

pub(in crate::db) async fn check_reach(_db: &Database) -> Result<()> {
    Ok(())
}
