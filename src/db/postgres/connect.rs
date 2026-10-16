//! Opening a postgres connection: the pool and the session connection.

// Every engine has these functions with one signature, the generated
// dispatch calling them alike (build.rs): some needn't wait for anything.
#![allow(clippy::unused_async)]

use super::super::*;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions, PgSslMode, Postgres};
use super::DriverConnection;

pub(in crate::db) async fn connect(params: &ConnectParams, host: &str, port: u16) -> Result<(Pool, Session)> {
    // `new()` would read ~/.pgpass for the default host and user: it's
    // looked up for this connection's (`db::passwords`, before connecting).
    let mut opts = PgConnectOptions::new_without_pgpass()
        .port(port)
        .ssl_mode(ssl_mode(params.ssl_mode))
        .application_name("podelis");
    if params.read_only {
        opts = opts.options([("default_transaction_read_only", "on")]);
    }
    // Leave unset fields to libpq-style defaults (OS user, PGPASSWORD...).
    if !params.user.is_empty() {
        opts = opts.username(&params.user);
    }
    if !params.password.is_empty() {
        opts = opts.password(&params.password);
    }
    opts = if host.starts_with('/') { opts.socket(host) } else { opts.host(host) };
    if !params.database.is_empty() {
        opts = opts.database(&params.database);
    }
    let pool = PgPoolOptions::new()
        .max_connections(3)
        .acquire_timeout(CONNECT_TIMEOUT)
        .connect_with(opts)
        .await
        .context("failed to connect to database")?;
    let conn = pool.acquire().await.context("failed to open session")?;
    Ok((Pool::Postgres(pool), Session::Postgres(conn)))
}

/// Podėlis's SSL mode as PostgreSQL's (libpq's names, one for one).
fn ssl_mode(mode: SslMode) -> PgSslMode {
    match mode {
        SslMode::Disable => PgSslMode::Disable,
        SslMode::Prefer => PgSslMode::Prefer,
        SslMode::Require => PgSslMode::Require,
        SslMode::VerifyCa => PgSslMode::VerifyCa,
        SslMode::VerifyFull => PgSslMode::VerifyFull,
    }
}

/// The session's backend process id (for cancelling its queries).
pub(in crate::db) async fn backend_id(c: &mut PoolConnection<Postgres>) -> Result<i64> {
    let id: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut **c)
        .await
        .context("failed to read backend pid")?;
    Ok(id.into())
}

/// The server is told to cancel (`queries::cancel`): nothing to watch here.
pub(in crate::db) async fn watch_interrupt(_c: &mut DriverConnection, _interrupt: &Arc<AtomicBool>) -> Result<()> {
    Ok(())
}

/// Before connecting: something answers at the address (a bare TCP
/// connection, which PostgreSQL 13+ doesn't even log), so a server that's
/// down says why at once.
pub(in crate::db) async fn check_before_connect(host: &str, port: u16) -> Result<()> {
    crate::db::reach(host, port).await
}

/// Before reopening the session or cancelling: the same.
pub(in crate::db) async fn check_reach(db: &Database) -> Result<()> {
    match &db.0.address {
        Some((host, port)) => crate::db::reach(host, *port).await,
        None => Ok(()),
    }
}
