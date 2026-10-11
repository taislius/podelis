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

/// The session's connection id (for cancelling its queries).
pub(in crate::db) async fn backend_id(c: &mut PoolConnection<MySql>) -> Result<i64> {
    let id: u64 = sqlx::query_scalar("SELECT CONNECTION_ID()")
        .fetch_one(&mut **c)
        .await
        .context("failed to read connection id")?;
    Ok(id as i64)
}
