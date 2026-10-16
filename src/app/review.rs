//! Running writes in a transaction to check them before committing: the
//! confirmation dialog's `t`, or every write in the manual-commit and
//! rollback-only transaction modes; then commit or roll back.

use super::*;
pub use crate::db::TxMode;
use crate::keys::Action;
use crate::params::Filled;

/// A dry run, rolled back when it ends.
#[derive(Debug)]
pub(super) enum DryRun {
    Running,
    /// It changed this many rows; now being rolled back.
    Done(u64),
    /// It failed with this message; now being rolled back.
    Failed(String),
}

/// "N rows changed, not committed yet": commit or roll back?
pub struct Review {
    pub rows_affected: u64,
}

impl App {
    /// Runs `sql` after a `BEGIN`, so its changes wait for commit/rollback
    /// (or, in a dry run, are rolled back as soon as it finishes).
    pub(super) fn run_in_transaction(&mut self, sql: &str, offset: usize, filled: Option<Filled>, purpose: RunPurpose) {
        let original = filled.as_ref().map_or_else(|| sql.to_string(), |f| f.original.clone());
        let wrapped = Filled::prefixed(filled, &original, "BEGIN;\n");
        let infos = analyze::analyze(&parse(&wrapped.sql, self.engine()));
        if self.tx_mode == TxMode::RollbackOnly {
            self.run_extra.dry_run = Some(DryRun::Running);
        } else {
            self.run_extra.reviewing = true;
        }
        self.run_now(wrapped.sql.clone(), offset, infos, purpose, Some(wrapped));
    }

    /// Runs a checked (and confirmed) script: writes go through a transaction
    /// in the review and dry-run modes.
    pub(super) fn run_writes(
        &mut self,
        sql: String,
        offset: usize,
        infos: Vec<StatementInfo>,
        purpose: RunPurpose,
        filled: Option<Filled>,
    ) {
        let wrap = self.tx_mode != TxMode::AutoCommit
            && self.txn == TxnState::Idle
            && infos.iter().any(StatementInfo::modifies)
            && matches!(purpose, RunPurpose::Query | RunPurpose::RowChange(_) | RunPurpose::CellEdit { .. });
        if wrap {
            self.run_in_transaction(&sql, offset, filled, purpose);
        } else {
            self.run_now(sql, offset, infos, purpose, filled);
        }
    }

    /// Why the write mode can't run `infos` (None: it can).
    pub(super) fn tx_mode_refusal(&self, infos: &[StatementInfo], purpose: &RunPurpose) -> Option<String> {
        let mode = self.tx_mode;
        if mode == TxMode::AutoCommit || self.txn != TxnState::Idle || !infos.iter().any(StatementInfo::modifies) {
            return None;
        }
        let switch = self.keymap.label(Action::TxMode);
        let label = if mode == TxMode::RollbackOnly { "Rollback-only" } else { "Manual commit" };
        if infos.iter().any(|i| matches!(i.kind, Kind::Transaction(_))) {
            return Some(format!(
                "{label}: the script has its own BEGIN/COMMIT. Remove it, or switch to auto-commit ({switch})"
            ));
        }
        if self.engine().capabilities().ddl_commits && infos.iter().any(|i| i.kind == Kind::Ddl) {
            let engine = self.engine().label();
            return Some(format!(
                "{label}: {engine} saves schema changes at once, so they can't be rolled back. Switch to auto-commit ({switch}) to run them"
            ));
        }
        if mode == TxMode::RollbackOnly && matches!(purpose, RunPurpose::CellEdit { .. }) {
            return Some(format!(
                "Rollback-only: edits would be rolled back. Switch to auto-commit or manual commit ({switch}) to edit"
            ));
        }
        None
    }

    /// Whether the write mode can change now; if not, says why.
    fn tx_mode_changeable(&mut self) -> bool {
        let why = if !self.connected() {
            self.not_connected_message()
        } else if self.active_mode == ConnectionMode::ReadOnly {
            "Read-only connection: writes are refused anyway".to_string()
        } else if self.txn != TxnState::Idle {
            format!(
                "Finish the open transaction first: {} commits, {} rolls back",
                self.keymap.label(Action::Commit),
                self.keymap.label(Action::Rollback)
            )
        } else {
            return true;
        };
        self.set_status(why, true);
        false
    }

    /// Ctrl+K x: pick auto-commit, manual commit or rollback-only.
    pub(super) fn open_tx_picker(&mut self) {
        if self.tx_mode_changeable() {
            self.tx_picker = Some(self.tx_mode);
        }
    }

    /// Clicking the modes in the editor's footer: auto-commit → manual →
    /// rollback-only.
    pub(super) fn cycle_tx_mode(&mut self) {
        if self.tx_mode_changeable() {
            self.set_tx_mode(self.tx_mode.next());
        }
    }

    /// `s` / `r` / `d` (or ↑↓ and Enter) pick; Esc keeps the mode.
    pub(super) fn handle_tx_picker_key(&mut self, key: KeyEvent) {
        let Some(highlighted) = self.tx_picker else { return };
        let chosen = match key.code {
            KeyCode::Esc => {
                self.tx_picker = None;
                return;
            }
            KeyCode::Down | KeyCode::Tab => return self.tx_picker = Some(highlighted.next()),
            KeyCode::Up | KeyCode::BackTab => return self.tx_picker = Some(highlighted.next().next()),
            KeyCode::Enter => highlighted,
            KeyCode::Char(c) => match TxMode::ALL.iter().find(|m| m.letter() == c.to_ascii_lowercase()) {
                Some(mode) => *mode,
                None => return,
            },
            _ => return,
        };
        self.tx_picker = None;
        self.set_tx_mode(chosen);
    }

    fn set_tx_mode(&mut self, mode: TxMode) {
        self.tx_mode = mode;
        let msg = match self.tx_mode {
            TxMode::AutoCommit => "Auto-commit: each statement is committed as it runs".to_string(),
            TxMode::Manual => format!(
                "Manual commit: changes wait for COMMIT ({}) or ROLLBACK ({})",
                self.keymap.label(Action::Commit),
                self.keymap.label(Action::Rollback)
            ),
            TxMode::RollbackOnly => {
                "Rollback-only: statements run, show what they change, and are always rolled back".to_string()
            }
        };
        self.set_status(msg, false);
    }

    pub(super) fn open_review(&mut self, rows_affected: u64) {
        if self.txn == TxnState::Idle {
            return;
        }
        self.review = Some(Review { rows_affected });
    }

    pub(super) fn handle_review_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('c' | 'C') => {
                self.review = None;
                self.end_transaction("COMMIT");
            }
            KeyCode::Char('r' | 'R') => {
                self.review = None;
                self.end_transaction("ROLLBACK");
            }
            KeyCode::Esc => {
                self.review = None;
                let msg = format!(
                    "Transaction left open: check with SELECTs, then {} commits, {} rolls back",
                    self.keymap.label(Action::Commit),
                    self.keymap.label(Action::Rollback)
                );
                self.set_status(msg, false);
            }
            _ => {}
        }
    }

    /// Commits or rolls back the open transaction, keeping the results.
    pub(super) fn end_transaction(&mut self, command: &'static str) {
        if self.executing || self.database.is_none() {
            return;
        }
        if self.txn == TxnState::Idle {
            return self.set_status("No transaction is open".to_string(), true);
        }
        let infos = analyze::analyze(&parse(command, self.engine()));
        self.run_now(command.to_string(), 0, infos, RunPurpose::TxControl(command), None);
    }

    /// A dry run finished: roll it back.
    pub(super) fn end_dry_run(&mut self, outcome: DryRun) {
        self.run_extra.dry_run = Some(outcome);
        if self.txn == TxnState::Idle {
            // Nothing to roll back (e.g. the connection was reopened).
            self.run_extra.dry_run = None;
            return;
        }
        self.end_transaction("ROLLBACK");
    }

    pub(super) fn transaction_ended(&mut self, command: &str) {
        if let Some(dry_run) = self.run_extra.dry_run.take() {
            self.tab.grid.result_note = Some("rolled back (rollback-only)".to_string());
            return match dry_run {
                DryRun::Failed(error) => self.set_status(error, true),
                DryRun::Running => {}
                DryRun::Done(rows) => {
                    let msg = format!("Rollback-only: {rows} row(s) changed, then rolled back");
                    self.log(msg.clone(), false);
                    self.set_status(msg, false);
                }
            };
        }
        let msg = if command == "COMMIT" { "Committed" } else { "Rolled back" };
        // The shown "N rows affected" no longer stands after a rollback.
        if command == "ROLLBACK" {
            self.tab.grid.result_note = Some("rolled back".to_string());
        }
        self.log(msg.to_string(), false);
        self.set_status(msg.to_string(), false);
    }
}
