//! The sandbox write mode (`TxMode::Sandbox`, `Ctrl+K x s`): every write
//! runs in one transaction kept open between runs, a savepoint before each
//! (`podelis_1`, `podelis_2`…). A step sees the ones before it; a step that
//! fails rolls back alone and the sandbox goes on; the last step can be
//! undone. Nothing is kept until it's ended: discarded (`ROLLBACK`), kept
//! as a script in a new tab (then rolled back), or run for real (rolled
//! back, then the script runs again in manual commit, through the usual
//! guards, and asks to commit at the end).

use super::*;
use crate::sql::TokenKind;
use crate::sql::params::Filled;

/// A sandbox open on a connection.
#[derive(Debug, Default)]
pub struct Sandbox {
    pub steps: Vec<Step>,
    pub started: Option<Instant>,
}

/// One run in the sandbox: what ran, the tables it wrote, the rows it
/// changed.
#[derive(Debug, Clone)]
pub struct Step {
    pub sql: String,
    pub tables: Vec<String>,
    pub rows: u64,
    /// It changes the schema (CREATE, ALTER, DROP…).
    pub ddl: bool,
}

/// A step on its way: its number (its savepoint's), and what it is.
#[derive(Debug, Clone)]
pub(super) struct PendingStep {
    number: usize,
    step: Step,
}

/// What's done with a sandbox's script once its transaction is rolled back.
#[derive(Debug, Clone)]
pub(super) enum Then {
    /// Into a new tab.
    Tab(String),
    /// Run again for real, in manual commit.
    ForReal(String),
}

/// The sandbox window (`Ctrl+K x` while one is open).
#[derive(Debug, Default)]
pub struct SandboxView {
    pub scroll: u16,
}

impl Sandbox {
    /// Per table: the rows the steps changed there, and whether its schema
    /// changed, in the order first written.
    pub fn summary(&self) -> Vec<(String, u64, bool)> {
        let mut out: Vec<(String, u64, bool)> = Vec::new();
        for step in &self.steps {
            for table in &step.tables {
                match out.iter_mut().find(|(t, _, _)| t == table) {
                    Some(row) => {
                        row.1 += step.rows;
                        row.2 |= step.ddl;
                    }
                    None => out.push((table.clone(), step.rows, step.ddl)),
                }
            }
        }
        out
    }

    /// The steps as one script, with a header saying where it came from.
    pub fn script(&self, connection: &str) -> String {
        let mut script = format!("-- {}\n", t!("sandbox.script_header", connection = connection));
        for step in &self.steps {
            let sql = step.sql.trim().trim_end_matches(';');
            let _ = writeln!(script, "{sql};");
        }
        script
    }
}

/// The tables `sql` writes (INSERT INTO, UPDATE, DELETE FROM, MERGE INTO,
/// CREATE / ALTER / DROP TABLE, TRUNCATE), as written.
fn written_tables(sql: &str, engine: crate::db::Engine) -> Vec<String> {
    let tokens: Vec<(TokenKind, &str)> = crate::sql::tokenize(sql, engine)
        .into_iter()
        .filter(|t| !matches!(t.kind, TokenKind::Whitespace | TokenKind::Comment))
        .map(|t| (t.kind, &sql[t.range]))
        .collect();
    let word = |i: usize| tokens.get(i).map(|(_, w)| w.to_ascii_lowercase()).unwrap_or_default();
    // A name, `schema.name` too, starting at `i`: names joined by dots.
    let is_name = |j: usize| matches!(tokens.get(j), Some((TokenKind::Word | TokenKind::QuotedIdent, _)));
    let name_at = |i: usize| {
        if !is_name(i) {
            return None;
        }
        let mut name = tokens[i].1.to_string();
        let mut j = i + 1;
        while matches!(tokens.get(j), Some((TokenKind::Punct, "."))) && is_name(j + 1) {
            name.push('.');
            name.push_str(tokens[j + 1].1);
            j += 2;
        }
        Some(name)
    };
    let mut out = Vec::new();
    for i in 0..tokens.len() {
        let after = match word(i).as_str() {
            "insert" | "merge" | "replace" if word(i + 1) == "into" => i + 2,
            "update" if i == 0 || word(i - 1) != "for" => i + 1,
            "delete" if word(i + 1) == "from" => i + 2,
            "truncate" => i + 1 + usize::from(word(i + 1) == "table"),
            "create" | "alter" | "drop" if word(i + 1) == "table" => {
                let skip = if word(i + 2) == "if" { if word(i + 3) == "not" { 5 } else { 4 } } else { 2 };
                i + skip
            }
            _ => continue,
        };
        // UPDATE ONLY t (PostgreSQL)
        let after = if word(after) == "only" { after + 1 } else { after };
        if let Some(name) = name_at(after)
            && !out.contains(&name)
        {
            out.push(name);
        }
    }
    out
}

impl App {
    /// Runs `sql` as the sandbox's next step: in its transaction (begun by
    /// the first), after a savepoint of its own.
    pub(super) fn run_sandbox_step(&mut self, sql: &str, offset: usize, filled: Option<Filled>, purpose: RunPurpose) {
        let number = self.connection.sandbox.as_ref().map_or(0, |s| s.steps.len()) + 1;
        let begin = if self.connection.txn == TxnState::Idle { "BEGIN;\n" } else { "" };
        let original = filled.as_ref().map_or_else(|| sql.to_string(), |f| f.original.clone());
        let wrapped = Filled::prefixed(filled, &original, &format!("{begin}SAVEPOINT podelis_{number};\n"));
        let engine = self.engine();
        let infos = analyze::analyze(&parse(&wrapped.sql, engine));
        let step = Step {
            tables: written_tables(&original, engine),
            ddl: analyze::analyze(&parse(&original, engine)).iter().any(|i| i.kind == Kind::Ddl),
            sql: original,
            rows: 0,
        };
        self.tab.run.extra.sandbox_step = Some(PendingStep { number, step });
        self.run_now(wrapped.sql.clone(), offset, infos, purpose, Some(wrapped));
    }

    /// A step ran: it's in the sandbox now.
    pub(super) fn sandbox_step_done(&mut self, pending: PendingStep, rows: u64) {
        let sandbox = self.connection.sandbox.get_or_insert_with(Sandbox::default);
        sandbox.started.get_or_insert_with(Instant::now);
        sandbox.steps.truncate(pending.number - 1);
        sandbox.steps.push(Step { rows, ..pending.step });
        let n = sandbox.steps.len();
        let undo = self.keymap.label(crate::config::keys::Action::TxMode);
        let _ = write!(self.notes.status, " · {}", t!("sandbox.step_kept", n = n, key = undo));
    }

    /// A step failed: back to before it (on PostgreSQL the transaction is
    /// spoiled until then), and the sandbox goes on. Its first step's
    /// failure leaves nothing to go back to: the sandbox didn't begin.
    pub(super) fn sandbox_step_failed(&mut self, pending: &PendingStep) {
        if self.connection.txn == TxnState::Idle {
            return;
        }
        if pending.number == 1 && self.connection.sandbox.as_ref().is_none_or(|s| s.steps.is_empty()) {
            // Its BEGIN ran: end the empty transaction.
            return self.end_transaction("ROLLBACK");
        }
        self.sandbox_back_to(pending.number, false);
    }

    /// Back to before step `number` (`ROLLBACK TO SAVEPOINT`): a failed one,
    /// or the last one undone.
    fn sandbox_back_to(&mut self, number: usize, undo: bool) {
        let sql = format!("ROLLBACK TO SAVEPOINT podelis_{number}");
        let infos = analyze::analyze(&parse(&sql, self.engine()));
        self.run_now(sql, 0, infos, RunPurpose::SandboxBack { number, undo }, None);
    }

    /// Went back to before step `number`.
    pub(super) fn sandbox_went_back(&mut self, number: usize, undo: bool) {
        if let Some(sandbox) = &mut self.connection.sandbox {
            sandbox.steps.truncate(number - 1);
        }
        let msg = if undo { t!("sandbox.undone", n = number) } else { t!("sandbox.step_failed", n = number) };
        if undo {
            self.set_status(msg, false);
        } else {
            let _ = write!(self.notes.status, " · {msg}");
        }
    }

    /// Ctrl+K x while a sandbox is open: its window.
    pub(super) fn open_sandbox_view(&mut self) {
        self.sandbox_view = Some(SandboxView::default());
    }

    pub(super) fn handle_sandbox_key(&mut self, key: KeyEvent) {
        let Some(view) = &mut self.sandbox_view else { return };
        match key.code {
            KeyCode::Esc => self.sandbox_view = None,
            KeyCode::Down | KeyCode::Char('j') => view.scroll = view.scroll.saturating_add(1),
            KeyCode::Up | KeyCode::Char('k') => view.scroll = view.scroll.saturating_sub(1),
            KeyCode::Char('u') => {
                self.sandbox_view = None;
                self.undo_sandbox_step();
            }
            KeyCode::Char('d') => {
                self.sandbox_view = None;
                self.end_transaction("ROLLBACK");
            }
            KeyCode::Char('s') => {
                self.sandbox_view = None;
                self.keep_sandbox_as_script();
            }
            KeyCode::Char('r') => {
                self.sandbox_view = None;
                self.run_sandbox_for_real();
            }
            _ => {}
        }
    }

    /// `u`: the last step undone.
    fn undo_sandbox_step(&mut self) {
        let n = self.connection.sandbox.as_ref().map_or(0, |s| s.steps.len());
        if self.tab.run.executing || n == 0 {
            return self.set_status(t!("sandbox.nothing_to_undo").to_string(), true);
        }
        self.sandbox_back_to(n, true);
    }

    /// `s`: the steps as a script in a new tab, then the sandbox discarded.
    fn keep_sandbox_as_script(&mut self) {
        let Some(sandbox) = &self.connection.sandbox else { return };
        self.sandbox_then = Some(Then::Tab(sandbox.script(&self.connection.form.short_name())));
        self.end_transaction("ROLLBACK");
    }

    /// `r`: rolled back, then the script runs again in manual commit (the
    /// guards ask what they ask; a commit is asked for at the end).
    fn run_sandbox_for_real(&mut self) {
        let Some(sandbox) = &self.connection.sandbox else { return };
        self.sandbox_then = Some(Then::ForReal(sandbox.script(&self.connection.form.short_name())));
        self.end_transaction("ROLLBACK");
    }

    /// The sandbox's transaction ended: the sandbox with it; its script
    /// goes where it was asked to now.
    pub(super) fn sandbox_ended(&mut self) {
        self.connection.sandbox = None;
        match self.sandbox_then.take() {
            Some(Then::Tab(script)) => {
                self.new_tab(Some(t!("sandbox.script_tab").to_string()), &script, None);
                self.set_status(t!("sandbox.kept_as_script").to_string(), false);
            }
            // Started once the rollback's run is wholly done
            // (`run_sandbox_script`): this one's ending would take the new
            // run's review with it.
            then @ Some(Then::ForReal(_)) => self.sandbox_then = then,
            None => {}
        }
    }

    /// After a run ends: a sandbox rolled back to run for real runs now,
    /// in manual commit (the guards ask what they ask; a commit is asked
    /// for at the end).
    pub(super) fn run_sandbox_script(&mut self) {
        if self.connection.sandbox.is_some() || self.connection.txn != TxnState::Idle {
            return;
        }
        let Some(Then::ForReal(script)) = self.sandbox_then.take() else { return };
        self.connection.tx_mode = TxMode::Manual;
        self.set_status(t!("sandbox.running_for_real").to_string(), false);
        self.execute(script, 0);
    }
}

#[cfg(test)]
mod tests {
    use super::written_tables;
    use crate::db::Engine;

    #[test]
    fn the_tables_a_step_writes() {
        let t = |sql| written_tables(sql, Engine::Postgres);
        assert_eq!(t("insert into public.orders (id) values (1)"), ["public.orders"]);
        assert_eq!(t("update only orders set x = 1; delete from items where id = 2"), ["orders", "items"]);
        assert_eq!(t("create table if not exists t (id int); alter table t add c int"), ["t"]);
        assert_eq!(t("select * from orders for update"), Vec::<String>::new());
        assert_eq!(t("truncate table logs"), ["logs"]);
    }
}
