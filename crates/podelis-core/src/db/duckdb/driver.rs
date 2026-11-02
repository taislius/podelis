//! How `db` talks to DuckDB: through the `duckdb` crate, which is sync, so
//! each call runs on a blocking thread, holding the connection while it
//! does. Results come as Arrow batches (`cells.rs` reads them).

#![expect(clippy::unused_async, reason = "every engine's driver has these functions with one signature, the generated dispatch calling them alike (build.rs): some needn't wait for anything")]

use std::sync::{Arc, Mutex};

use anyhow::{Result, anyhow};
use ::duckdb::arrow::record_batch::RecordBatch;
use futures_util::StreamExt as _;

use super::cells;
use crate::db::errors::{ErrorFacts, ServerError};
use crate::db::{Engine, ErrorPos, FetchedRow, Rows, StatementResult};

/// The open database: each connection is a clone of the first (one
/// database, shared), and the session's interrupt is kept here, where a
/// cancel finds it while the session is busy.
pub struct DuckPool {
    first: Arc<Mutex<Option<::duckdb::Connection>>>,
    /// Its connections only read (`DuckConnection::read_only`).
    read_only: bool,
    interrupt: Mutex<Option<Arc<::duckdb::InterruptHandle>>>,
}

/// A pool's first connection, as `connect` keeps it to share the instance.
pub(super) type First = Arc<Mutex<Option<::duckdb::Connection>>>;

impl DuckPool {
    pub(super) fn new(first: ::duckdb::Connection) -> Self {
        DuckPool { first: Arc::new(Mutex::new(Some(first))), read_only: false, interrupt: Mutex::new(None) }
    }

    /// Its first connection, for another pool on the same file to clone.
    pub(super) fn first(&self) -> &First {
        &self.first
    }

    /// A pool sharing `first`'s instance, its connections read-only where
    /// `read_only`.
    pub(super) fn sharing(first: ::duckdb::Connection, read_only: bool) -> Self {
        DuckPool { read_only, ..DuckPool::new(first) }
    }

    /// Stops what the session runs (it fails with "Interrupted").
    pub(super) fn interrupt(&self) {
        if let Some(handle) = self.interrupt.lock().ok().and_then(|h| h.clone()) {
            handle.interrupt();
        }
    }
}

/// A connection, shared with the blocking thread that runs its calls.
pub struct DuckConnection {
    conn: Arc<Mutex<::duckdb::Connection>>,
    /// Read-only though its instance writes (one instance per file, shared:
    /// `connect`): only reads run on it.
    read_only: bool,
}

/// An error unless `sql` only reads (or opens and ends transactions), on a
/// read-only connection. Not `SET`: many of DuckDB's settings are the
/// instance's, which other connections share.
fn check_reads(c: &DuckConnection, sql: &str) -> Result<()> {
    use crate::sql::analyze::Kind;
    let only_reads = || {
        crate::sql::analyze::analyze(&crate::sql::parse::parse(sql, Engine::DuckDb))
            .iter()
            .all(|s| matches!(s.kind, Kind::Read | Kind::Transaction(_)))
    };
    if c.read_only && !only_reads() {
        anyhow::bail!("this connection is read-only: only queries run on it");
    }
    Ok(())
}

/// `f` on the connection, on a blocking thread.
pub(super) async fn blocking<T: Send + 'static>(
    c: &DuckConnection,
    f: impl FnOnce(&::duckdb::Connection) -> Result<T> + Send + 'static,
) -> Result<T> {
    let conn = Arc::clone(&c.conn);
    tokio::task::spawn_blocking(move || {
        let guard = conn.lock().map_err(|_| anyhow!("the connection broke (a call on it panicked)"))?;
        f(&guard)
    })
    .await?
}

/// A connection of the database's, which a cancel then interrupts.
pub(in crate::db) async fn acquire(pool: &DuckPool) -> Result<DuckConnection> {
    let first = pool.first.lock().map_err(|_| anyhow!("the database broke"))?;
    let conn = first.as_ref().ok_or_else(|| anyhow!("the database is closed"))?.try_clone()?;
    if let Ok(mut interrupt) = pool.interrupt.lock() {
        *interrupt = Some(conn.interrupt_handle());
    }
    Ok(DuckConnection { conn: Arc::new(Mutex::new(conn)), read_only: pool.read_only })
}

/// Closes the database (once the last connection goes).
pub(in crate::db) async fn close(pool: &DuckPool) {
    if let Ok(mut first) = pool.first.lock() {
        first.take();
    }
}

/// A file always answers.
pub(in crate::db) async fn ping(_c: &mut DuckConnection) {}

pub(in crate::db) fn detach(c: DuckConnection) {
    drop(c);
}

/// Whether `statement` only reads.
fn reads(statement: &str) -> bool {
    crate::sql::analyze::analyze(&crate::sql::parse::parse(statement, Engine::DuckDb))
        .iter()
        .all(|s| s.kind == crate::sql::analyze::Kind::Read)
}

/// Runs `;`-separated statements, one by one: each one's result, with at
/// most `limit` rows (and the count of all). A change's result is
/// DuckDB's one `Count` row (its rows affected) or `Success`.
pub(in crate::db) async fn run(c: &mut DuckConnection, sql: &str, limit: usize) -> Result<Vec<StatementResult>> {
    check_reads(c, sql)?;
    let statements: Vec<String> =
        crate::sql::split_statements(sql, Engine::DuckDb).into_iter().map(str::to_string).collect();
    blocking(c, move |conn| {
        let mut results = Vec::new();
        for statement in statements {
            let mut stmt = conn.prepare(&statement)?;
            let arrow = stmt.query_arrow([])?;
            let schema = arrow.get_schema();
            let batches: Vec<RecordBatch> = arrow.collect();
            let mut result = StatementResult::default();
            // A change's result is one `Count` (rows changed) or `Success`
            // column: no rows of its own.
            let alone = (schema.fields().len() == 1 && !reads(&statement)).then(|| schema.field(0).name().as_str());
            if alone == Some("Success") {
            } else if alone == Some("Count") {
                let count = batches.first().and_then(|b| cells::cell(b.column(0).as_ref(), 0));
                result.rows_affected = count.and_then(|n| n.parse().ok()).unwrap_or(0);
            } else if !schema.fields().is_empty() {
                result.columns = Some(cells::columns(&schema));
                for batch in &batches {
                    for row in 0..batch.num_rows() {
                        result.total_rows += 1;
                        if result.rows.len() < limit {
                            result.rows.push(cells::row(batch, row));
                        }
                    }
                }
            }
            results.push(result);
        }
        Ok(results)
    })
    .await
}

/// One statement's rows as they come (through a channel from the blocking
/// thread), the first with the columns. Dropping the stream stops it.
pub(in crate::db) fn fetch(c: &mut DuckConnection, sql: String) -> Rows<'_> {
    if let Err(e) = check_reads(c, &sql) {
        return futures_util::stream::once(async move { Err(e) }).boxed();
    }
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<FetchedRow>>(256);
    let conn = Arc::clone(&c.conn);
    tokio::task::spawn_blocking(move || {
        let sent = (|| -> Result<()> {
            let guard = conn.lock().map_err(|_| anyhow!("the connection broke"))?;
            let mut stmt = guard.prepare(&sql)?;
            let arrow = stmt.query_arrow([])?;
            let mut columns = Some(cells::columns(&arrow.get_schema()));
            for batch in arrow {
                for row in 0..batch.num_rows() {
                    let fetched = FetchedRow { columns: columns.take(), cells: cells::row(&batch, row) };
                    if tx.blocking_send(Ok(fetched)).is_err() {
                        return Ok(());
                    }
                }
            }
            Ok(())
        })();
        if let Err(e) = sent {
            let _ = tx.blocking_send(Err(e));
        }
    });
    futures_util::stream::unfold(rx, |mut rx| async move { rx.recv().await.map(|row| (row, rx)) }).boxed()
}

/// Prepares `sql` without running it: DuckDB's parser and binder check it.
pub(in crate::db) async fn prepare(c: &mut DuckConnection, sql: &str) -> Result<()> {
    let sql = sql.to_string();
    blocking(c, move |conn| {
        conn.prepare(&sql)?;
        Ok(())
    })
    .await
}

/// The columns a read would return, from `DESCRIBE` (which doesn't run it);
/// `None` for anything else.
pub(in crate::db) async fn columns(c: &mut DuckConnection, sql: &str) -> Option<Vec<(String, String)>> {
    if !reads(sql) {
        return None;
    }
    let sql = format!("DESCRIBE {}", sql.trim().trim_end_matches(';'));
    blocking(c, move |conn| {
        let mut stmt = conn.prepare(&sql)?;
        let described = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        Ok(described.collect::<Result<Vec<_>, _>>()?)
    })
    .await
    .ok()
}

/// The one text value `sql` returns.
pub(in crate::db) async fn text(c: &mut DuckConnection, sql: &str) -> Result<String> {
    let sql = sql.to_string();
    blocking(c, move |conn| Ok(conn.query_row(&sql, [], |r| r.get::<_, String>(0))?)).await
}

/// What one of DuckDB's errors says. Its messages start with their class:
/// `Parser Error: syntax error at or near "FRO"`, `Catalog Error: Table
/// with name t does not exist!`, `Binder Error: …`.
pub(in crate::db) fn error_facts(cause: &(dyn std::error::Error + 'static)) -> Option<ErrorFacts> {
    let ::duckdb::Error::DuckDBFailure(_, Some(message)) = cause.downcast_ref::<::duckdb::Error>()? else { return None };
    let parser = message.starts_with("Parser Error:");
    let unknown = message.starts_with("Catalog Error:") || message.starts_with("Binder Error:");
    let position = message
        .split_once("at or near \"")
        .and_then(|(_, rest)| rest.split_once('"'))
        .map(|(token, _)| ErrorPos::Token(token.to_string()));
    Some(ErrorFacts {
        server: Some(ServerError {
            message: message.trim_end().to_string(),
            position,
            missing_feature: parser || (unknown && message.contains("Function with name")),
            closes_connection: false,
            check: if parser { Some(true) } else { unknown.then_some(false) },
        }),
        ..ErrorFacts::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn failure(message: &str) -> ::duckdb::Error {
        let code = ::duckdb::ffi::Error::new(1);
        ::duckdb::Error::DuckDBFailure(code, Some(message.to_string()))
    }

    /// A read-only connection takes reads and their plans, not changes.
    #[test]
    fn what_a_read_only_connection_takes() {
        for sql in ["SELECT 1", "EXPLAIN SELECT * FROM t WHERE x = 2", "WITH a AS (SELECT 1) SELECT * FROM a"] {
            assert!(reads(sql), "{sql}");
        }
        for sql in ["INSERT INTO t VALUES (1)", "EXPLAIN ANALYZE UPDATE t SET x = 1", "DROP TABLE t"] {
            assert!(!reads(sql), "{sql}");
        }
    }

    #[test]
    fn its_errors_say_what_they_are() {
        let syntax = error_facts(&failure("Parser Error: syntax error at or near \"FRO\"")).unwrap().server.unwrap();
        assert_eq!((syntax.check, syntax.position), (Some(true), Some(ErrorPos::Token("FRO".into()))));
        let missing = error_facts(&failure("Catalog Error: Table with name t does not exist!")).unwrap().server.unwrap();
        assert_eq!((missing.check, missing.missing_feature), (Some(false), false));
        let function = error_facts(&failure("Catalog Error: Scalar Function with name f does not exist!"));
        assert!(function.unwrap().server.unwrap().missing_feature);
        assert!(error_facts(&std::io::Error::other("not DuckDB's")).is_none());
    }
}
