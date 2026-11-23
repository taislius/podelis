//! What the engines that talk through sqlx share: each one's `driver.rs`
//! is `driver!()` (acquiring, pinging, running, streaming, preparing,
//! reading one value) and its own errors' reading, through
//! [`error_facts`]. Nothing outside the engine folders that use it names
//! sqlx: they ask their `driver`, and an engine with another driver writes
//! its own.

use super::errors::{ErrorFacts, ServerError};

/// What an sqlx error says, in no driver's terms; `server` reads a
/// database error, if it's this engine's (`None` for another engine's,
/// so that one is asked).
pub(super) fn error_facts(
    cause: &(dyn std::error::Error + 'static),
    server: impl Fn(&dyn sqlx::error::DatabaseError) -> Option<ServerError>,
) -> Option<ErrorFacts> {
    use super::errors::{io_words, looks_tls, tls_words};
    Some(match cause.downcast_ref::<sqlx::Error>()? {
        sqlx::Error::Tls(tls) => {
            // Broken, as any TLS failure (a session the pool hands out after
            // the server's certificate changed is reopened, or said lost).
            let said = tls_words(&tls.to_string());
            ErrorFacts { broken: true, tls: Some(said.clone()), said: Some(said), ..ErrorFacts::default() }
        }
        sqlx::Error::Io(io) => {
            let text = io.to_string();
            let tls = looks_tls(&text).then(|| tls_words(&text));
            ErrorFacts { broken: true, tls, said: Some(io_words(io)), ..ErrorFacts::default() }
        }
        sqlx::Error::Protocol(text) => ErrorFacts { broken: true, said: protocol_words(text), ..ErrorFacts::default() },
        sqlx::Error::PoolTimedOut => ErrorFacts {
            no_connection: true,
            said: Some("no connection in time: the server didn't answer (timed out)".into()),
            ..ErrorFacts::default()
        },
        sqlx::Error::PoolClosed => {
            ErrorFacts { no_connection: true, said: Some("the connection was closed".into()), ..ErrorFacts::default() }
        }
        sqlx::Error::Database(db) => {
            let server = server(db.as_ref())?;
            // A connection exception (SQLSTATE class 08), or what the
            // engine says as it closes the connection.
            let broken = db.code().is_some_and(|c| c.starts_with("08")) || server.closes_connection;
            ErrorFacts { broken, server: Some(server), ..ErrorFacts::default() }
        }
        _ => return None,
    })
}

/// A protocol error in a user's words. Another program on the port (the
/// server's first answer isn't this database's) is said as such; any
/// other keeps its own words (an authentication method the client and the
/// server disagree on, a pooler's), without the driver's wrapping and
/// where in its code it was (`(sqlx_postgres::connection::tls:95)`).
fn protocol_words(text: &str) -> Option<String> {
    let t = text.to_ascii_lowercase();
    if t.contains("response from sslrequest") || t.contains("expected to read") || t.contains("unknown message type") {
        return Some(
            "the server answered in a way this database's protocol doesn't: is that host and port this database's?"
                .into(),
        );
    }
    let mut plain = text.trim();
    for wrapping in ["encountered unexpected or invalid data: ", "protocol error: "] {
        if plain.to_ascii_lowercase().starts_with(wrapping) {
            plain = &plain[wrapping.len()..];
        }
    }
    // Where in the driver's code: no one else's business.
    let plain = match plain.rfind(" (sqlx") {
        Some(at) if plain.ends_with(')') => &plain[..at],
        _ => plain,
    };
    (plain != text).then(|| plain.to_string())
}

/// An sqlx engine's `driver` module: what `db` asks of a connection, for
/// the engine's `DriverPool`, `DriverConnection` and `cell` (`super::`).
macro_rules! driver {
    () => {
        use futures_util::{StreamExt as _, TryStreamExt as _};
        use sqlx::{
            AssertSqlSafe, Column as _, Either, Executor as _, Row as _, SqlSafeStr as _, Statement as _,
            TypeInfo as _, ValueRef as _,
        };

        use super::{DriverConnection, DriverPool, cell};
        use $crate::db::{FetchedRow, Rows, StatementResult};

        /// A row's columns: (name, type name) each. SQLite declares no type
        /// for an expression (`sum(total)`), which sqlx calls `NULL`: the
        /// (first) row's value says what it is instead.
        fn columns_of<R: sqlx::Row>(row: &R) -> Vec<(String, String)>
        where
            usize: sqlx::ColumnIndex<R>,
        {
            row.columns()
                .iter()
                .enumerate()
                .map(|(i, c)| {
                    let declared = c.type_info();
                    let ty = if declared.is_null() {
                        row.try_get_raw(i)
                            .ok()
                            .filter(|v| !v.is_null())
                            .map_or_else(|| declared.name().to_string(), |v| v.type_info().name().to_string())
                    } else {
                        declared.name().to_string()
                    };
                    (c.name().to_string(), ty)
                })
                .collect()
        }

        /// A connection of the pool's.
        pub(in crate::db) async fn acquire(pool: &DriverPool) -> anyhow::Result<DriverConnection> {
            Ok(pool.acquire().await?)
        }

        /// Closes the pool, once every connection is back in it.
        pub(in crate::db) async fn close(pool: &DriverPool) {
            pool.close().await;
        }

        /// The protocol's ping: whether the server answered (an error it
        /// sent is an answer; a link that broke isn't).
        pub(in crate::db) async fn ping(c: &mut DriverConnection) -> bool {
            use sqlx::Connection as _;
            match c.ping().await {
                Ok(()) => true,
                // The server's last word before it ended the session (a
                // kill, a shutdown) isn't an answer either.
                Err(e) => {
                    !matches!(
                        e,
                        sqlx::Error::Io(_) | sqlx::Error::Tls(_) | sqlx::Error::PoolClosed | sqlx::Error::WorkerCrashed
                    ) && !crate::db::errors::is_broken(&anyhow::Error::new(e))
                }
            }
        }

        /// The connection out of the pool, not back into it.
        pub(in crate::db) fn detach(c: DriverConnection) {
            drop(c.detach());
        }

        /// Runs `;`-separated statements: each one's result, with at most
        /// `limit` rows (and the count of all).
        /// `ran` counts the statements done, so a failure says how many ran
        /// before it.
        pub(in crate::db) async fn run(
            c: &mut DriverConnection,
            sql: &str,
            limit: usize,
            ran: &mut usize,
        ) -> anyhow::Result<Vec<StatementResult>> {
            let mut results: Vec<StatementResult> = Vec::new();
            let mut current = StatementResult::default();
            let mut stream = sqlx::raw_sql(AssertSqlSafe(sql.to_string())).fetch_many(&mut **c);
            while let Some(item) = stream.try_next().await? {
                match item {
                    Either::Right(row) => {
                        if current.columns.is_none() {
                            current.columns = Some(columns_of(&row));
                        }
                        current.total_rows += 1;
                        if current.rows.len() < limit {
                            current.rows.push((0..row.columns().len()).map(|i| cell(&row, i)).collect());
                        }
                    }
                    Either::Left(done) => {
                        current.rows_affected = done.rows_affected();
                        results.push(std::mem::take(&mut current));
                        *ran = results.len();
                    }
                }
            }
            Ok(results)
        }

        /// One statement's rows as they come, the first with the columns.
        pub(in crate::db) fn fetch(c: &mut DriverConnection, sql: String) -> Rows<'_> {
            let mut first = true;
            sqlx::raw_sql(AssertSqlSafe(sql))
                .fetch(&mut **c)
                .map(move |row| {
                    let row = row?;
                    let columns = std::mem::take(&mut first).then(|| columns_of(&row));
                    Ok(FetchedRow { columns, cells: (0..row.columns().len()).map(|i| cell(&row, i)).collect() })
                })
                .boxed()
        }

        /// The driver's prepared statements dropped, so the next prepare
        /// asks the server: one kept from before a schema change (made
        /// elsewhere: another tool, a migration) would answer for the old
        /// shape. Runs don't use them (`raw_sql`), so nothing else is slower.
        async fn forget_prepared(c: &mut DriverConnection) -> anyhow::Result<()> {
            use sqlx::Connection as _;
            c.clear_cached_statements().await?;
            Ok(())
        }

        /// Prepares `sql` without running it: the server's parser and
        /// catalog check it, as the schema is now.
        pub(in crate::db) async fn prepare(c: &mut DriverConnection, sql: &str) -> anyhow::Result<()> {
            forget_prepared(c).await?;
            (&mut **c).prepare(AssertSqlSafe(sql.to_string()).into_sql_str()).await?;
            Ok(())
        }

        /// The columns `sql` would return, from preparing it, as the schema
        /// is now; `None` if it can't be prepared.
        pub(in crate::db) async fn columns(c: &mut DriverConnection, sql: &str) -> Option<Vec<(String, String)>> {
            forget_prepared(c).await.ok()?;
            let stmt = (&mut **c).prepare(AssertSqlSafe(sql.to_string()).into_sql_str()).await.ok()?;
            Some(stmt.columns().iter().map(|c| (c.name().to_string(), c.type_info().name().to_string())).collect())
        }

        /// The one text value `sql` returns.
        pub(in crate::db) async fn text(c: &mut DriverConnection, sql: &str) -> anyhow::Result<String> {
            Ok(sqlx::query_scalar(AssertSqlSafe(sql.to_string())).fetch_one(&mut **c).await?)
        }
    };
}
pub(super) use driver;

#[cfg(test)]
mod tests {
    use super::*;

    fn facts(err: &sqlx::Error) -> ErrorFacts {
        error_facts(err, |_| None).unwrap()
    }

    #[test]
    fn tls_failures_are_told_from_other_connect_errors() {
        let io = |text: &str| sqlx::Error::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, text.to_string()));
        let handshake = facts(&io("received fatal alert: HandshakeFailure"));
        assert_eq!(handshake.tls.as_deref(), Some("the TLS handshake failed"));
        assert!(handshake.broken);
        assert!(facts(&io("Connection refused (os error 111)")).tls.is_none());
        let denied = facts(&sqlx::Error::Protocol("Access denied for user".into()));
        assert!(denied.tls.is_none() && denied.broken);
        assert!(facts(&sqlx::Error::PoolTimedOut).no_connection);
        assert!(error_facts(&sqlx::Error::RowNotFound, |_| None).is_none(), "nothing to say");
    }

    #[test]
    fn the_drivers_errors_are_said_in_a_users_words() {
        let io = |kind, text: &str| sqlx::Error::Io(std::io::Error::new(kind, text.to_string()));
        use std::io::ErrorKind as K;
        let said = |e: &sqlx::Error| facts(e).said.unwrap();
        assert_eq!(
            said(&io(K::InvalidData, "invalid peer certificate: UnknownIssuer")),
            "the server's TLS certificate isn't signed by an authority this computer trusts"
        );
        assert_eq!(
            said(&io(K::InvalidData, "invalid peer certificate: NotValidForName")),
            "the server's TLS certificate is for another host name"
        );
        assert_eq!(
            said(&io(K::InvalidData, "invalid peer certificate: Expired")),
            "the server's TLS certificate has expired"
        );
        assert_eq!(
            said(&io(K::Other, "failed to lookup address information: nodename nor servname provided")),
            "no such host"
        );
        assert_eq!(said(&io(K::ConnectionReset, "Connection reset by peer (os error 54)")), "the connection was cut");
        assert_eq!(
            said(&io(K::UnexpectedEof, "expected to read 5526600 bytes, got 24 bytes at EOF")),
            "the server closed the connection",
            "mid-session too: not said as the login's"
        );
        assert!(
            said(&sqlx::Error::Protocol(
                "unexpected response from SSLRequest: 0x48 (sqlx_postgres::connection::tls:95)".into()
            ))
            .contains("this database's")
        );
        // A server's message that came as a protocol error is its own; one
        // about the login method keeps its words, without the wrapping.
        assert!(facts(&sqlx::Error::Protocol("Access denied for user".into())).said.is_none());
        assert_eq!(
            said(&sqlx::Error::Protocol(
                "encountered unexpected or invalid data: expected AuthenticationSaslFinal (sqlx_postgres::connection::sasl:120)".into()
            )),
            "expected AuthenticationSaslFinal"
        );
        assert!(said(&sqlx::Error::PoolTimedOut).contains("timed out"), "the timeout's hint still finds it");
    }

    /// Through `errors`: the driver's error found under context added on
    /// top, which the message keeps.
    #[test]
    fn errors_are_read_under_their_context() {
        let err =
            anyhow::Error::from(sqlx::Error::Io(std::io::Error::other("reset by peer"))).context("loading tables");
        assert!(crate::db::errors::is_broken(&err));
        assert!(crate::db::errors::is_unreachable(&anyhow::Error::from(sqlx::Error::PoolTimedOut)));
        assert!(!crate::db::errors::is_broken(&anyhow::anyhow!("no such table")));
    }
}
