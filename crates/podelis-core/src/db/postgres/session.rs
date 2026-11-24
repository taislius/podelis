//! PostgreSQL's session memory: what changes a session (`SET`, `SET ROLE`,
//! `LISTEN`, temporary tables, prepared statements; `RESET` and `DISCARD`
//! undo), so a reconnect can say what's gone with the old one. There's no
//! `USE`: another database is another connection.

use super::super::session_memory::{SessionMemory, calls, leading_words};
use super::super::*;

/// Whether it keeps anything of a session (else what runs isn't read for
/// it).
pub(in crate::db) const KEEPS: bool = true;

/// Words a statement that changes a session has (lowercase): a run with
/// none of them isn't read for it.
pub(in crate::db) const CHANGED_BY: &[&str] = &["set", "reset", "discard", "deallocate", "create", "prepare", "listen", "unlisten"];

/// Functions whose call anywhere in a statement changes the session
/// (lowercase, as they start): advisory locks held by the session (not a
/// transaction's, `pg_advisory_xact_lock`, which end with it).
pub(in crate::db) const CALLS: &[&str] = &["pg_advisory_lock", "pg_try_advisory_lock", "pg_advisory_unlock_all"];

/// A run (one query string) that fails is undone back to its last `COMMIT`
/// or `ROLLBACK`: the statements sent together run as one transaction.
pub(in crate::db) const FAILED_RUN_UNDONE: bool = true;

/// Whether a session's changes made in a transaction are undone by its
/// `ROLLBACK`. `SET`, `SET ROLE`, temporary tables: a `ROLLBACK` undoes them.
pub(in crate::db) const UNDONE_BY_ROLLBACK: bool = true;

/// Words of the engine's own statements that open or end a transaction
/// (lowercase), besides `BEGIN`, `COMMIT` and `ROLLBACK`: none here.
pub(in crate::db) const TRANSACTION_WORDS: &[&str] = &[];

/// Whether `statement` opens (`Some(true)`) or ends (`Some(false)`) a
/// transaction its own way: none here.
pub(in crate::db) fn transaction_of(_statement: &str) -> Option<bool> {
    None
}

/// Whether the database lives in the session itself, so a new session
/// can't have it: a server's doesn't.
pub(in crate::db) fn owns_data(_target: &str) -> bool {
    false
}

/// Notes what `statement` (one, run without error) changed on the session.
pub(in crate::db) fn remember(statement: &str, memory: &mut SessionMemory) {
    let words = leading_words(statement, Engine::Postgres, 6);
    let word = |i: usize| words.get(i).map_or("", String::as_str);
    // A setting's whole name (`app.tenant_id`, not `app`), from word `i`.
    let name = |i: usize| match (word(i + 1), word(i + 2)) {
        (".", rest) if !rest.is_empty() => format!("{}.{rest}", word(i)),
        _ => word(i).to_string(),
    };
    match (word(0), word(1)) {
        // The transaction's only, or the next transaction's.
        ("set", "local" | "transaction" | "constraints") => {}
        ("set", "role") => memory.set("role", "role"),
        // A new authorization resets the role.
        ("set", "session") if word(2) == "authorization" => {
            memory.reset("role");
            memory.set("authorization", "session authorization");
        }
        ("set", "session") if word(2) == "characteristics" => memory.set("characteristics", "transaction settings"),
        ("set", "session") if word(2) == "time" => memory.set("timezone", "time zone"),
        ("set", "session") if !word(2).is_empty() => memory.set(&name(2), &name(2)),
        ("set", "time") => memory.set("timezone", "time zone"),
        ("set", n) if !n.is_empty() => memory.set(&name(1), &name(1)),
        // Not the role or the session authorization: RESET ALL leaves them.
        ("reset", "all") => memory.settings.retain(|s| matches!(s.key.as_str(), "role" | "authorization")),
        ("reset", "session") if word(2) == "authorization" => {
            memory.reset("role");
            memory.reset("authorization");
        }
        ("reset", "time") => memory.reset("timezone"),
        ("reset", _) => memory.reset(&name(1)),
        ("discard", "all") => *memory = SessionMemory::default(),
        ("discard", "temp" | "temporary") => memory.temporary_tables = false,
        ("deallocate", "all") | ("discard", "plans") => memory.prepared = false,
        ("create", "temp" | "temporary") => memory.temporary_tables = true,
        ("create", "global" | "local") if matches!(word(2), "temp" | "temporary") => memory.temporary_tables = true,
        ("prepare", _) => memory.prepared = true,
        ("listen", channel) if !channel.is_empty() => memory.set(&format!("listen {channel}"), &format!("LISTEN {channel}")),
        ("unlisten", "*") => memory.settings.retain(|s| !s.key.starts_with("listen ")),
        ("unlisten", channel) => memory.reset(&format!("listen {channel}")),
        _ => {}
    }
}

/// The memory's keys for locks: forgotten again after a run whose lock
/// calls all came back not taken (`lock_taken`).
pub(in crate::db) const LOCK_KEYS: &[&str] = &["advisory locks"];

/// Whether `statement` took a lock, by what it returned (`single`: its one
/// cell, when its result is one row of one column): a
/// `pg_try_advisory_lock` that says false didn't; without a single cell,
/// it may have. `None` when it calls no lock function.
pub(in crate::db) fn lock_taken(statement: &str, single: Option<&str>) -> Option<bool> {
    let calls = calls(statement, Engine::Postgres, CALLS);
    let called = |name: &str| calls.iter().any(|c| c.starts_with(name));
    if called("pg_try_advisory_lock") && !called("pg_advisory_lock") {
        return Some(!matches!(single, Some("f" | "false")));
    }
    called("pg_advisory_lock").then_some(true)
}

/// Notes what `statement` changed on the session that no ROLLBACK undoes:
/// advisory locks taken or all let go of by a call in it (its code, not its
/// strings), even in a transaction rolled back or a run that failed later.
/// One let go of (`pg_advisory_unlock`) may leave others: they're still
/// said to be held.
pub(in crate::db) fn remember_kept(statement: &str, memory: &mut SessionMemory) {
    let calls = calls(statement, Engine::Postgres, CALLS);
    let called = |name: &str| calls.iter().any(|c| c.starts_with(name));
    if called("pg_advisory_unlock_all") {
        memory.reset("advisory locks");
    }
    if called("pg_advisory_lock") || called("pg_try_advisory_lock") {
        memory.set("advisory locks", "advisory locks");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn remembered(statements: &[&str]) -> SessionMemory {
        let mut memory = SessionMemory::default();
        for s in statements {
            remember_kept(s, &mut memory);
            remember(s, &mut memory);
        }
        memory
    }

    #[test]
    fn what_changes_a_session_is_remembered_and_reset_undoes_it() {
        let memory = remembered(&[
            "SET search_path TO app, public",
            "SET LOCAL statement_timeout = 0",
            "set session work_mem = '64MB'",
            "SET ROLE reporting",
            "SET TIME ZONE 'UTC'",
            "LISTEN jobs",
            "CREATE TEMP TABLE t (x int)",
            "PREPARE q AS SELECT 1",
            "RESET work_mem",
        ]);
        let labels: Vec<&str> = memory.settings.iter().map(|s| s.label.as_str()).collect();
        assert_eq!(labels, ["search_path", "role", "time zone", "LISTEN jobs"]);
        assert!(memory.temporary_tables && memory.prepared);
        let memory = remembered(&["SET search_path TO app", "RESET ALL"]);
        assert!(memory.settings.is_empty());
        // RESET ALL leaves the role and the session authorization (as the
        // server does); a new authorization resets the role.
        let memory = remembered(&["SET ROLE r", "SET search_path TO app", "RESET ALL"]);
        let labels: Vec<&str> = memory.settings.iter().map(|s| s.label.as_str()).collect();
        assert_eq!(labels, ["role"]);
        let memory = remembered(&["SET SESSION AUTHORIZATION u1", "SET ROLE r", "SET SESSION AUTHORIZATION u2"]);
        let labels: Vec<&str> = memory.settings.iter().map(|s| s.label.as_str()).collect();
        assert_eq!(labels, ["session authorization"]);
        // A dotted name is one setting: two of them under `app` are two.
        let memory = remembered(&["SET app.tenant_id = '42'", "SET app.request_id = 'abc'", "RESET app.request_id"]);
        assert_eq!(memory.lost(), ["app.tenant_id"]);
        let memory = remembered(&["SET search_path TO app", "CREATE TEMPORARY TABLE t (x int)", "DISCARD ALL"]);
        assert!(memory.is_empty());
    }

    #[test]
    fn session_advisory_locks_are_remembered_not_a_transactions() {
        assert_eq!(remembered(&["SELECT pg_advisory_lock(42)"]).lost(), ["advisory locks"]);
        assert_eq!(remembered(&["select pg_try_advisory_lock_shared(1, 2)"]).lost(), ["advisory locks"]);
        assert!(remembered(&["SELECT pg_advisory_xact_lock(42)"]).is_empty(), "ends with the transaction");
        assert!(remembered(&["SELECT 'pg_advisory_lock(1)'"]).is_empty(), "a string, not a call");
        assert!(remembered(&["SELECT pg_advisory_lock(1)", "SELECT pg_advisory_unlock_all()"]).is_empty());
        assert!(remembered(&["SELECT pg_advisory_lock(1)", "DISCARD ALL"]).is_empty());
        assert_eq!(lock_taken("SELECT pg_try_advisory_lock(1)", Some("f")), Some(false));
        assert_eq!(lock_taken("SELECT pg_try_advisory_lock(1)", Some("true")), Some(true));
        assert_eq!(lock_taken("SELECT pg_advisory_lock(1)", None), Some(true));
        assert_eq!(lock_taken("SELECT id, pg_try_advisory_lock(id) FROM jobs", None), Some(true), "unknown: kept");
        assert_eq!(lock_taken("SELECT pg_advisory_xact_lock(1)", None), None);
        // Written down, not run: a plan of it, a function made with it.
        assert!(remembered(&["EXPLAIN SELECT pg_advisory_lock(1)"]).is_empty());
        assert!(remembered(&["CREATE FUNCTION f() RETURNS void AS 'SELECT pg_advisory_lock(1)' LANGUAGE sql"]).is_empty());
        assert_eq!(lock_taken("EXPLAIN SELECT pg_try_advisory_lock(1)", None), None);
    }

    #[test]
    fn authorization_listening_and_what_discard_and_deallocate_undo() {
        let memory = remembered(&[
            "SET SESSION AUTHORIZATION app",
            "SET SESSION CHARACTERISTICS AS TRANSACTION READ ONLY",
            "LISTEN a",
            "LISTEN b",
            "UNLISTEN a",
            "CREATE GLOBAL TEMPORARY TABLE t (x int)",
            "PREPARE q AS SELECT 1",
            "SET TRANSACTION ISOLATION LEVEL SERIALIZABLE",
            "SET CONSTRAINTS ALL DEFERRED",
        ]);
        let labels: Vec<&str> = memory.settings.iter().map(|s| s.label.as_str()).collect();
        assert_eq!(labels, ["session authorization", "transaction settings", "LISTEN b"]);
        assert!(memory.temporary_tables && memory.prepared);
        let memory = remembered(&[
            "SET SESSION AUTHORIZATION app",
            "RESET SESSION AUTHORIZATION",
            "LISTEN a",
            "UNLISTEN *",
            "CREATE TEMP TABLE t (x int)",
            "DISCARD TEMP",
            "PREPARE q AS SELECT 1",
            "DEALLOCATE ALL",
        ]);
        assert!(memory.is_empty(), "{memory:?}");
    }
}
