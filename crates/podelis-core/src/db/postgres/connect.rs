//! Opening a postgres connection: the pool and the session connection.

#![expect(clippy::unused_async, reason = "every engine has these functions with one signature, the generated dispatch calling them alike (build.rs): some needn't wait for anything")]

use super::super::*;
use sqlx::pool::PoolConnection;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions, PgSslMode, Postgres};
use super::DriverConnection;

pub(in crate::db) async fn connect(params: &ConnectParams, host: &str, port: u16) -> Result<(Pool, Session)> {
    // `new()` would read ~/.pgpass for the default host and user: it's
    // looked up for this connection's (`db::passwords`, before connecting).
    let mut opts = PgConnectOptions::new_without_pgpass()
        .port(port)
        .ssl_mode(ssl_mode(params.tls_mode()))
        .application_name("podelis");
    if params.read_only {
        opts = opts.options([("default_transaction_read_only", "on")]);
    }
    if let Some(ca) = &params.tls.ca {
        opts = opts.ssl_root_cert(TlsFiles::expanded(ca));
    }
    if let Some(cert) = &params.tls.cert {
        opts = opts.ssl_client_cert(TlsFiles::expanded(cert));
    }
    if let Some(key) = &params.tls.key {
        opts = opts.ssl_client_key(TlsFiles::expanded(key));
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
        // Not kept idle long: a link that died while one waited in the pool
        // (a VPN, a laptop's sleep, a NAT's timeout) would hold up a `d` or a
        // cancel until the acquire timed out; a new one is quick to open.
        .idle_timeout(POOL_IDLE)
        .connect_with(opts)
        .await
        .context("couldn't connect")?;
    let mut conn = pool.acquire().await.context("failed to open session")?;
    if let Some(why) = transaction_pooled(&mut conn, host, port).await {
        // Back to the pool first: closing it waits for every connection
        // out of it.
        drop(conn);
        pool.close().await;
        anyhow::bail!(why);
    }
    Ok((Pool::Postgres(pool), Session::Postgres(conn)))
}

/// Why Podėlis won't use the session, if a pooler in transaction mode
/// (`PgBouncer`, Supavisor, RDS Proxy…) answers at `host:port`: it hands
/// each transaction another server session, so the backend's process id
/// changes from one statement to the next. A SQL client needs one session
/// (`SET`, transactions across runs, temporary tables), and cancelling by
/// that id could stop another client's query. (Simple protocol: such a
/// pooler may refuse the driver's named statements.) Best-effort: a quiet
/// pool often hands back the server connection it gave last (`PgBouncer`'s
/// LIFO), and then both ids agree; the supervisor's checks compare the id
/// again later (`check_server_session`), and say so then.
async fn transaction_pooled(conn: &mut PoolConnection<Postgres>, host: &str, port: u16) -> Option<String> {
    use sqlx::Row as _;
    let mut pid = async || -> Option<String> {
        let row = sqlx::raw_sql("SELECT pg_backend_pid()::text").fetch_optional(&mut **conn).await.ok()??;
        row.try_get::<String, _>(0).ok()
    };
    let (first, second) = (pid().await?, pid().await?);
    (first != second).then(|| {
        format!(
            "{} hands each transaction another server session (a pooler in transaction mode: PgBouncer, \
             Supavisor, RDS Proxy…), and a SQL client needs one session (SET, transactions across runs, \
             cancelling a query)\nhint: connect to the pooler's session mode, or to the server itself",
            crate::db::address_text(host, port)
        )
    })
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
