//! The sandbox write mode (`TxMode::Sandbox`, `Ctrl+K x s`): every write
//! runs in one transaction kept open between runs, a savepoint before each
//! (`podelis_1`, `podelis_2`…). A step sees the ones before it; a step that
//! fails rolls back alone and the sandbox goes on; the last step can be
//! undone. Nothing is kept until it's ended: discarded (`ROLLBACK`), kept
//! as a script in a new tab (then rolled back), or run for real (rolled
//! back, then the script runs again in manual commit, through the usual
//! guards, and asks to commit at the end).

use crate::app::*;
use crate::language::TokenKind;
use crate::language::statements;
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
    /// Held while the sandbox lives: its folder (the copy's) isn't another
    /// sandbox's to clean up.
    pub lock: std::fs::File,
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
pub(in crate::app) struct PendingStep {
    number: usize,
    step: Step,
}

/// What's done with a sandbox's script once its transaction is rolled back.
#[derive(Debug, Clone)]
pub(in crate::app) enum Then {
    /// Into a new tab.
    Tab(String),
    /// Run again for real, in manual commit.
    ForReal(String),
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
    let tokens: Vec<(TokenKind, &str)> = crate::language::tokenize(sql, engine)
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
    /// Why `sql` can't run in the sandbox or rollback-only: it writes a
    /// table a rollback doesn't undo (MySQL's `MyISAM`, `MEMORY`), so the
    /// write would stay while nothing should.
    pub(in crate::app) fn unrollable_refusal(&self, sql: &str) -> Option<String> {
        let label = match self.connection.tx_mode {
            TxMode::Sandbox => t!("review.sandbox"),
            TxMode::RollbackOnly => t!("review.rollback_only"),
            _ => return None,
        };
        // None on an engine whose every table a rollback undoes.
        self.engine().dialect().and_then(crate::db::SqlDialect::tables_without_rollback)?;
        let written = written_tables(sql, self.engine());
        if written.is_empty() {
            return None;
        }
        let switch = self.keymap.label(crate::config::keys::Action::TxMode);
        // Which tables those are isn't known yet: a write might stay.
        let Some(unrollable) = &self.connection.unrollable else {
            return Some(match &self.connection.unrollable_failed {
                Some(why) => t!("sandbox.unrollable_unread", why = why, label = label, switch = switch),
                None => t!("sandbox.unrollable_unknown", label = label, switch = switch),
            });
        };
        let hit: Vec<String> = written.into_iter().filter(|t| unrollable.iter().any(|u| u.is(t))).collect();
        (!hit.is_empty()).then(|| t!("sandbox.unrollable", tables = hit.join(", "), label = label, switch = switch))
    }

    /// Runs `sql` as the sandbox's next step: in its transaction (begun by
    /// the first), after a savepoint of its own.
    pub(super) fn run_sandbox_step(&mut self, sql: &str, offset: usize, filled: Option<Filled>, purpose: RunPurpose) {
        // A SQLite file: its first step makes a copy to work on, then goes on.
        if self.connection.sandbox.is_none() && self.connection.txn == TxnState::Idle && self.copies_file() {
            return self.copy_for_sandbox(sql.to_string(), offset, filled, purpose);
        }
        let Some(dialect) = self.sql_dialect() else { return };
        let number = self.connection.sandbox.as_ref().map_or(0, |s| s.steps.len()) + 1;
        let begin =
            if self.connection.txn == TxnState::Idle { format!("{};\n", dialect.begin(false)) } else { String::new() };
        let original = filled.as_ref().map_or_else(|| sql.to_string(), |f| f.original.clone());
        let savepoint = dialect.savepoint(&format!("podelis_{number}"));
        let wrapped = Filled::prefixed(filled, &original, &format!("{begin}{savepoint};\n"));
        let engine = self.engine();
        let infos = statements::analyze(&parse(&wrapped.sql, engine));
        let step = Step {
            tables: written_tables(&original, engine),
            ddl: statements::analyze(&parse(&original, engine)).iter().any(|i| i.kind == Kind::Ddl),
            sql: original,
            rows: 0,
        };
        self.tab.run.extra.sandbox_step = Some(PendingStep { number, step });
        let added = usize::from(!begin.is_empty()) + 1;
        self.run_now(wrapped.sql.clone(), offset, infos, purpose, Some(wrapped));
        self.tab.run.extra.added_statements = added;
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
        let all = self.data_dir.clone().unwrap_or_else(crate::config::store::config_dir).join("sandbox");
        // A folder of its own, locked while it lives.
        let dir = all.join(uuid::Uuid::new_v4().simple().to_string());
        let path = dir.join("copy.db");
        params.host = path.to_string_lossy().into_owned();
        self.sandbox_flow.waiting = Some((sql, offset, filled, purpose));
        self.set_result_status(t!("sandbox.copying").to_string(), false);
        let send = self.sender();
        tokio::spawn(async move {
            let copied = async {
                // Copies left by a sandbox that never ended (a crash) go first;
                // not one that's open (another tab's, another Podėlis's).
                remove_ended_sandboxes(&all);
                std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
                // Locked before it's found by its name: another start's
                // clean-up can't take it in between.
                let made = dir.join(format!("{LOCK}.new"));
                let lock = std::fs::File::create(&made).map_err(|e| e.to_string())?;
                lock.lock().map_err(|e| e.to_string())?;
                std::fs::rename(&made, dir.join(LOCK)).map_err(|e| e.to_string())?;
                let copy = db.engine().dialect().and_then(|d| d.copy_into(&path.to_string_lossy()));
                let copy = copy.ok_or_else(|| format!("{} can't copy its database", db.engine().label()))?;
                db.run(&copy, 1).await.map_err(|e| crate::db::error_message(&e))?;
                let copy = Database::connect(params).await.map_err(|e| crate::db::error_message(&e))?;
                Ok::<_, String>((copy, path, lock))
            };
            send(DbEventKind::SandboxCopied(copied.await));
        });
    }

    /// The copy is ready: the connection works on it now, and the step that
    /// waited runs.
    pub(in crate::app) fn sandbox_copied(
        &mut self,
        copied: Result<(Database, std::path::PathBuf, std::fs::File), String>,
    ) {
        let Some((sql, offset, filled, purpose)) = self.sandbox_flow.waiting.take() else { return };
        let (copy, path, lock) = match copied {
            Ok(copied) => copied,
            Err(e) => return self.set_status(t!("sandbox.copy_failed", e = e), true),
        };
        let Some(original) = self.connection.database.replace(copy.clone()) else { return };
        // The copy's reports are the slot's now, not the original's.
        if let Some(id) = self.connection.slots.active {
            self.follow_health(id, &copy);
        }
        self.connection.sandbox = Some(Sandbox {
            copy: Some(FileCopy { original, path, lock }),
            started: Some(Instant::now()),
            ..Sandbox::default()
        });
        self.run_sandbox_step(&sql, offset, filled, purpose);
    }

    /// A step ran: it's in the sandbox now.
    pub(in crate::app) fn sandbox_step_done(&mut self, pending: PendingStep, rows: u64) {
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
    pub(in crate::app) fn sandbox_step_failed(&mut self, pending: &PendingStep) {
        if self.connection.txn == TxnState::Idle {
            return;
        }
        if pending.number == 1 && self.connection.sandbox.as_ref().is_none_or(|s| s.steps.is_empty()) {
            // Its BEGIN ran: end the empty transaction.
            return self.end_transaction(TxEnd::Rollback);
        }
        self.sandbox_back_to(pending.number, false);
    }

    /// Back to before step `number` (`ROLLBACK TO SAVEPOINT`): a failed one,
    /// or the last one undone.
    fn sandbox_back_to(&mut self, number: usize, undo: bool) {
        let Some(dialect) = self.sql_dialect() else { return };
        let sql = dialect.rollback_to_savepoint(&format!("podelis_{number}"));
        let infos = statements::analyze(&parse(&sql, self.engine()));
        self.run_now(sql, 0, infos, RunPurpose::SandboxBack { number, undo }, None);
    }

    /// Went back to before step `number`.
    pub(in crate::app) fn sandbox_went_back(&mut self, number: usize, undo: bool) {
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

    /// What the sandbox holds, for its window.
    pub(in crate::app) fn sandbox_locks(&mut self, said: String) {
        if let Some(view) = &mut self.popups.sandbox_view {
            view.locks = Some(said);
        }
    }

    /// Once a second while a sandbox is open: said once when it's been open
    /// long or changed many rows (its locks block others; on PostgreSQL a
    /// long transaction holds back vacuum), with how to end it.
    pub(in crate::app) fn sandbox_tick(&mut self) {
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
        self.for_terminal.notification = Some(said);
    }

    /// `u`: the last step undone.
    pub(in crate::app) fn undo_sandbox_step(&mut self) {
        let n = self.connection.sandbox.as_ref().map_or(0, |s| s.steps.len());
        if self.tab.run.executing || n == 0 {
            return self.set_status(t!("sandbox.nothing_to_undo").to_string(), true);
        }
        self.sandbox_back_to(n, true);
    }

    /// `s`: the steps as a script in a new tab, then the sandbox discarded.
    pub(in crate::app) fn keep_sandbox_as_script(&mut self) {
        let Some(sandbox) = &self.connection.sandbox else { return };
        self.sandbox_flow.then = Some(Then::Tab(sandbox.script(&self.connection.form.short_name())));
        self.end_transaction(TxEnd::Rollback);
    }

    /// `r`: rolled back, then the script runs again in manual commit (the
    /// guards ask what they ask; a commit is asked for at the end).
    pub(in crate::app) fn run_sandbox_for_real(&mut self) {
        let Some(sandbox) = &self.connection.sandbox else { return };
        self.sandbox_flow.then = Some(Then::ForReal(sandbox.script(&self.connection.form.short_name())));
        self.end_transaction(TxEnd::Rollback);
    }

    /// Connection `id`'s sandbox is gone (ended, or its transaction lost
    /// with a reconnect): on a file's copy, the original's put back (its
    /// health followed again) and the copy deleted, with its folder.
    pub(in crate::app) fn sandbox_gone(&mut self, id: ConnId) {
        let Some(slot) = self.connection.slot_mut(id) else { return };
        let Some(FileCopy { original, path, lock }) = slot.sandbox.take().and_then(|s| s.copy) else { return };
        let copy = slot.database.replace(original.clone());
        self.follow_health(id, &original);
        if let Some(copy) = copy {
            tokio::spawn(async move {
                copy.close().await;
                // Its own folder, the lock let go of first (Windows removes
                // no file that's open).
                drop(lock);
                if let Some(dir) = path.parent() {
                    let _ = std::fs::remove_dir_all(dir);
                }
            });
        }
        // Its schema changes were the copy's.
        if self.connection.slots.active == Some(id) {
            self.load_catalog();
        }
    }

    /// The sandbox's transaction ended: the sandbox with it; its script
    /// goes where it was asked to now.
    pub(super) fn sandbox_ended(&mut self) {
        match self.connection.slots.active {
            Some(id) => self.sandbox_gone(id),
            None => self.connection.sandbox = None,
        }
        match self.sandbox_flow.then.take() {
            Some(Then::Tab(script)) => {
                self.new_tab(Some(t!("sandbox.script_tab").to_string()), &script, None);
                self.set_status(t!("sandbox.kept_as_script").to_string(), false);
            }
            // Started once the rollback's run is wholly done
            // (`run_sandbox_script`): this one's ending would take the new
            // run's review with it.
            then @ Some(Then::ForReal(_)) => self.sandbox_flow.then = then,
            None => {}
        }
    }

    /// After a run ends: a sandbox rolled back to run for real runs now,
    /// in manual commit (the guards ask what they ask; a commit is asked
    /// for at the end).
    pub(in crate::app) fn run_sandbox_script(&mut self) {
        if self.connection.sandbox.is_some() || self.connection.txn != TxnState::Idle {
            return;
        }
        let Some(Then::ForReal(script)) = self.sandbox_flow.then.take() else { return };
        self.connection.tx_mode = TxMode::Manual;
        self.set_status(t!("sandbox.running_for_real").to_string(), false);
        self.execute(script, 0);
    }
}

/// The lock file in a sandbox's folder.
const LOCK: &str = "lock";

/// How old a sandbox folder with no lock can be and still be one being
/// made.
const UNMADE: Duration = Duration::from_hours(1);

/// How long an older Podėlis's copy (a file straight in the folder) is
/// left unchanged before it's taken for one whose sandbox ended.
const OLD_LAYOUT_UNUSED: Duration = Duration::from_hours(24);

/// Whether `path` was last changed more than `age` ago.
fn older_than(path: &std::path::Path, age: Duration) -> bool {
    path.metadata().and_then(|m| m.modified()).is_ok_and(|at| at.elapsed().is_ok_and(|e| e > age))
}

/// Removes the sandbox folders in `all` whose sandbox has ended without
/// removing its own (a crash): those whose lock can be taken. One without
/// a lock file yet is being made; one locked is open. Files lying in `all`
/// itself are copies an older Podėlis left (a copy and its `-wal`, each
/// straight in the folder): every sandbox has a folder now, so they go once
/// unchanged for a day.
fn remove_ended_sandboxes(all: &std::path::Path) {
    let Ok(entries) = std::fs::read_dir(all) else { return };
    for entry in entries.filter_map(|e| e.ok().map(|e| e.path())) {
        if !entry.is_dir() {
            // Not one an older Podėlis has open now (its copy or WAL changes
            // as it's used).
            if older_than(&entry, OLD_LAYOUT_UNUSED) {
                let _ = std::fs::remove_file(&entry);
            }
            continue;
        }
        let dir = entry;
        let Ok(lock) = std::fs::File::open(dir.join(LOCK)) else {
            // No lock yet: being made, unless it's been an hour (a crash
            // while it was).
            if older_than(&dir, UNMADE) {
                let _ = std::fs::remove_dir_all(&dir);
            }
            continue;
        };
        if lock.try_lock().is_ok() {
            // Let go of first: Windows removes no file that's open.
            drop(lock);
            let _ = std::fs::remove_dir_all(&dir);
        }
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

/// A sandbox's way in and out.
#[derive(Default)]
pub(in crate::app) struct SandboxFlow {
    /// Where a sandbox's script goes once it's rolled back.
    pub(in crate::app) then: Option<Then>,
    /// A sandbox's first step, waiting for the file's copy to be made.
    pub(in crate::app) waiting: Option<(String, usize, Option<crate::sql::params::Filled>, RunPurpose)>,
}
