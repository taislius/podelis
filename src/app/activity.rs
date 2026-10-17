//! The activity monitor: sessions on the server and what they run, with
//! cancel and kill. Refreshes every couple of seconds while open.

use super::*;
use crate::db::Activity;

const REFRESH: Duration = Duration::from_secs(2);

#[derive(Default)]
pub struct ActivityView {
    pub rows: Vec<Activity>,
    pub selected: usize,
    pub error: Option<String>,
    /// Asking "cancel / kill session id?" (`true`: kill).
    pub confirm: Option<(String, bool)>,
    loading: bool,
    refreshed: Option<Instant>,
}

impl App {
    pub(super) fn open_activity(&mut self) {
        if self.connection.database.is_none() {
            return self.set_status("This tab isn't connected".to_string(), true);
        }
        if !self.engine().capabilities().activity {
            return self.set_status("SQLite has no server sessions to show".to_string(), true);
        }
        self.activity = Some(ActivityView {
            rows: Vec::new(),
            selected: 0,
            error: None,
            confirm: None,
            loading: false,
            refreshed: None,
        });
        self.refresh_activity();
    }

    fn refresh_activity(&mut self) {
        let Some(db) = self.connection.database.clone() else { return };
        let Some(view) = &mut self.activity else { return };
        if view.loading {
            return;
        }
        view.loading = true;
        let send = self.sender();
        tokio::spawn(async move {
            send(DbEventKind::ActivityLoaded(db.activity().await.map_err(|e| crate::db::error_message(&e))));
        });
    }

    /// Called every second: refreshes the open activity view.
    pub(super) fn activity_tick(&mut self) {
        if self.activity.as_ref().is_some_and(|v| v.refreshed.is_none_or(|t| t.elapsed() >= REFRESH)) {
            self.refresh_activity();
        }
    }

    pub(super) fn activity_loaded(&mut self, result: Result<Vec<Activity>, String>) {
        let Some(view) = &mut self.activity else { return };
        view.loading = false;
        view.refreshed = Some(Instant::now());
        match result {
            Ok(rows) => {
                // Keep the same session selected across refreshes.
                let keep = view.rows.get(view.selected).map(|a| a.id.clone());
                view.rows = rows;
                view.selected = keep
                    .and_then(|id| view.rows.iter().position(|a| a.id == id))
                    .unwrap_or(view.selected)
                    .min(view.rows.len().saturating_sub(1));
                view.error = None;
            }
            Err(e) => view.error = Some(e),
        }
    }

    pub(super) fn handle_activity_key(&mut self, key: KeyEvent) {
        let Some(view) = &mut self.activity else { return };
        if let Some((id, kill)) = view.confirm.take() {
            if matches!(key.code, KeyCode::Char('y' | 'Y')) {
                self.stop_session(id, kill);
            }
            return;
        }
        if let Some(i) = list::step(view.selected, view.rows.len(), key, Nav::LIST) {
            view.selected = i;
            return;
        }
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => self.activity = None,
            KeyCode::Char('r') => {
                view.refreshed = None;
                self.refresh_activity();
            }
            KeyCode::Enter => {
                if let Some(a) = view.rows.get(view.selected) {
                    let title = format!("SESSION {} · {}@{}", a.id, a.user, a.client);
                    let text = if a.query.is_empty() { "(no query)".to_string() } else { a.query.clone() };
                    self.cell_view = Some(CellView::new(title, text));
                }
            }
            KeyCode::Char(c @ ('c' | 'x')) => {
                if self.connection.mode == ConnectionMode::ReadOnly {
                    return self.set_status("Read-only connection: not stopping other sessions".to_string(), true);
                }
                let Some(view) = &mut self.activity else { return };
                if let Some(a) = view.rows.get(view.selected) {
                    view.confirm = Some((a.id.clone(), c == 'x'));
                }
            }
            _ => {}
        }
    }

    fn stop_session(&mut self, id: String, kill: bool) {
        let Some(db) = self.connection.database.clone() else { return };
        let what = if kill { "Ended session" } else { "Cancelled the query of session" };
        let send = self.sender();
        tokio::spawn(async move {
            let result = db.stop_session(&id, kill).await.map(|()| format!("{what} {id}"));
            send(DbEventKind::SessionStopped(result.map_err(|e| crate::db::error_message(&e))));
        });
    }

    pub(super) fn session_stopped(&mut self, result: Result<String, String>) {
        match result {
            Ok(msg) => {
                self.log(msg.clone(), false);
                self.set_status(msg, false);
            }
            Err(e) => self.set_status(format!("Couldn't stop it: {e}"), true),
        }
        if let Some(view) = &mut self.activity {
            view.refreshed = None;
        }
        self.refresh_activity();
    }

    /// Shift+F7: runs the statement under EXPLAIN ANALYZE, showing actual
    /// times and rows. Writes run in a transaction (or savepoint) that's
    /// rolled back, after a confirmation.
    pub(super) fn explain_analyze_current(&mut self) {
        if self.run.executing || self.connection.database.is_none() {
            return;
        }
        let engine = self.engine();
        let Some((stmt, line)) = self.statement_to_explain() else {
            return self.set_status("No statement at cursor".to_string(), true);
        };
        let version = self.connection.database.as_ref().map_or("", |db| db.version());
        let Some(explain) = engine.dialect().explain_analyze(&stmt, version) else {
            let server = if engine.is_file_based() { engine.label() } else { "This server" };
            self.set_status(format!("{server} can't EXPLAIN ANALYZE this statement; showing the plan instead"), false);
            return self.explain_current();
        };
        // Tracking uses the EXPLAIN alone, so the wrapper doesn't look
        // like the user's own transaction ending.
        let infos = analyze::analyze(&parse(&explain, engine));
        let writes = analyze::analyze(&parse(&stmt, engine)).iter().any(StatementInfo::modifies);
        self.plan_request = Some((stmt.clone(), true, line));
        if !writes {
            return self.run_now(explain, 0, infos, RunPurpose::Explain, None);
        }
        let sql = if self.connection.txn == TxnState::Idle {
            format!("BEGIN;\n{explain};\nROLLBACK")
        } else {
            format!("SAVEPOINT podelis_explain;\n{explain};\nROLLBACK TO SAVEPOINT podelis_explain")
        };
        let action = ConfirmAction::Run { sql, offset: 0, infos, purpose: RunPurpose::Explain, filled: None };
        self.confirm = Some(Confirm {
            message: "EXPLAIN ANALYZE runs the statement. Its changes are rolled back afterwards. Run it?".into(),
            details: vec![stmt.lines().next().unwrap_or_default().to_string()],
            danger: self.connection.mode == ConnectionMode::Production,
            action,
            offer_transaction: false,
        });
    }
}
