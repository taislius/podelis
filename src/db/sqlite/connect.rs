//! Opening a sqlite connection: the pool and the session connection.

use super::super::*;

pub(in crate::db) async fn connect(params: &ConnectParams, host: &str) -> Result<(Pool, Session)> {
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
