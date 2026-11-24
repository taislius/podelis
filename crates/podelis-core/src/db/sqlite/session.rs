//! SQLite's session memory: what changes a session (a temporary table,
//! an attached database, a `PRAGMA` set), so a new session (after work on
//! the old one was given up on) can say what's gone with it. The file's
//! own data stays.

use super::super::session_memory::{SessionMemory, leading_words};
use super::super::*;

/// Whether it keeps anything of a session (else what runs isn't read for
/// it).
pub(in crate::db) const KEEPS: bool = true;

/// Words a statement that changes a session has (lowercase): a run with
/// none of them isn't read for it.
pub(in crate::db) const CHANGED_BY: &[&str] = &["create", "attach", "pragma"];

/// Functions whose call anywhere in a statement changes the session
/// (lowercase, as they start): none here.
pub(in crate::db) const CALLS: &[&str] = &[];

/// A run that fails keeps what ran before the failing statement: each
/// stands by itself.
pub(in crate::db) const FAILED_RUN_UNDONE: bool = false;

/// Whether a session's changes made in a transaction are undone by its
/// `ROLLBACK`: a temporary table is (an `ATTACH` can't be made in one, and
/// a `PRAGMA` there is rare: `foreign_keys` doesn't even change).
pub(in crate::db) const UNDONE_BY_ROLLBACK: bool = true;

/// Words of the engine's own statements that open or end a transaction
/// (lowercase), besides `BEGIN`, `COMMIT` and `ROLLBACK`.
pub(in crate::db) const TRANSACTION_WORDS: &[&str] = &["savepoint"];

/// Whether `statement` opens (`Some(true)`) or ends (`Some(false)`) a
/// transaction its own way: a `SAVEPOINT` outside one begins one.
pub(in crate::db) fn transaction_of(statement: &str) -> Option<bool> {
    leading_words(statement, Engine::Sqlite, 1).first().is_some_and(|w| w == "savepoint").then_some(true)
}

/// Whether the database lives in the session itself, so a new session
/// can't have it: an in-memory one (`:memory:`, a `mode=memory` URI) or
/// an unnamed temporary one is the one connection's own, gone with it.
pub(in crate::db) fn owns_data(target: &str) -> bool {
    let target = target.trim();
    target.is_empty()
        || target == ":memory:"
        || target.starts_with("file::memory:")
        || (target.starts_with("file:") && target.contains("mode=memory"))
}

/// Notes what `statement` (one, run without error) changed on the session.
pub(in crate::db) fn remember(statement: &str, memory: &mut SessionMemory) {
    let words = leading_words(statement, Engine::Sqlite, 4);
    let word = |i: usize| words.get(i).map_or("", String::as_str);
    match word(0) {
        "create" if matches!(word(1), "temp" | "temporary") => memory.temporary_tables = true,
        "attach" => memory.set("attach", "attached databases"),
        // One set (`PRAGMA foreign_keys = ON`), not one read.
        // `PRAGMA main.foreign_keys`: the pragma's name, after the schema.
        "pragma" if statement.contains('=') && !word(1).is_empty() => {
            let name = if word(2) == "." { word(3) } else { word(1) };
            memory.set(name, &format!("PRAGMA {name}"));
        }
        _ => {}
    }
}

/// Notes what `statement` changed on the session that no ROLLBACK undoes,
/// beside `remember`'s: nothing here.
pub(in crate::db) fn remember_kept(_statement: &str, _memory: &mut SessionMemory) {}

/// The memory's keys for locks: none here.
pub(in crate::db) const LOCK_KEYS: &[&str] = &[];

/// Whether `statement` took a lock: it takes none here.
pub(in crate::db) fn lock_taken(_statement: &str, _first: Option<&str>) -> Option<bool> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_changes_a_session() {
        let mut memory = SessionMemory::default();
        for s in [
            "CREATE TEMP TABLE scratch (x)",
            "ATTACH 'other.db' AS o",
            "PRAGMA foreign_keys = ON",
            "PRAGMA main.journal_size_limit = 100",
            "PRAGMA table_info(t)",
            "CREATE TABLE kept (x)",
        ] {
            remember(s, &mut memory);
        }
        assert!(memory.temporary_tables);
        assert_eq!(
            memory.lost(),
            ["attached databases", "PRAGMA foreign_keys", "PRAGMA journal_size_limit", "temporary tables"]
        );
    }

    #[test]
    fn what_lives_in_its_session() {
        for target in ["", ":memory:", "file::memory:?cache=shared", "file:a?mode=memory&cache=shared"] {
            assert!(super::owns_data(target), "{target}");
        }
        for target in ["shop.db", "/tmp/memory.db", "file:shop.db"] {
            assert!(!super::owns_data(target), "{target}");
        }
    }

    #[test]
    fn a_savepoint_opens_a_transaction() {
        assert_eq!(transaction_of("SAVEPOINT a"), Some(true));
        assert_eq!(transaction_of("SELECT 1"), None);
    }
}
