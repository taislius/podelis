//! Handling results from background tasks (connects, queries, catalog
//! loads, the servers' answers), each in the tab it belongs to.

use super::*;
use crate::config::keys::Action;
use crate::config::store::ProjectId;

impl App {
    pub fn handle_db_event(&mut self, event: DbEvent) {
        match event.payload {
            // Background work has its own identity and is never dropped for
            // a connection change.
            DbEventPayload::Connection { kind: DbEventKind::Background(background), .. } => {
                self.apply_background(background);
            }
            // A run-owned result: for its tab by run id, even if another is
            // shown; none for a run stopped or superseded since.
            DbEventPayload::Run { id, kind } => {
                let Some(tab) = (0..self.tabs.len()).find(|&i| self.tab_run(i).is_some_and(|r| r.id == id)) else {
                    return;
                };
                self.apply_in_tab(tab, |app| app.apply_run_shown(kind));
            }
            DbEventPayload::Conn { id, kind } => self.apply_conn_event(id, kind),
            DbEventPayload::Connection { generation, kind } if generation != self.generation => {
                // A connection that finished after being cancelled is still open.
                if let DbEventKind::Connected(db) | DbEventKind::DatabaseOpened(db, ..) = kind {
                    tokio::spawn(async move { db.close().await });
                }
            }
            DbEventPayload::Connection { kind, .. } => {
                if let DbEventKind::PlanAside { tab, .. } = &kind {
                    let Some(index) = self.tabs.iter().position(|meta| meta.id == *tab) else { return };
                    return self.apply_in_tab(index, |app| app.apply_shown(kind));
                }
                self.apply_shown(kind);
            }
        }
    }

    /// What's learnt about connection `id`. The shown one takes all of it;
    /// a parked one keeps where it stands (its tables load again as its
    /// tab is shown, a check's word is for the shown one's).
    fn apply_conn_event(&mut self, id: ConnId, kind: ConnEvent) {
        let current_load = |schema: &Option<SchemaPanel>, load: u64| schema.as_ref().is_some_and(|p| p.load == load);
        if self.connection.slots.active == Some(id) {
            match kind {
                ConnEvent::Catalog { load, result } if current_load(&self.connection.schema, load) => {
                    self.unswap_plan_grid();
                    self.apply_catalog(result);
                    self.sync_plan_grid();
                }
                ConnEvent::Routines { load, routines } if current_load(&self.connection.schema, load) => {
                    if let Some(panel) = &mut self.connection.schema {
                        panel.routines = routines;
                    }
                }
                ConnEvent::Unrollable { load, tables } if current_load(&self.connection.schema, load) => {
                    self.connection.set_unrollable(tables);
                }
                ConnEvent::Pinged(result) => self.pinged(result),
                ConnEvent::Health(health) => self.apply_health(id, health),
                ConnEvent::Followed { follower, health } => self.apply_followed(id, follower, health),
                // An earlier load's.
                ConnEvent::Catalog { .. } | ConnEvent::Routines { .. } | ConnEvent::Unrollable { .. } => {}
            }
            return;
        }
        match kind {
            // A check asked for (`r`) is the shown connection's: one that
            // came back after another was shown is said by its reports.
            ConnEvent::Pinged(_) => {
                if let Some(slot) = self.connection.slot_mut(id) {
                    slot.checking = false;
                }
            }
            ConnEvent::Health(health) => self.apply_health(id, health),
            ConnEvent::Followed { follower, health } => self.apply_followed(id, follower, health),
            ConnEvent::Catalog { load, result } => {
                if let Some(slot) = self.connection.slot_mut(id)
                    && current_load(&slot.schema, load)
                {
                    slot.state.tables_loaded(result.as_ref().map(|_| ()).map_err(String::as_str));
                }
            }
            // Kept for when its tab's shown again (that doesn't load them
            // again).
            ConnEvent::Routines { load, routines } => {
                if let Some(slot) = self.connection.slot_mut(id)
                    && current_load(&slot.schema, load)
                    && let Some(panel) = &mut slot.schema
                {
                    panel.routines = routines;
                }
            }
            ConnEvent::Unrollable { load, tables } => {
                if let Some(slot) = self.connection.slot_mut(id)
                    && current_load(&slot.schema, load)
                {
                    slot.set_unrollable(tables);
                }
            }
        }
    }

    /// Connection `id`'s supervisor reported (shown or parked: it's that
    /// connection's either way): the one place its being lost or back
    /// comes from. Said: lost, back, paused, and a reconnect, which
    /// loses an open transaction (said at once, a hidden tab's too).
    /// A followed report: only the slot's connection now's.
    fn apply_followed(&mut self, id: ConnId, follower: u64, health: crate::db::Health) {
        if self.connection.slot_mut(id).is_some_and(|slot| slot.follower == follower) {
            self.apply_health(id, health);
        }
    }

    pub(super) fn apply_health(&mut self, id: ConnId, health: crate::db::Health) {
        use crate::db::HealthState;
        let shown = self.connection.slots.active == Some(id);
        // A run on it now made the reconnect: the run says it, with its
        // result (and the transaction it lost).
        let running_here = shown && self.tab.run.executing;
        let paused_said = self.paused_said();
        let Some(slot) = self.connection.slot_mut(id) else { return };
        let name = slot.form.short_name();
        let was_lost = slot.state.is_lost();
        if let Some(latency) = health.latency {
            slot.latency = Some(latency);
        }
        let mut said: Option<(String, bool)> = None;
        let mut sandbox_lost = false;
        // A reconnect not said yet, whatever the report's state now (one
        // that answered since can come as one report with it). In the
        // connection log once; on screen once, by the run if one's going.
        if health.reconnects > slot.reconnects_said {
            if health.reconnects > slot.reconnects_logged {
                slot.reconnects_logged = health.reconnects;
                crate::system::connection_log::record("reconnected", &name, "the session was reopened");
            }
            if !running_here {
                slot.reconnects_said = health.reconnects;
                // Said: the runs after it go on.
                if let Some(db) = &slot.database {
                    db.reconnect_told(health.reconnects);
                }
                let had_transaction = slot.txn != TxnState::Idle;
                slot.set_txn(TxnState::Idle);
                sandbox_lost = had_transaction;
                let mut message = reconnect_said(&health);
                if had_transaction {
                    let _ = write!(message, " · {}", t!("info.transaction_lost"));
                }
                let message = if shown { message } else { format!("{name}: {message}") };
                said = Some((message, had_transaction || !health.lost.is_empty()));
            }
        }
        match &health.state {
            HealthState::Live | HealthState::Reconnected => {
                slot.state.answered();
                // (A reconnect the run's to say says it's back.)
                let runs_to_say = running_here && health.reconnects > slot.reconnects_said;
                if was_lost && said.is_none() && !runs_to_say {
                    crate::system::connection_log::record("back", &name, "it answered");
                    said = Some((t!("info.back").to_string(), false));
                }
            }
            HealthState::Lost(why) => {
                if !was_lost {
                    crate::system::connection_log::record("lost", &name, why);
                }
                slot.state.lose();
            }
            // Said once, as it pauses: not again with each later report.
            HealthState::Paused(why) => {
                let was_paused = matches!(slot.health.as_ref().map(|h| &h.state), Some(HealthState::Paused(_)));
                if !was_paused {
                    crate::system::connection_log::record("paused", &name, why);
                    if shown {
                        said = Some((paused_said, true));
                    }
                }
                slot.state.lose();
            }
            HealthState::Closed => {}
        }
        slot.health = Some(health);
        // Its transaction gone: a sandbox with it.
        if sandbox_lost {
            self.sandbox_gone(id);
        }
        if let Some((message, is_error)) = said
            && (shown || is_error)
        {
            self.log(message.clone(), is_error);
            self.set_status(message, is_error);
        }
    }

    /// See [`Background`]: each kind finds what it's for on its own.
    fn apply_background(&mut self, event: Background) {
        match event {
            Background::Restored { project_id, id, attempt, form, result } => {
                self.apply_restored(&project_id, id, attempt, *form, result);
            }
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

    fn apply_run_shown(&mut self, kind: RunEvent) {
        self.unswap_plan_grid();
        self.apply_run_event(kind);
        self.sync_plan_grid();
    }

    fn apply_run_event(&mut self, event: RunEvent) {
        match event {
            RunEvent::QueryDone(outcome, txn, stream) => {
                self.on_query_done(outcome, txn, stream);
                self.run_queued_plan();
            }
            RunEvent::QueryFailed { message, position, txn, missing_feature, link } => {
                self.tab.run.missing_feature = missing_feature;
                self.on_query_failed(&message, position, txn);
                self.tab.run_error_link = link && self.tab.run_error.is_some();
                self.drop_queued_plan();
            }
            RunEvent::MoreRows { page, shown } => self.append_rows(page, shown),
            RunEvent::MoreFailed(err) => {
                self.tab.run.fetching_more = false;
                self.tab.run.set_stream(None);
                let status = t!("run.load_more_failed", err = one_line(&err));
                self.log(status.clone(), true);
                self.set_status(status, true);
            }
            RunEvent::CancelFailed { message, unreachable } => self.cancel_failed_with(&message, unreachable),
            RunEvent::SessionSeen(seen) => self.session_seen(seen),
            RunEvent::SessionReplaced(why) => self.session_replaced(&why),
        }
    }

    /// Applies `kind` to tab `i`; when that's not the one shown, what it says
    /// goes to the status bar, with the tab's name.
    fn apply_in_tab(&mut self, i: usize, apply: impl FnOnce(&mut Self)) {
        if i == self.active_tab {
            return apply(self);
        }
        let before = self.notes.status.clone();
        self.hidden_tab = Some(i);
        self.in_tab(i, apply);
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
                let connect_now = std::mem::take(&mut self.connection.slots.auto_connect);
                self.connection.form.error = note.as_ref().map(|n| t!("run.no_saved_password", note = n));
                self.connection.form.keychain_note = note;
                self.screen = Screen::Connect;
                if connect_now {
                    self.start_connect();
                }
            }
            DbEventKind::Connected(db) => self.on_connected(db),
            DbEventKind::DatabasesListed(listed) => self.on_databases_listed(listed),
            // For the tab it was asked in, if that's still the one shown and
            // free (a tab switched to since, or a query started there, keeps
            // its connection).
            DbEventKind::DatabaseOpened(db, form, swap) => {
                // One asked for before the latest: not this tab's now.
                if self.latest_swap != Some(swap) {
                    tokio::spawn(async move { db.close().await });
                    return self.swap_dropped(swap);
                }
                self.latest_swap = None;
                self.notes.editor_note = None;
                // A failed transaction too: it's the tab's to roll back.
                let busy = self.tab.run.executing || self.connection.txn != TxnState::Idle;
                if self.tabs[self.active_tab].id != swap.tab || busy {
                    tokio::spawn(async move { db.close().await });
                    return self.set_status(t!("palette.database_busy").to_string(), true);
                }
                self.on_connected_with(db, *form);
            }
            DbEventKind::DatabaseFailed { swap, .. } if self.latest_swap != Some(swap) => self.swap_dropped(swap),
            DbEventKind::DatabaseFailed { name, error, .. } => {
                self.latest_swap = None;
                self.connect_started = None;
                // "Opening…" has had its answer.
                self.notes.editor_note = None;
                self.set_status(t!("palette.database_failed", name = name, e = crate::db::with_hint(&error)), true);
            }
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
            DbEventKind::PlanAside { stmt, line, plan, .. } => self.plan_aside(stmt, line, plan),
            // `handle_db_event` gives these to `apply_background` first.
            DbEventKind::Background(_) => {}
            DbEventKind::SyntaxChecked { text, hint } => {
                // Ignore results for text that has since changed.
                if text == self.tab.editor.text() && self.editing.lint_due.is_none() {
                    self.tab.syntax_hint = hint;
                }
            }
            DbEventKind::SandboxCopied(copied) => self.sandbox_copied(copied),
            DbEventKind::ActivityLoaded(result) => self.activity_loaded(result),
            DbEventKind::SessionStopped(result) => self.session_stopped(result),
            DbEventKind::Diagnosed { title, report } => {
                self.connection.form.info = None;
                self.popups.cell_view = Some(CellView::new(title, report));
            }
            DbEventKind::PipeDone(command, result) => self.pipe_done(&command, result),
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
        // (That it's lost, the supervisor reports.)
        let stuck = self.tab.run.executing && unreachable;
        self.tab.run.cancel_failed = stuck;
        let again = if stuck { format!("{} · ", t!("run.esc_again")) } else { String::new() };
        let status = t!("run.cancel_failed", again = again, err = one_line(err));
        self.log(status.clone(), true);
        self.set_status(status, true);
    }

    /// The connect form's connection is open: make it the tab's.
    fn on_connected(&mut self, db: Database) {
        // The connection shown keeps its own form (a failed one, still its
        // other tabs', its name): the new one gets a copy.
        let form = self.connection.form.duplicate();
        self.connection.form.connecting = false;
        self.connection.form.error = None;
        self.on_connected_with(db, form);
    }

    /// `db` is open with `form`'s settings: made the tab's.
    fn on_connected_with(&mut self, db: Database, form: ConnectForm) {
        tracing::info!(engine = ?db.engine(), version = db.version(), "connected");
        self.tab.run.task = None;
        // The connection the tab had goes (unless other tabs use it:
        // `attach_connection` releases the unused).
        let project = self.project().map(|p| p.name.clone()).unwrap_or_default();
        self.attach_connection(db);
        self.connection.form = form;
        self.connection.summary = format!("[{project}] {}", self.connection.form.summary());
        self.connection.mode = self.connection.form.mode;
        self.connection.tx_mode = self.connection.form.tx_mode();
        self.connection.set_txn(TxnState::Idle);
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
        let version = self.connection.database.as_ref().map(|d| d.version().to_string()).unwrap_or_default();
        crate::system::connection_log::record(
            "connected",
            &self.connection.form.short_name(),
            &format!("{version}{ms}"),
        );
        self.set_status(String::new(), false);
        if let Some((said, is_error)) = self.keep_new_connection() {
            self.connection.summary = format!("[{project}] {}", self.connection.form.summary());
            self.set_status(said, is_error);
        }
        self.take_over_failed_slots();
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
        // A reconnect before this run (its report not here yet) lost the
        // transaction the tab had: said now, before this result's own
        // transaction state, which a late report then can't clear.
        let ran_on = self.tab.run.db.take();
        // The transaction it lost, said with the result, as an error: the
        // result ran on a new session, outside it (a later ROLLBACK undoes
        // nothing of it).
        let mut lost_transaction = false;
        if self.reconnected_before(ran_on.as_ref()) {
            let had_transaction = self.connection.txn != TxnState::Idle;
            let mut said = t!("info.reconnected").to_string();
            lost_transaction = self.say_runs_reconnect(had_transaction, &mut said);
            self.log(said, lost_transaction);
        }
        self.tab.run.sql.clear();
        self.tab.run.task = None;
        self.tab.run.executing = false;
        self.note_session_changes();
        // The schema (or, after USE/SET, its visibility) may have changed.
        if self.tab.run.infos.iter().any(|i| matches!(i.kind, Kind::Ddl | Kind::Session)) {
            self.load_catalog();
        }
        // The server's word, else the session's (what ran on it).
        let txn = txn.unwrap_or_else(|| ran_on.as_ref().map_or(self.connection.txn, Self::txn_of));
        // A COMMIT of a failed transaction (PostgreSQL's) rolls it back.
        let commit_rolled_back = self.connection.txn == TxnState::Aborted
            && txn == TxnState::Idle
            && self.tab.run.infos.iter().any(|i| i.kind == Kind::Transaction(TxnOp::Commit));
        self.connection.set_txn(txn);
        let rows_affected = outcome.rows_affected.unwrap_or(0);
        self.connection.state.answered();
        self.tab.run.cancel_failed = false;
        tracing::debug!(rows = outcome.rows.len(), elapsed_ms = outcome.elapsed.as_millis(), "query done");
        match std::mem::take(&mut self.tab.run.purpose) {
            RunPurpose::Query => {
                self.tab.run_error = None;
                let grid = &self.tab.grid;
                let place = (grid.table_state.selected(), grid.selected_col, grid.row_offset);
                self.show_results(outcome);
                // The connection its rows were read on (not the tab's now:
                // a reconnect put a sandbox's original back meanwhile).
                if let Some(db) = &ran_on {
                    self.tab.grid.from = Some(db.identity());
                }
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
                if commit_rolled_back {
                    let _ = write!(self.notes.status, " · {}", t!("review.commit_rolled_back"));
                }
                let status = self.notes.status.clone();
                self.log(status, commit_rolled_back);
                self.notes.status_is_error |= commit_rolled_back;
            }
            RunPurpose::Explain => self.show_plan(outcome),
            RunPurpose::CellEdit { row, col, value } => self.apply_cell_edit(&outcome, row, col, value),
            RunPurpose::TxControl(how) => self.transaction_ended(how),
            RunPurpose::RowChange(what) => self.rows_changed(what, &outcome),
            RunPurpose::SandboxBack { number, undo } => self.sandbox_went_back(number, undo),
        }
        if lost_transaction {
            self.notes.status = format!("{} · {}", t!("info.transaction_lost"), self.notes.status);
            self.notes.status_is_error = true;
        }
        if let Some(step) = self.tab.run.extra.sandbox_step.take() {
            self.sandbox_step_done(step, rows_affected);
        }
        // The transaction gone (a reconnect): a sandbox with it.
        if self.connection.txn == TxnState::Idle
            && self.sandbox_flow.then.is_none()
            && let Some(id) = self.connection.slots.active
        {
            self.sandbox_gone(id);
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

    /// A reconnect a run made: its transaction (and sandbox) gone, what came
    /// back on the new session added to `status` (its report, coming before
    /// or after, doesn't say it again). Whether a transaction was lost.
    fn say_runs_reconnect(&mut self, had_transaction: bool, status: &mut String) -> bool {
        self.connection.set_txn(TxnState::Idle);
        if self.sandbox_flow.then.is_none()
            && let Some(id) = self.connection.slots.active
        {
            self.sandbox_gone(id);
        }
        let health = self.connection.database.as_ref().map(|db| db.health().borrow().clone());
        if let Some(health) = health
            && health.reconnects > self.connection.reconnects_said
        {
            self.connection.reconnects_said = health.reconnects;
            if let Some(db) = &self.connection.database {
                db.reconnect_told(health.reconnects);
            }
            if let Some(lost) = lost_with_session(&health) {
                let _ = write!(status, " · {lost}");
            }
        }
        if had_transaction {
            let _ = write!(status, " · {}", t!("info.transaction_lost"));
            self.log(t!("info.transaction_lost").to_string(), true);
        }
        had_transaction
    }

    /// What a paused connection's said: after how many tries, and what
    /// tries again.
    pub fn paused_said(&self) -> String {
        let key = self.keymap.label(Action::CheckConnection);
        t!("bar.paused", tries = crate::db::LOST_CHECKS, key = key)
    }

    /// A reconnect the shown tab's run was to say (its report came while it
    /// ran), left unsaid: the run stopped without its answer (dropped, the
    /// tab's connection changed).
    pub(super) fn reconnect_unsaid(&self) -> bool {
        !self.tab.run.executing
            && self.connection.health.as_ref().is_some_and(|h| h.reconnects > self.connection.reconnects_said)
    }

    /// Says it, by its report, as if it came now.
    pub(super) fn say_unsaid_reconnect(&mut self) {
        if self.reconnect_unsaid()
            && let (Some(id), Some(health)) = (self.connection.slots.active, self.connection.health.clone())
        {
            self.apply_health(id, health);
        }
    }

    /// Whether the shown connection, the one `ran_on` (a run's), has a
    /// reconnect not said yet.
    fn reconnected_before(&self, ran_on: Option<&Database>) -> bool {
        let shown = self.connection.database.as_ref();
        ran_on.zip(shown).is_some_and(|(ran, shown)| {
            ran.same(shown) && shown.health().borrow().reconnects > self.connection.reconnects_said
        })
    }

    pub(super) fn on_query_failed(&mut self, err: &str, position: Option<ErrorPos>, txn: Option<TxnState>) {
        // The run's own connection (another tab's may be shown now): no
        // other server's version says anything about it.
        let ran_on = self.tab.run.db.take();
        self.stop_watch(&format!(": {}", t!("run.query_failed")));
        // Whether the connection was lost is the supervisor's to say (its
        // `Health` report), not this message's.
        self.tab.run.cancel_failed = false;
        tracing::debug!(error = err, "query failed");
        self.tab.run.task = None;
        let reviewing = std::mem::take(&mut self.tab.run.extra.reviewing);
        self.tab.run.executing = false;
        // Statements before the failing one ran.
        self.note_session_changes();
        let query = matches!(std::mem::take(&mut self.tab.run.purpose), RunPurpose::Query);
        // As it was before this run (a reconnect's error says Idle: the
        // transaction that was open is what it lost).
        let had_transaction = self.connection.txn != TxnState::Idle;
        if let Some(txn) = txn {
            self.connection.set_txn(txn);
        }
        let prefix = match self.tab.run.extra.timed_out.take() {
            Some(limit) => format!("{}: ", t!("run.timed_out", s = limit.as_secs())),
            None if self.tab.run.cancelling => format!("{}: ", t!("run.cancelled")),
            None => String::new(),
        };
        self.tab.run.cancelling = false;
        let mut status = format!("{prefix}{}", one_line(err));
        // The run reopened the session (and wasn't run again): its
        // transaction's gone with the old one, said now, not on a later
        // report.
        if self.reconnected_before(ran_on.as_ref()) {
            self.say_runs_reconnect(had_transaction, &mut status);
        } else if txn.is_none()
            && let Some(db) = ran_on.as_ref()
        {
            // No word from the server (MySQL, SQLite): what ran before the
            // failing statement says it (a `BEGIN` that ran left one open).
            let txn = Self::txn_of(db);
            if (txn == TxnState::Idle) != (self.connection.txn == TxnState::Idle) {
                self.connection.set_txn(txn);
            }
        }
        let mut failed_at = None;
        let mut place = None;
        if let Some(marker) = position.and_then(|p| self.locate_error(&p)) {
            place = Some((self.tabs[self.active_tab].id, marker.0, marker.1));
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
            if dry_run {
                let _ = write!(status, " · {}", t!("run.rolled_back"));
            } else {
                let rollback = self.keymap.label(Action::Rollback);
                let _ = write!(status, " · {}", t!("run.still_open", rollback = rollback));
            }
        }
        self.log(status.clone(), true);
        if let Some(message) = self.notes.log.last_mut() {
            message.at = place;
        }
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
        panel.loading_since = None;
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
                self.connection.state.tables_loaded(Ok(()));
                self.restore_expanded_tables();
                let Some(panel) = &mut self.connection.schema else { return };
                // Details were dropped with the old list: reload the open ones.
                let open: Vec<usize> = (0..panel.tables.len()).filter(|&t| panel.tables[t].expanded).collect();
                for t in open {
                    self.request_details(t);
                }
            }
            Err(e) => {
                panel.error = Some(e.clone());
                self.connection.state.tables_loaded(Err(&e));
            }
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

    /// A transaction's open on `db`'s session, as what ran on it says
    /// (where its server doesn't: MySQL, SQLite, DuckDB).
    fn txn_of(db: &Database) -> TxnState {
        if db.in_transaction() { TxnState::Open } else { TxnState::Idle }
    }
}

/// How a connection check failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ping {
    /// The server said no, or the connection broke.
    Failed,
    /// Nothing came back in time: the session may be stuck on a dead link.
    NoAnswer,
    /// The check didn't get the session in time (a result's rows still
    /// open on it): not a sign the connection's lost.
    Busy,
}

/// A tab's connection being swapped for another opening in the
/// background: only the latest one asked for lands (A then B: B, whichever
/// opens first).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Swap {
    pub tab: super::TabId,
    pub attempt: u64,
}

pub struct DbEvent {
    payload: DbEventPayload,
}

impl DbEvent {
    pub(in crate::app) fn connection(generation: ConnectionGeneration, kind: DbEventKind) -> Self {
        Self { payload: DbEventPayload::Connection { generation, kind } }
    }

    pub(in crate::app) fn conn(id: ConnId, kind: ConnEvent) -> Self {
        Self { payload: DbEventPayload::Conn { id, kind } }
    }

    pub(in crate::app) fn run(id: RunId, kind: RunEvent) -> Self {
        Self { payload: DbEventPayload::Run { id, kind } }
    }

    #[cfg(test)]
    pub(in crate::app) fn as_connection_kind(&self) -> Option<&DbEventKind> {
        match &self.payload {
            DbEventPayload::Connection { kind, .. } => Some(kind),
            DbEventPayload::Run { .. } | DbEventPayload::Conn { .. } => None,
        }
    }
}

enum DbEventPayload {
    /// A result about the active connection. Old generations are discarded.
    Connection { generation: ConnectionGeneration, kind: DbEventKind },
    /// A result about connection `id`, wherever it is now (shown, or
    /// parked behind another tab); none for one closed since.
    Conn { id: ConnId, kind: ConnEvent },
    /// A result owned by one query run, routed to the tab that started it.
    Run { id: RunId, kind: RunEvent },
}

/// What's learnt about one connection: for it, shown or not. The tables'
/// results carry which load they're of (`SchemaPanel::load`): an earlier
/// load's, finishing late, is dropped.
pub(super) enum ConnEvent {
    Catalog {
        load: u64,
        result: Result<Vec<(TableInfo, Vec<ColumnInfo>)>, String>,
    },
    Routines {
        load: u64,
        routines: Vec<RoutineInfo>,
    },
    /// The connection's tables a rollback doesn't undo, or why they
    /// couldn't be read.
    Unrollable {
        load: u64,
        tables: Result<Vec<super::session::Unrollable>, String>,
    },
    /// A connection check: its latency and whether it reconnected, or why
    /// not and whether it got no answer at all (`Ping::NoAnswer`).
    Pinged(Result<(Duration, bool), (String, Ping)>),
    /// What the connection's supervisor reports: it answers, it's lost,
    /// its session was reopened, it's closed.
    #[cfg_attr(not(test), expect(dead_code, reason = "the tests send reports straight"))]
    Health(crate::db::Health),
    /// …as followed for the slot (`follower`: which connection it had then;
    /// one reopened in place since has another, and the old one's late
    /// reports are dropped).
    Followed {
        follower: u64,
        health: crate::db::Health,
    },
}

pub(super) enum RunEvent {
    QueryDone(QueryOutcome, Option<TxnState>, Option<RowStream>),
    MoreRows {
        page: Page,
        shown: bool,
    },
    MoreFailed(String),
    QueryFailed {
        message: String,
        position: Option<ErrorPos>,
        txn: Option<TxnState>,
        missing_feature: bool,
        /// The link's fault (no answer, couldn't reconnect), not the SQL's.
        link: bool,
    },
    CancelFailed {
        message: String,
        unreachable: bool,
    },
    SessionSeen(SessionSeen),
    SessionReplaced(String),
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
    /// The server's databases (`None`: a file, the one database).
    DatabasesListed(Result<Option<Vec<String>>, String>),
    /// Another database opened for a tab (`Swap`), with its form.
    DatabaseOpened(Database, Box<ConnectForm>, Swap),
    DatabaseFailed {
        name: String,
        error: String,
        swap: Swap,
    },
    TableDetails {
        schema: String,
        table: String,
        result: Result<TableDetails, String>,
    },
    /// The server's sessions, for the activity view.
    ActivityLoaded(Result<Vec<crate::db::Activity>, String>),
    /// A session's query was cancelled or the session ended.
    SessionStopped(Result<String, String>),
    /// A connection test's report.
    Diagnosed {
        title: String,
        report: String,
    },
    /// The output of piping rows through a command.
    PipeDone(String, Result<String, String>),
    /// A SQLite file's copy for a sandbox, opened (or why not).
    SandboxCopied(Result<(Database, PathBuf, std::fs::File), String>),
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
        /// Or why it couldn't be planned.
        scans: Result<Vec<(String, f64)>, String>,
    },
    /// A plan made on a connection of its own while a query ran: the
    /// statement, its line, and the plan (`None`: it couldn't be made there).
    PlanAside {
        tab: TabId,
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
/// their own request id), an export, a question to the gateway, the sandbox's
/// locks. None is dropped for a newer generation.
pub(super) enum Background {
    /// Rows written so far by "export all rows", its size on disk, and
    /// what's free there.
    ExportProgress { rows: u64, size: u64, free: Option<u64> },
    /// About how big "export all rows" would be, if that can be told: the
    /// request it answers, its rows by the plan, and bytes a row.
    ExportEstimated {
        request: query_screen::results::export::ExportRequestId,
        format: Format,
        rows: Option<f64>,
        row_bytes: f64,
    },
    /// "Export all rows" reached a size to ask about, and waits.
    ExportAsk { rows: u64, size: u64, free: Option<u64> },
    /// "Export all rows" finished: the file, and how many rows (or why not).
    ExportDone(PathBuf, Result<u64, String>),
    /// An export of the rows shown, written off the UI thread (Parquet):
    /// where, and how many of them it had (" (10 rows loaded)").
    ExportShownDone(PathBuf, Result<(), String>, String),
    /// A connection reopened at startup.
    Restored {
        project_id: ProjectId,
        id: ConnId,
        attempt: u64,
        form: Box<ConnectForm>,
        result: Result<Database, String>,
    },
    /// The running database containers, or why they couldn't be listed.
    ContainersFound(ContainerRequestId, Result<Vec<crate::system::containers::Container>, String>),
    /// Characters of an answer being written, for ask `n`.
    AskProgress(query_screen::ask::AskId, usize),
    /// Ask `n`'s key is slow to come: the keychain (or a password
    /// manager) is asking the user.
    AskWaitingKey(query_screen::ask::AskId),
    /// The locks a sandbox's session holds (or why that can't be seen).
    SandboxLocks(String),
    /// Ask `n`'s answer: the SQL and what the model said, or why not.
    AskDone(query_screen::ask::AskId, Box<Result<crate::gateway::methods::AskResult, query_screen::ask::AskFailure>>),
    /// A gateway call's answer (the memory window's).
    GatewayDone(windows::memory::GatewayPurpose, Box<Result<serde_json::Value, query_screen::ask::AskFailure>>),
}

impl From<Background> for DbEventKind {
    fn from(event: Background) -> Self {
        DbEventKind::Background(event)
    }
}

/// A reconnect, as said, with what the old session had that's gone
/// (`Reconnected · gone with the old session: database shop, sql_mode`).
pub(super) fn reconnect_said(health: &crate::db::Health) -> String {
    let mut said = t!("info.reconnected").to_string();
    if let Some(lost) = lost_with_session(health) {
        let _ = write!(said, " · {lost}");
    }
    said
}

/// What the old session had that's gone with it, said (none: `None`).
fn lost_with_session(health: &crate::db::Health) -> Option<String> {
    (!health.lost.is_empty()).then(|| t!("info.lost_with_session", what = health.lost.join(", ")))
}

#[cfg(test)]
mod event_identity_tests {
    use super::{DbEvent, DbEventKind, RunEvent};
    use crate::app::{ConnectionGeneration, RunId};

    #[test]
    fn connection_and_run_events_keep_distinct_payloads() {
        let connection = DbEvent::connection(ConnectionGeneration::default(), DbEventKind::ConnectFailed("no".into()));
        let run = DbEvent::run(
            RunId::default(),
            RunEvent::QueryFailed {
                message: "failed".into(),
                position: None,
                txn: None,
                missing_feature: false,
                link: false,
            },
        );

        assert!(connection.as_connection_kind().is_some());
        assert!(run.as_connection_kind().is_none());
    }

    #[test]
    fn run_ids_advance_without_using_a_raw_counter() {
        let mut id = RunId::default();
        let first = id.next();
        let second = id.next();

        assert_ne!(first, second);
    }
}
