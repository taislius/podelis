use super::errors::sqlite_near;
use super::*;

/// Reads up to `n` more rows; `true` once the rows have run out.
pub(super) async fn read_rows<R, S>(
    stream: &mut S,
    n: usize,
    cell: fn(&R, usize) -> Option<String>,
    columns: &mut Option<Vec<(String, String)>>,
    out: &mut Vec<Vec<Option<String>>>,
) -> Result<bool, sqlx::Error>
where
    R: Row,
    S: futures_util::Stream<Item = Result<R, sqlx::Error>> + Unpin,
{
    while out.len() < n {
        let Some(row) = stream.try_next().await? else { return Ok(true) };
        if columns.is_none() {
            *columns =
                Some(row.columns().iter().map(|c| (c.name().to_string(), c.type_info().name().to_string())).collect());
        }
        out.push((0..row.columns().len()).map(|i| cell(&row, i)).collect());
    }
    Ok(false)
}

/// The streaming task for one connection: sends the first page, then serves
/// page requests until the rows run out or every [`RowStream`] is dropped,
/// in which case the query is cancelled on the server.
pub(super) async fn serve_stream<R, S>(
    mut stream: S,
    cell: fn(&R, usize) -> Option<String>,
    page: usize,
    first_tx: oneshot::Sender<Result<FirstPage>>,
    mut req_rx: mpsc::Receiver<PageRequest>,
    db: Database,
) where
    R: Row,
    S: futures_util::Stream<Item = Result<R, sqlx::Error>> + Unpin,
{
    let mut columns = None;
    let mut rows = Vec::new();
    let done = match read_rows(&mut stream, page, cell, &mut columns, &mut rows).await {
        Ok(done) => done,
        Err(e) => {
            let _ = first_tx.send(Err(e.into()));
            return;
        }
    };
    let _ = first_tx.send(Ok((columns.clone(), rows, done)));
    if done {
        return;
    }
    while let Some((n, reply)) = req_rx.recv().await {
        let mut rows = Vec::new();
        match read_rows(&mut stream, n, cell, &mut columns, &mut rows).await {
            Ok(done) => {
                let _ = reply.send(Ok(Page { rows, done }));
                if done {
                    return;
                }
            }
            Err(e) => {
                let _ = reply.send(Err(e.to_string()));
                return;
            }
        }
    }
    // Every handle dropped with rows left: stop the query rather than
    // draining the rest over the network.
    let _ = db.cancel().await;
}

/// Reads a `fetch_many` stream into per-statement results. A macro rather
/// than a generic fn because the executor bounds for `&mut Connection` are
/// unwieldy to spell out.
macro_rules! collect_statements {
    ($conn:expr, $sql:expr, $limit:expr, $cell:expr) => {{
        let mut results: Vec<StatementResult> = Vec::new();
        let mut current = StatementResult::default();
        let mut stream = sqlx::raw_sql(AssertSqlSafe($sql)).fetch_many($conn);
        while let Some(item) = stream.try_next().await? {
            match item {
                Either::Right(row) => {
                    if current.columns.is_none() {
                        current.columns = Some(
                            row.columns()
                                .iter()
                                .map(|c| (c.name().to_string(), c.type_info().name().to_string()))
                                .collect(),
                        );
                    }
                    current.total_rows += 1;
                    if current.rows.len() < $limit {
                        current.rows.push((0..row.columns().len()).map(|i| $cell(&row, i)).collect());
                    }
                }
                Either::Left(done) => {
                    current.rows_affected = done.rows_affected();
                    results.push(std::mem::take(&mut current));
                }
            }
        }
        results
    }};
}

impl Database {
    /// Whether the session is inside a transaction. Postgres is asked
    /// directly (`now()` is frozen at transaction start); other engines
    /// return `None` and the caller tracks BEGIN/COMMIT itself.
    pub async fn transaction_state(&self) -> Option<TxnState> {
        let mut session = self.0.session.lock().await;
        let Session::Postgres(c) = &mut *session else { return None };
        // Simple protocol, so the "statement" is exactly this query.
        let probe = sqlx::raw_sql("SELECT now() <> statement_timestamp()").fetch_one(&mut **c).await;
        match probe {
            Ok(row) => Some(if pg_cell(&row, 0).as_deref() == Some("true") { TxnState::Open } else { TxnState::Idle }),
            Err(sqlx::Error::Database(e)) if e.code().as_deref() == Some("25P02") => Some(TxnState::Aborted),
            Err(_) => None,
        }
    }

    /// Runs one or more `;`-separated statements on the session connection
    /// and returns the result of the last statement that produced rows (or
    /// the last statement, if none did).
    /// Runs one read-only statement, returning its first `page` rows at once
    /// and, if there are more, a [`RowStream`] that fetches further pages on
    /// demand. While the stream is open it holds the session; dropping it
    /// cancels the query on the server.
    /// Like [`run`], for one read: the first `page` rows, and a stream of
    /// the rest. Reconnects and retries once if the connection had dropped.
    pub async fn run_streaming(&self, sql: &str, page: usize) -> Result<(QueryOutcome, Option<RowStream>)> {
        match self.run_streaming_once(sql, page).await {
            Err(e) if errors::is_broken(&e) => {
                let mut session = self.0.session.lock().await;
                if let Err(reopen) = self.reopen_session(&mut session).await {
                    return Err(e.context(format!("connection lost; couldn't reconnect ({reopen:#})")));
                }
                drop(session);
                let (mut outcome, stream) = self.run_streaming_once(sql, page).await?;
                outcome.reconnected = true;
                Ok((outcome, stream))
            }
            result => result,
        }
    }

    async fn run_streaming_once(&self, sql: &str, page: usize) -> Result<(QueryOutcome, Option<RowStream>)> {
        let start = Instant::now();
        let (first_tx, first_rx) = oneshot::channel();
        let (req_tx, req_rx) = mpsc::channel(4);
        let session = Arc::clone(&self.0.session);
        let db = self.clone();
        let sql_owned = sql.to_string();
        tokio::spawn(async move {
            let mut guard = session.lock_owned().await;
            db.clear_interrupt();
            match &mut *guard {
                Session::Postgres(c) => {
                    let stream = sqlx::raw_sql(AssertSqlSafe(sql_owned)).fetch(&mut **c);
                    serve_stream(stream, pg_cell, page, first_tx, req_rx, db).await;
                }
                Session::MySql(c) => {
                    let stream = sqlx::raw_sql(AssertSqlSafe(sql_owned)).fetch(&mut **c);
                    serve_stream(stream, mysql_cell, page, first_tx, req_rx, db).await;
                }
                Session::Sqlite(c) => {
                    let stream = sqlx::raw_sql(AssertSqlSafe(sql_owned)).fetch(&mut **c);
                    serve_stream(stream, sqlite_cell, page, first_tx, req_rx, db).await;
                }
                Session::Closed => {
                    let _ = first_tx.send(Err(anyhow::anyhow!("disconnected")));
                }
            }
        });
        let (columns, rows, done) = first_rx.await.map_err(|_| anyhow::anyhow!("query task ended"))??;
        let elapsed = start.elapsed();
        // No rows: recover the column names by preparing the statement.
        let columns = if let Some(c) = columns {
            c
        } else {
            let mut session = self.0.session.lock().await;
            self.clear_interrupt();
            session.describe_columns(sql).await.unwrap_or_default()
        };
        let (columns, column_types): (Vec<String>, Vec<String>) = columns.into_iter().unzip();
        let outcome = QueryOutcome {
            columns,
            column_types,
            total_rows: rows.len() as u64,
            rows,
            rows_affected: None,
            statements: 1,
            elapsed,
            reconnected: false,
            earlier: Vec::new(),
            statement: 1,
        };
        Ok((outcome, (!done).then_some(RowStream { tx: req_tx })))
    }

    pub async fn run(&self, sql: &str, row_limit: usize) -> Result<QueryOutcome> {
        self.run_retrying(sql, row_limit, true).await
    }

    /// Like [`run`], but a statement isn't run again after the connection
    /// dropped: for SQL inside a transaction opened by an earlier call, which
    /// the new session doesn't have (it would run outside it).
    pub async fn run_once(&self, sql: &str, row_limit: usize) -> Result<QueryOutcome> {
        self.run_retrying(sql, row_limit, false).await
    }

    async fn run_retrying(&self, sql: &str, row_limit: usize, retry_reads: bool) -> Result<QueryOutcome> {
        let start = Instant::now();
        let mut session = self.0.session.lock().await;
        self.clear_interrupt();
        let result = session.run(sql, row_limit).await;
        let mut reconnected = false;
        let elapsed = start.elapsed();

        let mut results = match result {
            Ok(results) => results,
            Err(e) if errors::is_broken(&e) => {
                if let Err(reopen) = self.reopen_session(&mut session).await {
                    return Err(e.context(format!("connection lost; couldn't reconnect ({reopen:#})")));
                }
                // Reads are safe to run again; a write may or may not have
                // happened, so it's left to the user.
                let reads_only = crate::analyze::analyze(&crate::parse::parse(sql, self.0.engine))
                    .iter()
                    .all(|i| i.kind == crate::analyze::Kind::Read);
                if !reads_only || !retry_reads {
                    return Err(e.context(errors::Reconnected));
                }
                reconnected = true;
                session.run(sql, row_limit).await?
            }
            Err(e) => return Err(e),
        };

        let statements = results.len();
        // sqlx exposes no column names when a query returns zero rows, so
        // prepare the statement to recover them (e.g. an empty SELECT): the
        // last one, and earlier reads, which get their own result sets.
        let parts = sql::split_statements(sql, self.0.engine);
        if parts.len() == statements {
            for (i, result) in results.iter_mut().enumerate() {
                let read = || {
                    crate::analyze::analyze(&crate::parse::parse(parts[i], self.0.engine))
                        .iter()
                        .all(|s| s.kind == crate::analyze::Kind::Read)
                };
                if result.columns.is_none() && (i + 1 == statements || read()) {
                    result.columns = session.describe_columns(parts[i]).await;
                }
            }
        }
        let has_rows = |r: &StatementResult| r.columns.as_ref().is_some_and(|c| !c.is_empty());
        let Some(shown) = results.iter().rposition(has_rows).or(statements.checked_sub(1)) else {
            return Ok(QueryOutcome { elapsed, reconnected, ..QueryOutcome::default() });
        };
        let mut earlier = Vec::new();
        for (i, result) in results.drain(..=shown).enumerate() {
            let outcome = result.into_outcome(i + 1, statements, elapsed, reconnected);
            if i == shown {
                return Ok(QueryOutcome { earlier, ..outcome });
            }
            if !outcome.columns.is_empty() {
                earlier.push(outcome);
            }
        }
        unreachable!("the shown statement is among the drained results")
    }

    /// After a run that changed the session's settings (`USE`, `SET`…).
    pub fn mark_session_changed(&self) {
        self.0.session_changed.store(true, Ordering::SeqCst);
    }

    /// Runs `sql` on a pool connection of its own while the session is busy
    /// (a plan while a query runs). `None` where that would differ from the
    /// session: SQLite (an in-memory database is the session's alone), or
    /// once the session's settings were changed. The caller leaves out open
    /// transactions, whose changes other connections don't see.
    pub async fn run_aside(&self, sql: &str, row_limit: usize) -> Result<Option<QueryOutcome>> {
        if self.0.session_changed.load(Ordering::SeqCst) {
            return Ok(None);
        }
        let start = Instant::now();
        let mut aside = match &self.0.pool {
            Pool::Postgres(p) => Session::Postgres(p.acquire().await?),
            Pool::MySql(p) => Session::MySql(p.acquire().await?),
            Pool::Sqlite(_) => return Ok(None),
        };
        let mut results = aside.run(sql, row_limit).await?;
        let statements = results.len();
        let Some(last) = results.pop() else { return Ok(Some(QueryOutcome::default())) };
        Ok(Some(last.into_outcome(statements, statements, start.elapsed(), false)))
    }

    pub(super) async fn reopen_session(&self, session: &mut Session) -> Result<()> {
        // A new session has the connection's own settings again.
        self.0.session_changed.store(false, Ordering::SeqCst);
        *session = match &self.0.pool {
            Pool::Postgres(p) => Session::Postgres(p.acquire().await?),
            Pool::MySql(p) => Session::MySql(p.acquire().await?),
            Pool::Sqlite(p) => Session::Sqlite(p.acquire().await?),
        };
        session.watch_interrupt(&self.0.interrupt).await?;
        self.0.backend_id.store(session.backend_id().await?, Ordering::SeqCst);
        Ok(())
    }

    /// Prepares one statement on the session without running it, so the
    /// server's own parser (and catalog) checks it. Waits briefly for the
    /// session (e.g. a catalog load); errors with "busy" if it stays in use.
    pub async fn check_statement(&self, sql: &str) -> Result<CheckOutcome> {
        let mut session = tokio::time::timeout(Duration::from_millis(500), self.0.session.lock())
            .await
            .map_err(|_| anyhow::anyhow!("session busy"))?;
        self.clear_interrupt();
        let sql = AssertSqlSafe(sql.to_string()).into_sql_str();
        let prepared = match &mut *session {
            Session::Postgres(c) => (&mut **c).prepare(sql).await.map(drop),
            Session::MySql(c) => (&mut **c).prepare(sql).await.map(drop),
            Session::Sqlite(c) => (&mut **c).prepare(sql).await.map(drop),
            Session::Closed => anyhow::bail!("disconnected"),
        };
        let err = match prepared {
            Ok(()) => return Ok(CheckOutcome::Ok),
            Err(e) => e,
        };
        let Some(db) = err.as_database_error() else {
            // I/O or protocol trouble: not something to show as a hint.
            return Ok(CheckOutcome::Unsupported);
        };
        let problem = match self.0.engine {
            Engine::Postgres => postgres::queries::prepare_problem(db),
            Engine::MySql => mysql::queries::prepare_problem(db),
            Engine::Sqlite => sqlite::queries::prepare_problem(db),
        };
        let Some(syntax) = problem else { return Ok(CheckOutcome::Unsupported) };
        let err = anyhow::Error::from(err);
        let message = error_message(&err);
        let position = error_position(&err).or_else(|| sqlite_near(&message));
        Ok(CheckOutcome::Problem { message, position, syntax })
    }

    /// Runs `sql`, cancelling it once `time_limit` passes. A cancelled query
    /// is let finish (with its error) first, so the session is free again.
    /// With `retry` false, a read isn't run again after a reconnect (see
    /// [`run_once`]).
    pub async fn run_within(
        &self,
        sql: &str,
        row_limit: usize,
        time_limit: Option<Duration>,
        retry: bool,
    ) -> Result<QueryOutcome, RunError> {
        let run = self.run_retrying(sql, row_limit, retry);
        tokio::pin!(run);
        let Some(limit) = time_limit else { return run.await.map_err(RunError::Failed) };
        tokio::select! {
            result = &mut run => result.map_err(RunError::Failed),
            () = tokio::time::sleep(limit) => {
                let _ = self.cancel().await;
                let _ = run.await;
                Err(RunError::TimedOut(limit))
            }
        }
    }

    /// Cancels whatever the session connection is running: the server is
    /// asked over a separate pool connection, since the session one is busy;
    /// SQLite's progress handler sees the interrupt flag.
    pub async fn cancel(&self) -> Result<()> {
        let id = self.0.backend_id.load(Ordering::SeqCst);
        match &self.0.pool {
            Pool::Postgres(p) => postgres::queries::cancel(p, id).await?,
            Pool::MySql(p) => mysql::queries::cancel(p, id).await?,
            Pool::Sqlite(_) => self.0.interrupt.store(true, Ordering::SeqCst),
        }
        Ok(())
    }

    pub async fn close(&self) {
        // Hand the session connection back first: the pool's close() waits
        // for every checked-out connection.
        *self.0.session.lock().await = Session::Closed;
        match &self.0.pool {
            Pool::Postgres(p) => p.close().await,
            Pool::MySql(p) => p.close().await,
            Pool::Sqlite(p) => p.close().await,
        }
    }
}

/// Why [`Database::run_within`] gave no result.
#[derive(Debug)]
pub enum RunError {
    /// The time limit ran out, and the query was cancelled.
    TimedOut(Duration),
    Failed(anyhow::Error),
}

impl StatementResult {
    fn into_outcome(self, statement: usize, statements: usize, elapsed: Duration, reconnected: bool) -> QueryOutcome {
        let (columns, column_types): (Vec<String>, Vec<String>) = self.columns.unwrap_or_default().into_iter().unzip();
        let rows_affected = columns.is_empty().then_some(self.rows_affected);
        QueryOutcome {
            columns,
            column_types,
            rows: self.rows,
            total_rows: self.total_rows,
            rows_affected,
            statements,
            elapsed,
            reconnected,
            earlier: Vec::new(),
            statement,
        }
    }
}

/// How many SQLite virtual machine steps go between checks for a cancel:
/// often enough to stop within milliseconds, rarely enough to cost nothing.
const INTERRUPT_CHECK_STEPS: i32 = 10_000;

impl Session {
    /// SQLite: stops the running statement once `interrupt` is set. The
    /// handler stays with the connection.
    pub(super) async fn watch_interrupt(&mut self, interrupt: &Arc<AtomicBool>) -> Result<()> {
        if let Session::Sqlite(c) = self {
            let interrupt = Arc::clone(interrupt);
            c.lock_handle()
                .await?
                .set_progress_handler(INTERRUPT_CHECK_STEPS, move || !interrupt.load(Ordering::SeqCst));
        }
        Ok(())
    }

    pub(super) async fn server_version(&mut self) -> String {
        let sql = match self {
            Session::Postgres(_) => Engine::Postgres.dialect().version_query(),
            Session::MySql(_) => Engine::MySql.dialect().version_query(),
            Session::Sqlite(_) => Engine::Sqlite.dialect().version_query(),
            Session::Closed => return String::new(),
        };
        let version: Result<String, _> = match self {
            Session::Postgres(c) => sqlx::query_scalar(sql).fetch_one(&mut **c).await,
            Session::MySql(c) => sqlx::query_scalar(sql).fetch_one(&mut **c).await,
            Session::Sqlite(c) => sqlx::query_scalar(sql).fetch_one(&mut **c).await,
            Session::Closed => return String::new(),
        };
        // "16.2 (Debian 16.2-1)" → "16.2"
        version.map(|v| v.split_whitespace().next().unwrap_or_default().to_string()).unwrap_or_default()
    }

    /// The session's `sql_mode`, for engines that have one.
    pub(super) async fn sql_mode(&mut self) -> Option<String> {
        match self {
            Session::MySql(c) => {
                let sql = Engine::MySql.dialect().sql_mode_query()?;
                sqlx::query_scalar(sql).fetch_one(&mut **c).await.ok()
            }
            _ => None,
        }
    }

    pub(super) async fn backend_id(&mut self) -> Result<i64> {
        match self {
            Session::Postgres(c) => postgres::connect::backend_id(c).await,
            Session::MySql(c) => mysql::connect::backend_id(c).await,
            Session::Sqlite(_) | Session::Closed => Ok(0),
        }
    }

    pub(super) async fn run(&mut self, sql: &str, limit: usize) -> Result<Vec<StatementResult>> {
        let sql = sql.to_string();
        Ok(match self {
            Session::Closed => anyhow::bail!("disconnected"),
            Session::Postgres(c) => collect_statements!(&mut **c, sql, limit, pg_cell),
            Session::MySql(c) => collect_statements!(&mut **c, sql, limit, mysql_cell),
            Session::Sqlite(c) => collect_statements!(&mut **c, sql, limit, sqlite_cell),
        })
    }

    /// Column names and types of a single statement, via a server-side
    /// prepare. `None` if it can't be prepared (e.g. some utility commands).
    pub(super) async fn describe_columns(&mut self, sql: &str) -> Option<Vec<(String, String)>> {
        let sql = AssertSqlSafe(sql.to_string()).into_sql_str();
        let stmt_columns: Vec<(String, String)> = match self {
            Session::Closed => return None,
            Session::Postgres(c) => {
                let stmt = (&mut **c).prepare(sql).await.ok()?;
                stmt.columns().iter().map(|c| (c.name().to_string(), c.type_info().name().to_string())).collect()
            }
            Session::MySql(c) => {
                let stmt = (&mut **c).prepare(sql).await.ok()?;
                stmt.columns().iter().map(|c| (c.name().to_string(), c.type_info().name().to_string())).collect()
            }
            Session::Sqlite(c) => {
                let stmt = (&mut **c).prepare(sql).await.ok()?;
                stmt.columns().iter().map(|c| (c.name().to_string(), c.type_info().name().to_string())).collect()
            }
        };
        Some(stmt_columns)
    }
}
