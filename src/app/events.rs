//! Handling results from background tasks (connects, queries, catalog
//! loads, syntax checks) and the periodic tick.

use super::*;
use crate::config::keys::Action;

impl App {
    pub fn handle_db_event(&mut self, event: DbEvent) {
        // Reopened connections are matched to tabs by name, not generation.
        if let DbEventKind::Restored { project_id, form, result } = event.kind {
            return self.apply_restored(&project_id, *form, result);
        }
        // Container searches by their own number: nothing else cancels them.
        if let DbEventKind::ContainersFound(request, result) = event.kind {
            return self.containers_found(request, result);
        }
        // An export, or a question, goes on while other tabs are used.
        match event.kind {
            DbEventKind::ExportProgress(rows) => return self.export_progress(rows),
            DbEventKind::ExportDone(path, result) => return self.export_done(&path, result),
            DbEventKind::AskProgress(id, chars) => return self.ask_progress(id, chars),
            DbEventKind::AskDone(id, result) => return self.ask_done(id, *result),
            DbEventKind::GatewayDone(purpose, result) => return self.gateway_done(purpose, *result),
            _ => {}
        }
        match event.run {
            // A run's outcome: for the tab it ran in, even if another is shown.
            Some(run) if run != self.run.id => return,
            Some(_) => {
                let owner = self.run.owner.filter(|&t| t < self.tabs.len()).unwrap_or(self.active_tab);
                return self.apply_in_tab(owner, event.kind);
            }
            None if event.generation != self.generation => {
                // A connection that finished after being cancelled is still open.
                if let DbEventKind::Connected(db) = event.kind {
                    tokio::spawn(async move { db.close().await });
                }
                return;
            }
            None => {}
        }
        // A plan made beside a running query: for the tab it was asked in.
        if let DbEventKind::PlanAside { tab, .. } = &event.kind
            && *tab < self.tabs.len()
        {
            return self.apply_in_tab(*tab, event.kind);
        }
        self.apply_shown(event.kind);
    }

    fn apply_shown(&mut self, kind: DbEventKind) {
        // Outcomes land in the result's grid, not a plan's shown in its place.
        self.unswap_plan_grid();
        self.apply_event(kind);
        self.sync_plan_grid();
    }

    /// Applies `kind` to tab `i`; when that's not the one shown, what it says
    /// goes to the status bar, with the tab's name.
    fn apply_in_tab(&mut self, i: usize, kind: DbEventKind) {
        if i == self.active_tab {
            return self.apply_shown(kind);
        }
        let before = self.notes.status.clone();
        self.in_tab(i, |app| app.apply_shown(kind));
        if self.notes.status != before && !self.notes.status.is_empty() {
            self.notes.status = format!("{}: {}", self.tabs[i].name, self.notes.status);
            self.notes.status_in_results = false;
        }
    }

    fn export_progress(&mut self, rows: u64) {
        if self.export_task.is_some() {
            #[allow(clippy::cast_precision_loss)] // display only, one decimal
            let rows = crate::db::human_rows(rows as f64);
            self.set_status(t!("run.exporting", rows = rows), false);
        }
    }

    fn export_done(&mut self, path: &std::path::Path, result: Result<u64, String>) {
        // A stopped export's last word arrives after it was removed.
        if self.export_task.as_ref().is_none_or(|(_, p)| p != path) {
            return;
        }
        self.export_task = None;
        match result {
            Ok(rows) => {
                let shown = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
                self.set_status(t!("run.exported_all", rows = rows, path = shown.display()), false);
            }
            Err(e) => {
                let status = t!("run.export_failed", e = one_line(&e));
                self.log(status.clone(), true);
                self.set_status(status, true);
            }
        }
    }

    fn apply_event(&mut self, kind: DbEventKind) {
        match kind {
            DbEventKind::PasswordLoaded(profile, password, note) => {
                self.manager.loading_profile = false;
                self.stash_active_form();
                self.connection.form = ConnectForm::from_profile(&profile, password);
                // Without the keychain, the database's own sources (PGPASSWORD,
                // ~/.pgpass) may still have the password: connect anyway.
                let connect_now = std::mem::take(&mut self.conns.auto_connect);
                self.connection.form.error = note.as_ref().map(|n| t!("run.no_saved_password", note = n));
                self.connection.form.keychain_note = note;
                self.screen = Screen::Connect;
                if connect_now {
                    self.start_connect();
                }
            }
            DbEventKind::Connected(db) => self.on_connected(db),
            DbEventKind::ConnectFailed(err) => {
                tracing::info!(error = %err, "connect failed");
                self.task = None;
                let form = &mut self.connection.form;
                form.connecting = false;
                // A wrong or missing password: ready to type it (unless a
                // password command supplied it; that's for the command to fix).
                if crate::db::is_auth_failure(&err) && form.password_command.trim().is_empty() {
                    // Said before clearing: the keychain is only to blame if
                    // there was no password.
                    let said = t!("run.type_password", err = err);
                    form.error = Some(form.connect_failure(&said));
                    form.password.clear();
                    form.focus = ConnectField::Password;
                } else {
                    form.error = Some(form.connect_failure(&crate::db::with_hint(&err)));
                }
            }
            DbEventKind::QueryDone(outcome, txn, stream) => {
                self.on_query_done(outcome, txn, stream);
                self.run_queued_plan();
            }
            DbEventKind::QueryFailed(err, position, txn) => {
                self.on_query_failed(&err, position, txn);
                self.drop_queued_plan();
            }
            DbEventKind::PlanAside { stmt, line, plan, .. } => self.plan_aside(stmt, line, plan),
            DbEventKind::MoreRows(page) => self.append_rows(page),
            DbEventKind::ExportProgress(rows) => self.export_progress(rows),
            DbEventKind::ExportDone(path, result) => self.export_done(&path, result),
            DbEventKind::MoreFailed(err) => {
                self.run.fetching_more = false;
                self.run.stream = None;
                let status = t!("run.load_more_failed", err = one_line(&err));
                self.log(status.clone(), true);
                self.set_status(status, true);
            }
            DbEventKind::CancelFailed(err, unreachable) => self.cancel_failed_with(&err, unreachable),
            DbEventKind::SyntaxChecked { text, hint } => {
                // Ignore results for text that has since changed.
                if text == self.tab.editor.text() && self.lint_due.is_none() {
                    self.tab.syntax_hint = hint;
                }
            }
            DbEventKind::Catalog(result) => self.apply_catalog(result),
            // Handled before the generation check.
            DbEventKind::Restored { .. }
            | DbEventKind::ContainersFound(..)
            | DbEventKind::AskProgress(..)
            | DbEventKind::AskDone(..)
            | DbEventKind::GatewayDone(..) => {}
            DbEventKind::Pinged(result) => self.pinged(result),
            DbEventKind::ActivityLoaded(result) => self.activity_loaded(result),
            DbEventKind::SessionStopped(result) => self.session_stopped(result),
            DbEventKind::Diagnosed(title, report) => {
                self.connection.form.info = None;
                self.cell_view = Some(CellView::new(title, report));
            }
            DbEventKind::PipeDone(command, result) => self.pipe_done(&command, result),
            DbEventKind::Routines(routines) => {
                if let Some(panel) = &mut self.connection.schema {
                    panel.routines = routines;
                }
            }
            DbEventKind::TableDetails { schema, table, result } => self.apply_details(&schema, &table, result),
            DbEventKind::SchemaCompared { other, result } => self.schema_compared(&other, result),
            DbEventKind::SnapshotSaved(result) => self.snapshot_saved(result),
            DbEventKind::Guarded { sql, offset, infos, filled, scans } => {
                self.guarded(sql, offset, infos, filled, &scans);
            }
            DbEventKind::SnapshotCompared(result) => self.snapshot_compared(result),
            DbEventKind::RowsCompared { other, result } => self.rows_compared(&other, *result),
            DbEventKind::Opened(what, command, result) => self.opened(&what, &command, result),
            DbEventKind::Followed(followed) => {
                let (found, result) = *followed;
                self.followed(found, result);
            }
            DbEventKind::Definition(title, result) => {
                let browse = std::mem::take(&mut self.routine_definition_pending);
                match result {
                    Ok(text) if browse => self.cell_view = Some(CellView::definition(title, text)),
                    Ok(text) => self.cell_view = Some(CellView::new(title, text)),
                    Err(e) => self.set_status(e, true),
                }
            }
        }
    }

    /// Cancelling failed: said, and while it still runs, Esc again stops
    /// waiting for it.
    fn cancel_failed_with(&mut self, err: &str, unreachable: bool) {
        self.run.cancelling = false;
        // The server couldn't be reached to be told (not one that refused:
        // a missing privilege): the connection looks down, and while the
        // query still runs, Esc again stops waiting for it.
        let stuck = self.run.executing && unreachable;
        self.run.cancel_failed = stuck;
        self.connection_lost |= stuck;
        let again = if stuck { format!("{} · ", t!("run.esc_again")) } else { String::new() };
        let status = t!("run.cancel_failed", again = again, err = one_line(err));
        self.log(status.clone(), true);
        self.set_status(status, true);
    }

    /// The connect form's connection is open: make it the tab's.
    fn on_connected(&mut self, db: Database) {
        tracing::info!(engine = ?db.engine(), version = db.version(), "connected");
        self.task = None;
        let project = self.project().map(|p| p.name.clone()).unwrap_or_default();
        let form = std::mem::take(&mut self.connection.form);
        self.attach_connection(db);
        self.connection.form = form;
        self.connection.summary = format!("[{project}] {}", self.connection.form.summary());
        self.connection.mode = self.connection.form.mode;
        self.connection.tx_mode = self.connection.form.tx_mode();
        self.connection.txn = TxnState::Idle;
        self.connection.form.connecting = false;
        self.connection.form.error = None;
        self.tab.grid.results = None;
        self.tab.grid.result_sets.clear();
        self.connection.schema = None;
        self.panes.schema_visible = true;
        self.load_catalog();
        self.screen = Screen::Query;
        self.focus = Focus::Editor;
        self.connection.latency = self.connect_started.take().map(|s| s.elapsed());
        let ms =
            self.connection.latency.map(|d| format!(" {}", t!("run.in_ms", ms = d.as_millis()))).unwrap_or_default();
        self.log(t!("run.connected", summary = self.connection.form.summary(), ms = ms), false);
        self.set_status(String::new(), false);
        if let Some((said, is_error)) = self.keep_new_connection() {
            self.connection.summary = format!("[{project}] {}", self.connection.form.summary());
            self.set_status(said, is_error);
        }
        // SSL mode `prefer` gave up on TLS: say so, it's not what was asked.
        let tls_failed = self.connection.database.as_ref().and_then(|db| db.tls_failed().map(str::to_string));
        if let Some(why) = tls_failed {
            let ssh = if self.connection.form.ssh.trim().is_empty() {
                String::new()
            } else {
                format!(" ({})", t!("run.ssh_encrypts"))
            };
            let warning = t!("run.not_encrypted", ssh = ssh);
            self.log(t!("run.tls_failed", warning = warning, why = why), true);
            self.set_status(t!("run.details_in_messages", warning = warning), true);
        }
    }

    pub(super) fn on_query_done(&mut self, outcome: QueryOutcome, txn: Option<TxnState>, stream: Option<RowStream>) {
        self.run.db = None;
        self.task = None;
        self.run.executing = false;
        self.note_session_changes();
        // The schema (or, after USE/SET, its visibility) may have changed.
        if self.run.infos.iter().any(|i| matches!(i.kind, Kind::Ddl | Kind::Session)) {
            self.load_catalog();
        }
        self.connection.txn = txn.unwrap_or_else(|| self.tracked_txn_state());
        let rows_affected = outcome.rows_affected.unwrap_or(0);
        self.connection_lost = false;
        self.run.cancel_failed = false;
        tracing::debug!(
            rows = outcome.rows.len(),
            elapsed_ms = outcome.elapsed.as_millis(),
            reconnected = outcome.reconnected,
            "query done"
        );
        let reconnected = outcome.reconnected;
        if reconnected {
            self.connection.txn = TxnState::Idle;
        }
        match std::mem::take(&mut self.run.purpose) {
            RunPurpose::Query => {
                self.tab.run_error = None;
                let grid = &self.tab.grid;
                let place = (grid.table_state.selected(), grid.selected_col, grid.row_offset);
                self.show_results(outcome);
                if self.watch.is_some() {
                    self.after_watched_run(place);
                }
                // `\G` turns the vertical layout on for its result; the next
                // run without it turns it off again (`X` keeps it on).
                let grid = &mut self.tab.grid;
                if self.run.extra.vertical {
                    (grid.vertical, grid.vertical_by_g) = (true, true);
                } else if std::mem::take(&mut grid.vertical_by_g) {
                    grid.vertical = false;
                }
                if let Some(prefix) = self.run.extra.status_prefix.take() {
                    self.notes.status = format!("{prefix} · {}", self.notes.status);
                }
                // (The RESULT tab was shown as it started: a plan asked for
                // meanwhile stays in view.)
                self.tab.tab_scroll = 0;
                if stream.is_some() {
                    let _ = write!(self.notes.status, " · {}", t!("run.more_as_you_scroll"));
                }
                self.run.stream = stream;
                let status = self.notes.status.clone();
                self.log(status, false);
            }
            RunPurpose::Explain => self.show_plan(outcome),
            RunPurpose::CellEdit { row, col, value } => self.apply_cell_edit(&outcome, row, col, value),
            RunPurpose::TxControl(command) => self.transaction_ended(command),
            RunPurpose::RowChange(what) => self.rows_changed(what, &outcome),
        }
        if reconnected {
            let _ = write!(self.notes.status, " · {}", t!("run.reconnected"));
        }
        if std::mem::take(&mut self.run.extra.reviewing) {
            self.open_review(rows_affected);
        }
        if self.run.extra.dry_run.is_some() {
            self.end_dry_run(review::DryRun::Done(rows_affected));
        }
        // MySQL reports some interrupted statements (e.g. SLEEP) as
        // ordinary results: stopped by the connection's limit, it's said so.
        if let Some(limit) = self.run.extra.timed_out.take() {
            self.run.cancelling = false;
            self.notes.status = t!("run.timed_out_early", s = limit.as_secs(), status = self.notes.status);
        } else if std::mem::take(&mut self.run.cancelling) {
            let _ = write!(self.notes.status, " · {}", t!("run.cancel_requested"));
        }
        // The outcome is shown with the results.
        self.notes.status_in_results = true;
        let status = self.notes.status.clone();
        self.notify_if_long(|took| t!("run.finished_in", took = format_elapsed(took), status = status));
    }

    pub(super) fn on_query_failed(&mut self, err: &str, position: Option<ErrorPos>, txn: Option<TxnState>) {
        self.run.db = None;
        self.stop_watch(&format!(": {}", t!("run.query_failed")));
        if err.contains("couldn't reconnect") || err.contains("the server doesn't answer") {
            self.connection_lost = true;
        }
        self.run.cancel_failed = false;
        tracing::debug!(error = err, "query failed");
        self.task = None;
        let reviewing = std::mem::take(&mut self.run.extra.reviewing);
        self.run.executing = false;
        // Statements before the failing one ran.
        self.note_session_changes();
        let query = matches!(std::mem::take(&mut self.run.purpose), RunPurpose::Query);
        if let Some(txn) = txn {
            self.connection.txn = txn;
        }
        let prefix = match self.run.extra.timed_out.take() {
            Some(limit) => format!("{}: ", t!("run.timed_out", s = limit.as_secs())),
            None if self.run.cancelling => format!("{}: ", t!("run.cancelled")),
            None => String::new(),
        };
        self.run.cancelling = false;
        let mut status = format!("{prefix}{}", one_line(err));
        if let Some(marker) = position.and_then(|p| self.locate_error(&p)) {
            let (row, col) = marker.0;
            let _ = write!(status, " ({})", t!("run.at_line_col", line = row + 1, col = col + 1));
            self.tab.error_marker = Some(marker);
        }
        let dry_run = self.run.extra.dry_run.is_some();
        if reviewing || dry_run {
            // BEGIN ran before the failing statement.
            if txn.is_none() {
                self.connection.txn = TxnState::Open;
            }
            if dry_run {
                let _ = write!(status, " · {}", t!("run.rolled_back"));
            } else {
                let rollback = self.keymap.label(Action::Rollback);
                let _ = write!(status, " · {}", t!("run.still_open", rollback = rollback));
            }
        }
        self.log(status.clone(), true);
        self.set_status(status.clone(), true);
        self.notes.status_in_results = true;
        // Said where its result would be.
        if query {
            self.tab.run_error = Some(status.clone());
        }
        self.notify_if_long(|took| t!("run.failed_after", took = format_elapsed(took), status = status));
        if dry_run {
            self.end_dry_run(review::DryRun::Failed(status));
        }
    }

    /// Installs a freshly loaded catalog, keeping expanded tables expanded.
    pub(super) fn apply_catalog(&mut self, result: Result<Vec<(TableInfo, Vec<ColumnInfo>)>, String>) {
        let Some(panel) = &mut self.connection.schema else { return };
        panel.loading = false;
        match result {
            Ok(tables) => {
                // Keep expanded tables expanded across a refresh.
                let expanded: std::collections::HashSet<(String, String)> = panel
                    .tables
                    .iter()
                    .filter(|t| t.expanded)
                    .map(|t| (t.info.schema.clone(), t.info.name.clone()))
                    .collect();
                panel.tables = tables
                    .into_iter()
                    .map(|(info, columns)| {
                        let mut table = SchemaTable::new(info, columns);
                        table.expanded = expanded.contains(&(table.info.schema.clone(), table.info.name.clone()));
                        table
                    })
                    .collect();
                panel.error = None;
                panel.selected = query::step_selectable(&panel.nodes(), panel.selected, 1);
                self.restore_expanded_tables();
                let Some(panel) = &mut self.connection.schema else { return };
                // Details were dropped with the old list: reload the open ones.
                let open: Vec<usize> = (0..panel.tables.len()).filter(|&t| panel.tables[t].expanded).collect();
                for t in open {
                    self.request_details(t);
                }
            }
            Err(e) => panel.error = Some(e),
        }
    }

    /// When the background syntax check is due, if one is pending.
    pub fn lint_due(&self) -> Option<Instant> {
        self.lint_due
    }

    /// Runs the background syntax check once typing has paused.
    /// Called every second while a query runs: enforces the connection's
    /// timeout and notices background tasks that panicked.
    pub fn tick_running(&mut self) {
        self.watch_tick();
        self.keepalive_tick();
        self.activity_tick();
        if self.watching_agents() {
            self.approvals_tick();
            self.agents_tick();
        }
        self.share_tick();
        if let Some((deadline, limit)) = self.run.extra.deadline
            && self.run.executing
            && Instant::now() >= deadline
        {
            self.run.extra.deadline = None;
            self.run.extra.timed_out = Some(limit);
            self.cancel_query();
        }
        self.check_background_panic();
    }

    /// Reports a background task that panicked (it never reports back).
    fn check_background_panic(&mut self) {
        if let Some(message) = crate::system::crash::take_background() {
            self.run.executing = false;
            self.task = None;
            self.run.stream = None;
            let log = crate::system::crash::log_path();
            self.set_status(t!("run.internal_error", message = message, log = log.display()), true);
        }
    }

    pub fn tick(&mut self) {
        if self.lint_due.is_none_or(|due| Instant::now() < due) {
            return;
        }
        self.lint_due = None;
        // Typing has paused: a good moment to save the tabs, so a crash or a
        // killed terminal loses at most a few seconds of editing.
        self.save_draft_in_background();
        let text = self.tab.editor.text();
        // Ask the server when it's safe: never inside a transaction (preparing
        // can take locks there) or while a query runs on the session.
        let server_ok =
            self.settings.server_syntax_check && self.connection.txn == TxnState::Idle && !self.run.executing;
        let db = self.connection.database.clone().filter(|_| server_ok);
        let check = self.local_check();
        // Parsing and linting a long script takes a while: off the UI thread,
        // like the server check. The result is dropped if the text changed.
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            self.tab.syntax_hint = check.hint(&text);
            return;
        };
        let engine = self.engine();
        let send = self.sender();
        runtime.spawn(async move {
            let local = |text: String, lint_only: bool| async move {
                let check = move || if lint_only { check.first_lint(&parse(&text, engine)) } else { check.hint(&text) };
                tokio::task::spawn_blocking(check).await.unwrap_or_default()
            };
            let hint = match db {
                Some(db) => match server_syntax_hint(&db, &text, engine).await {
                    ServerCheck::Done(Some(hint)) => Some(hint),
                    ServerCheck::Done(None) => local(text.clone(), true).await,
                    ServerCheck::Busy => local(text.clone(), false).await,
                },
                None => local(text.clone(), false).await,
            };
            send(DbEventKind::SyntaxChecked { text, hint });
        });
    }

    /// What the editor's own checks need, captured so they can run off the
    /// UI thread.
    fn local_check(&self) -> LocalCheck {
        LocalCheck {
            engine: self.engine(),
            lint: self.settings.lint,
            sql_mode: self.connection.database.as_ref().map(Database::sql_mode).unwrap_or_default(),
        }
    }

    /// Every lint warning, unless lints are turned off.
    fn lint_hints(&self, parsed: &ParsedScript) -> Vec<SyntaxHint> {
        self.local_check().lints(parsed)
    }

    /// Moves the cursor to the next lint warning after it (wrapping) and
    /// shows that warning.
    pub(super) fn next_warning(&mut self) {
        let text = self.tab.editor.text();
        let hints = self.lint_hints(&parse(&text, self.engine()));
        let cursor = self.tab.editor.cursor_offset();
        let next = hints.iter().find(|h| h.offset.is_some_and(|o| o > cursor)).or_else(|| hints.first()).cloned();
        let Some(hint) = next else {
            let why = if self.settings.lint { t!("run.no_lint") } else { t!("run.lint_off") };
            return self.set_status(why.to_string(), false);
        };
        if let Some(offset) = hint.offset {
            self.tab.editor.set_cursor(self.tab.editor.pos_of_offset(offset));
        }
        self.tab.syntax_hint = Some(hint);
        self.focus = Focus::Editor;
    }

    /// Maps a server error position to an editor range to underline, if the
    /// editor still holds the text that was run.
    pub(super) fn locate_error(&self, position: &ErrorPos) -> Option<(Pos, Pos)> {
        let text = self.tab.editor.text();
        if text != self.tab.last_run_text {
            return None;
        }
        let (start, end) = if let Some(filled) = &self.run.filled {
            let (start, len) = analyze::error_range(&filled.sql, position)?;
            let start = filled.to_original(start);
            (start, filled.to_original(start + len).max(start))
        } else {
            let sent = text.get(self.tab.last_run_offset..)?;
            let (start, len) = analyze::error_range(sent, position)?;
            (start, start + len)
        };
        let base = self.tab.last_run_offset;
        Some((self.tab.editor.pos_of_offset(base + start), self.tab.editor.pos_of_offset(base + end)))
    }

    /// Transaction state after the last successful run, for engines the
    /// server can't be asked about: follows BEGIN/COMMIT/ROLLBACK (and
    /// MySQL's implicit commit on DDL).
    pub(super) fn tracked_txn_state(&self) -> TxnState {
        let mut state = self.connection.txn;
        for info in &self.run.infos {
            state = match info.kind {
                Kind::Transaction(TxnOp::Begin) => TxnState::Open,
                Kind::Transaction(TxnOp::Commit | TxnOp::Rollback) => TxnState::Idle,
                Kind::Ddl if self.engine().capabilities().ddl_commits => TxnState::Idle,
                _ => state,
            };
        }
        state
    }
}

/// The editor's own checks: `sqlparser`'s syntax hint and the lint rules.
struct LocalCheck {
    engine: Engine,
    lint: bool,
    /// The session's (MySQL), for rules that depend on it.
    sql_mode: String,
}

impl LocalCheck {
    /// The parser's syntax error, or else the first lint warning.
    fn hint(&self, text: &str) -> Option<SyntaxHint> {
        let parsed = parse(text, self.engine);
        analyze::syntax_hint(&parsed).or_else(|| self.first_lint(&parsed))
    }

    fn first_lint(&self, parsed: &ParsedScript) -> Option<SyntaxHint> {
        self.lints(parsed).into_iter().next()
    }

    /// Every lint warning, unless lints are turned off.
    fn lints(&self, parsed: &ParsedScript) -> Vec<SyntaxHint> {
        if self.lint { crate::sql::lint::lints(parsed, &self.sql_mode) } else { Vec::new() }
    }
}

/// Prepares each statement of `text` on the server (without running it)
/// and returns the first problem.
/// Unknown-object errors are skipped when the script creates objects, since
/// those may only exist once earlier statements have run.
async fn server_syntax_hint(db: &Database, text: &str, engine: Engine) -> ServerCheck {
    const MAX_STATEMENTS: usize = 50;
    let parsed = parse(text, engine);
    let creates_objects = analyze::analyze(&parsed).iter().any(|i| i.kind == Kind::Ddl);
    for stmt in parsed.statements.iter().take(MAX_STATEMENTS) {
        let (base, stmt) = (stmt.offset, stmt.text);
        let Ok(outcome) = db.check_statement(stmt).await else { return ServerCheck::Busy };
        match outcome {
            CheckOutcome::Ok | CheckOutcome::Unsupported => {}
            CheckOutcome::Problem { syntax: false, .. } if creates_objects => {}
            CheckOutcome::Problem { message, position, .. } => {
                let range = position.and_then(|p| analyze::error_range(stmt, &p));
                return ServerCheck::Done(Some(SyntaxHint {
                    message,
                    offset: Some(base + range.map_or(0, |r| r.0)),
                    len: range.map(|r| r.1),
                    source: HintSource::Server,
                    nth: None,
                }));
            }
        }
    }
    ServerCheck::Done(None)
}
