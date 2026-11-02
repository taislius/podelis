//! The y/n questions (`Confirm`): what each one does once answered.

use super::*;

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
    Disconnect,
    /// Close an open connection (`Ctrl+K j`, `Ctrl+D`): every tab on it
    /// is disconnected.
    CloseConnection(u64),
    Quit,
    CloseTab,
    /// Leave the project, for another one (or the project list); `true`
    /// lands in its connections.
    LeaveProject(Option<String>, bool),
    /// An agent's write waiting for the user: y approves it, anything else
    /// says no.
    AgentWrite(crate::agents::Request),
    /// The project doesn't share its schema with the model asked: `y`
    /// sends it this once, `a` always for the project.
    ShareSchema {
        question: ask::Question,
        project_id: Option<String>,
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
        let yes =
            matches!(key.code, KeyCode::Char('y' | 'Y')) || (quit_key && matches!(confirm.action, ConfirmAction::Quit));
        if let ConfirmAction::AgentWrite(request) = &confirm.action {
            // Only y, or n / Esc, answer; and not in the first moment
            // (a key being typed as it appeared).
            let settled = self.agents.asked.is_none_or(|at| at.elapsed() >= AGENT_SETTLE);
            let answer = match key.code {
                KeyCode::Char('y' | 'Y') => Some(true),
                KeyCode::Char('n' | 'N') | KeyCode::Esc => Some(false),
                _ => None,
            };
            match answer {
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
            let answer = match key.code {
                KeyCode::Char('y' | 'Y') => Some(Some(false)),
                KeyCode::Char('a' | 'A') => Some(Some(true)),
                KeyCode::Char('n' | 'N') | KeyCode::Esc => Some(None),
                _ => None,
            };
            match answer {
                Some(Some(always)) if settled => {
                    return self.answer_share(question.clone(), project_id.as_deref(), always);
                }
                Some(None) if settled => return self.set_status(t!("app.not_sent").into(), false),
                _ => {
                    self.popups.confirm = Some(confirm);
                    return;
                }
            }
        }
        if !yes {
            self.tab.run.queued_plan = None;
            self.set_status("Cancelled".to_string(), false);
            return;
        }
        match confirm.action {
            ConfirmAction::Run { sql, offset, infos, purpose, filled } => {
                self.run_writes(sql, offset, infos, purpose, filled);
            }
            ConfirmAction::RunHeavy { sql, offset, infos, filled } => {
                self.run_writes(sql, offset, infos, RunPurpose::Query, filled);
            }
            ConfirmAction::Disconnect => self.disconnect(),
            ConfirmAction::CloseConnection(id) => self.close_connection(id),
            ConfirmAction::Quit => self.should_quit = true,
            ConfirmAction::CloseTab => self.close_tab(),
            ConfirmAction::LeaveProject(target, panes) => self.go_to_project(target, panes),
            // Answered above.
            ConfirmAction::AgentWrite(_) | ConfirmAction::ShareSchema { .. } => {}
        }
    }
}
