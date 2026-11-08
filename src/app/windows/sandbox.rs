//! The sandbox's window (`Ctrl+K x` while one is open): its steps, what
//! they changed per table, and how to end it. The sandbox itself is
//! `app::sandbox`.

use crate::app::*;

/// The sandbox window (`Ctrl+K x` while one is open).
#[derive(Debug, Default)]
pub struct SandboxView {
    pub scroll: u16,
    /// What it holds: the locks the session has (asked when the window
    /// opens), or why that can't be seen.
    pub locks: Option<String>,
}

impl App {
    /// Ctrl+K x while a sandbox is open: its window, and (asked of the
    /// server meanwhile) the locks it holds.
    pub(in crate::app) fn open_sandbox_view(&mut self) {
        self.popups.sandbox_view = Some(SandboxView::default());
        let engine = self.engine();
        if engine.is_file_based() {
            let copied = self.connection.sandbox.as_ref().is_some_and(|s| s.copy.is_some());
            let locks = if copied { t!("sandbox.on_a_copy") } else { t!("sandbox.file_lock") };
            return self.sandbox_locks(locks.to_string());
        }
        let (Some(db), Some(query)) =
            (self.connection.database.clone(), engine.dialect().and_then(crate::db::SqlDialect::locks_held_query))
        else {
            return;
        };
        // (Not while a query runs on the session: the window says nothing.)
        if self.conn_busy() {
            return;
        }
        let send = self.sender();
        tokio::spawn(async move {
            let said = match db.run_once(query, 1).await {
                Ok(o) => {
                    let n = o.rows.first().and_then(|r| r.first().cloned().flatten()).unwrap_or_default();
                    tn!("sandbox.locks", n.parse::<u64>().unwrap_or(0))
                }
                Err(e) => t!("sandbox.locks_unseen", e = crate::db::error_message(&e)),
            };
            send(Background::SandboxLocks(said).into());
        });
    }

    pub(in crate::app) fn handle_sandbox_key(&mut self, key: KeyEvent) {
        let Some(view) = &mut self.popups.sandbox_view else { return };
        match key.code {
            KeyCode::Esc => self.popups.sandbox_view = None,
            KeyCode::Down | KeyCode::Char('j') => view.scroll = view.scroll.saturating_add(1),
            KeyCode::Up | KeyCode::Char('k') => view.scroll = view.scroll.saturating_sub(1),
            KeyCode::Char('u') => {
                self.popups.sandbox_view = None;
                self.undo_sandbox_step();
            }
            KeyCode::Char('d') => {
                self.popups.sandbox_view = None;
                self.end_transaction(TxEnd::Rollback);
            }
            KeyCode::Char('s') => {
                self.popups.sandbox_view = None;
                self.keep_sandbox_as_script();
            }
            KeyCode::Char('r') => {
                self.popups.sandbox_view = None;
                self.run_sandbox_for_real();
            }
            _ => {}
        }
    }
}
