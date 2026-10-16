//! Opening a sqlite connection: the pool and the session connection.

// Every engine has these functions with one signature, the generated
// dispatch calling them alike (build.rs): some needn't wait for anything.
#![allow(clippy::unused_async)]

use super::super::*;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use super::DriverConnection;

pub(in crate::db) async fn connect(params: &ConnectParams, host: &str, _port: u16) -> Result<(Pool, Session)> {
    let opts =
        SqliteConnectOptions::new().filename(host).read_only(params.read_only).create_if_missing(!params.read_only);
    // One connection: each `:memory:` connection is its own database.
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .acquire_timeout(CONNECT_TIMEOUT)
        .connect_with(opts)
        .await
        .context("failed to open database file")?;
    let conn = pool.acquire().await.context("failed to open session")?;
    Ok((Pool::Sqlite(pool), Session::Sqlite(conn)))
}

/// SQLite has no server to tell about a session: cancelling is the
/// interrupt flag (`watch_interrupt`).
pub(in crate::db) async fn backend_id(_c: &mut DriverConnection) -> Result<i64> {
    Ok(0)
}

/// How many SQLite virtual machine steps go between checks for a cancel:
/// often enough to stop within milliseconds, rarely enough to cost nothing.
const INTERRUPT_CHECK_STEPS: i32 = 10_000;

/// Stops the running statement once `interrupt` is set. The handler stays
/// with the connection.
pub(in crate::db) async fn watch_interrupt(c: &mut DriverConnection, interrupt: &Arc<AtomicBool>) -> Result<()> {
    let interrupt = Arc::clone(interrupt);
    c.lock_handle().await?.set_progress_handler(INTERRUPT_CHECK_STEPS, move || !interrupt.load(Ordering::SeqCst));
    Ok(())
}

/// A file: nothing to reach first.
pub(in crate::db) async fn check_before_connect(_host: &str, _port: u16) -> Result<()> {
    Ok(())
}

pub(in crate::db) async fn check_reach(_db: &Database) -> Result<()> {
    Ok(())
}
