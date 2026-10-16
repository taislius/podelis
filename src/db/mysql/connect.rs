//! Opening a mysql connection: the pool and the session connection.

use super::super::*;

pub(in crate::db) async fn connect(params: &ConnectParams, host: &str, port: u16) -> Result<(Pool, Session)> {
    // sqlx adds PIPES_AS_CONCAT to sql_mode by default; keep the server's
    // own, so `||` means here what it means in the user's application.
    let mut opts = MySqlConnectOptions::new()
        .port(port)
        .username(&params.user)
        .ssl_mode(params.ssl_mode.mysql())
        .pipes_as_concat(false);
    if !params.password.is_empty() {
        opts = opts.password(&params.password);
    }
    opts = if host.starts_with('/') { opts.socket(host) } else { opts.host(host) };
    if !params.database.is_empty() {
        opts = opts.database(&params.database);
    }
    // TCP only: a socket on this machine answers or doesn't at once.
    if !host.starts_with('/') {
        try_login(&opts).await?;
    }
    let read_only = params.read_only;
    let pool = MySqlPoolOptions::new()
        .max_connections(3)
        .acquire_timeout(CONNECT_TIMEOUT)
        .after_connect(move |conn, _| {
            Box::pin(async move {
                if read_only {
                    conn.execute("SET SESSION TRANSACTION READ ONLY").await?;
                }
                Ok(())
            })
        })
        .connect_with(opts)
        .await
        .context("failed to connect to database")?;
    let conn = pool.acquire().await.context("failed to open session")?;
    Ok((Pool::MySql(pool), Session::MySql(conn)))
}

/// One login of its own, outside the pool: a server that's down (refused,
/// no route, no such host) says so at once and why, where the pool would
/// try again until `CONNECT_TIMEOUT` and say only "timed out". A whole
/// login, not a bare TCP connect (`db::reach`): MySQL counts a connection
/// closed before its handshake against the host (`max_connect_errors`),
/// and blocks the host after enough; a login that completes resets that.
pub(in crate::db) async fn try_login(opts: &MySqlConnectOptions) -> Result<()> {
    use sqlx::Connection as _;
    match tokio::time::timeout(CONNECT_TIMEOUT, sqlx::MySqlConnection::connect_with(opts)).await {
        Ok(Ok(conn)) => {
            let _ = conn.close().await;
            Ok(())
        }
        // The network's reasons as `reach` words them; the server's own
        // (a wrong password) as they come.
        Ok(Err(sqlx::Error::Io(e))) => Err(unreachable(opts, &crate::db::io_reason(&e))),
        Ok(Err(e)) => Err(anyhow::Error::new(e).context("failed to connect to database")),
        Err(_) => Err(unreachable(opts, &format!("no answer: timed out after {} s", CONNECT_TIMEOUT.as_secs()))),
    }
}

fn unreachable(opts: &MySqlConnectOptions, why: &str) -> anyhow::Error {
    let at = crate::db::address_text(opts.get_host(), opts.get_port());
    anyhow::Error::new(crate::db::errors::Unreachable(format!("can't reach {at}: {why}")))
}

/// The session's connection id (for cancelling its queries).
pub(in crate::db) async fn backend_id(c: &mut PoolConnection<MySql>) -> Result<i64> {
    let id: u64 = sqlx::query_scalar("SELECT CONNECTION_ID()")
        .fetch_one(&mut **c)
        .await
        .context("failed to read connection id")?;
    Ok(id as i64)
}
