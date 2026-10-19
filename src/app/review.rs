//! Running writes in a transaction to check them before committing: the
//! confirmation dialog's `t`, or every write in the manual-commit and
//! rollback-only transaction modes; then commit or roll back.

use super::*;
use crate::config::keys::Action;
pub use crate::db::TxMode;
use crate::sql::params::Filled;

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
        if self.connection.tx_mode == TxMode::RollbackOnly {
            self.run.extra.dry_run = Some(DryRun::Running);
        } else {
            self.run.extra.reviewing = true;
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
        let wrap = self.connection.tx_mode != TxMode::AutoCommit
            && self.connection.txn == TxnState::Idle
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
        let mode = self.connection.tx_mode;
        if mode == TxMode::AutoCommit
            || self.connection.txn != TxnState::Idle
            || !infos.iter().any(StatementInfo::modifies)
        {
            return None;
        }
        let switch = self.keymap.label(Action::TxMode);
        let label = if mode == TxMode::RollbackOnly { t!("review.rollback_only") } else { t!("review.manual") };
        if infos.iter().any(|i| matches!(i.kind, Kind::Transaction(_))) {
            return Some(t!("review.own_begin", label = label, switch = switch));
        }
        if self.engine().capabilities().ddl_commits && infos.iter().any(|i| i.kind == Kind::Ddl) {
            let engine = self.engine().label();
            return Some(t!("review.ddl_commits", label = label, engine = engine, switch = switch));
        }
        if mode == TxMode::RollbackOnly && matches!(purpose, RunPurpose::CellEdit { .. }) {
            return Some(t!("review.edits_rolled_back", switch = switch));
        }
        None
    }

    /// Whether the write mode can change now; if not, says why.
    fn tx_mode_changeable(&mut self) -> bool {
        let why = if !self.connected() {
            self.not_connected_message()
        } else if self.connection.mode == ConnectionMode::ReadOnly {
            t!("review.read_only").to_string()
        } else if self.connection.txn != TxnState::Idle {
            t!(
                "review.finish_first",
                commit = self.keymap.label(Action::Commit),
                rollback = self.keymap.label(Action::Rollback)
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
            self.tx_picker = Some(self.connection.tx_mode);
        }
    }

    /// Clicking the modes in the editor's footer: auto-commit → manual →
    /// rollback-only.
    pub(super) fn cycle_tx_mode(&mut self) {
        if self.tx_mode_changeable() {
            self.set_tx_mode(self.connection.tx_mode.next());
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
        self.connection.tx_mode = mode;
        let msg = match self.connection.tx_mode {
            TxMode::AutoCommit => t!("review.set_autocommit").to_string(),
            TxMode::Manual => t!(
                "review.set_manual",
                commit = self.keymap.label(Action::Commit),
                rollback = self.keymap.label(Action::Rollback)
            ),
            TxMode::RollbackOnly => t!("review.set_rollback_only").to_string(),
        };
        self.set_status(msg, false);
    }

    pub(super) fn open_review(&mut self, rows_affected: u64) {
        if self.connection.txn == TxnState::Idle {
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
        if self.run.executing || self.connection.database.is_none() {
            return;
        }
        if self.connection.txn == TxnState::Idle {
            return self.set_status(t!("review.none_open").to_string(), true);
        }
        let infos = analyze::analyze(&parse(command, self.engine()));
        self.run_now(command.to_string(), 0, infos, RunPurpose::TxControl(command), None);
    }

    /// A dry run finished: roll it back.
    pub(super) fn end_dry_run(&mut self, outcome: DryRun) {
        self.run.extra.dry_run = Some(outcome);
        if self.connection.txn == TxnState::Idle {
            // Nothing to roll back (e.g. the connection was reopened).
            self.run.extra.dry_run = None;
            return;
        }
        self.end_transaction("ROLLBACK");
    }

    pub(super) fn transaction_ended(&mut self, command: &str) {
        if let Some(dry_run) = self.run.extra.dry_run.take() {
            self.tab.grid.result_note = Some(t!("run.rolled_back").to_string());
            return match dry_run {
                DryRun::Failed(error) => self.set_status(error, true),
                DryRun::Running => {}
                DryRun::Done(rows) => {
                    let msg = tn!("review.dry_run_done", rows);
                    self.log(msg.clone(), false);
                    self.set_status(msg, false);
                }
            };
        }
        let msg = if command == "COMMIT" { t!("review.committed") } else { t!("review.rolled_back") };
        // The shown "N rows affected" no longer stands after a rollback.
        if command == "ROLLBACK" {
            self.tab.grid.result_note = Some(t!("review.rolled_back_note").to_string());
        }
        self.log(msg.to_string(), false);
        self.set_status(msg.to_string(), false);
    }
}
