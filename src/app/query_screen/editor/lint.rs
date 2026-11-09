//! The editor's checks as typing pauses: the parser's syntax hint and the
//! lint rules, off the UI thread, and the server's own (each statement
//! prepared, not run) where that's safe; a syntax error that's really a
//! feature the server is too old for says so (`version_note`).

use crate::app::*;
use crate::language::statements;

impl App {
    /// When the background syntax check is due, if one is pending.
    pub fn lint_due(&self) -> Option<Instant> {
        self.editing.lint_due
    }

    /// Runs the background syntax check once typing has paused.
    pub fn tick(&mut self) {
        if self.editing.lint_due.is_none_or(|due| Instant::now() < due) {
            return;
        }
        self.editing.lint_due = None;
        // Typing has paused: a good moment to save the tabs, so a crash or a
        // killed terminal loses at most a few seconds of editing.
        self.save_draft_in_background();
        let text = self.tab.editor.text();
        // Ask the server when it's safe: never inside a transaction (preparing
        // can take locks there) or while a query runs on the session.
        let server_ok = self.settings.server_syntax_check && self.connection.txn == TxnState::Idle && !self.conn_busy();
        let db = self.connection.database.clone().filter(|_| server_ok);
        let check = self.local_check();
        // Parsing and linting a long script takes a while: off the UI thread,
        // like the server check. The result is dropped if the text changed.
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            self.tab.syntax_hint = check.hint(&text);
            return;
        };
        let engine = self.engine();
        let send = self.sender();
        runtime.spawn(async move {
            let local = |text: String, lint_only: bool| async move {
                let check = move || if lint_only { check.first_lint(&parse(&text, engine)) } else { check.hint(&text) };
                tokio::task::spawn_blocking(check).await.unwrap_or_default()
            };
            let hint = match db {
                Some(db) => match server_syntax_hint(&db, &text, engine).await {
                    // A syntax error that is really a feature the server
                    // hasn't got says so.
                    ServerCheck::Done(Some((mut hint, missing))) => {
                        if let Some(said) = version_note(engine, db.version(), &text, hint.offset, missing) {
                            hint.message = format!("{} · {said}", hint.message);
                        }
                        Some(hint)
                    }
                    ServerCheck::Done(None) => local(text.clone(), true).await,
                    ServerCheck::Busy => local(text.clone(), false).await,
                },
                None => local(text.clone(), false).await,
            };
            send(DbEventKind::SyntaxChecked { text, hint });
        });
    }

    /// What the editor's own checks need, captured so they can run off the
    /// UI thread.
    fn local_check(&self) -> LocalCheck {
        LocalCheck {
            engine: self.engine(),
            lint: self.settings.lint,
            sql_mode: self.connection.database.as_ref().map(Database::sql_mode).unwrap_or_default(),
        }
    }

    /// Every lint warning, unless lints are turned off.
    fn lint_hints(&self, parsed: &ParsedScript) -> Vec<SyntaxHint> {
        self.local_check().lints(parsed)
    }

    /// Moves the cursor to the next lint warning after it (wrapping) and
    /// shows that warning.
    pub(in crate::app) fn next_warning(&mut self) {
        let text = self.tab.editor.text();
        let hints = self.lint_hints(&parse(&text, self.engine()));
        let cursor = self.tab.editor.cursor_offset();
        let next = hints.iter().find(|h| h.offset.is_some_and(|o| o > cursor)).or_else(|| hints.first()).cloned();
        let Some(hint) = next else {
            let why = if self.settings.lint { t!("run.no_lint") } else { t!("run.lint_off") };
            return self.set_status(why.to_string(), false);
        };
        if let Some(offset) = hint.offset {
            self.tab.editor.set_cursor(self.tab.editor.pos_of_offset(offset));
        }
        self.tab.syntax_hint = Some(hint);
        self.focus = Focus::Editor;
    }
}

/// The editor's own checks: `sqlparser`'s syntax hint and the lint rules.
struct LocalCheck {
    engine: Engine,
    lint: bool,
    /// The session's (MySQL), for rules that depend on it.
    sql_mode: String,
}

impl LocalCheck {
    /// The parser's syntax error, or else the first lint warning.
    fn hint(&self, text: &str) -> Option<SyntaxHint> {
        let parsed = parse(text, self.engine);
        analyze::syntax_hint(&parsed).or_else(|| self.first_lint(&parsed))
    }

    fn first_lint(&self, parsed: &ParsedScript) -> Option<SyntaxHint> {
        self.lints(parsed).into_iter().next()
    }

    /// Every lint warning, unless lints are turned off.
    fn lints(&self, parsed: &ParsedScript) -> Vec<SyntaxHint> {
        if self.lint { crate::sql::lint::lints(parsed, &self.sql_mode) } else { Vec::new() }
    }
}

/// Prepares each statement of `text` on the server (without running it)
/// and returns the first problem.
/// Unknown-object errors are skipped when the script creates objects, since
/// those may only exist once earlier statements have run.
async fn server_syntax_hint(db: &Database, text: &str, engine: Engine) -> ServerCheck {
    const MAX_STATEMENTS: usize = 50;
    let parsed = parse(text, engine);
    let creates_objects = statements::analyze(&parsed).iter().any(|i| i.kind == Kind::Ddl);
    for stmt in parsed.statements.iter().take(MAX_STATEMENTS) {
        let (base, stmt) = (stmt.offset, stmt.text);
        let Ok(outcome) = db.check_statement(stmt).await else { return ServerCheck::Busy };
        match outcome {
            CheckOutcome::Ok | CheckOutcome::Unsupported => {}
            CheckOutcome::Problem { syntax: false, .. } if creates_objects => {}
            CheckOutcome::Problem { message, position, missing_feature, .. } => {
                let range = position.and_then(|p| analyze::error_range(stmt, &p));
                return ServerCheck::Done(Some((
                    SyntaxHint {
                        message,
                        offset: Some(base + range.map_or(0, |r| r.0)),
                        len: range.map(|r| r.1),
                        source: HintSource::Server,
                        nth: None,
                    },
                    missing_feature,
                )));
            }
        }
    }
    ServerCheck::Done(None)
}

/// What to add to an error from running, or checking, `text` on a server
/// of `version`, when it's a syntax error or an unknown function
/// (`missing`, by its code) because of a feature the server is too old
/// for: looked for in the statement at `at` (where the error is), else in
/// `text` if it's one statement.
pub(in crate::app) fn version_note(
    engine: Engine,
    version: &str,
    text: &str,
    at: Option<usize>,
    missing: bool,
) -> Option<String> {
    if !missing {
        return None;
    }
    let range = match at {
        Some(offset) => crate::language::statement_range(text, offset, engine)?,
        None => match crate::language::statement_ranges(text, engine).as_slice() {
            [only] => only.clone(),
            _ => return None,
        },
    };
    let n = engine.dialect()?.too_new(&text[range], version)?;
    Some(t!("run.needs_version", feature = n.feature, needs = n.needs, server = n.server))
}

#[cfg(test)]
mod tests {
    use super::{Engine, version_note};

    #[test]
    fn a_version_note_is_about_the_statement_that_failed() {
        let syntax = true;
        let one = "explain analyze select * from t";
        assert!(version_note(Engine::MySql, "5.7.44", one, None, syntax).unwrap().contains("EXPLAIN ANALYZE"));
        // Not for errors that aren't about syntax (an unknown table).
        assert!(version_note(Engine::MySql, "5.7.44", one, None, false).is_none());
        // In a script: the statement where the error is, else nothing.
        let script = "with c as (select 1) select * from c;\nselct 1;";
        assert!(version_note(Engine::MySql, "5.7.44", script, None, syntax).is_none());
        let typo = script.find("selct").unwrap();
        assert!(version_note(Engine::MySql, "5.7.44", script, Some(typo), syntax).is_none());
        assert!(version_note(Engine::MySql, "5.7.44", script, Some(3), syntax).is_some());
    }
}
