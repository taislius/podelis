//! `COPY … FROM STDIN` on PostgreSQL, for loading many rows at once.

use super::super::*;
use sqlx::postgres::PgDatabaseError;

/// `Database::copy_in` on PostgreSQL: `COPY`'s data sent on the session.
pub(in crate::db) async fn copy_in(
    db: &Database,
    statement: &str,
    first_line: u64,
    chunks: impl Iterator<Item = Result<Vec<u8>>>,
) -> Result<u64> {
    let mut session = db.0.session.lock().await;
    let Session::Postgres(conn) = &mut *session else {
        anyhow::bail!("COPY needs PostgreSQL");
    };
    let mut copy = conn.copy_in_raw(statement).await?;
    for chunk in chunks {
        let sent = match chunk {
            Ok(data) => copy.send(data).await.map(drop).map_err(|e| failed_line(e, first_line)),
            Err(e) => Err(e),
        };
        if let Err(e) = sent {
            let _ = copy.abort(format!("{e:#}")).await;
            return Err(e);
        }
    }
    copy.finish().await.map_err(|e| failed_line(e, first_line))
}

/// The server's error, with the line of the file it's about: PostgreSQL
/// says `COPY t, line 3, column c: …`, counting the data's rows from 1.
fn failed_line(e: sqlx::Error, first_line: u64) -> anyhow::Error {
    let row = e
        .as_database_error()
        .and_then(|db| db.try_downcast_ref::<PgDatabaseError>())
        .and_then(|pg| pg.r#where())
        .and_then(|w| w.split(", ").find_map(|part| part.strip_prefix("line ")?.parse::<u64>().ok()));
    match row {
        Some(row) => anyhow::Error::from(e).context(format!("line {}", first_line + row - 1)),
        None => e.into(),
    }
}
