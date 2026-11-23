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
    // The data is read here, as it's sent there: the session's work takes
    // each chunk from a channel, a few ahead.
    // `None` after the last chunk: the data's end. The channel closing
    // without it means the import was stopped, and the copy is aborted
    // (finishing it would keep half the file).
    let (feed, mut receive) = mpsc::channel::<Option<Result<Vec<u8>>>>(4);
    let statement = statement.to_string();
    let on = db.clone();
    let copying = db.with_session(move |session| {
        let db = on;
        Box::pin(async move {
            let Session::Postgres(conn) = session else {
                anyhow::bail!("COPY needs PostgreSQL");
            };
            let mut copy = conn.copy_in_raw(&statement).await?;
            loop {
                let chunk = match receive.recv().await {
                    Some(Some(chunk)) => chunk,
                    Some(None) => break,
                    None => {
                        abort(copy, "the import was stopped", &db).await;
                        anyhow::bail!("the import was stopped");
                    }
                };
                let sent = match chunk {
                    Ok(data) => copy.send(data).await.map(drop).map_err(|e| failed_line(e, first_line)),
                    Err(e) => Err(e),
                };
                if let Err(e) = sent {
                    abort(copy, &format!("{e:#}"), &db).await;
                    return Err(e);
                }
            }
            copy.finish().await.map_err(|e| failed_line(e, first_line))
        })
    });
    let feeding = async move {
        for chunk in chunks {
            // The copy stopped (failed): nothing more to send.
            if feed.send(Some(chunk)).await.is_err() {
                return;
            }
        }
        let _ = feed.send(None).await;
    };
    let (copied, ()) = tokio::join!(copying, feeding);
    copied?
}

/// How long the server has to answer a `COPY`'s abort.
const ABORT_WITHIN: Duration = if cfg!(test) { Duration::from_secs(1) } else { Duration::from_secs(10) };

/// Aborts `copy`, the server telling why: unanswered in time, or answered
/// with anything but the abort's own error (a link that broke while it
/// was sent), the session's state isn't known, so it's given up on (the
/// next run opens a new one, and says the old one's gone).
async fn abort<C: std::ops::DerefMut<Target = sqlx::PgConnection>>(
    copy: sqlx::postgres::PgCopyIn<C>,
    why: &str,
    db: &Database,
) {
    let why = match tokio::time::timeout(ABORT_WITHIN, copy.abort(why)).await {
        Ok(Ok(())) => return,
        Ok(Err(e)) => format!("the copy's abort failed: {e}"),
        Err(_) => format!("the copy's abort wasn't answered in {} s", ABORT_WITHIN.as_secs()),
    };
    tracing::warn!(why, "giving the session up");
    db.0.stale.store(true, Ordering::SeqCst);
    db.report_lost(&why);
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
