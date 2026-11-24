//! The y/n questions (`Confirm`): what each one does once answered.

use super::*;
use crate::config::store::ProjectId;

/// What a key answers to a y/n question: `y` yes, `n` or Esc no (either
/// case); anything else isn't an answer.
pub(in crate::app) fn answer(key: KeyEvent) -> Option<bool> {
    match key.code {
        KeyCode::Char('y' | 'Y') => Some(true),
        KeyCode::Char('n' | 'N') | KeyCode::Esc => Some(false),
        _ => None,
    }
}

/// Whether a key answers yes: where anything else means no.
pub(in crate::app) fn is_yes(key: KeyEvent) -> bool {
    answer(key) == Some(true)
}

/// Something to do once the user answers "y".
pub enum ConfirmAction {
    /// A read on production that would read a whole big table: run it as
    /// it is, without asking again.
    RunHeavy {
        sql: String,
        offset: usize,
        infos: Vec<StatementInfo>,
        filled: Option<Filled>,
    },
    Run {
        sql: String,
        offset: usize,
        infos: Vec<StatementInfo>,
        purpose: RunPurpose,
        filled: Option<Filled>,
    },
    /// An export that may be big: start it; it asks next after this size.
    /// Of these rows: a run's id, as they may have changed by the answer.
    /// The file is the export form's `name`, else a new one.
    ExportAll {
        format: Format,
        agreed: u64,
        run: RunId,
        name: Option<std::path::PathBuf>,
    },
    /// An export that grew to a size to ask about: go on (else it stops).
    KeepExporting,
    Disconnect,
    /// Close an open connection (`Ctrl+K j`, `Ctrl+D`): every tab on it
    /// is disconnected.
    CloseConnection(ConnId),
    /// Every open connection (Ctrl+X in the connections list).
    CloseAllConnections,
    Quit,
    CloseTab,
    /// Leave the project, for another one (or the project list); `true`
    /// lands in its connections.
    LeaveProject(Option<ProjectId>, bool),
    /// An agent's write waiting for the user: y approves it, anything else
    /// says no.
    AgentWrite(crate::agents::Request),
    /// The project doesn't share its schema with the model asked: `y`
    /// sends it this once, `a` always for the project.
    ShareSchema {
        question: query_screen::ask::Question,
        project_id: Option<ProjectId>,
    },
}

pub struct Confirm {
    pub message: String,
    pub details: Vec<String>,
    /// Style the dialog as dangerous (production, destructive statements).
    pub danger: bool,
    pub action: ConfirmAction,
    /// Offer `t`: run the writes in a transaction to review before committing.
    pub offer_transaction: bool,
}

impl App {
    /// A key while a confirmation is open: what it answers. `quit_key`:
    /// Ctrl+C or Ctrl+Q, which confirm quitting.
    pub(in crate::app) fn answer_confirm(&mut self, confirm: Confirm, key: KeyEvent, quit_key: bool) {
        // A second Ctrl+C or Ctrl+Q confirms quitting.
        if confirm.offer_transaction
            && matches!(key.code, KeyCode::Char('t' | 'T'))
            && let ConfirmAction::Run { sql, offset, filled, .. } = confirm.action
        {
            return self.run_in_transaction(&sql, offset, filled, RunPurpose::Query);
        }
        let yes = is_yes(key) || (quit_key && matches!(confirm.action, ConfirmAction::Quit));
        // An export's size is asked about by itself, maybe while typing, and
        // no would throw its file away: only y, or n / Esc, and not at once.
        if matches!(confirm.action, ConfirmAction::KeepExporting | ConfirmAction::ExportAll { .. }) {
            // Ctrl+Q quits, not held up by it (Ctrl+C, which copies, mustn't:
            // it comes by itself, maybe while copying). Kept from quitting, a
            // paused export asks again: it's still waiting.
            if quit_key && key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('q') {
                return self.request_quit();
            }
            let settled = self.exports.asked.is_none_or(|at| at.elapsed() >= AGENT_SETTLE);
            let answers = yes || answer(key).is_some();
            if !(settled && answers) {
                self.popups.confirm = Some(confirm);
                return;
            }
        }
        if let ConfirmAction::AgentWrite(request) = &confirm.action {
            // Only y, or n / Esc, answer; and not in the first moment
            // (a key being typed as it appeared).
            let settled = self.agents.asked.is_none_or(|at| at.elapsed() >= AGENT_SETTLE);
            match answer(key) {
                Some(approve) if settled => {
                    let request = request.clone();
                    return self.answer_agent(&request, approve);
                }
                _ => {
                    self.popups.confirm = Some(confirm);
                    return;
                }
            }
        }
        if let ConfirmAction::ShareSchema { question, project_id } = &confirm.action {
            // It came by itself, maybe while typing: only y, a, or n / Esc
            // answer it, and not in its first moment (as an agent's write).
            let settled = self.agents.asked.is_none_or(|at| at.elapsed() >= AGENT_SETTLE);
            // `a`: yes, and always.
            let share = match key.code {
                KeyCode::Char('a' | 'A') => Some(Some(true)),
                _ => answer(key).map(|yes| yes.then_some(false)),
            };
            match share {
                Some(Some(always)) if settled => {
                    return self.answer_share(question.clone(), project_id.as_ref(), always);
                }
                Some(None) if settled => return self.set_status(t!("app.not_sent").into(), false),
                _ => {
                    self.popups.confirm = Some(confirm);
                    return;
                }
            }
        }
        if !yes {
            if matches!(confirm.action, ConfirmAction::KeepExporting) {
                return self.stop_export();
            }
            self.tab.run.queued_plan = None;
            self.set_status(t!("run.cancelled").to_string(), false);
            return;
        }
        match confirm.action {
            ConfirmAction::Run { sql, offset, infos, purpose, filled } => {
                self.run_writes(sql, offset, infos, purpose, filled);
            }
            ConfirmAction::RunHeavy { sql, offset, infos, filled } => {
                self.run_writes(sql, offset, infos, RunPurpose::Query, filled);
            }
            ConfirmAction::ExportAll { format, agreed, run, name } => {
                if run == self.tab.run.id {
                    self.start_export_all(format, agreed, name);
                } else {
                    self.set_status(t!("export.other_tab").to_string(), true);
                }
            }
            ConfirmAction::KeepExporting => self.keep_exporting(),
            ConfirmAction::Disconnect => self.disconnect(),
            ConfirmAction::CloseConnection(id) => self.close_connection(id),
            ConfirmAction::CloseAllConnections => self.close_every_connection(),
            ConfirmAction::Quit => self.should_quit = true,
            ConfirmAction::CloseTab => self.close_tab(),
            ConfirmAction::LeaveProject(target, panes) => self.go_to_project(target, panes),
            // Answered above.
            ConfirmAction::AgentWrite(_) | ConfirmAction::ShareSchema { .. } => {}
        }
    }
}
