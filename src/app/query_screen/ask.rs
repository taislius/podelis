//! Asking in plain language (`Ctrl+K q`): a question about the tab's
//! connection goes to the gateway, which has a model write the SQL over
//! the tables that matter; the SQL lands at the end of the editor, with
//! the question above it, and never runs by itself.
//!
//! A question can also be written in the editor, among the SQL, as a
//! comment: `-- ? revenue per city` (or `--?`), or `/*? … */` over several
//! lines. `Ctrl+K q` with the cursor on it asks it, and the SQL comes right
//! under it, after a `-- ↳ <model>` line; asking again replaces that answer
//! (only one that starts so: SQL the user wrote there stays). Being
//! comments, they're left alone by the server, lint and running.
//!
//! A model that isn't on this machine sees the project's schema only if
//! the project shares it: otherwise the gateway refuses, and a question
//! asks the user (`y` this once, `a` always for the project).

use crate::app::*;
use crate::gateway::client::{CallError, connect_or_start, spawn_gateway};
use crate::gateway::protocol::code;

/// What `Ctrl+K q` keeps besides its prompt (a popup).
#[derive(Default)]
pub struct AskState {
    /// The question being answered.
    pub asking: Option<Asking>,
    /// The gateway has the schema this app last loaded: it was told to read
    /// it again since. Not at first (the gateway outlives the app, so it
    /// may keep a schema from before a change), nor after a reload here
    /// (`load_catalog`: after DDL, `r` in the navigator).
    pub(super) gateway_current: bool,
    /// Numbers the asks, so a stopped one's answer is dropped.
    pub(super) count: u64,
    /// The sharing question, waiting while another confirmation is open.
    pub(super) pending_share: Option<Confirm>,
}

/// A question, and where it was asked: the tab (where the SQL goes) and
/// its saved connection then. Kept whole, so asking it again (after the
/// user allowed the schema to go) asks about the same thing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Question {
    pub text: String,
    pub tab: usize,
    pub connection: String,
    /// `text` is SQL to explain in plain words, not a question.
    pub explain: bool,
    /// Written in the editor: its last line there (the answer goes under
    /// it), not asked in the pop-up.
    pub at: Option<usize>,
}

/// The mark after a question's comment opener on the first line of an
/// answer written under it: asking again replaces what follows it.
const ANSWER_MARK: &str = "↳";

/// A question written in the editor: what it asks, and its rows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Written {
    pub text: String,
    pub rows: std::ops::RangeInclusive<usize>,
}

/// The questions in `text`, as `engine`'s language reads its comments (so
/// never inside a string): a line comment alone on its line whose text
/// starts with `?` (`-- ? …`, `--?`, mongosh's `// ? …`), or a block
/// comment `/*? … */`, over as many lines as it likes.
pub(crate) fn questions(text: &str, engine: crate::db::Engine) -> Vec<Written> {
    use crate::language::TokenKind;
    let mut found = Vec::new();
    let (mut row, mut counted) = (0, 0);
    for token in crate::language::tokenize(text, engine) {
        if token.kind != TokenKind::Comment {
            continue;
        }
        row += text[counted..token.range.start].matches('\n').count();
        counted = token.range.start;
        let comment = &text[token.range.clone()];
        let last = row + comment.matches('\n').count();
        let line_start = text[..token.range.start].rfind('\n').map_or(0, |i| i + 1);
        let alone = text[line_start..token.range.start].trim().is_empty();
        if let Some(words) = asked(comment, alone) {
            found.push(Written { text: words, rows: row..=last });
        }
    }
    found
}

/// What a comment (its whole text, from `--`, `//` or `/*`) asks, if it's
/// a question: a block comment `/*? … */`, or a line comment `-- ? …`
/// that's `alone` on its line.
pub(crate) fn asked(comment: &str, alone: bool) -> Option<String> {
    let asked = if let Some(inner) = comment.strip_prefix("/*") {
        inner.strip_suffix("*/").unwrap_or(inner).strip_prefix('?')
    } else {
        comment.get(2..).map(str::trim_start).and_then(|c| c.strip_prefix('?')).filter(|_| alone)
    };
    Some(asked?.split_whitespace().collect::<Vec<_>>().join(" ")).filter(|words| !words.is_empty())
}

/// The question at `row`, if the cursor's line is (in) one: its text, and
/// its last row.
pub(crate) fn question_at(text: &str, engine: crate::db::Engine, row: usize) -> Option<(String, usize)> {
    questions(text, engine).into_iter().find(|q| q.rows.contains(&row)).map(|q| (q.text, *q.rows.end()))
}

/// Which of `rows` are (in) questions, for drawing them.
#[cfg(test)]
pub(crate) fn question_rows(text: &str, engine: crate::db::Engine, rows: std::ops::Range<usize>) -> Vec<bool> {
    let mut marked = vec![false; rows.len()];
    for q in questions(text, engine) {
        for row in q.rows.filter(|r| rows.contains(r)) {
            marked[row - rows.start] = true;
        }
    }
    marked
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
    /// `Ctrl+K q`: on a question written in the editor, asks it; anywhere
    /// else, starts one there (`-- ? `), to type and ask the same way.
    pub(super) fn open_ask_prompt(&mut self) {
        if let Some(why) = self.why_cant_ask() {
            return self.set_status(why, true);
        }
        // A question written in the editor, at the cursor: asked as it is.
        if self.focus == Focus::Editor
            && let Some((text, at)) = question_at(&self.tab.editor.text(), self.engine(), self.tab.editor.cursor_row)
        {
            let connection = self.connection.form.name.clone();
            let question = Question { text, tab: self.active_tab, connection, explain: false, at: Some(at) };
            return self.start_ask(question, false);
        }
        self.start_question();
    }

    /// A new question line in the editor, the cursor after its `?`: on
    /// the cursor's line when it's empty, else under it; at the end, from
    /// another pane. Then `Ctrl+K q` on it asks it.
    fn start_question(&mut self) {
        let marker = format!("{} ? ", self.comment_opener());
        let editor = &self.tab.editor;
        let last = editor.lines.len() - 1;
        let (rows, new, row) = if self.focus == Focus::Editor {
            let here = editor.cursor_row.min(last);
            if editor.lines[here].trim().is_empty() {
                (here..here + 1, vec![marker.clone()], here)
            } else {
                (here + 1..here + 1, vec![marker.clone()], here + 1)
            }
        } else {
            // From another pane: after everything, a blank line apart.
            let keep = editor.lines.iter().rposition(|l| !l.trim().is_empty()).map_or(0, |i| i + 1);
            let new = if keep == 0 { vec![marker.clone()] } else { vec![String::new(), marker.clone()] };
            let row = keep + new.len() - 1;
            (keep..editor.lines.len(), new, row)
        };
        self.tab.editor.replace_rows(rows, new, (row, marker.chars().count()));
        self.edited();
        self.focus = Focus::Editor;
        let key = self.keymap.label(crate::config::keys::Action::Ask);
        self.set_status(t!("ask.type_question", key = key), false);
    }

    /// The row after an earlier answer under the question ending at `row`
    /// (`row + 1` if there's none): its mark, the comments after it, and
    /// the one statement that follows them, to its `;`. What comes after
    /// that, even with no blank line between, is the user's and stays.
    fn answer_end(&self, row: usize, written: &[Written]) -> usize {
        let opener = self.comment_opener();
        let lines = &self.tab.editor.lines;
        let mark = format!("{opener} {ANSWER_MARK}");
        if !lines.get(row + 1).is_some_and(|l| l.trim_start().starts_with(&mark)) {
            return row + 1;
        }
        // The next question isn't part of it, whatever it looks like.
        let next = written.iter().map(|q| *q.rows.start()).find(|start| *start > row).unwrap_or(lines.len());
        let mut sql = row + 2;
        while sql < next.min(lines.len()) && lines[sql].trim_start().starts_with(opener) {
            sql += 1;
        }
        if sql >= next.min(lines.len()) || lines[sql].trim().is_empty() {
            return sql;
        }
        let text = self.tab.editor.text();
        let start: usize = lines[..sql].iter().map(|l| l.len() + 1).sum();
        let Some(statement) =
            crate::language::statement_ranges(&text, self.engine()).into_iter().find(|r| r.end > start)
        else {
            return sql;
        };
        // On its first line, else it isn't the answer's.
        if text[start..statement.start].contains('\n') {
            return sql;
        }
        let last = text[..statement.end].matches('\n').count();
        (last + 1).min(next)
    }

    /// How a line comment starts in this tab's language: `--` in SQL, `//`
    /// in mongosh's.
    fn comment_opener(&self) -> &'static str {
        crate::language::line_comment(self.engine())
    }

    /// `Ctrl+K y y`: the statement at the cursor (or the selection) goes to
    /// a model, which says in plain words what it does (and what looks
    /// wrong in it); the answer shows in a pop-over, the editor untouched.
    pub(super) fn ai_explain(&mut self) {
        if let Some(why) = self.why_cant_ask() {
            return self.set_status(why, true);
        }
        let Some(sql) = self.statement_or_selection() else {
            return self.set_status(t!("editor.no_statement_here").into(), true);
        };
        let question = Question {
            text: sql,
            tab: self.active_tab,
            connection: self.connection.form.name.clone(),
            explain: true,
            at: None,
        };
        self.start_ask(question, false);
    }

    /// Why asking can't start here, if it can't.
    fn why_cant_ask(&self) -> Option<String> {
        if self.ai_state() == AiState::Off {
            return Some(t!("ask.no_model").into());
        }
        if self.ask.asking.is_some() {
            return Some(t!("ask.busy").into());
        }
        if !self.connected() {
            return Some(self.not_connected_message());
        }
        let saved = self.project().is_some_and(|p| p.connections.iter().any(|c| c.name == self.connection.form.name));
        if !saved {
            return Some(t!("ask.needs_saved").into());
        }
        None
    }

    /// Sends `question` to the gateway (started if it isn't running);
    /// `share_once`: the user allowed the schema to go to the model.
    fn start_ask(&mut self, question: Question, share_once: bool) {
        if question.tab >= self.tabs.len() && !question.explain {
            return self.set_status(t!("ask.tab_closed").into(), true);
        }
        // By id: two projects' names may differ only in case.
        let project = self.current_project.as_ref().map(|id| format!("id:{id}")).unwrap_or_default();
        let connection = question.connection.clone();
        let config_dir = self.data_dir.clone().unwrap_or_else(crate::config::store::config_dir);
        self.ask.count += 1;
        let id = self.ask.count;
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
        // The schema read again where the gateway may keep an older one.
        if !std::mem::replace(&mut self.ask.gateway_current, true) {
            params["refresh"] = serde_json::json!(true);
        }
        if question.explain {
            params["explain"] = serde_json::json!(true);
        }
        let task = tokio::spawn(async move {
            let started = connect_or_start(&config_dir, "podelis", || spawn_gateway(&config_dir)).await;
            let result = match started {
                Ok(mut client) => {
                    client
                        .call_streaming("ask", params, |method, note| {
                            if method == "ask.delta" {
                                let chars = note["text"].as_str().map_or(0, |t| t.chars().count());
                                send(Background::AskProgress(id, chars).into());
                            } else if method == "key.waiting" {
                                send(Background::AskWaitingKey(id).into());
                            }
                        })
                        .await
                }
                Err(e) => Err(e),
            };
            let result = result.map_err(|e| match e {
                CallError::Rpc(e) => AskFailure { code: Some(e.code), message: e.message, data: e.data },
                CallError::Io(e) => {
                    AskFailure { code: None, message: t!("ask.gateway_unreachable", e = e), data: None }
                }
            });
            send(Background::AskDone(id, Box::new(result)).into());
        });
        self.ask.asking = Some(Asking { id, question, chars: 0, started: Instant::now(), task });
        self.say_asking(t!("ask.asking").into());
    }

    /// Progress goes to the status bar, not a pane's footer (which keeps a
    /// message until the next key, and would still say "Asking…" after).
    fn say_asking(&mut self, status: String) {
        self.notes.editor_note = None;
        self.notes.results_note = None;
        self.notes.status = status;
        self.notes.status_is_error = false;
        self.notes.status_in_results = false;
    }

    /// Tab `closing` is about to close: a question asked from it stops,
    /// and one asked from a tab after it follows that tab's new place.
    pub(super) fn tab_closing(&mut self, closing: usize) {
        // (An explanation goes to a pop-over, not the tab: it goes on.)
        if self.ask.asking.as_ref().is_some_and(|a| a.question.tab == closing && !a.question.explain)
            || matches!(&self.ask.pending_share, Some(Confirm { action: ConfirmAction::ShareSchema { question, .. }, .. }) if question.tab == closing)
        {
            return self.stop_asking();
        }
        let shift = |tab: &mut usize| {
            if *tab > closing {
                *tab -= 1;
            }
        };
        if let Some(asking) = &mut self.ask.asking {
            shift(&mut asking.question.tab);
        }
        if let Some(Confirm { action: ConfirmAction::ShareSchema { question, .. }, .. }) = &mut self.ask.pending_share {
            shift(&mut question.tab);
        }
    }

    /// Esc while asking: stops waiting (the gateway's answer is dropped).
    pub(in crate::app) fn stop_asking(&mut self) {
        // A sharing question waiting to be shown goes too: its tab, or its
        // project, is no longer the one it was about.
        self.ask.pending_share = None;
        if let Some(ConfirmAction::ShareSchema { .. }) = self.popups.confirm.as_ref().map(|c| &c.action) {
            self.popups.confirm = None;
        }
        if let Some(asking) = self.ask.asking.take() {
            asking.task.abort();
            self.set_status(t!("ask.stopped").into(), false);
        }
    }

    /// The key for the model is slow: most likely a dialog to answer.
    pub(in crate::app) fn ask_waiting_key(&mut self, id: u64) {
        if self.ask.asking.as_ref().is_some_and(|a| a.id == id) {
            self.say_asking(t!("ask.waiting_key").to_string());
        }
    }

    pub(in crate::app) fn ask_progress(&mut self, id: u64, chars: usize) {
        let Some(asking) = self.ask.asking.as_mut().filter(|a| a.id == id) else { return };
        asking.chars += chars;
        let secs = asking.started.elapsed().as_secs();
        let status = t!("ask.asking_progress", chars = asking.chars, secs = secs);
        self.say_asking(status);
    }

    pub(in crate::app) fn ask_done(&mut self, id: u64, result: Result<serde_json::Value, AskFailure>) {
        if self.ask.asking.as_ref().is_none_or(|a| a.id != id) {
            return;
        }
        let Some(asking) = self.ask.asking.take() else { return };
        self.say_asking(String::new());
        match result {
            Ok(answer) => {
                // Closing a tab or leaving the project stops asking, so the
                // tab is still the one asked from.
                if asking.question.explain {
                    self.show_explanation(&answer);
                } else if asking.question.tab < self.tabs.len() {
                    let question = asking.question;
                    self.in_tab(question.tab, |app| app.insert_answer(&question, &answer));
                }
            }
            Err(e) if e.code == Some(code::NOT_SHARED) => {
                let project =
                    e.data.as_ref().and_then(|d| d["project"].as_str()).unwrap_or(t!("ask.this_project")).to_string();
                // Shown once no other confirmation is open (`share_tick`).
                self.ask.pending_share = Some(Confirm {
                    message: t!("ask.share_schema", project = project),
                    details: vec![e.message, t!("ask.rows_never_sent").into()],
                    danger: false,
                    action: ConfirmAction::ShareSchema {
                        question: asking.question,
                        project_id: e.data.and_then(|d| d["project_id"].as_str().map(str::to_string)),
                    },
                    offer_transaction: false,
                });
            }
            Err(e) => {
                let status = t!("ask.failed", e = e.message);
                self.log(status.clone(), true);
                self.set_status(status, true);
            }
        }
        self.share_tick();
    }

    /// The sharing question, once no other confirmation is open (one the
    /// user is answering, or an agent's write, isn't replaced).
    pub(in crate::app) fn share_tick(&mut self) {
        if self.popups.confirm.is_none()
            && let Some(question) = self.ask.pending_share.take()
        {
            // A question that came by itself: a moment before keys count.
            self.agents.asked = Some(Instant::now());
            self.popups.confirm = Some(question);
        }
    }

    /// The answer to the sharing question: `always` saves it for the
    /// project, then the question goes again.
    pub(in crate::app) fn answer_share(&mut self, question: Question, project_id: Option<&str>, always: bool) {
        // Always: saved for the project (its own, or the open one if the
        // gateway didn't say), then asked as allowed.
        let id = project_id.map(str::to_string).or_else(|| self.current_project.clone()).filter(|_| always);
        let saved = match id {
            Some(id) => match self.store.set_share_schema(&id, crate::config::store::ShareSchema::All) {
                Ok(()) => true,
                Err(e) => return self.set_status(format!("Couldn't save it: {e:#}"), true),
            },
            None => false,
        };
        self.start_ask(question, !saved);
    }

    /// What the model said a statement does, in a pop-over: its words
    /// wrapped, its code blocks (the query it would write instead, if it
    /// found a mistake) as they are, indented, without their fences.
    fn show_explanation(&mut self, answer: &serde_json::Value) {
        let model = answer["model"].as_str().unwrap_or(t!("ask.the_model"));
        let said = answer["said"].as_str().unwrap_or_default().trim();
        if said.is_empty() {
            return self.set_status(format!("{model}: {}", t!("ask.no_explanation")), true);
        }
        let mut text = String::new();
        let mut code = false;
        for line in said.lines() {
            if line.trim_start().starts_with("```") {
                code = !code;
                continue;
            }
            if code {
                let _ = writeln!(text, "  {line}");
            } else if line.trim().is_empty() {
                text.push('\n');
            } else {
                // A list item's wrapped lines keep its indent.
                let indent = if line.trim_start().starts_with("- ") { "  " } else { "" };
                let _ = writeln!(text, "{}", wrap_comment(line, 76).join(&format!("\n{indent}")));
            }
        }
        let title = t!("ask.explained_by", model = model);
        self.popups.cell_view = Some(CellView::new(title, text.trim_end().to_string()));
        let tokens = &answer["usage"];
        let status = t!(
            "ask.explained",
            model = model,
            tokens_in = tokens["input_tokens"],
            tokens_out = tokens["output_tokens"]
        );
        self.set_status(status, false);
    }

    /// The SQL at the end of the editor, a blank line apart, under the
    /// question and what the model said, as comments; the cursor on it. A
    /// question written in the editor gets it right under it instead.
    pub(in crate::app) fn insert_answer(&mut self, asked: &Question, answer: &serde_json::Value) {
        let question = asked.text.as_str();
        let model = answer["model"].as_str().unwrap_or(t!("ask.the_model"));
        let said = answer["said"].as_str().unwrap_or_default();
        let Some(sql) = answer["sql"].as_str() else {
            let why = if said.is_empty() { t!("ask.no_sql").to_string() } else { one_line(said) };
            return self.set_status(format!("{model}: {why}"), true);
        };
        let in_editor = asked.at.is_some();
        let c = self.comment_opener();
        let mut block =
            if in_editor { format!("{c} {ANSWER_MARK} {model}\n") } else { format!("{c} {}\n", one_line(question)) };
        for line in wrap_comment(said, 76) {
            let _ = writeln!(block, "{c} {line}");
        }
        let problems: Vec<&str> =
            answer["problems"].as_array().into_iter().flatten().filter_map(serde_json::Value::as_str).collect();
        if !problems.is_empty() {
            let _ = writeln!(block, "{c} {}: {}", t!("ask.check"), one_line(&problems.join("; ")));
        }
        // The tables it looked up itself, beyond those it was shown.
        let looked: Vec<&str> =
            answer["looked_at"].as_array().into_iter().flatten().filter_map(serde_json::Value::as_str).collect();
        if !looked.is_empty() {
            let _ = writeln!(block, "{c} {}", one_line(&t!("ask.looked_at", tables = looked.join(", "))));
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
            // The model's words: a line break in them would end the comment.
            let _ = writeln!(block, "{c} {}", one_line(&t!("ask.assumed", fact = a)));
        }
        block.push_str(&ended(sql.trim_end(), self.engine()));
        match asked.at {
            Some(at) => self.answer_under(question, at, &block),
            None => self.append_statement(&block),
        }
        let tokens = &answer["usage"];
        let mut status =
            t!("ask.sql_from", model = model, tokens_in = tokens["input_tokens"], tokens_out = tokens["output_tokens"]);
        if !assumed.is_empty() {
            let n = assumed.len();
            let _ = write!(status, " · {}", tn!("ask.assumed_things", n));
        }
        if !problems.is_empty() {
            status = t!("ask.check_it", model = model, problems = problems.join("; "));
        }
        self.set_status(status, !problems.is_empty());
    }

    /// `block` right under the question written in the editor (its last
    /// line `at`, or, if lines moved meanwhile, wherever it is now),
    /// replacing the answer under it from an earlier ask; at the end of the
    /// editor if the question isn't there any more. One undo takes it back.
    fn answer_under(&mut self, question: &str, at: usize, block: &str) {
        let engine = self.engine();
        let written = questions(&self.tab.editor.text(), engine);
        let mine = |q: &&Written| q.text == question;
        let Some(asked) =
            written.iter().filter(mine).find(|q| *q.rows.end() == at).or_else(|| written.iter().find(mine))
        else {
            return self.append_statement(block);
        };
        let row = *asked.rows.end();
        let opener = self.comment_opener();
        let after = self.answer_end(row, &written);
        let mut new: Vec<String> = block.lines().map(str::to_string).collect();
        let first_sql = row + 1 + new.iter().take_while(|l| l.trim_start().starts_with(opener)).count();
        // What follows stays apart from the answer.
        if self.tab.editor.lines.get(after).is_some_and(|l| !l.trim().is_empty()) {
            new.push(String::new());
        }
        self.tab.editor.replace_rows(row + 1..after, new, (first_sql, 0));
        self.edited();
        self.focus = Focus::Editor;
    }

    /// Adds `text` after everything in the editor, a blank line apart,
    /// with the cursor at its end (one undo takes it back). The text there
    /// is kept as it is, except that a last statement left without its
    /// `;` gets one, so the two don't run together (not in a MySQL script
    /// with `DELIMITER`, where `;` means something else).
    pub(in crate::app) fn append_statement(&mut self, text: &str) {
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
    use crate::language::TokenKind;
    let tokens = crate::language::tokenize(sql, engine);
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

    fn at(text: &str, row: usize) -> Option<(String, usize)> {
        super::question_at(text, Engine::Postgres, row)
    }

    #[test]
    fn a_question_is_a_comment_that_starts_with_a_question_mark() {
        let editor = "SELECT 1;\n-- ? revenue per city\n--?  top 5 products \n-- a note\n-- ?\n";
        assert_eq!(at(editor, 1), Some(("revenue per city".into(), 1)));
        assert_eq!(at(editor, 2), Some(("top 5 products".into(), 2)));
        assert_eq!(at(editor, 0), None, "SQL");
        assert_eq!(at(editor, 3), None, "a plain comment");
        assert_eq!(at(editor, 4), None, "nothing asked");
        // mongosh's comments.
        assert_eq!(at("SELECT 1; -- ? after code", 0), None, "not alone on its line");
        assert_eq!(at("SELECT '-- ? in a string';", 0), None);
        assert_eq!(at("SELECT '/*? in a string */';", 0), None);
        assert_eq!(at("/* a note\n-- ? inside it\n*/", 1), None, "inside a block comment");
        // For drawing: which rows are questions, in one pass.
        let editor = "SELECT 1;\n-- ? one\n/* plain */\n/*? two\n  lines */\nSELECT 2;";
        assert_eq!(super::question_rows(editor, Engine::Postgres, 0..6), [false, true, false, true, true, false]);
        assert_eq!(
            super::question_rows(editor, Engine::Postgres, 4..6),
            [true, false],
            "a block opened above the rows shown"
        );
    }

    #[cfg(feature = "mongodb")]
    #[test]
    fn a_question_in_mongosh_is_a_slash_comment() {
        let text = "db.orders.find()\n// ? open orders\n-- ? not a comment here";
        assert_eq!(super::question_at(text, Engine::MongoDb, 1), Some(("open orders".into(), 1)));
        assert_eq!(super::question_at(text, Engine::MongoDb, 2), None);
        assert_eq!(crate::language::line_comment(Engine::MongoDb), "//");
    }

    #[test]
    fn a_question_over_several_lines_is_a_block_comment() {
        let editor = "/* old */\nSELECT 1;\n/*? customers who\n   never ordered,\n   by city */\nSELECT 2;";
        let asked = Some(("customers who never ordered, by city".into(), 4));
        assert_eq!(at(editor, 2), asked);
        assert_eq!(at(editor, 3), asked);
        assert_eq!(at(editor, 4), asked);
        assert_eq!(at(editor, 5), None, "after it");
        assert_eq!(at(editor, 1), None, "after a plain block comment");
    }

    #[test]
    fn a_statement_is_ended_before_a_comment_after_it() {
        assert_eq!(super::ended("SELECT 1\n-- per country", Engine::Postgres), "SELECT 1;\n-- per country");
        assert_eq!(super::ended("SELECT 1; -- done", Engine::Postgres), "SELECT 1; -- done");
        assert_eq!(super::ended("-- only", Engine::Postgres), "-- only");
    }
}
