//! Session memory: none here. The driver keeps its own sessions, and a database is chosen per call: nothing a new session lacks.

use super::super::session_memory::SessionMemory;

/// Whether it keeps anything of a session (else what runs isn't read for
/// it).
pub(in crate::db) const KEEPS: bool = false;

/// Words a statement that changes a session has (lowercase): a run with
/// none of them isn't read for it.
pub(in crate::db) const CHANGED_BY: &[&str] = &[];

/// (Nothing is kept.)
pub(in crate::db) const FAILED_RUN_UNDONE: bool = false;

/// Whether a session's changes made in a transaction are undone by its
/// `ROLLBACK`. Nothing is kept here.
pub(in crate::db) const UNDONE_BY_ROLLBACK: bool = false;

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

pub(in crate::db) fn remember(_statement: &str, _memory: &mut SessionMemory) {}
