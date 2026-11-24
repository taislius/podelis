//! MySQL's session memory: what changes a session (`USE`, `SET`, roles,
//! temporary tables, prepared statements), so a reconnect can say what's
//! gone with the old one.

use super::super::session_memory::{DATABASE, SessionMemory, calls, leading_words, top_level_parts};
use super::super::*;

/// Whether it keeps anything of a session (else what runs isn't read for
/// it).
pub(in crate::db) const KEEPS: bool = true;

/// Words a statement that changes a session has (lowercase): a run with
/// none of them isn't read for it.
/// (`begin` and `start`: a transaction begun lets go of LOCK TABLES.)
pub(in crate::db) const CHANGED_BY: &[&str] = &["use", "set", "create", "prepare", "lock", "unlock", "begin", "start"];

/// Functions whose call anywhere in a statement changes the session
/// (lowercase, as they start): named locks, held until let go of or the
/// session ends.
pub(in crate::db) const CALLS: &[&str] = &["get_lock", "release_all_locks"];

/// A run that fails keeps what ran before the failing statement: each
/// stands by itself.
pub(in crate::db) const FAILED_RUN_UNDONE: bool = false;

/// Whether a session's changes made in a transaction are undone by its
/// `ROLLBACK`. `SET` and `USE` stay whatever the transaction does.
pub(in crate::db) const UNDONE_BY_ROLLBACK: bool = false;

/// Words of the engine's own statements that open or end a transaction
/// (lowercase), besides `BEGIN`, `COMMIT` and `ROLLBACK`.
pub(in crate::db) const TRANSACTION_WORDS: &[&str] = &["set"];

/// Whether `statement` opens (`Some(true)`) or ends (`Some(false)`) a
/// transaction its own way: `SET autocommit = 0` leaves every statement
/// after it in one (taken as open from then: the next may write), `= 1`
/// commits it.
pub(in crate::db) fn transaction_of(statement: &str) -> Option<bool> {
    let lower = statement.to_ascii_lowercase();
    if !lower.trim_start().starts_with("set") {
        return None;
    }
    let after = &lower[lower.find("autocommit")? + "autocommit".len()..];
    let value = after.trim_start().strip_prefix('=').unwrap_or(after).trim_start();
    let value: String = value.chars().take_while(char::is_ascii_alphanumeric).collect();
    match value.as_str() {
        "0" | "off" | "false" => Some(true),
        "1" | "on" | "true" => Some(false),
        _ => None,
    }
}

/// Whether the database lives in the session itself, so a new session
/// can't have it: a server's doesn't.
pub(in crate::db) fn owns_data(_target: &str) -> bool {
    false
}

/// Notes what `statement` (one, run without error) changed on the session.
pub(in crate::db) fn remember(statement: &str, memory: &mut SessionMemory) {
    remember_locks(statement, memory);
    let words = leading_words(statement, Engine::MySql, 2);
    let word = |i: usize| words.get(i).map_or("", String::as_str);
    match word(0) {
        "use" if !word(1).is_empty() => memory.set(DATABASE, &format!("database {}", word(1))),
        // Each assignment by itself (`SET @x = 1, GLOBAL y = 2` changes the
        // session's `@x` and the server's `y`).
        "set" => {
            for (i, part) in top_level_parts(statement, Engine::MySql).into_iter().enumerate() {
                let words = if i == 0 {
                    leading_words(part, Engine::MySql, 6)
                } else {
                    let mut words = vec!["set".to_string()];
                    words.extend(leading_words(part, Engine::MySql, 5));
                    words
                };
                remember_set(&words, memory);
            }
        }
        "create" if word(1) == "temporary" => memory.temporary_tables = true,
        "lock" if matches!(word(1), "table" | "tables") => memory.set("table locks", "LOCK TABLES"),
        "unlock" if matches!(word(1), "table" | "tables") => memory.reset("table locks"),
        // A transaction begun lets go of the tables locked.
        "begin" => memory.reset("table locks"),
        "start" if word(1) == "transaction" => memory.reset("table locks"),
        "prepare" => memory.prepared = true,
        _ => {}
    }
}

/// The memory's keys for locks: forgotten again after a run whose lock
/// calls all came back not taken (`lock_taken`).
pub(in crate::db) const LOCK_KEYS: &[&str] = &["named locks"];

/// Whether `statement` took a lock, by what it returned (`single`: its one
/// cell, when its result is one row of one column): a `GET_LOCK` that timed
/// out says 0 (an error, NULL). Without a single cell (`DO GET_LOCK(…)`,
/// `SET @got = GET_LOCK(…)`), it may have. `None` when it calls none.
pub(in crate::db) fn lock_taken(statement: &str, single: Option<&str>) -> Option<bool> {
    let got = |cell: Option<&str>| cell.is_none_or(|c| c == "1");
    calls(statement, Engine::MySql, CALLS).iter().any(|c| c == "get_lock").then(|| got(single))
}

/// Named locks taken (`GET_LOCK`) or all let go of (`RELEASE_ALL_LOCKS`)
/// by a call in `statement` (its code, not its strings). One let go of
/// (`RELEASE_LOCK`) may leave others: they're still said to be held.
fn remember_locks(statement: &str, memory: &mut SessionMemory) {
    let calls = calls(statement, Engine::MySql, CALLS);
    let called = |name: &str| calls.iter().any(|c| c == name);
    if called("release_all_locks") {
        memory.reset("named locks");
    }
    if called("get_lock") {
        memory.set("named locks", "named locks (GET_LOCK)");
    }
}

/// One assignment of a `SET` (its words, `set` first): the session's own
/// (not `GLOBAL`, not the next transaction's).
fn remember_set(words: &[String], memory: &mut SessionMemory) {
    let word = |i: usize| words.get(i).map_or("", String::as_str);
    let (key, label) = match word(1) {
        // Not the session's (the server's, an account's: `SET DEFAULT ROLE
        // … TO user`; the next transaction's only).
        "global" | "persist" | "persist_only" | "password" | "transaction" | "default" | "@@global"
        | "@@persist" | "@@persist_only" => return,
        "names" => ("names", "SET NAMES"),
        "character" | "charset" => ("charset", "character set"),
        "role" => ("role", "role"),
        "session" | "local" if word(2) == "transaction" => ("transaction", "transaction settings"),
        "session" | "local" => (word(2), word(2)),
        // `@@session.x`, `@@x`: the variable after the dot.
        "@@session" | "@@local" => (word(3), word(3)),
        at if at.starts_with("@@") => (&at[2..], &at[2..]),
        // A user variable, or a variable by its name.
        name => (name, name),
    };
    if !key.is_empty() {
        memory.set(key, label);
    }
}

/// Notes what `statement` changed on the session that no ROLLBACK undoes,
/// beside `remember`'s: nothing here.
pub(in crate::db) fn remember_kept(_statement: &str, _memory: &mut SessionMemory) {}

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
    fn what_changes_a_session_is_remembered_latest_first_database() {
        // The same pair set again and again: two settings, not one a time.
        let many: Vec<String> = (0..1000).map(|i| format!("SET @a = {i}, @b = {i}")).collect();
        let many: Vec<&str> = many.iter().map(String::as_str).collect();
        assert_eq!(remembered(&many).lost(), ["@a", "@b"]);
        let memory = remembered(&[
            "SET sql_mode = 'ANSI'",
            "use shop",
            "SET @id = 1",
            "SET SESSION time_zone = '+00:00'",
            "set @@session.sql_mode = 'TRADITIONAL'",
            "SET GLOBAL max_connections = 10",
            "SET TRANSACTION ISOLATION LEVEL SERIALIZABLE",
            "SELECT 1",
            "CREATE TEMPORARY TABLE t (x int)",
        ]);
        let labels: Vec<&str> = memory.settings.iter().map(|s| s.label.as_str()).collect();
        assert_eq!(labels, ["database shop", "@id", "time_zone", "sql_mode"], "in the order they last ran");
        assert!(memory.temporary_tables && !memory.prepared);
        let memory = remembered(&["USE `my db`", "SET NAMES utf8mb4", "SET autocommit = 0"]);
        let labels: Vec<&str> = memory.settings.iter().map(|s| s.label.as_str()).collect();
        assert_eq!(labels, ["database my db", "SET NAMES", "autocommit"]);
    }

    #[test]
    fn several_at_once_roles_and_what_isnt_the_sessions() {
        let memory = remembered(&[
            "SET a = 1, @b = 2",
            "SET @x = 1, GLOBAL max_connections = 500",
            "SET @@max_execution_time = 100",
            "SET CHARACTER SET utf8mb4",
            "SET ROLE reporting",
            "SET PERSIST max_connections = 10",
            "SET @@GLOBAL.max_connections = 10",
            "SET SESSION TRANSACTION READ ONLY",
            "PREPARE q FROM 'SELECT 1'",
            "SET DEFAULT ROLE app TO 'u'@'%'",
        ]);
        let labels: Vec<&str> = memory.settings.iter().map(|s| s.label.as_str()).collect();
        assert_eq!(
            labels,
            ["a", "@b", "@x", "max_execution_time", "character set", "role", "transaction settings"],
            "each assignment by itself; the server's own (in a list too) and an account's left out"
        );
        assert!(memory.prepared);
    }

    #[test]
    fn table_and_named_locks_are_remembered_until_let_go_of() {
        assert_eq!(remembered(&["LOCK TABLES t READ"]).lost(), ["LOCK TABLES"]);
        assert!(remembered(&["LOCK TABLES t WRITE", "UNLOCK TABLES"]).is_empty());
        assert_eq!(remembered(&["SELECT GET_LOCK('job', 10)"]).lost(), ["named locks (GET_LOCK)"]);
        assert!(remembered(&["SELECT GET_LOCK('a', 1)", "DO RELEASE_ALL_LOCKS()"]).is_empty());
        assert!(remembered(&["SELECT 'get_lock(1)'"]).is_empty(), "a string, not a call");
        assert!(remembered(&["LOCK TABLES t WRITE", "START TRANSACTION"]).is_empty(), "a transaction lets them go");
        assert_eq!(lock_taken("SELECT GET_LOCK('a', 1)", Some("0")), Some(false));
        assert_eq!(lock_taken("SELECT GET_LOCK('a', 1)", Some("1")), Some(true));
        assert_eq!(lock_taken("SELECT 1", Some("1")), None);
        assert_eq!(lock_taken("DO GET_LOCK('a', 1)", None), Some(true), "no answer to read: kept");
        assert!(remembered(&["EXPLAIN SELECT GET_LOCK('a', 1)"]).is_empty(), "a plan doesn't run it");
        assert!(remembered(&["CREATE PROCEDURE p() BEGIN DO GET_LOCK('a', 1); END"]).is_empty());
    }

    #[test]
    fn autocommit_opens_and_ends_a_transaction() {
        assert_eq!(transaction_of("SET autocommit = 0"), Some(true));
        assert_eq!(transaction_of("set AUTOCOMMIT=OFF"), Some(true));
        assert_eq!(transaction_of("SET autocommit = 1"), Some(false));
        assert_eq!(transaction_of("SET @x = 1"), None);
        assert_eq!(transaction_of("SELECT 'autocommit = 0'"), None);
    }
}
