//! PostgreSQL's session memory: what changes a session (`SET`, `SET ROLE`,
//! `LISTEN`, temporary tables, prepared statements; `RESET` and `DISCARD`
//! undo), so a reconnect can say what's gone with the old one. There's no
//! `USE`: another database is another connection.

use super::super::session_memory::{SessionMemory, leading_words};
use super::super::*;

/// Whether it keeps anything of a session (else what runs isn't read for
/// it).
pub(in crate::db) const KEEPS: bool = true;

/// Words a statement that changes a session has (lowercase): a run with
/// none of them isn't read for it.
pub(in crate::db) const CHANGED_BY: &[&str] = &["set", "reset", "discard", "deallocate", "create", "prepare", "listen", "unlisten"];

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

#[cfg(test)]
mod tests {
    use super::*;

    fn remembered(statements: &[&str]) -> SessionMemory {
        let mut memory = SessionMemory::default();
        for s in statements {
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
