//! Running queries: the checks before (connection, guards), the run
//! itself, more rows, cancelling and disconnecting.

use super::super::*;

/// A run after this long without one first checks the server answers.
const CHECK_AFTER_IDLE: Duration = Duration::from_secs(60);

/// How long the server has to answer that check.
const ANSWER_WITHIN: Duration = Duration::from_secs(5);

impl App {
    /// Runs the selection if there is one, otherwise the whole editor.
    pub(in crate::app) fn execute_all(&mut self) {
        let text = self.tab.editor.text();
        match self.tab.editor.selection_offsets() {
            Some((a, b)) => self.execute(text[a..b].to_string(), a),
            None => self.execute(text, 0),
        }
    }

    /// Runs the selection if there is one, otherwise the statement at the cursor.
    pub(in crate::app) fn execute_current(&mut self) {
        if self.tab.editor.selection().is_some() {
            return self.execute_all();
        }
        let text = self.tab.editor.text();
        match sql::statement_at(&text, self.tab.editor.cursor_offset(), self.engine()) {
            Some(stmt) => {
                let offset = stmt.as_ptr() as usize - text.as_ptr() as usize;
                self.execute(stmt.to_string(), offset);
            }
            None => self.set_status(t!("editor.no_statement").to_string(), true),
        }
    }

    /// Checks what `sql` would do against the connection's mode, asking for
    /// confirmation where needed. `offset`: where `sql` starts in the editor.
    pub(in crate::app) fn execute(&mut self, sql: String, offset: usize) {
        self.execute_with(sql, offset, RunPurpose::Query);
    }

    pub(in crate::app) fn execute_with(&mut self, sql: String, offset: usize, purpose: RunPurpose) {
        if self.run.executing {
            return self.set_status(self.running_elsewhere(t!("run.wait_or_esc_there")), true);
        }
        if self.connection.database.is_none() {
            return self.set_status(self.not_connected_message(), true);
        }
        if sql.trim().is_empty() {
            self.set_status(t!("run.nothing").to_string(), true);
            return;
        }
        if matches!(purpose, RunPurpose::Query) && self.ask_for_params(&sql, offset, false) {
            return;
        }
        self.check_and_run(sql, offset, purpose, None);
    }

    pub(in crate::app) fn not_connected_message(&self) -> String {
        if self.conns.restoring.is_empty() {
            t!("run.not_connected").to_string()
        } else {
            t!("run.still_reconnecting", names = self.conns.restoring.join(", "))
        }
    }

    /// Runs `sql` unless the connection's mode forbids it or it needs
    /// confirming first. `filled`: the parameters substituted into it.
    pub(in crate::app) fn check_and_run(
        &mut self,
        sql: String,
        offset: usize,
        purpose: RunPurpose,
        filled: Option<Filled>,
    ) {
        let infos = analyze::analyze(&parse(&sql, self.engine()));
        let writes: Vec<&StatementInfo> = infos.iter().filter(|i| i.modifies()).collect();
        if self.connection.mode == ConnectionMode::ReadOnly
            && let Some(first) = writes.first()
        {
            let msg = t!("run.read_only_refused", what = first.summary);
            return self.set_status(msg, true);
        }
        if let Some(msg) = self.tx_mode_refusal(&infos, &purpose) {
            return self.set_status(msg, true);
        }
        let risky: Vec<String> = infos.iter().filter_map(|i| i.risk.map(|r| format!("{r}: {}", i.summary))).collect();
        let production = self.connection.mode == ConnectionMode::Production && !writes.is_empty();
        if production || !risky.is_empty() {
            let message =
                if production { tn!("run.production_writes", writes.len()) } else { t!("run.destructive").to_string() };
            let mut details = risky;
            if production {
                details.extend(writes.iter().filter(|w| w.risk.is_none()).map(|w| w.summary.clone()));
            }
            details.truncate(6);
            // Writes can instead run in a transaction, to review first.
            let offer_transaction = !writes.is_empty()
                && self.connection.tx_mode == TxMode::AutoCommit
                && self.connection.txn == TxnState::Idle
                && matches!(purpose, RunPurpose::Query | RunPurpose::RowChange(_));
            let action = ConfirmAction::Run { sql, offset, infos, purpose, filled };
            self.confirm = Some(Confirm { message, details, danger: true, action, offer_transaction });
            return;
        }
        // A read on production that would read a whole big table asks
        // first: planned (not run) in the background, then run or asked.
        let guarded = self.connection.mode == ConnectionMode::Production
            && self.settings.guard_rows > 0
            && matches!(purpose, RunPurpose::Query)
            && !infos.is_empty()
            && infos.iter().all(|i| i.kind == Kind::Read);
        if guarded && let Some(db) = self.connection.database.clone() {
            self.set_result_status(t!("run.checking_plan").to_string(), false);
            // Running, as far as the rest of the app goes: nothing else
            // starts, and Esc stops it.
            self.run.executing = true;
            self.run.since = Some(Instant::now());
            self.run.owner = None;
            let send = self.sender();
            tokio::spawn(async move {
                let scans = db.full_scans(&sql).await.unwrap_or_default();
                send(DbEventKind::Guarded { sql, offset, infos, filled, scans });
            });
            return;
        }
        self.run_writes(sql, offset, infos, purpose, filled);
    }

    /// After the plan of a read on production: runs it, or asks first when
    /// it would read a whole table of at least `guard_rows` rows.
    pub(in crate::app) fn guarded(
        &mut self,
        sql: String,
        offset: usize,
        infos: Vec<StatementInfo>,
        filled: Option<Filled>,
        scans: &[(String, f64)],
    ) {
        self.run.executing = false;
        if std::mem::take(&mut self.run.cancelling) {
            return self.set_status("Cancelled".to_string(), false);
        }
        #[allow(clippy::cast_precision_loss)] // a threshold
        let limit = self.settings.guard_rows as f64;
        let heavy: Vec<String> = scans
            .iter()
            .filter(|(_, rows)| *rows >= limit)
            .map(|(table, rows)| t!("run.full_scan", table = table, rows = crate::db::human_rows(*rows)))
            .collect();
        if heavy.is_empty() {
            return self.run_writes(sql, offset, infos, RunPurpose::Query, filled);
        }
        self.set_status(String::new(), false);
        self.confirm = Some(Confirm {
            message: t!("run.big_read").to_string(),
            details: heavy,
            danger: true,
            action: ConfirmAction::RunHeavy { sql, offset, infos, filled },
            offer_transaction: false,
        });
    }

    /// Re-runs the last query with a larger row limit (read-only queries only,
    /// since it executes them again).
    pub(in crate::app) fn load_more(&mut self) {
        if self.run.executing {
            return self.set_status(self.running_elsewhere(t!("run.load_more_after")), true);
        }
        if self.run.stream.is_some() {
            return self.fetch_more(self.settings.row_limit * 10);
        }
        let Some(results) = &self.tab.grid.results else { return };
        if !results.truncated() && !self.tab.grid.cut_short {
            return self.set_status(t!("run.all_loaded").to_string(), false);
        }
        if !self.tab.last_query_infos.iter().all(|i| i.kind == Kind::Read) {
            return self.set_status(t!("run.reads_only_more").to_string(), true);
        }
        let Some((sql, offset)) = self.tab.last_sql.clone() else { return };
        self.next_row_limit = Some((results.rows.len() * 10).min(MAX_ROW_LIMIT));
        let infos = self.tab.last_query_infos.clone();
        self.rerun_shown(sql, offset, infos);
    }

    /// "A query is running", naming its tab when that isn't this one.
    /// Says which tab, on which connection, runs the query (one at a time
    /// across the tabs), how to get there, then `then`.
    pub(in crate::app) fn running_elsewhere(&self, then: &str) -> String {
        let Some(t) = self.run.owner.filter(|&t| t != self.active_tab) else {
            return t!("run.running", then = then);
        };
        let Some(tab) = self.tabs.get(t) else { return t!("run.running", then = then) };
        let on = tab.conn.and_then(|id| self.conn_name(id)).map(|n| format!(" on {n}")).unwrap_or_default();
        let go = if t < 9 { format!("{}, ", t!("run.alt_goes", n = t + 1)) } else { String::new() };
        t!("run.tab_running", tab = tab.name, on = on, go = go, then = then)
    }

    /// Settings changed by the last run (`USE`, `SET`…) are the session's
    /// alone: from then on plans aren't made on another connection.
    pub(in crate::app) fn note_session_changes(&mut self) {
        if self.run.infos.iter().any(|i| i.kind == Kind::Session)
            && let Some(db) = &self.connection.database
        {
            db.mark_session_changed();
        }
    }

    pub(in crate::app) fn run_now(
        &mut self,
        sql: String,
        offset: usize,
        infos: Vec<StatementInfo>,
        purpose: RunPurpose,
        filled: Option<Filled>,
    ) {
        // A query's own result, unless it's the shown one run again.
        // (Taken whatever runs, so it can't outlive the run it was for.)
        let same_result = std::mem::take(&mut self.same_result);
        let new_result = matches!(purpose, RunPurpose::Query) && !self.watch_rerun && !same_result;
        // Anything else run stops watch mode.
        if !self.watch_rerun {
            self.stop_watch(&format!(": {}", t!("run.another_query")));
            // The tab for what's coming, while it runs.
            match purpose {
                RunPurpose::Query => self.set_result_tab(ResultTab::Rows),
                RunPurpose::Explain => self.set_result_tab(ResultTab::Plan),
                _ => {}
            }
        }
        let Some(db) = self.connection.database.clone() else { return };
        // A still-open streamed result holds the session: close it (which
        // cancels it on the server) so this run can start; so does an export.
        self.stop_export();
        self.close_stream();
        self.completion = None;
        // One read-only statement can be streamed page by page.
        let stream = matches!(purpose, RunPurpose::Query) && infos.len() == 1 && infos[0].kind == Kind::Read;
        self.run.infos = infos;
        self.run.purpose = purpose;
        self.run.extra.vertical = false;
        if matches!(self.run.purpose, RunPurpose::Query) {
            self.run.extra.vertical = sql::ends_vertical(&sql, self.engine());
            let original = filled.as_ref().map_or(&sql, |f| &f.original);
            if new_result {
                self.open_run(Some((original, offset)));
            }
            // Remembered for "load more" and error locations; helper runs
            // (EXPLAIN, cell edits) don't replace them.
            self.tab.last_sql = Some((sql.clone(), offset));
            self.tab.last_filled.clone_from(&filled);
            self.tab.last_query_infos.clone_from(&self.run.infos);
            // History keeps the placeholders, not the values, and no
            // statement that sets a password.
            if !sql::sets_password(original, self.engine()) {
                self.history.push(original);
            }
            self.tab.last_run_text = self.tab.editor.text();
        } else {
            self.tab.last_run_text.clear();
        }
        self.tab.last_run_offset = offset;
        self.run.filled = filled;
        // After a quiet while (or a lost connection), the server is asked
        // to answer first: a link that died meanwhile fails in seconds
        // rather than leaving the run waiting.
        let check_first = self.connection_lost || self.last_activity.elapsed() >= CHECK_AFTER_IDLE;
        self.last_activity = Instant::now();
        self.run.extra.timed_out = None;
        self.run.extra.started = Some(Instant::now());
        self.run.extra.deadline =
            self.connection.form.query_timeout().ok().flatten().map(|limit| (Instant::now() + limit, limit));
        self.tab.error_marker = None;
        self.run.executing = true;
        self.run.db = Some(db.clone());
        self.run.since = Some(Instant::now());
        // A read leaves the other tabs free meanwhile (see `in_tab`).
        self.run.id += 1;
        let reads = !self.run.infos.is_empty() && self.run.infos.iter().all(|i| i.kind == Kind::Read);
        let plan = self.plan_request.as_ref().is_some_and(|(_, analyzed, _)| !analyzed);
        let free = match self.run.purpose {
            RunPurpose::Query => reads,
            RunPurpose::Explain => plan,
            _ => false,
        };
        self.run.owner = free.then_some(self.active_tab);
        self.set_result_status(t!("tabs.running").to_string(), false);
        let send = self.run_sender();
        let row_limit = self.next_row_limit.take().unwrap_or(self.settings.row_limit);
        tracing::debug!(statements = self.run.infos.len(), stream, row_limit, "run");
        let sql = if self.run.extra.vertical { sql::without_vertical(&sql, self.engine()) } else { sql };
        self.task = Some(tokio::spawn(async move {
            if check_first && let Err(e) = db.answers_within(ANSWER_WITHIN).await {
                return send(DbEventKind::QueryFailed(crate::db::error_message(&e), None, None));
            }
            if stream {
                // No transaction probe: the open stream holds the session,
                // and a single read can't change the transaction state.
                match db.run_streaming(&sql, row_limit).await {
                    Ok((outcome, rows)) => send(DbEventKind::QueryDone(outcome, None, rows)),
                    Err(e) => {
                        let txn = after_error(&db, &e).await;
                        send(DbEventKind::QueryFailed(
                            crate::db::error_message(&e),
                            crate::db::error_position(&e),
                            txn,
                        ));
                    }
                }
                return;
            }
            match db.run(&sql, row_limit).await {
                Ok(outcome) => {
                    let txn = db.transaction_state().await;
                    send(DbEventKind::QueryDone(outcome, txn, None));
                }
                Err(e) => {
                    let txn = after_error(&db, &e).await;
                    send(DbEventKind::QueryFailed(crate::db::error_message(&e), crate::db::error_position(&e), txn));
                }
            }
        }));
    }

    /// Asks the open stream for `rows` more rows (once at a time).
    pub(in crate::app) fn fetch_more(&mut self, rows: usize) {
        let Some(stream) = self.run.stream.clone() else { return };
        if self.run.fetching_more {
            return;
        }
        self.run.fetching_more = true;
        let send = self.sender();
        tokio::spawn(async move {
            match stream.next_page(rows).await {
                Ok(page) => send(DbEventKind::MoreRows(page)),
                Err(e) => send(DbEventKind::MoreFailed(e)),
            }
        });
    }

    /// Adds a fetched page to the results, keeping the selection.
    pub(in crate::app) fn append_rows(&mut self, page: Page) {
        self.run.fetching_more = false;
        let Some(results) = &mut self.tab.grid.results else { return };
        let start = results.rows.len();
        results.total_rows += page.rows.len() as u64;
        self.tab.grid.col_widths = widen_columns(std::mem::take(&mut self.tab.grid.col_widths), &page.rows);
        results.rows.extend(page.rows);
        if page.done {
            self.run.stream = None;
        }
        let loaded = results.rows.len();
        if self.tab.grid.sort.is_some() || !self.tab.grid.filter.is_empty() {
            let selected = self.tab.grid.table_state.selected();
            self.tab.grid.view = compute_view(results, self.tab.grid.filter.text(), self.tab.grid.sort);
            self.tab.grid.table_state.select(selected.map(|s| s.min(self.tab.grid.view.len().saturating_sub(1))));
        } else {
            self.tab.grid.view.extend(start..loaded);
        }
        let more = if self.run.stream.is_some() { t!("run.more_as_you_scroll") } else { t!("run.all_rows_loaded") };
        self.set_status(format!("{} · {more}", t!("run.rows_loaded", n = loaded)), false);
    }

    /// Near the end of the loaded rows of a streamed result, fetch more.
    pub(in crate::app) fn maybe_fetch_more(&mut self) {
        const LOOKAHEAD: usize = 50;
        // The stream's rows are the result's, not the plan's.
        if self.tab.plan_swapped {
            return;
        }
        let near_end = self.tab.grid.table_state.selected().is_some_and(|s| s + LOOKAHEAD >= self.tab.grid.view.len());
        if near_end {
            self.fetch_more(self.settings.row_limit);
        }
    }

    pub(in crate::app) fn cancel_query(&mut self) {
        // Cancelling failed (the server can't be reached to be told): Esc
        // again stops waiting here.
        if self.run.cancel_failed {
            return self.stop_waiting();
        }
        if self.run.cancelling {
            return;
        }
        // The run's own connection: another tab's may be shown now.
        let Some(db) = self.run.db.clone().or_else(|| self.connection.database.clone()) else { return };
        self.run.cancelling = true;
        self.queued_plan = None;
        self.set_result_status("Cancelling...".to_string(), false);
        let send = self.sender();
        tokio::spawn(async move {
            if let Err(e) = db.cancel().await {
                send(DbEventKind::CancelFailed(crate::db::error_message(&e), crate::db::is_unreachable(&e)));
            }
        });
    }

    /// Gives up on the running query without the server's word: its task is
    /// dropped, and the session with it (the next run opens a new one). The
    /// query may still run on the server until it notices.
    pub(in crate::app) fn stop_waiting(&mut self) {
        self.run.cancel_failed = false;
        self.run.cancelling = false;
        if let Some(task) = self.task.take() {
            task.abort();
        }
        if let Some(db) = self.run.db.as_ref().or(self.connection.database.as_ref()) {
            db.abandon_session();
        }
        self.on_query_failed(
            "stopped waiting: the server doesn't answer, so it couldn't be told to cancel (the query may still \
             run there); the next run reconnects",
            None,
            None,
        );
    }

    /// Disconnects, asking first if a transaction would be rolled back.
    pub(in crate::app) fn request_disconnect(&mut self) {
        // (In its own tab, disconnecting stops it.)
        if self.run.executing && !self.running_here() {
            return self.set_status(self.running_elsewhere(t!("run.disconnect_after")), true);
        }
        if self.connection.txn == TxnState::Idle {
            return self.disconnect();
        }
        self.confirm = Some(Confirm {
            message: t!("run.disconnect_rollback").to_string(),
            details: vec![],
            danger: true,
            action: ConfirmAction::Disconnect,
            offer_transaction: false,
        });
    }
}

/// The transaction state after a failed run: none once the session was
/// reopened, otherwise what the server says.
async fn after_error(db: &Database, err: &anyhow::Error) -> Option<TxnState> {
    if crate::db::is_reconnected(err) { Some(TxnState::Idle) } else { db.transaction_state().await }
}
