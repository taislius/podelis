//! What AI agents may do through `podelis mcp`, and the log of what they
//! did: one JSON line per query in `agents/<project>.jsonl`, for reviewing
//! afterwards in Podėlis.

use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::analyze::Kind;
use crate::db::{AgentAccess, ConnectionMode, Engine};

/// The log is cut back to its newer half past this size.
const MAX_LOG_BYTES: u64 = 5_000_000;

/// What happens to an agent's SQL. It always runs in a transaction of
/// Podėlis's own, which the agent can't end (it may not send BEGIN,
/// COMMIT or SET): that is what holds, since a session's read-only setting
/// is one the SQL could otherwise switch off.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wrap {
    /// In a read-only transaction, rolled back: writes fail on the server,
    /// even one hidden in a function, and settings changed go back.
    ReadOnly,
    /// In a transaction that's rolled back, saying what it would change.
    DryRun,
    /// In a transaction that's committed if it all ran.
    Commit,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    Run(Wrap),
    Refuse(String),
}

/// Statements an agent may start with: reads anywhere, and data and
/// schema changes where it may write. Nothing else: not SET, BEGIN,
/// COMMIT, USE, CALL, DO, COPY, LOCK, GRANT… (they change the session,
/// end Podėlis's transaction, or reach outside the database).
const READS: &[&str] = &["select", "with", "values", "table", "show", "describe", "desc", "explain", "pragma"];
const WRITES: &[&str] =
    &["insert", "update", "delete", "merge", "replace", "create", "alter", "drop", "truncate", "comment"];

/// Reads run wherever an agent may look, in a read-only transaction.
/// Writes need write access and a connection that isn't read-only; nothing
/// destructive (DROP, TRUNCATE, a change without WHERE) ever runs; and on
/// production every call is rolled back (a write is only a dry run, and
/// schema changes not even that: MySQL commits them at once).
pub fn verdict(access: AgentAccess, mode: ConnectionMode, sql: &str, engine: Engine) -> Verdict {
    let infos = match allowed(sql, engine) {
        Ok(infos) => infos,
        Err(why) => return Verdict::Refuse(why),
    };
    let write = infos.iter().find(|i| i.modifies());
    let may_write = access == AgentAccess::Write && mode != ConnectionMode::ReadOnly;
    if let Some(write) = write
        && !may_write
    {
        return Verdict::Refuse(format!(
            "“{}” changes data, and this connection is shared with agents to read only",
            write.summary
        ));
    }
    if let Some(risky) = infos.iter().find(|i| i.risk.is_some()) {
        return Verdict::Refuse(format!(
            "“{}” is destructive ({}): agents can't run it; ask the user",
            risky.summary,
            risky.risk.unwrap_or_default()
        ));
    }
    if mode == ConnectionMode::Production {
        if let Some(ddl) = infos.iter().find(|i| i.kind == Kind::Ddl) {
            return Verdict::Refuse(format!(
                "“{}” changes the schema of a production database: agents can't, not even as a dry run",
                ddl.summary
            ));
        }
        return Verdict::Run(if write.is_some() { Wrap::DryRun } else { Wrap::ReadOnly });
    }
    Verdict::Run(if may_write { Wrap::Commit } else { Wrap::ReadOnly })
}

/// Whether `sql` can be planned for an agent: one statement (so nothing can
/// ride along after it), of a kind it may run.
pub fn explainable(sql: &str, engine: Engine) -> Result<(), String> {
    if crate::sql::split_statements(sql, engine).len() != 1 {
        return Err("`explain` plans one statement at a time".to_string());
    }
    allowed(sql, engine).map(drop)
}

/// Functions and clauses that reach past the data, which a read-only
/// transaction doesn't stop: the server's files, other sessions, locks
/// that outlive the transaction, other servers. Refusing them is a safety
/// net, not a boundary: a database user with only the rights the job needs
/// is that.
const OUTSIDE: &[&str] = &[
    // MySQL: files on the server, and locks held past the transaction.
    "outfile",
    "dumpfile",
    "load_file",
    "get_lock",
    // PostgreSQL: the server's files, other sessions, its configuration,
    // session locks, other servers.
    "pg_read_file",
    "pg_read_binary_file",
    "pg_ls_dir",
    "pg_stat_file",
    "lo_import",
    "lo_export",
    "pg_terminate_backend",
    "pg_cancel_backend",
    "pg_reload_conf",
    "pg_rotate_logfile",
    "pg_promote",
    "pg_switch_wal",
    "pg_advisory_lock",
    "pg_advisory_lock_shared",
    "pg_try_advisory_lock",
    "pg_try_advisory_lock_shared",
    "dblink",
    "dblink_exec",
    "dblink_connect",
    // SQLite: other database files, and extensions.
    "attach",
    "load_extension",
];

/// The statements of `sql` if an agent may send them at all (whether it may
/// write is up to [`verdict`]): each starts with a word on [`READS`] or
/// [`WRITES`], none changes the session or its transaction, and none uses
/// anything on [`OUTSIDE`].
fn allowed(sql: &str, engine: Engine) -> Result<Vec<crate::analyze::StatementInfo>, String> {
    let statements = crate::sql::split_statements(sql, engine);
    if statements.is_empty() {
        return Err("there's no SQL to run".to_string());
    }
    for statement in &statements {
        let words: Vec<String> = crate::sql::tokenize(statement, engine)
            .into_iter()
            .filter(|t| t.kind == crate::sql::TokenKind::Word)
            .map(|t| statement[t.range].to_ascii_lowercase())
            .collect();
        let first = words.first().map_or("", String::as_str);
        if !READS.contains(&first) && !WRITES.contains(&first) {
            return Err(format!(
                "“{}”: agents can run queries and, where allowed, INSERT, UPDATE, DELETE and schema changes; \
                 not {}: Podėlis runs each call in a transaction of its own",
                one_line(statement),
                first.to_uppercase()
            ));
        }
        if let Some(outside) = words.iter().find(|w| OUTSIDE.contains(&w.as_str())) {
            return Err(format!(
                "“{}” uses {}, which reaches past the data (the server's files, other sessions, locks or \
                 servers): agents can't",
                one_line(statement),
                outside.to_uppercase()
            ));
        }
    }
    let infos = crate::analyze::analyze(&crate::parse::parse(sql, engine));
    if let Some(other) = infos.iter().find(|i| matches!(i.kind, Kind::Transaction(_) | Kind::Session)) {
        return Err(format!(
            "“{}”: agents can't change the session or its transaction; Podėlis runs each call in a transaction of its own",
            other.summary
        ));
    }
    Ok(infos)
}

fn one_line(s: &str) -> String {
    let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if s.chars().count() > 60 { format!("{}…", s.chars().take(60).collect::<String>()) } else { s }
}

/// One thing an agent did.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    /// When, as RFC 3339 local time.
    pub at: String,
    /// The MCP client's name (e.g. `claude-code`).
    pub agent: String,
    pub connection: String,
    /// The connection's engine, to read the SQL by (none if it wasn't one
    /// shared with agents).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub engine: Option<Engine>,
    /// `query` or `explain`.
    pub tool: String,
    pub sql: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rows: Option<u64>,
    /// Rows changed, or that would have been in a dry run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub changed: Option<u64>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub dry_run: bool,
    pub ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Why it wasn't run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refused: Option<String>,
}

pub fn log_path(config_dir: &Path, project_id: &str) -> PathBuf {
    config_dir.join("agents").join(format!("{project_id}.jsonl"))
}

/// Adds `entry` to the log, private to the user. A statement that sets a
/// password isn't written out, as in the history: read as any engine
/// would, so it's caught whichever it was meant for.
///
/// Several agents (each its own `podelis mcp`) may write at once: the file
/// is locked while a line is added, and while it's cut back in place.
pub fn append(path: &Path, mut entry: Entry) -> std::io::Result<()> {
    if [Engine::Postgres, Engine::MySql, Engine::Sqlite].iter().any(|e| crate::sql::sets_password(&entry.sql, *e)) {
        // Messages may quote the statement, so they go too.
        entry.sql = "(a statement that sets a password: not logged)".to_string();
        entry.refused = entry.refused.map(|_| "refused".to_string());
        entry.error = entry.error.map(|_| "failed".to_string());
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
        restrict(dir, 0o700);
    }
    let line = serde_json::to_string(&entry).map_err(std::io::Error::other)? + "\n";
    let mut file = std::fs::OpenOptions::new().create(true).read(true).append(true).open(path)?;
    restrict(path, 0o600);
    file.lock()?;
    file.write_all(line.as_bytes())?;
    if file.metadata()?.len() > MAX_LOG_BYTES {
        let mut text = String::new();
        std::io::Seek::rewind(&mut file)?;
        file.read_to_string(&mut text)?;
        let lines: Vec<&str> = text.lines().collect();
        let kept = lines[lines.len() / 2..].join("\n") + "\n";
        file.set_len(0)?;
        file.write_all(kept.as_bytes())?;
    }
    Ok(())
}

/// Every entry, oldest first; lines that don't read are skipped.
pub fn read(path: &Path) -> Vec<Entry> {
    let Ok(mut file) = std::fs::File::open(path) else { return Vec::new() };
    let mut text = String::new();
    if file.lock_shared().is_err() || file.read_to_string(&mut text).is_err() {
        return Vec::new();
    }
    text.lines().filter_map(|l| serde_json::from_str(l).ok()).collect()
}

fn restrict(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Engine;

    #[test]
    fn what_agents_may_run() {
        use AgentAccess::{Read, Write};
        use ConnectionMode::{Normal, Production, ReadOnly};
        let v = |access, mode, sql| verdict(access, mode, sql, Engine::Postgres);
        let refused = |access, mode, sql| matches!(v(access, mode, sql), Verdict::Refuse(_));
        assert_eq!(v(Read, Normal, "select * from t"), Verdict::Run(Wrap::ReadOnly));
        assert_eq!(v(Read, Production, "select * from t"), Verdict::Run(Wrap::ReadOnly));
        assert!(refused(Read, Normal, "update t set x = 1 where id = 2"));
        assert!(refused(Write, ReadOnly, "insert into t values (1)"));
        assert_eq!(v(Write, Normal, "update t set x = 1 where id = 2"), Verdict::Run(Wrap::Commit));
        assert_eq!(v(Write, Normal, "create table u (id int)"), Verdict::Run(Wrap::Commit));
        assert_eq!(v(Write, Production, "update t set x = 1 where id = 2"), Verdict::Run(Wrap::DryRun));
        // Reads on production are rolled back too: a function may write.
        assert_eq!(v(Write, Production, "select nextval('s')"), Verdict::Run(Wrap::ReadOnly));
        for destructive in ["delete from t", "drop table t", "truncate t", "update t set x = 1"] {
            assert!(refused(Write, Normal, destructive), "{destructive}");
        }
        assert!(refused(Write, Production, "alter table t add column y int"));
        // Nothing that changes the session or ends Podėlis's transaction.
        for sql in [
            "SET default_transaction_read_only = off",
            "BEGIN READ WRITE; insert into t values (1); COMMIT",
            "update t set x = 1 where id = 2; commit",
            "select 1; set autocommit = 1",
            "use other_db",
            "call purge()",
            "do $$ begin delete from t; end $$",
            "copy t to program 'rm -rf /'",
            "lock tables t write",
            "grant all on t to public",
            "select * from users into outfile '/tmp/x'",
            "select pg_terminate_backend(pid) from pg_stat_activity",
            "select lo_export(1, '/tmp/x')",
            "select pg_advisory_lock(1)",
            "",
        ] {
            assert!(refused(Write, Normal, sql), "{sql}");
            assert!(refused(Read, Normal, sql), "{sql}");
        }
    }

    #[test]
    fn what_agents_may_explain() {
        let e = |sql| explainable(sql, Engine::Postgres);
        assert!(e("select * from t where id = 1").is_ok());
        assert!(e("delete from t").is_ok(), "planning it doesn't run it");
        assert!(e("select 1; delete from t").is_err());
        // A message quoting the agent's own SQL can't open the door.
        assert!(e("set x = 'destructive'").is_err());
        assert!(e("select pg_terminate_backend(1)").is_err());
    }

    /// Agents writing at once (as separate servers do) lose no lines and
    /// mix none together.
    #[test]
    fn the_log_takes_writers_at_once() {
        let dir = tempfile::tempdir().unwrap();
        let path = log_path(dir.path(), "p1");
        let writers: Vec<_> = (0..8)
            .map(|w| {
                let path = path.clone();
                std::thread::spawn(move || {
                    for i in 0..200 {
                        let sql = format!("select {w}, {i}, '{}'", "x".repeat(500));
                        append(&path, Entry { agent: format!("agent{w}"), sql, ..Entry::default() }).unwrap();
                    }
                })
            })
            .collect();
        for w in writers {
            w.join().unwrap();
        }
        assert_eq!(read(&path).len(), 1600);
    }

    #[test]
    fn the_log_keeps_entries_but_not_passwords() {
        let dir = tempfile::tempdir().unwrap();
        let path = log_path(dir.path(), "p1");
        let entry = |sql: &str| Entry {
            agent: "claude-code".into(),
            connection: "dev".into(),
            sql: sql.into(),
            ..Entry::default()
        };
        append(&path, entry("select 1")).unwrap();
        append(&path, entry("alter user bob password 'hunter2'")).unwrap();
        append(&path, entry("SET PASSWORD FOR app = 'hunter3'")).unwrap();
        let refused = Entry {
            refused: Some("“alter user bob password 'hunter4'” changes data".into()),
            ..entry("alter user bob password 'hunter4'")
        };
        append(&path, refused).unwrap();
        let read = read(&path);
        assert_eq!(read.len(), 4);
        assert_eq!(read[0].sql, "select 1");
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("hunter"), "{text}");
        assert_eq!(read[3].refused.as_deref(), Some("refused"));
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
    }
}
