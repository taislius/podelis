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
#[derive(Default)]
pub struct Sandbox {
    pub steps: Vec<Step>,
    pub started: Option<Instant>,
    /// It was said it's getting old or big (once).
    warned: bool,
    /// A SQLite file's sandbox works on a copy of the file (the original
    /// isn't locked meanwhile): the original connection, and the copy's
    /// file, put back and deleted when it ends.
    pub copy: Option<FileCopy>,
}

/// The original connection of a sandbox working on a file's copy.
pub struct FileCopy {
    pub original: Database,
    pub path: std::path::PathBuf,
}

/// A sandbox open this long is said to be holding its locks too long.
const OLD: Duration = Duration::from_mins(15);
/// A sandbox that changed this many rows is said to be getting big.
const BIG: u64 = 100_000;

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
    /// What it holds: the locks the session has (asked when the window
    /// opens), or why that can't be seen.
    pub locks: Option<String>,
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
        // A SQLite file: its first step makes a copy to work on, then goes on.
        if self.connection.sandbox.is_none() && self.connection.txn == TxnState::Idle && self.copies_file() {
            return self.copy_for_sandbox(sql.to_string(), offset, filled, purpose);
        }
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

    /// Whether a sandbox here works on a copy: a SQLite file (not one in
    /// memory, which has no file to copy).
    fn copies_file(&self) -> bool {
        let host = self.connection.form.host.trim();
        self.engine().is_file_based() && !host.is_empty() && host != ":memory:" && self.connection.database.is_some()
    }

    /// Copies the file (`VACUUM INTO`, consistent even in WAL mode) into the
    /// sandbox folder and opens the copy; the step waits meanwhile.
    fn copy_for_sandbox(&mut self, sql: String, offset: usize, filled: Option<Filled>, purpose: RunPurpose) {
        let (Some(db), Ok(mut params)) = (self.connection.database.clone(), self.connection.form.params()) else {
            return;
        };
        let dir = self.data_dir.clone().unwrap_or_else(crate::config::store::config_dir).join("sandbox");
        let path = dir.join(format!("{}.db", uuid::Uuid::new_v4().simple()));
        params.host = path.to_string_lossy().into_owned();
        self.sandbox_waiting = Some((sql, offset, filled, purpose));
        self.set_result_status(t!("sandbox.copying").to_string(), false);
        let send = self.sender();
        tokio::spawn(async move {
            let copied = async {
                // Copies left by a sandbox that never ended (a crash) go first.
                let _ = std::fs::remove_dir_all(&dir);
                std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
                let target = crate::db::sql_literal(&path.to_string_lossy(), crate::db::Engine::Sqlite);
                db.run_once(&format!("VACUUM INTO {target}"), 1).await.map_err(|e| crate::db::error_message(&e))?;
                let copy = Database::connect(params).await.map_err(|e| crate::db::error_message(&e))?;
                Ok::<_, String>((copy, path))
            };
            send(DbEventKind::SandboxCopied(copied.await));
        });
    }

    /// The copy is ready: the connection works on it now, and the step that
    /// waited runs.
    pub(super) fn sandbox_copied(&mut self, copied: Result<(Database, std::path::PathBuf), String>) {
        let Some((sql, offset, filled, purpose)) = self.sandbox_waiting.take() else { return };
        let (copy, path) = match copied {
            Ok(copied) => copied,
            Err(e) => return self.set_status(t!("sandbox.copy_failed", e = e), true),
        };
        let Some(original) = self.connection.database.replace(copy) else { return };
        self.connection.sandbox = Some(Sandbox {
            copy: Some(FileCopy { original, path }),
            started: Some(Instant::now()),
            ..Sandbox::default()
        });
        self.run_sandbox_step(&sql, offset, filled, purpose);
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

    /// Ctrl+K x while a sandbox is open: its window, and (asked of the
    /// server meanwhile) the locks it holds.
    pub(super) fn open_sandbox_view(&mut self) {
        self.sandbox_view = Some(SandboxView::default());
        let engine = self.engine();
        if engine.is_file_based() {
            let copied = self.connection.sandbox.as_ref().is_some_and(|s| s.copy.is_some());
            let locks = if copied { t!("sandbox.on_a_copy") } else { t!("sandbox.file_lock") };
            return self.sandbox_locks(locks.to_string());
        }
        let (Some(db), Some(query)) = (self.connection.database.clone(), engine.dialect().locks_held_query()) else {
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
            send(DbEventKind::SandboxLocks(said));
        });
    }

    /// What the sandbox holds, for its window.
    pub(super) fn sandbox_locks(&mut self, said: String) {
        if let Some(view) = &mut self.sandbox_view {
            view.locks = Some(said);
        }
    }

    /// Once a second while a sandbox is open: said once when it's been open
    /// long or changed many rows (its locks block others; on PostgreSQL a
    /// long transaction holds back vacuum), with how to end it.
    pub(super) fn sandbox_tick(&mut self) {
        let Some(sandbox) = &mut self.connection.sandbox else { return };
        let rows: u64 = sandbox.steps.iter().map(|s| s.rows).sum();
        let old = sandbox.started.is_some_and(|s| s.elapsed() >= OLD);
        if sandbox.warned || !(old || rows >= BIG) {
            return;
        }
        sandbox.warned = true;
        let age = sandbox.started.map_or(0, |s| s.elapsed().as_secs());
        let key = self.keymap.label(crate::config::keys::Action::TxMode);
        let said = t!("sandbox.getting_big", time = mm_ss(age), rows = rows, key = key);
        self.set_status(said.clone(), true);
        self.notification = Some(said);
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
        // Working on a file's copy: the original back, the copy deleted.
        if let Some(FileCopy { original, path }) = self.connection.sandbox.take().and_then(|s| s.copy) {
            if let Some(copy) = self.connection.database.replace(original) {
                tokio::spawn(async move {
                    copy.close().await;
                    for end in ["", "-wal", "-shm", "-journal"] {
                        let _ = std::fs::remove_file(format!("{}{end}", path.display()));
                    }
                });
            }
            // Its schema changes were the copy's.
            self.load_catalog();
        }
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
