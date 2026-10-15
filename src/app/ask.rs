//! Asking in plain language (`Ctrl+K q`): a question about the tab's
//! connection goes to the gateway, which has a model write the SQL over
//! the tables that matter; the SQL lands at the end of the editor, with
//! the question above it, and never runs by itself.
//!
//! A model that isn't on this machine sees the project's schema only if
//! the project shares it: otherwise the gateway refuses, and a question
//! asks the user (`y` this once, `a` always for the project).

use super::*;
use crate::gateway::client::{CallError, connect_or_start, spawn_gateway};
use crate::gateway::protocol::code;

/// A question, and where it was asked: the tab (where the SQL goes) and
/// its saved connection then. Kept whole, so asking it again (after the
/// user allowed the schema to go) asks about the same thing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Question {
    pub text: String,
    pub tab: usize,
    pub connection: String,
}

/// A question on its way to a model.
pub struct Asking {
    /// Which ask this is: events of an earlier one are ignored.
    pub id: u64,
    pub question: Question,
    /// How much of the answer has come.
    pub chars: usize,
    pub started: Instant,
    task: tokio::task::JoinHandle<()>,
}

/// Why an ask came back without SQL.
#[derive(Debug, Clone)]
pub struct AskFailure {
    pub code: Option<i64>,
    pub message: String,
    pub data: Option<serde_json::Value>,
}

impl App {
    /// `Ctrl+K q`: asks for the question, for the tab's saved connection.
    pub(super) fn open_ask_prompt(&mut self) {
        if let Some(why) = self.why_cant_ask() {
            return self.set_status(why, true);
        }
        self.ask_prompt = Some(LineInput::new(String::new()));
    }

    /// Why asking can't start here, if it can't.
    fn why_cant_ask(&self) -> Option<String> {
        if self.asking.is_some() {
            return Some("A question is being answered: wait for it, or Esc stops it".into());
        }
        if !self.connected() {
            return Some(self.not_connected_message());
        }
        let saved = self.project().is_some_and(|p| p.connections.iter().any(|c| c.name == self.connect_form.name));
        if !saved {
            return Some("Asking works on saved connections (the gateway opens it itself): save this one first".into());
        }
        None
    }

    pub(super) fn handle_ask_prompt_key(&mut self, key: KeyEvent) {
        let Some(input) = &mut self.ask_prompt else { return };
        match input.handle(key) {
            InputResult::Cancel => self.ask_prompt = None,
            InputResult::Submit => {
                let Some(text) = self.ask_prompt.take().map(|q| q.text().trim().to_string()) else { return };
                if !text.is_empty() {
                    let question = Question { text, tab: self.active_tab, connection: self.connect_form.name.clone() };
                    self.start_ask(question, false);
                }
            }
            InputResult::Changed | InputResult::Moved | InputResult::Ignored => {}
        }
    }

    /// Sends `question` to the gateway (started if it isn't running);
    /// `share_once`: the user allowed the schema to go to the model.
    fn start_ask(&mut self, question: Question, share_once: bool) {
        if question.tab >= self.tabs.len() {
            return self.set_status("The tab the question was about is closed".into(), true);
        }
        // By id: two projects' names may differ only in case.
        let project = self.current_project.as_ref().map(|id| format!("id:{id}")).unwrap_or_default();
        let connection = question.connection.clone();
        let config_dir = self.data_dir.clone().unwrap_or_else(crate::store::config_dir);
        self.ask_count += 1;
        let id = self.ask_count;
        let tx = self.db_tx.clone();
        let generation = self.generation;
        let send = move |kind| {
            let _ = tx.send(DbEvent { generation, run: None, kind });
        };
        // Where the app runs: a relative SQLite path is from here.
        let dir = std::env::current_dir().ok().map(|d| d.display().to_string());
        let mut params =
            serde_json::json!({ "project": project, "connection": connection, "question": question.text, "dir": dir });
        if share_once {
            params["share_once"] = serde_json::json!(true);
        }
        let task = tokio::spawn(async move {
            let started = connect_or_start(&config_dir, "podelis", || spawn_gateway(&config_dir)).await;
            let result = match started {
                Ok(mut client) => {
                    client
                        .call_streaming("ask", params, |method, note| {
                            if method == "ask.delta" {
                                let chars = note["text"].as_str().map_or(0, |t| t.chars().count());
                                send(DbEventKind::AskProgress(id, chars));
                            }
                        })
                        .await
                }
                Err(e) => Err(e),
            };
            let result = result.map_err(|e| match e {
                CallError::Rpc(e) => AskFailure { code: Some(e.code), message: e.message, data: e.data },
                CallError::Io(e) => {
                    AskFailure { code: None, message: format!("the gateway couldn't be reached: {e}"), data: None }
                }
            });
            send(DbEventKind::AskDone(id, Box::new(result)));
        });
        self.asking = Some(Asking { id, question, chars: 0, started: Instant::now(), task });
        self.say_asking("Asking… the SQL comes to the editor (Esc stops it)".into());
    }

    /// Progress goes to the status bar, not a pane's footer (which keeps a
    /// message until the next key, and would still say "Asking…" after).
    fn say_asking(&mut self, status: String) {
        self.editor_note = None;
        self.results_note = None;
        self.status = status;
        self.status_is_error = false;
        self.status_in_results = false;
    }

    /// Tab `closing` is about to close: a question asked from it stops,
    /// and one asked from a tab after it follows that tab's new place.
    pub(super) fn tab_closing(&mut self, closing: usize) {
        if self.asking.as_ref().is_some_and(|a| a.question.tab == closing)
            || matches!(&self.pending_share, Some(Confirm { action: ConfirmAction::ShareSchema { question, .. }, .. }) if question.tab == closing)
        {
            return self.stop_asking();
        }
        let shift = |tab: &mut usize| {
            if *tab > closing {
                *tab -= 1;
            }
        };
        if let Some(asking) = &mut self.asking {
            shift(&mut asking.question.tab);
        }
        if let Some(Confirm { action: ConfirmAction::ShareSchema { question, .. }, .. }) = &mut self.pending_share {
            shift(&mut question.tab);
        }
    }

    /// Esc while asking: stops waiting (the gateway's answer is dropped).
    pub(super) fn stop_asking(&mut self) {
        // A sharing question waiting to be shown goes too: its tab, or its
        // project, is no longer the one it was about.
        self.pending_share = None;
        if let Some(ConfirmAction::ShareSchema { .. }) = self.confirm.as_ref().map(|c| &c.action) {
            self.confirm = None;
        }
        if let Some(asking) = self.asking.take() {
            asking.task.abort();
            self.set_status("Stopped asking".into(), false);
        }
    }

    pub(super) fn ask_progress(&mut self, id: u64, chars: usize) {
        let Some(asking) = self.asking.as_mut().filter(|a| a.id == id) else { return };
        asking.chars += chars;
        let secs = asking.started.elapsed().as_secs();
        let status = format!("Asking… {} characters written, {secs} s (Esc stops it)", asking.chars);
        self.say_asking(status);
    }

    pub(super) fn ask_done(&mut self, id: u64, result: Result<serde_json::Value, AskFailure>) {
        if self.asking.as_ref().is_none_or(|a| a.id != id) {
            return;
        }
        let Some(asking) = self.asking.take() else { return };
        self.say_asking(String::new());
        match result {
            Ok(answer) => {
                // Closing a tab or leaving the project stops asking, so the
                // tab is still the one asked from.
                if asking.question.tab < self.tabs.len() {
                    self.in_tab(asking.question.tab, |app| app.insert_answer(&asking.question.text, &answer));
                }
            }
            Err(e) if e.code == Some(code::NOT_SHARED) => {
                let project = e.data.as_ref().and_then(|d| d["project"].as_str()).unwrap_or("this project").to_string();
                // Shown once no other confirmation is open (`share_tick`).
                self.pending_share = Some(Confirm {
                    message: format!("Send {project}'s table and column names to the model?"),
                    details: vec![
                        e.message,
                        "Rows are never sent; only the tables that matter for the question.".into(),
                    ],
                    danger: false,
                    action: ConfirmAction::ShareSchema {
                        question: asking.question,
                        project_id: e.data.and_then(|d| d["project_id"].as_str().map(str::to_string)),
                    },
                    offer_transaction: false,
                });
            }
            Err(e) => {
                let status = format!("Couldn't ask: {}", e.message);
                self.log(status.clone(), true);
                self.set_status(status, true);
            }
        }
        self.share_tick();
    }

    /// The sharing question, once no other confirmation is open (one the
    /// user is answering, or an agent's write, isn't replaced).
    pub(super) fn share_tick(&mut self) {
        if self.confirm.is_none()
            && let Some(question) = self.pending_share.take()
        {
            // A question that came by itself: a moment before keys count.
            self.agent_asked = Some(Instant::now());
            self.confirm = Some(question);
        }
    }

    /// The answer to the sharing question: `always` saves it for the
    /// project, then the question goes again.
    pub(super) fn answer_share(&mut self, question: Question, project_id: Option<&str>, always: bool) {
        // Always: saved for the project (its own, or the open one if the
        // gateway didn't say), then asked as allowed.
        let id = project_id.map(str::to_string).or_else(|| self.current_project.clone()).filter(|_| always);
        let saved = match id {
            Some(id) => match self.store.set_share_schema(&id, crate::store::ShareSchema::All) {
                Ok(()) => true,
                Err(e) => return self.set_status(format!("Couldn't save it: {e:#}"), true),
            },
            None => false,
        };
        self.start_ask(question, !saved);
    }

    /// The SQL at the end of the editor, a blank line apart, under the
    /// question and what the model said, as comments; the cursor on it.
    fn insert_answer(&mut self, question: &str, answer: &serde_json::Value) {
        let model = answer["model"].as_str().unwrap_or("the model");
        let said = answer["said"].as_str().unwrap_or_default();
        let Some(sql) = answer["sql"].as_str() else {
            let why = if said.is_empty() { "it wrote no SQL".to_string() } else { one_line(said) };
            return self.set_status(format!("{model}: {why}"), true);
        };
        let mut block = format!("-- {}\n", one_line(question));
        for line in wrap_comment(said, 76) {
            let _ = writeln!(block, "-- {line}");
        }
        let problems: Vec<&str> =
            answer["problems"].as_array().into_iter().flatten().filter_map(serde_json::Value::as_str).collect();
        if !problems.is_empty() {
            let _ = writeln!(block, "-- CHECK: {}", problems.join("; "));
        }
        let assumed: Vec<String> = answer["suggested"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|f| {
                format!(
                    "{} {} {}",
                    f["subject"].as_str().unwrap_or("?"),
                    f["relation"].as_str().unwrap_or("?"),
                    f["value"].as_str().unwrap_or("?")
                )
            })
            .collect();
        for a in &assumed {
            let _ = writeln!(block, "-- assumed: {a} (Ctrl+K k: confirm or forget)");
        }
        block.push_str(&ended(sql.trim_end(), self.engine()));
        self.append_statement(&block);
        let tokens = &answer["usage"];
        let mut status = format!(
            "SQL from {model} ({} tokens in, {} out): read it, then F6 runs it — nothing ran",
            tokens["input_tokens"], tokens["output_tokens"]
        );
        if !assumed.is_empty() {
            let n = assumed.len();
            let _ = write!(status, " · it assumed {n} thing{}: Ctrl+K k to confirm", if n == 1 { "" } else { "s" });
        }
        if !problems.is_empty() {
            status = format!("SQL from {model}: check it — {}", problems.join("; "));
        }
        self.set_status(status, !problems.is_empty());
    }

    /// Adds `text` after everything in the editor, a blank line apart,
    /// with the cursor at its end (one undo takes it back). The text there
    /// is kept as it is, except that a last statement left without its
    /// `;` gets one, so the two don't run together (not in a MySQL script
    /// with `DELIMITER`, where `;` means something else).
    pub(super) fn append_statement(&mut self, text: &str) {
        let existing = self.tab.editor.text();
        let delimiter = existing.lines().any(|l| l.trim_start().to_ascii_uppercase().starts_with("DELIMITER "));
        let existing = if delimiter { existing } else { ended(&existing, self.engine()) };
        let gap = if existing.trim().is_empty() || existing.ends_with("\n\n") {
            ""
        } else if existing.ends_with('\n') {
            "\n"
        } else {
            "\n\n"
        };
        let new = format!("{existing}{gap}{text}");
        self.tab.editor.replace_text(&new, new.len());
        self.edited();
        self.focus = Focus::Editor;
    }
}

/// `sql` with its last statement ended: a `;` right after its code if it
/// has none (before a comment after it: one added after a `--` comment
/// would be part of it).
fn ended(sql: &str, engine: crate::db::Engine) -> String {
    use crate::sql::TokenKind;
    let tokens = crate::sql::tokenize(sql, engine);
    match tokens.iter().rev().find(|t| !matches!(t.kind, TokenKind::Whitespace | TokenKind::Comment)) {
        Some(t) if t.kind != TokenKind::Semicolon => format!("{};{}", &sql[..t.range.end], &sql[t.range.end..]),
        _ => sql.to_string(),
    }
}

/// `text` in lines of at most `width` characters, at spaces.
fn wrap_comment(text: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        if !line.is_empty() && line.chars().count() + word.chars().count() + 1 > width {
            lines.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
}

#[cfg(test)]
mod tests {
    use crate::db::Engine;

    #[test]
    fn a_statement_is_ended_before_a_comment_after_it() {
        assert_eq!(super::ended("SELECT 1\n-- per country", Engine::Postgres), "SELECT 1;\n-- per country");
        assert_eq!(super::ended("SELECT 1; -- done", Engine::Postgres), "SELECT 1; -- done");
        assert_eq!(super::ended("-- only", Engine::Postgres), "-- only");
    }
}
