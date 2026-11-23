//! MySQL's session memory: what changes a session (`USE`, `SET`, roles,
//! temporary tables, prepared statements), so a reconnect can say what's
//! gone with the old one.

use super::super::session_memory::{DATABASE, SessionMemory, leading_words, top_level_parts};
use super::super::*;

/// Whether it keeps anything of a session (else what runs isn't read for
/// it).
pub(in crate::db) const KEEPS: bool = true;

/// Words a statement that changes a session has (lowercase): a run with
/// none of them isn't read for it.
pub(in crate::db) const CHANGED_BY: &[&str] = &["use", "set", "create", "prepare"];

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
        "prepare" => memory.prepared = true,
        _ => {}
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
    fn autocommit_opens_and_ends_a_transaction() {
        assert_eq!(transaction_of("SET autocommit = 0"), Some(true));
        assert_eq!(transaction_of("set AUTOCOMMIT=OFF"), Some(true));
        assert_eq!(transaction_of("SET autocommit = 1"), Some(false));
        assert_eq!(transaction_of("SET @x = 1"), None);
        assert_eq!(transaction_of("SELECT 'autocommit = 0'"), None);
    }
}
