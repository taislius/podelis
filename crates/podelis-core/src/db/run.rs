use super::*;

/// How long a check before a run waits for the session to be free before
/// it's taken for one stuck (see [`Database::answers_within`]).
const SESSION_HELD: Duration = Duration::from_secs(30);

/// Reads up to `n` more rows; `true` once the rows have run out.
async fn read_rows(
    stream: &mut Rows<'_>,
    n: usize,
    columns: &mut Option<Vec<(String, String)>>,
    out: &mut Vec<Vec<Option<String>>>,
) -> Result<bool> {
    while out.len() < n {
        let Some(row) = stream.try_next().await? else { return Ok(true) };
        if columns.is_none() {
            *columns = row.columns;
        }
        out.push(row.cells);
    }
    Ok(false)
}

/// Reads up to `n` more rows, marked as reading while it does (see
/// `Handle::reading`).
async fn read_page(
    db: &Database,
    stream: &mut Rows<'_>,
    n: usize,
    columns: &mut Option<Vec<(String, String)>>,
    out: &mut Vec<Vec<Option<String>>>,
) -> Result<bool> {
    db.0.supervisor.set_reading(true);
    let done = read_rows(stream, n, columns, out).await;
    db.0.supervisor.set_reading(false);
    done
}

/// The streaming task for one connection: reads the first page (returned,
/// for the caller to send), then serves page requests until the rows run
/// out or every [`RowStream`] is dropped, in which case the query is
/// cancelled on the server. A page whose link broke sets `broken`.
async fn serve_stream(
    mut stream: Rows<'_>,
    page: usize,
    first_tx: oneshot::Sender<Result<FirstPage>>,
    mut req_rx: mpsc::Receiver<PageRequest>,
    broken: &AtomicBool,
    db: &Database,
) -> Option<(anyhow::Error, oneshot::Sender<Result<FirstPage>>)> {
    let mut columns = None;
    let mut rows = Vec::new();
    let done = match read_page(db, &mut stream, page, &mut columns, &mut rows).await {
        Ok(done) => done,
        Err(e) => return Some((e, first_tx)),
    };
    let _ = first_tx.send(Ok((columns.clone(), rows, done)));
    if done {
        return None;
    }
    while let Some((n, reply)) = req_rx.recv().await {
        let mut rows = Vec::new();
        match read_page(db, &mut stream, n, &mut columns, &mut rows).await {
            Ok(done) => {
                let _ = reply.send(Ok(Page { rows, done }));
                if done {
                    return None;
                }
            }
            Err(e) => {
                broken.store(errors::is_broken(&e), Ordering::SeqCst);
                let _ = reply.send(Err(format!("{e:#}")));
                return None;
            }
        }
    }
    // Every handle dropped with rows left: stop the query rather than
    // draining the rest over the network (on the link as reading is: a
    // dead one holds it).
    db.0.supervisor.set_reading(true);
    let _ = db.cancel().await;
    db.0.supervisor.set_reading(false);
    None
}

/// A run not sent: the session had been given up on, and a new one is
/// open (without what the old one had).
fn not_sent() -> anyhow::Error {
    anyhow::anyhow!("the connection had dropped, so this wasn't sent; a new session is open: run it again")
        .context(errors::Reconnected)
}

/// A run whose link broke while it ran: whether it ran isn't known (a
/// write may have happened, a COMMIT may have committed). A new session is
/// open; it's the user's to check, and run it again.
fn outcome_unknown(e: anyhow::Error) -> anyhow::Error {
    e.context("the connection dropped while it ran, so whether it ran isn't known (check before running it again)")
        .context(errors::Reconnected)
}

/// Whether `word` (lowercase) starts a statement or a line in `sql`, as a
/// word of its own, in any case: at the start, after a `;`, or after a line
/// break (spaces before it). A word inside a statement's values doesn't.
fn starts_a_line(sql: &str, word: &str) -> bool {
    let (text, word) = (sql.as_bytes(), word.as_bytes());
    let part = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    (0..text.len().saturating_sub(word.len() - 1)).any(|at| {
        text[at..at + word.len()].eq_ignore_ascii_case(word)
            && text.get(at + word.len()).is_none_or(|&b| !part(b))
            && text[..at]
                .iter()
                .rev()
                .find(|b| !matches!(b, b' ' | b'\t'))
                .is_none_or(|&b| matches!(b, b';' | b'\n' | b'\r' | b'('))
    })
}

/// Of a failed run's `ran` (results counted as it ran), the statements
/// sure to have run: a `CALL` can give several results, so from the first
/// one, which ran isn't known.
fn surely_ran(statements: &[&str], engine: Engine, ran: usize) -> usize {
    let call = |s: &&str| session_memory::leading_words(s, engine, 1).first().is_some_and(|w| w == "call");
    statements.iter().position(call).map_or(ran, |first_call| ran.min(first_call))
}

impl Database {
    /// Whether the session was reopened since whoever runs was last told (a
    /// check did it): once, as they're told now (by the run's error).
    fn reconnect_untold(&self) -> bool {
        let now = self.health().borrow().reconnects;
        self.0.told_reconnects.fetch_max(now, Ordering::SeqCst) < now
    }

    /// The app has said the connection's `reconnects`: runs after go on.
    pub fn reconnect_told(&self, reconnects: u64) {
        self.0.told_reconnects.fetch_max(reconnects, Ordering::SeqCst);
    }
}

/// What came of a ping.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Answer {
    Yes,
    /// The link broke at once.
    Broke,
    /// Nothing in time.
    Silent,
}

impl Database {
    /// Runs `statement`, a `COPY … FROM STDIN`, on the session and sends it
    /// `chunks` of data in `COPY`'s format, on an engine that has it
    /// (`Capabilities::copy_in`). A chunk that fails aborts the copy.
    /// Returns how many rows were copied. The data's first row is line
    /// `first_line` of its file, so an error can name the line.
    pub async fn copy_in(
        &self,
        statement: &str,
        first_line: u64,
        chunks: impl Iterator<Item = Result<Vec<u8>>>,
    ) -> Result<u64> {
        on_driver!(self.0.engine, engine::queries::copy_in(self, statement, first_line, chunks).await)
    }

    /// Whether the session is inside a transaction, where its engine can
    /// ask (PostgreSQL); `None` elsewhere, and the caller tracks
    /// BEGIN/COMMIT itself.
    pub async fn transaction_state(&self) -> Option<TxnState> {
        self.with_session(|session| {
            Box::pin(
                async move { on_session!(session, |c| engine::queries::transaction_state(c).await, closed => None) },
            )
        })
        .await
        .ok()
        .flatten()
    }

    /// Runs one read-only statement, returning its first `page` rows at once
    /// and, if there are more, a [`RowStream`] that fetches further pages on
    /// demand. While the stream is open it holds the session; dropping it
    /// cancels the query on the server. Never run again on a new session
    /// (see [`run`]).
    pub async fn run_streaming(&self, sql: &str, page: usize) -> Result<(QueryOutcome, Option<RowStream>)> {
        // A session given up on: a new one, and this isn't sent.
        let db = self.clone();
        // Nor on a new session a check opened that nobody was told of.
        let renewed = self
            .with_session(move |session| {
                Box::pin(
                    async move { Ok::<_, anyhow::Error>(db.renew_if_stale(session).await? | db.reconnect_untold()) },
                )
            })
            .await??;
        if renewed {
            return Err(not_sent());
        }
        // A link that broke is reopened by the stream's own work.
        self.run_streaming_once(sql, page).await
    }

    pub(in crate::db) async fn run_streaming_once(
        &self,
        sql: &str,
        page: usize,
    ) -> Result<(QueryOutcome, Option<RowStream>)> {
        let start = Instant::now();
        let (first_tx, first_rx) = oneshot::channel();
        let (req_tx, req_rx) = mpsc::channel(4);
        let broken = Arc::new(AtomicBool::new(false));
        let broke = Arc::clone(&broken);
        let db = self.clone();
        let sql_owned = sql.to_string();
        // It holds the session while its rows are read: given up on with
        // the session.
        self.stream_on_session(move |session| {
            Box::pin(async move {
                // Its caller gone while it waited for its turn: nothing's
                // sent (some engines send it as soon as it's asked for).
                if first_tx.is_closed() {
                    db.0.supervisor.skipped();
                    return;
                }
                db.clear_interrupt();
                let first_failed = on_session!(
                    session,
                    |c| serve_stream(engine::driver::fetch(c, sql_owned), page, first_tx, req_rx, &broke, &db).await,
                    closed => {
                        let _ = first_tx.send(Err(anyhow::anyhow!("disconnected")));
                        None
                    }
                );
                // The link broke: a new session at once, in this same turn
                // (nothing else gets the broken one meanwhile), and said
                // (reconnected, or lost). The rows aren't read again.
                let first_broke = first_failed.as_ref().is_some_and(|(e, _)| errors::is_broken(e));
                let reopened = if first_broke || broke.load(Ordering::SeqCst) {
                    Some(db.reopen_session(session).await)
                } else {
                    None
                };
                // Its caller's told (the page that broke, the first page's
                // error): the next run goes on.
                if matches!(reopened, Some(Ok(()))) {
                    db.reconnect_untold();
                }
                if let Some((e, first_tx)) = first_failed {
                    let e = match reopened {
                        Some(Ok(())) => outcome_unknown(e),
                        Some(Err(reopen)) => e.context(format!("connection lost; couldn't reconnect ({reopen:#})")),
                        None => e,
                    };
                    let _ = first_tx.send(Err(e));
                }
            })
        })
        .await?;
        let (columns, rows, done) = first_rx.await.map_err(|_| anyhow::anyhow!("query task ended"))??;
        self.report_live();
        let elapsed = start.elapsed();
        // No rows: recover the column names by preparing the statement.
        let columns = if let Some(c) = columns {
            c
        } else {
            let db = self.clone();
            let sql = sql.to_string();
            self.with_session(move |session| {
                Box::pin(async move {
                    db.clear_interrupt();
                    session.describe_columns(&sql).await.unwrap_or_default()
                })
            })
            .await?
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
            earlier: Vec::new(),
            statement: 1,
        };
        Ok((outcome, (!done).then_some(RowStream { tx: req_tx, broken })))
    }

    /// Runs one or more `;`-separated statements on the session, and returns
    /// the result of the last that gave rows (or the last, if none did).
    /// Never run again on a new session: after a
    /// reconnect it fails, saying it wasn't sent (the session had been given
    /// up on), or that whether it ran isn't known (the link broke while it
    /// ran). A read too: one can write (`nextval()`, a function), and the
    /// new session may have another database, search path or role.
    pub async fn run(&self, sql: &str, row_limit: usize) -> Result<QueryOutcome> {
        let start = Instant::now();
        let db = self.clone();
        let sql = sql.to_string();
        self.with_session(move |session| Box::pin(async move { db.run_on(session, &sql, row_limit, start).await }))
            .await?
    }

    /// `run`'s work, on the session.
    pub(super) async fn run_on(
        &self,
        session: &mut Session,
        sql: &str,
        row_limit: usize,
        start: Instant,
    ) -> Result<QueryOutcome> {
        // A session given up on: a new one, and this isn't sent. Nor on one
        // a check opened that nobody was told of yet.
        if self.renew_if_stale(session).await? | self.reconnect_untold() {
            return Err(not_sent());
        }
        self.clear_interrupt();
        let mut ran = 0;
        let result = session.run_counted(sql, row_limit, &mut ran).await;
        let elapsed = start.elapsed();
        // What the statements before a failing one changed on the session
        // stands: remembered (and so said gone if the session's reopened).
        // (Read against the transaction as it was before the run: noted as
        // it is after only then.)
        self.read_ran(sql, result.as_ref().err().map(|_| ran));

        let mut results = match result {
            Ok(results) => results,
            Err(e) if errors::is_broken(&e) => {
                if let Err(reopen) = self.reopen_session(session).await {
                    return Err(e.context(format!("connection lost; couldn't reconnect ({reopen:#})")));
                }
                self.reconnect_untold();
                return Err(outcome_unknown(e));
            }
            // ProxySQL tied the session to one group of servers, and won't
            // let it reach another for as long as it lives: every read its
            // rules route elsewhere fails from now on. A new session for the
            // next run; the tied one goes out of the pool, not back into it.
            Err(e) if errors::is_proxysql_locked(&e) => {
                return Err(match self.reopen_session(session).await {
                    // Said as a reconnect: the old session's transaction is
                    // gone.
                    Ok(()) => {
                        self.reconnect_untold();
                        e.context(errors::Reconnected)
                    }
                    // Not one: the next run tries again (`open_session` left
                    // it given up on).
                    Err(reopen) => e.context(format!("connection lost; couldn't reconnect ({reopen:#})")),
                });
            }
            Err(e) => return Err(e),
        };

        self.report_live();
        let statements = results.len();
        // A driver may give no column names when a query returns no rows, so
        // prepare the statement to recover them (e.g. an empty SELECT): the
        // last one, and earlier reads, which get their own result sets.
        let parts = crate::language::split_statements(sql, self.0.engine);
        if parts.len() == statements {
            for (i, result) in results.iter_mut().enumerate() {
                let read = || {
                    crate::language::statements::analyze(&crate::sql::parse::parse(parts[i], self.0.engine))
                        .iter()
                        .all(|s| s.kind == crate::language::statements::Kind::Read)
                };
                if result.columns.is_none() && (i + 1 == statements || read()) {
                    result.columns = session.describe_columns(parts[i]).await;
                }
            }
        }
        let has_rows = |r: &StatementResult| r.columns.as_ref().is_some_and(|c| !c.is_empty());
        let Some(shown) = results.iter().rposition(has_rows).or(statements.checked_sub(1)) else {
            return Ok(QueryOutcome { elapsed, ..QueryOutcome::default() });
        };
        let mut earlier = Vec::new();
        for (i, result) in results.drain(..=shown).enumerate() {
            let outcome = result.into_outcome(i + 1, statements, elapsed);
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
        // A file's connection: an in-memory database is the session's alone.
        if self.0.engine.capabilities().file_based {
            return Ok(None);
        }
        let start = Instant::now();
        let mut aside = on_pool!(&self.0.pool, |p| Session::from(engine::driver::acquire(p).await?));
        let mut results = aside.run(sql, row_limit).await?;
        let statements = results.len();
        let Some(last) = results.pop() else { return Ok(Some(QueryOutcome::default())) };
        Ok(Some(last.into_outcome(statements, statements, start.elapsed())))
    }

    /// A new session for the one lost: reported either way (reconnected,
    /// with what the old one had that's gone; or lost and why).
    pub(super) async fn reopen_session(&self, session: &mut Session) -> Result<()> {
        let reopened = self.open_session(session).await;
        match &reopened {
            Ok(()) => {
                // Nothing of the old session's is run again: what it had is
                // said, as gone, and forgotten.
                let lost = self.0.memory.lock().map(|mut m| std::mem::take(&mut *m).lost()).unwrap_or_default();
                // Said before anything else is waited for: work stopped part
                // way after this still leaves the new session said (its
                // transaction gone with the old).
                self.0.supervisor.set_in_transaction(false);
                self.report_reconnected(lost);
                // The new session's own `sql_mode` (the old one's may have
                // been set): read before anything runs on it.
                if let Some(mode) = session.sql_mode().await
                    && let Ok(mut current) = self.0.sql_mode.lock()
                {
                    *current = mode;
                }
            }
            Err(e) => self.report_lost(&error_message(e)),
        }
        reopened
    }

    /// Notes what `sql`'s statements, run without error, changed on the
    /// session (each engine knows which do).
    ///
    /// Where a `ROLLBACK` undoes them (PostgreSQL), changes made in a
    /// transaction are kept only once it commits in the same run; left
    /// open, or begun in an earlier run, whether they stay isn't known, and
    /// they're not kept.
    ///
    /// `ran`: only its first statements ran (the next one failed).
    fn remember(&self, statements: Vec<&str>, failed: bool) {
        let engine = self.0.engine;
        let undone = on_driver!(engine, engine::session::UNDONE_BY_ROLLBACK);
        let statements = if failed && on_driver!(engine, engine::session::FAILED_RUN_UNDONE) {
            session_memory::before_last_end(&statements, engine)
        } else {
            statements
        };
        let settled = session_memory::settled(&statements, engine, self.in_transaction(), undone);
        let Ok(mut memory) = self.0.memory.lock() else { return };
        for statement in settled {
            on_driver!(engine, engine::session::remember(statement, &mut memory));
        }
    }

    /// What `sql`'s statements that ran (`ran`: only its first, the next
    /// failed) did to the session: what they changed on it (`remember`),
    /// then whether a transaction's open after them (`note_ran_transaction`),
    /// read against the transaction as it was before. Split once, and only
    /// if a word that could matter is in it (a long INSERT script isn't).
    fn read_ran(&self, sql: &str, ran: Option<usize>) {
        let engine = self.0.engine;
        // In SQL, a word where a statement can start (`use`, not in `user`,
        // nor in an INSERT's values); MongoDB's calls are words run
        // together (`startTransaction`).
        let sql_words = engine.dialect().is_some();
        let lower = if sql_words { String::new() } else { sql.to_ascii_lowercase() };
        let has = |word: &&str| if sql_words { starts_a_line(sql, word) } else { lower.contains(word) };
        let remembers = on_driver!(engine, engine::session::KEEPS)
            && on_driver!(engine, engine::session::CHANGED_BY).iter().any(has);
        let open = self.in_transaction();
        let ddl_commits = engine.capabilities().ddl_commits;
        let notes = if open {
            ["commit", "rollback", "abort", "end"]
                .iter()
                .chain(if ddl_commits { session_memory::COMMITTING } else { &[] })
                .any(has)
        } else {
            ["begin", "start"].iter().any(has)
        } || on_driver!(engine, engine::session::TRANSACTION_WORDS).iter().any(has);
        if !remembers && !notes {
            return;
        }
        let mut statements = crate::language::split_statements(sql, engine);
        if let Some(ran) = ran {
            statements.truncate(surely_ran(&statements, engine, ran));
        }
        if remembers {
            self.remember(statements.clone(), ran.is_some());
        }
        if notes {
            self.note_ran_transaction(&statements, open, ddl_commits);
        }
    }

    /// Notes whether a transaction is open after `statements` ran (`open`
    /// before them), before anything else gets the session: a check right
    /// after a `BEGIN` doesn't take the reconnecting way before the app's
    /// word on it arrives (`note_transaction`, which stands).
    fn note_ran_transaction(&self, statements: &[&str], open: bool, ddl_commits: bool) {
        let engine = self.0.engine;
        let after = if engine.dialect().is_some() {
            // Each engine's own way first (MySQL's autocommit, SQLite's
            // SAVEPOINT), then SQL's.
            statements.iter().fold(open, |open, s| {
                on_driver!(engine, engine::session::transaction_of(s))
                    .unwrap_or_else(|| session_memory::transaction_after(&[s], engine, open, ddl_commits))
            })
        } else {
            // No SQL (MongoDB): its own calls, as its language reads them.
            use crate::language::statements::{Kind, TxnOp, analyze};
            statements.iter().flat_map(|s| analyze(&crate::sql::parse::parse(s, engine))).fold(open, |open, info| {
                match info.kind {
                    Kind::Transaction(TxnOp::Begin) => true,
                    Kind::Transaction(TxnOp::Commit | TxnOp::Rollback) => false,
                    _ => open,
                }
            })
        };
        self.0.supervisor.set_in_transaction(after);
    }

    async fn open_session(&self, session: &mut Session) -> Result<()> {
        // The old one out first (out of the pool, not back into it: its
        // state is unknown; a pool of one has room for the new one then).
        // The new one's set up before it's the session: one that fails (or
        // whose caller gives up) part way is never used, and the session
        // stays given up on until a new one's in.
        self.0.stale.store(true, Ordering::SeqCst);
        self.check_reach().await?;
        on_session!(std::mem::replace(session, Session::Closed), |c| engine::driver::detach(c), closed => {});
        let mut new = on_pool!(&self.0.pool, |p| Session::from(engine::driver::acquire(p).await?));
        new.watch_interrupt(&self.0.interrupt).await?;
        let backend_id = new.backend_id().await?;
        *session = new;
        self.0.backend_id.store(backend_id, Ordering::SeqCst);
        // A new session has the connection's own settings again.
        self.0.session_changed.store(false, Ordering::SeqCst);
        self.0.stale.store(false, Ordering::SeqCst);
        Ok(())
    }

    /// Prepares one statement on the session without running it, so the
    /// server's own parser (and catalog) checks it. Waits briefly for the
    /// session (e.g. a catalog load); errors with "busy" if it stays in use.
    pub async fn check_statement(&self, sql: &str) -> Result<CheckOutcome> {
        let db = self.clone();
        let statement = sql.to_string();
        // Its turn not waited for long (given up on before it, it isn't run).
        let prepare = self.with_session_within(Duration::from_millis(500), move |session| {
            Box::pin(async move {
                db.clear_interrupt();
                on_session!(session, |c| engine::driver::prepare(c, &statement).await, closed => Err(anyhow::anyhow!("disconnected")))
            })
        });
        let Some(prepared) = prepare.await? else { anyhow::bail!("session busy") };
        let err = match prepared {
            Ok(()) => return Ok(CheckOutcome::Ok),
            Err(e) => e,
        };
        // Not the server's (I/O or protocol trouble), or not worth a hint.
        let Some(server) = errors::server_error(&err) else { return Ok(CheckOutcome::Unsupported) };
        let Some(syntax) = server.check else { return Ok(CheckOutcome::Unsupported) };
        let missing_feature = server.missing_feature;
        let message = error_message(&err);
        let position = server.position.or_else(|| self.0.engine.backend().position_in_message(&message));
        Ok(CheckOutcome::Problem { message, position, syntax, missing_feature })
    }

    /// Runs `sql`, cancelling it once `time_limit` passes. The time counts
    /// from when it has the session, not while it waits for its turn: the
    /// cancel is then for it, never for the work before it. A cancelled
    /// query is let finish (with its error) first, so the session is free
    /// again.
    pub async fn run_within(
        &self,
        sql: &str,
        row_limit: usize,
        time_limit: Option<Duration>,
    ) -> Result<QueryOutcome, RunError> {
        let Some(limit) = time_limit else { return self.run(sql, row_limit).await.map_err(RunError::Failed) };
        // Started once the statement's about to be sent: a session given up
        // on is reopened first, outside the time (a slow reopen doesn't use
        // it up, nor is the cancel for the new session's first moment), and
        // the run isn't sent on it. (Not `with_session_within`, whose turn
        // starts with the work.)
        let (started, has_started) = oneshot::channel::<()>();
        let db = self.clone();
        let sql = sql.to_string();
        let run = self.with_session(move |session| {
            Box::pin(async move {
                if db.renew_if_stale(session).await? | db.reconnect_untold() {
                    return Err(not_sent());
                }
                let _ = started.send(());
                db.run_on(session, &sql, row_limit, Instant::now()).await
            })
        });
        tokio::pin!(run);
        let timer = async {
            if has_started.await.is_err() {
                std::future::pending::<()>().await;
            }
            tokio::time::sleep(limit).await;
        };
        tokio::select! {
            biased;
            result = &mut run => result.and_then(|r| r).map_err(RunError::Failed),
            () = timer => {
                let _ = self.cancel().await;
                let _ = run.await;
                Err(RunError::TimedOut(limit))
            }
        }
    }

    /// The supervisor's check: the engine's ping, on a session given up on
    /// renewed first, and on one whose link broke, once more on a new one
    /// (Podėlis's own statement: run again, unlike the user's). Whether the
    /// session was reopened.
    pub(super) async fn ping_on(&self, session: &mut Session) -> Result<bool> {
        let renewed = self.renew_if_stale(session).await?;
        let ping = self.0.engine.backend().ping();
        match session.run(ping, 1).await {
            Ok(_) => {
                if !renewed {
                    self.report_live();
                }
                Ok(renewed)
            }
            Err(e) if errors::is_broken(&e) => {
                if let Err(reopen) = self.reopen_session(session).await {
                    return Err(e.context(format!("connection lost; couldn't reconnect ({reopen:#})")));
                }
                session.run(ping, 1).await?;
                Ok(true)
            }
            Err(e) => Err(e),
        }
    }

    /// Whether the server answers within `limit`, before a run after a
    /// quiet while: a link that died meanwhile (a VPN that dropped) would
    /// otherwise leave the run waiting for minutes. SQLite always answers.
    ///
    /// A protocol ping, not a statement: it can't fail in an aborted
    /// transaction or replace MySQL's warnings and row counts. An answer (an
    /// error too) lets the run go on; a link that broke at once gives the
    /// session up (the run then opens a new one, and isn't sent on it);
    /// only silence is "no answer". Nothing is reopened or retried here (an
    /// open transaction would be lost without a word). The time counts
    /// from when the session is free: one busy elsewhere (a schema load,
    /// an earlier result's rows being stopped) isn't a dead one, and the
    /// run waits its turn; but not for ever: a session held for
    /// `SESSION_HELD` is one stuck on a link that's gone (an earlier
    /// result's rows, stopped on a dead link, kept it while the check let
    /// the run go on to wait for good).
    pub async fn answers_within(&self, limit: Duration) -> Result<()> {
        // A file always answers.
        if self.0.engine.capabilities().file_based {
            return Ok(());
        }
        // The ping, in the session's turn: no answer within `limit` gives
        // the session up (its connection is mid-call in a state nobody
        // knows).
        let db = self.clone();
        let check = self.with_session(move |session| {
            Box::pin(async move {
                if db.0.stale.load(Ordering::SeqCst) {
                    return true;
                }
                match session.answer_within(limit).await {
                    Answer::Yes => {
                        db.report_live();
                        true
                    }
                    // Broken at once (a restart, a reset): given up on, so
                    // the run opens a new session (and says it wasn't sent).
                    Answer::Broke => {
                        db.0.stale.store(true, Ordering::SeqCst);
                        db.report_lost("the connection broke");
                        true
                    }
                    Answer::Silent => {
                        db.0.stale.store(true, Ordering::SeqCst);
                        db.report_lost(&format!("no answer in {} s", limit.as_secs()));
                        false
                    }
                }
            })
        });
        match tokio::time::timeout(SESSION_HELD.max(limit) + limit, check).await {
            Ok(Ok(true)) => Ok(()),
            Ok(Ok(false)) => Err(anyhow::anyhow!("no answer in {} s", limit.as_secs()).context(errors::NoAnswer)),
            Ok(Err(e)) => Err(e.context(errors::NoAnswer)),
            Err(_) => {
                // Held by a result's rows being read all that while: they're
                // stuck on the link, and given up on with the session. Held
                // by anything else (an open result waiting for its next
                // page to be asked for, in another tab; work in an open
                // transaction), it's left be: the run waits its turn, and
                // Esc can stop the wait.
                if self.0.supervisor.streaming() && self.0.supervisor.reading() {
                    self.abandon_session();
                    self.report_lost(&format!("no answer in {} s", SESSION_HELD.as_secs()));
                }
                let said = anyhow::anyhow!("the session was held for {} s", SESSION_HELD.as_secs());
                Err(said.context(errors::NoAnswer))
            }
        }
    }

    /// The session was given up on mid-call: the next run opens a new one.
    /// What's running on it (a run, a stream's rows) is stopped, so the
    /// session is free.
    ///
    /// A database that lives in the session (SQLite's `:memory:`) keeps it:
    /// only the work on it is stopped.
    pub fn abandon_session(&self) {
        if !self.0.owns_data {
            self.0.stale.store(true, Ordering::SeqCst);
        }
        self.0.supervisor.stop_work();
    }

    /// A session given up on, replaced by a new one: true if it was. Its
    /// connection goes out of the pool, not back into it: it's mid-call in
    /// a state nobody knows.
    async fn renew_if_stale(&self, session: &mut Session) -> Result<bool> {
        if !self.0.stale.load(Ordering::SeqCst) {
            return Ok(false);
        }
        // A database that lives in the session: a new one would be empty,
        // so the old one's kept, whatever its state.
        if self.0.owns_data && !matches!(session, Session::Closed) {
            self.0.stale.store(false, Ordering::SeqCst);
            return Ok(false);
        }
        // Given up on until a new one is open (`open_session`): stopped half
        // way (a check's time ran out), the next run tries again.
        self.reopen_session(session).await.map_err(|e| e.context(errors::NoAnswer))?;
        Ok(true)
    }

    /// Cancels whatever the session connection is running: the server is
    /// asked over a separate pool connection, since the session one is busy;
    /// SQLite's progress handler sees the interrupt flag.
    pub async fn cancel(&self) -> Result<()> {
        if let Err(e) = self.check_reach().await {
            if errors::is_unreachable(&e) {
                self.report_lost(&error_message(&e));
            }
            return Err(e);
        }
        let id = self.0.backend_id.load(Ordering::SeqCst);
        on_driver!(self.0.engine, engine::queries::cancel(self, id).await)
    }

    /// Closes the connection. What runs on its session doesn't hold it up:
    /// the server is asked to stop it (a write may or may not have been
    /// done by then), and its work is stopped; what waits isn't run.
    pub async fn close(&self) {
        if self.busy() {
            let _ = tokio::time::timeout(CLOSE_CANCEL_WITHIN, self.cancel()).await;
        }
        // Hand the session connection back first: the pool's close() waits
        // for every checked-out connection.
        self.close_session().await;
        on_pool!(&self.0.pool, |p| engine::driver::close(p).await);
        self.report_closed();
    }
}

/// How long a close waits for the server to say it stopped what the
/// session runs.
const CLOSE_CANCEL_WITHIN: Duration = Duration::from_secs(3);

/// Why [`Database::run_within`] gave no result.
#[derive(Debug)]
pub enum RunError {
    /// The time limit ran out, and the query was cancelled.
    TimedOut(Duration),
    Failed(anyhow::Error),
}

impl StatementResult {
    fn into_outcome(self, statement: usize, statements: usize, elapsed: Duration) -> QueryOutcome {
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
            earlier: Vec::new(),
            statement,
        }
    }
}

impl Session {
    /// Lets a cancel stop the running statement, where its engine needs
    /// that set up (SQLite's progress handler).
    pub(super) async fn watch_interrupt(&mut self, interrupt: &Arc<AtomicBool>) -> Result<()> {
        on_session!(self, |c| engine::connect::watch_interrupt(c, interrupt).await, closed => Ok(()))
    }

    pub(super) async fn server_version(&mut self) -> String {
        let version = on_session!(
            self,
            |c| engine::driver::text(c, engine::DIALECT.version_query()).await,
            closed => return String::new()
        );
        // "16.2 (Debian 16.2-1)" → "16.2"
        version.map(|v| v.split_whitespace().next().unwrap_or_default().to_string()).unwrap_or_default()
    }

    /// The session's `sql_mode`, for engines that have one.
    pub(super) async fn sql_mode(&mut self) -> Option<String> {
        on_session!(
            self,
            |c| {
                let sql = engine::DIALECT.sql_mode_query()?;
                engine::driver::text(c, sql).await.ok()
            },
            closed => None
        )
    }

    /// What came of a ping within `limit`: an answer, a link that broke at
    /// once, or nothing.
    pub(super) async fn answer_within(&mut self, limit: Duration) -> Answer {
        let ping = async { on_session!(self, |c| engine::driver::ping(c).await, closed => false) };
        match tokio::time::timeout(limit, ping).await {
            Ok(true) => Answer::Yes,
            Ok(false) => Answer::Broke,
            Err(_) => Answer::Silent,
        }
    }

    pub(super) async fn backend_id(&mut self) -> Result<i64> {
        on_session!(self, |c| engine::connect::backend_id(c).await, closed => Ok(0))
    }

    pub(super) async fn run(&mut self, sql: &str, limit: usize) -> Result<Vec<StatementResult>> {
        let mut ran = 0;
        self.run_counted(sql, limit, &mut ran).await
    }

    /// `run`, counting in `ran` the statements done (a failure's run those
    /// before it).
    pub(super) async fn run_counted(
        &mut self,
        sql: &str,
        limit: usize,
        ran: &mut usize,
    ) -> Result<Vec<StatementResult>> {
        on_session!(self, |c| engine::driver::run(c, sql, limit, ran).await, closed => anyhow::bail!("disconnected"))
    }

    /// Column names and types of a single statement, via a server-side
    /// prepare. `None` if it can't be prepared (e.g. some utility commands).
    pub(super) async fn describe_columns(&mut self, sql: &str) -> Option<Vec<(String, String)>> {
        on_session!(self, |c| engine::driver::columns(c, sql).await, closed => None)
    }
}

#[cfg(test)]
mod word_tests {
    /// A word where a statement or a line starts, not one inside another
    /// word or a statement's values.
    #[test]
    fn a_word_where_a_statement_starts() {
        assert!(super::starts_a_line("USE shop", "use"));
        assert!(super::starts_a_line("select 1; use`shop`", "use"));
        assert!(super::starts_a_line("select 1;\n  -- next\n  Set x = 1", "set"));
        assert!(!super::starts_a_line("select * from users", "use"));
        assert!(!super::starts_a_line("select * from t offset 5", "set"));
        assert!(!super::starts_a_line("insert into t values ('pending end')", "end"));
        assert!(super::starts_a_line("insert into t values (1); END", "end"));
        assert!(!super::starts_a_line("select created_at from t", "create"));
    }
}
