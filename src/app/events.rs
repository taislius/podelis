//! Handling results from background tasks (connects, queries, catalog
//! loads, the servers' answers), each in the tab it belongs to.

use super::*;
use crate::config::keys::Action;

impl App {
    pub fn handle_db_event(&mut self, event: DbEvent) {
        let kind = match event.kind {
            DbEventKind::Background(background) => return self.apply_background(background),
            kind => kind,
        };
        match event.run {
            // A run's outcome: for the tab it ran in (by its run id, unique
            // across the tabs), even if another is shown; none for a run
            // stopped since.
            Some(run) => {
                let Some(tab) = (0..self.tabs.len()).find(|&i| self.tab_run(i).is_some_and(|r| r.id == run)) else {
                    return;
                };
                return self.apply_in_tab(tab, kind);
            }
            None if event.generation != self.generation => {
                // A connection that finished after being cancelled is still open.
                if let DbEventKind::Connected(db) = kind {
                    tokio::spawn(async move { db.close().await });
                }
                return;
            }
            None => {}
        }
        // A plan made beside a running query: for the tab it was asked in.
        if let DbEventKind::PlanAside { tab, .. } = &kind
            && *tab < self.tabs.len()
        {
            return self.apply_in_tab(*tab, kind);
        }
        self.apply_shown(kind);
    }

    /// See [`Background`]: each kind finds what it's for on its own.
    fn apply_background(&mut self, event: Background) {
        match event {
            Background::Restored { project_id, form, result } => self.apply_restored(&project_id, *form, result),
            Background::ContainersFound(request, result) => self.containers_found(request, result),
            Background::ExportProgress { rows, size, free } => self.export_progress(rows, size, free),
            Background::ExportEstimated { request, format, rows, row_bytes } => {
                self.export_estimated(request, format, rows, row_bytes);
            }
            Background::ExportAsk { rows, size, free } => self.export_ask(rows, size, free),
            Background::ExportDone(path, result) => self.export_done(&path, result),
            Background::ExportShownDone(path, result, note) => {
                // One stopped meanwhile: its word comes after it went.
                if self.exports.task.as_ref().is_some_and(|run| run.path == path) {
                    self.exports.task = None;
                    self.clear_export_note();
                    self.export_said(&path, result, &note);
                }
            }
            Background::AskProgress(id, chars) => self.ask_progress(id, chars),
            Background::AskWaitingKey(id) => self.ask_waiting_key(id),
            Background::SandboxLocks(said) => self.sandbox_locks(said),
            Background::AskDone(id, result) => self.ask_done(id, *result),
            Background::GatewayDone(purpose, result) => self.gateway_done(purpose, *result),
        }
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
        self.hidden_tab = Some(i);
        self.in_tab(i, |app| app.apply_shown(kind));
        self.hidden_tab = None;
        if self.notes.status != before && !self.notes.status.is_empty() {
            self.notes.status = format!("{}: {}", self.tabs[i].name, self.notes.status);
            self.notes.status_in_results = false;
        }
    }

    fn export_progress(&mut self, rows: u64, size: u64, free: Option<u64>) {
        if self.exports.task.is_some() {
            self.clear_export_note();
            #[expect(clippy::cast_precision_loss, reason = "display only, one decimal")]
            let rows = crate::db::human_rows(rows as f64);
            let human = crate::db::human_bytes;
            let size = match free {
                Some(free) => t!("export.size_free", size = human(size), free = human(free)),
                None => t!("export.size_free_unknown", size = human(size)),
            };
            self.set_status(t!("run.exporting", rows = rows, size = size), false);
        }
    }

    fn export_done(&mut self, path: &std::path::Path, result: Result<u64, String>) {
        // A stopped export's last word arrives after it was removed.
        if self.exports.task.as_ref().is_none_or(|run| run.path != path) {
            return;
        }
        self.exports.task = None;
        self.clear_export_note();
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
            DbEventKind::PasswordLoaded { profile, password, note } => {
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
                self.tab.run.task = None;
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
            DbEventKind::QueryFailed { message, position, txn, missing_feature } => {
                self.tab.run.missing_feature = missing_feature;
                self.on_query_failed(&message, position, txn);
                self.drop_queued_plan();
            }
            DbEventKind::PlanAside { stmt, line, plan, .. } => self.plan_aside(stmt, line, plan),
            DbEventKind::MoreRows { page, shown } => self.append_rows(page, shown),
            // `handle_db_event` gives these to `apply_background` first.
            DbEventKind::Background(_) => {}
            DbEventKind::MoreFailed(err) => {
                self.tab.run.fetching_more = false;
                // (set_stream below forgets a waiting `M`.)
                self.tab.run.set_stream(None);
                let status = t!("run.load_more_failed", err = one_line(&err));
                self.log(status.clone(), true);
                self.set_status(status, true);
            }
            DbEventKind::CancelFailed { message, unreachable } => self.cancel_failed_with(&message, unreachable),
            DbEventKind::SyntaxChecked { text, hint } => {
                // Ignore results for text that has since changed.
                if text == self.tab.editor.text() && self.editing.lint_due.is_none() {
                    self.tab.syntax_hint = hint;
                }
            }
            DbEventKind::Catalog(result) => self.apply_catalog(result),
            DbEventKind::Pinged(result) => self.pinged(result),
            DbEventKind::SandboxCopied(copied) => self.sandbox_copied(copied),
            DbEventKind::ActivityLoaded(result) => self.activity_loaded(result),
            DbEventKind::SessionStopped(result) => self.session_stopped(result),
            DbEventKind::Diagnosed { title, report } => {
                self.connection.form.info = None;
                self.popups.cell_view = Some(CellView::new(title, report));
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
            DbEventKind::Opened { what, command, result } => self.opened(&what, &command, result),
            DbEventKind::Followed(followed) => {
                let (found, result) = *followed;
                self.followed(found, result);
            }
            DbEventKind::Definition { title, text: result } => {
                let browse = std::mem::take(&mut self.routine_definition_pending);
                match result {
                    Ok(text) if browse => self.popups.cell_view = Some(CellView::definition(title, text)),
                    Ok(text) => self.popups.cell_view = Some(CellView::new(title, text)),
                    Err(e) => self.set_status(e, true),
                }
            }
        }
    }

    /// Cancelling failed: said, and while it still runs, Esc again stops
    /// waiting for it.
    fn cancel_failed_with(&mut self, err: &str, unreachable: bool) {
        self.tab.run.cancelling = false;
        // The server couldn't be reached to be told (not one that refused:
        // a missing privilege): the connection looks down, and while the
        // query still runs, Esc again stops waiting for it.
        let stuck = self.tab.run.executing && unreachable;
        self.tab.run.cancel_failed = stuck;
        self.keepalive.connection_lost |= stuck;
        let again = if stuck { format!("{} · ", t!("run.esc_again")) } else { String::new() };
        let status = t!("run.cancel_failed", again = again, err = one_line(err));
        self.log(status.clone(), true);
        self.set_status(status, true);
    }

    /// The connect form's connection is open: make it the tab's.
    fn on_connected(&mut self, db: Database) {
        tracing::info!(engine = ?db.engine(), version = db.version(), "connected");
        self.tab.run.task = None;
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
        self.tab.run.sql.clear();
        self.tab.run.db = None;
        self.tab.run.task = None;
        self.tab.run.executing = false;
        self.note_session_changes();
        // The schema (or, after USE/SET, its visibility) may have changed.
        if self.tab.run.infos.iter().any(|i| matches!(i.kind, Kind::Ddl | Kind::Session)) {
            self.load_catalog();
        }
        self.connection.txn = txn.unwrap_or_else(|| self.tracked_txn_state());
        let rows_affected = outcome.rows_affected.unwrap_or(0);
        self.keepalive.connection_lost = false;
        self.tab.run.cancel_failed = false;
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
        match std::mem::take(&mut self.tab.run.purpose) {
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
                if self.tab.run.extra.vertical {
                    (grid.vertical, grid.vertical_by_g) = (true, true);
                } else if std::mem::take(&mut grid.vertical_by_g) {
                    grid.vertical = false;
                }
                if let Some(prefix) = self.tab.run.extra.status_prefix.take() {
                    self.notes.status = format!("{prefix} · {}", self.notes.status);
                }
                // (The RESULT tab was shown as it started: a plan asked for
                // meanwhile stays in view.)
                self.tab.tab_scroll = 0;
                if stream.is_some() {
                    let _ = write!(self.notes.status, " · {}", t!("run.more_as_you_scroll"));
                }
                self.tab.run.set_stream(stream);
                let status = self.notes.status.clone();
                self.log(status, false);
            }
            RunPurpose::Explain => self.show_plan(outcome),
            RunPurpose::CellEdit { row, col, value } => self.apply_cell_edit(&outcome, row, col, value),
            RunPurpose::TxControl(how) => self.transaction_ended(how),
            RunPurpose::RowChange(what) => self.rows_changed(what, &outcome),
            RunPurpose::SandboxBack { number, undo } => self.sandbox_went_back(number, undo),
        }
        if let Some(step) = self.tab.run.extra.sandbox_step.take() {
            self.sandbox_step_done(step, rows_affected);
        }
        // The transaction gone (a reconnect): a sandbox with it.
        if self.connection.txn == TxnState::Idle && self.sandbox_flow.then.is_none() {
            self.connection.sandbox = None;
        }
        if reconnected {
            let _ = write!(self.notes.status, " · {}", t!("run.reconnected"));
        }
        if std::mem::take(&mut self.tab.run.extra.reviewing) {
            self.open_review(rows_affected);
        }
        if self.tab.run.extra.dry_run.is_some() {
            self.end_dry_run(query_screen::review::DryRun::Done(rows_affected));
        }
        // MySQL reports some interrupted statements (e.g. SLEEP) as
        // ordinary results: stopped by the connection's limit, it's said so.
        if let Some(limit) = self.tab.run.extra.timed_out.take() {
            self.tab.run.cancelling = false;
            self.notes.status = t!("run.timed_out_early", s = limit.as_secs(), status = self.notes.status);
        } else if std::mem::take(&mut self.tab.run.cancelling) {
            let _ = write!(self.notes.status, " · {}", t!("run.cancel_requested"));
        }
        // The outcome is shown with the results.
        self.notes.status_in_results = true;
        let status = self.notes.status.clone();
        self.notify_if_long(|took| t!("run.finished_in", took = format_elapsed(took), status = status));
        self.run_sandbox_script();
    }

    pub(super) fn on_query_failed(&mut self, err: &str, position: Option<ErrorPos>, txn: Option<TxnState>) {
        // The run's own connection (another tab's may be shown now): no
        // other server's version says anything about it.
        let ran_on = self.tab.run.db.take();
        self.stop_watch(&format!(": {}", t!("run.query_failed")));
        if err.contains("couldn't reconnect") || err.contains("the server doesn't answer") {
            self.keepalive.connection_lost = true;
        }
        self.tab.run.cancel_failed = false;
        tracing::debug!(error = err, "query failed");
        self.tab.run.task = None;
        let reviewing = std::mem::take(&mut self.tab.run.extra.reviewing);
        self.tab.run.executing = false;
        // Statements before the failing one ran.
        self.note_session_changes();
        let query = matches!(std::mem::take(&mut self.tab.run.purpose), RunPurpose::Query);
        if let Some(txn) = txn {
            self.connection.txn = txn;
        }
        let prefix = match self.tab.run.extra.timed_out.take() {
            Some(limit) => format!("{}: ", t!("run.timed_out", s = limit.as_secs())),
            None if self.tab.run.cancelling => format!("{}: ", t!("run.cancelled")),
            None => String::new(),
        };
        self.tab.run.cancelling = false;
        let mut status = format!("{prefix}{}", one_line(err));
        let mut failed_at = None;
        if let Some(marker) = position.and_then(|p| self.locate_error(&p)) {
            let (row, col) = marker.0;
            let _ = write!(status, " ({})", t!("run.at_line_col", line = row + 1, col = col + 1));
            failed_at = Some(self.tab.editor.offset_of(marker.0));
            self.tab.error_marker = Some(marker);
        }
        // A syntax error that is really a feature this server hasn't got:
        // in the statement that failed (where the error was, or the only one).
        let ran = std::mem::take(&mut self.tab.run.sql);
        let missing = std::mem::take(&mut self.tab.run.missing_feature);
        let shown = failed_at.map(|_| self.tab.editor.text());
        // Where the error is in the editor; else what was sent (an EXPLAIN
        // around the statement, say), if it's one statement.
        let note = |db: &Database| {
            let at_error = shown.as_deref().and_then(|text| {
                query_screen::editor::lint::version_note(db.engine(), db.version(), text, failed_at, missing)
            });
            at_error
                .or_else(|| query_screen::editor::lint::version_note(db.engine(), db.version(), &ran, None, missing))
        };
        if let Some(said) = ran_on.filter(|_| prefix.is_empty()).as_ref().and_then(note) {
            let _ = write!(status, " · {said}");
        }
        let dry_run = self.tab.run.extra.dry_run.is_some();
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
            self.end_dry_run(query_screen::review::DryRun::Failed(status));
        }
        // A sandbox step that failed: back to before it, the sandbox goes on.
        if let Some(step) = self.tab.run.extra.sandbox_step.take() {
            self.sandbox_step_failed(&step);
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
                panel.selected = query_screen::navigator::step_selectable(&panel.nodes(), panel.selected, 1);
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

    /// Maps a server error position to an editor range to underline, if the
    /// editor still holds the text that was run.
    pub(super) fn locate_error(&self, position: &ErrorPos) -> Option<(Pos, Pos)> {
        let text = self.tab.editor.text();
        if text != self.tab.last_run_text {
            return None;
        }
        let (start, end) = if let Some(filled) = &self.tab.run.filled {
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
        for info in &self.tab.run.infos {
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

/// How a connection check failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ping {
    /// The server said no, or the connection broke.
    Failed,
    /// Nothing came back in time: the session may be stuck on a dead link.
    NoAnswer,
}

pub struct DbEvent {
    pub(in crate::app) generation: u64,
    /// A run's outcome, by its id: it outlives connection switches (another
    /// tab shown meanwhile), not a disconnect.
    pub(in crate::app) run: Option<u64>,
    pub(in crate::app) kind: DbEventKind,
}

pub(super) enum DbEventKind {
    /// A saved connection's password, and why it's missing if the keychain
    /// couldn't be read.
    PasswordLoaded {
        profile: ConnectionProfile,
        password: String,
        note: Option<String>,
    },
    Connected(Database),
    ConnectFailed(String),
    /// A finished run, with a stream of further rows if it was streamed.
    QueryDone(QueryOutcome, Option<TxnState>, Option<RowStream>),
    /// Another page of a streamed result; `shown` when `M` asked for it
    /// (the first of its rows gets selected, so they're seen arriving).
    MoreRows {
        page: Page,
        shown: bool,
    },
    MoreFailed(String),
    /// Why, where, the transaction after it, and whether it's a syntax
    /// error or unknown function (maybe a feature the server lacks).
    QueryFailed {
        message: String,
        position: Option<ErrorPos>,
        txn: Option<TxnState>,
        missing_feature: bool,
    },
    /// Why not, and whether the server couldn't be reached at all.
    CancelFailed {
        message: String,
        unreachable: bool,
    },
    Catalog(Result<Vec<(TableInfo, Vec<ColumnInfo>)>, String>),
    Routines(Vec<RoutineInfo>),
    TableDetails {
        schema: String,
        table: String,
        result: Result<TableDetails, String>,
    },
    /// The server's sessions, for the activity view.
    ActivityLoaded(Result<Vec<crate::db::Activity>, String>),
    /// A session's query was cancelled or the session ended.
    SessionStopped(Result<String, String>),
    /// A keep-alive check: latency and whether it had to reconnect.
    /// A connection check: its latency and whether it reconnected, or why
    /// not and whether it got no answer at all (`Ping::NoAnswer`).
    Pinged(Result<(Duration, bool), (String, Ping)>),
    /// A connection test's report.
    Diagnosed {
        title: String,
        report: String,
    },
    /// The output of piping rows through a command.
    PipeDone(String, Result<String, String>),
    /// A SQLite file's copy for a sandbox, opened (or why not).
    SandboxCopied(Result<(Database, PathBuf), String>),
    /// A definition to show, or why it can't be.
    Definition {
        title: String,
        text: Result<String, String>,
    },
    /// Rows opened in an app (`what` they were, by `command`), or why they
    /// couldn't be.
    Opened {
        what: String,
        command: String,
        result: Result<(), String>,
    },
    /// The row a foreign key points to (`f`), looked up.
    Followed(Box<(query_screen::results::follow_fk::Followed, Result<QueryOutcome, String>)>),
    /// The background syntax check of `text`: the server's problem, else
    /// the parser's hint, else the first lint warning.
    SyntaxChecked {
        text: String,
        hint: Option<SyntaxHint>,
    },
    /// A read on production, planned: the tables it would read in full.
    Guarded {
        sql: String,
        offset: usize,
        infos: Vec<StatementInfo>,
        filled: Option<Filled>,
        scans: Vec<(String, f64)>,
    },
    /// A plan made on a connection of its own while a query ran: the
    /// statement, its line, and the plan (`None`: it couldn't be made there).
    PlanAside {
        tab: usize,
        stmt: String,
        line: usize,
        plan: Option<QueryOutcome>,
    },
    /// A schema snapshot saved: its file and how many tables, or why not.
    SnapshotSaved(Result<(PathBuf, usize), String>),
    /// This connection's schema and a saved snapshot of it, to compare.
    SnapshotCompared(Result<(Vec<crate::transfer::diff::TableShape>, crate::transfer::snapshots::Snapshot), String>),
    /// Both sides' schemas, for comparing.
    SchemaCompared {
        other: query_screen::compare::Other,
        result: Result<query_screen::compare::Shapes, String>,
    },
    /// The same query's result on both sides.
    RowsCompared {
        other: query_screen::compare::Other,
        result: Box<Result<(QueryOutcome, QueryOutcome), String>>,
    },
    /// Not about the connection shown: see [`Background`].
    Background(Background),
}

/// Events that go on while other tabs and connections are used: reopening
/// the session's connections (matched by name), container searches (by
/// their own number), an export, a question to the gateway, the sandbox's
/// locks. None is dropped for a newer generation.
pub(super) enum Background {
    /// Rows written so far by "export all rows", its size on disk, and
    /// what's free there.
    ExportProgress { rows: u64, size: u64, free: Option<u64> },
    /// About how big "export all rows" would be, if that can be told: the
    /// request it answers, its rows by the plan, and bytes a row.
    ExportEstimated { request: u64, format: Format, rows: Option<f64>, row_bytes: f64 },
    /// "Export all rows" reached a size to ask about, and waits.
    ExportAsk { rows: u64, size: u64, free: Option<u64> },
    /// "Export all rows" finished: the file, and how many rows (or why not).
    ExportDone(PathBuf, Result<u64, String>),
    /// An export of the rows shown, written off the UI thread (Parquet):
    /// where, and how many of them it had (" (10 rows loaded)").
    ExportShownDone(PathBuf, Result<(), String>, String),
    /// A connection reopened at startup.
    Restored { project_id: String, form: Box<ConnectForm>, result: Result<Database, String> },
    /// The running database containers, or why they couldn't be listed.
    ContainersFound(u64, Result<Vec<crate::system::containers::Container>, String>),
    /// Characters of an answer being written, for ask `n`.
    AskProgress(u64, usize),
    /// Ask `n`'s key is slow to come: the keychain (or a password
    /// manager) is asking the user.
    AskWaitingKey(u64),
    /// The locks a sandbox's session holds (or why that can't be seen).
    SandboxLocks(String),
    /// Ask `n`'s answer: the SQL and what the model said, or why not.
    AskDone(u64, Box<Result<serde_json::Value, query_screen::ask::AskFailure>>),
    /// A gateway call's answer (the memory window's).
    GatewayDone(windows::memory::GatewayPurpose, Box<Result<serde_json::Value, query_screen::ask::AskFailure>>),
}

impl From<Background> for DbEventKind {
    fn from(event: Background) -> Self {
        DbEventKind::Background(event)
    }
}
