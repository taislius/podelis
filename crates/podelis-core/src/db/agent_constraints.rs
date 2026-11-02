//! What AI agents may send to a database, engine by engine: the statements
//! they may start with, and what reaches past the data. Each engine has its
//! own (`postgres`, `mysql`, `sqlite`: `agent_constraints.rs`), given by
//! `Dialect::agent_constraints`; the agents' rules (`crate::agents`) apply
//! them without knowing the engines.

/// One engine's constraints for agents.
pub struct AgentConstraints {
    /// Statements an agent may start with to read.
    pub reads: &'static [&'static str],
    /// … and to change data or the schema, where it may write.
    pub writes: &'static [&'static str],
    /// Functions and clauses that reach past the data, which a read-only
    /// transaction doesn't stop, each with what it does: refused in any
    /// statement. A safety net, not a boundary: a database user with only
    /// the rights the job needs is that.
    pub outside: &'static [(&'static str, &'static str)],
    /// Statements agents can't start with that have a better way, and
    /// that way: SQLite's `PRAGMA` (`describe_table`).
    pub instead: &'static [(&'static str, &'static str)],
}

/// Statements that change the session or its transaction, refused on
/// every engine because Podėlis runs each call in a transaction of its own.
const SESSION: &[&str] = &[
    "begin",
    "start",
    "commit",
    "end",
    "rollback",
    "abort",
    "savepoint",
    "release",
    "set",
    "reset",
    "use",
    "lock",
    "unlock",
    "discard",
    "listen",
    "notify",
    "prepare",
    "execute",
    "deallocate",
];

impl AgentConstraints {
    /// Whether a statement may start with `word` (lowercase).
    pub fn may_start_with(&self, word: &str) -> bool {
        self.reads.contains(&word) || self.writes.contains(&word)
    }

    /// Why a statement can't start with `word` (lowercase), on `engine`
    /// (its name): the session's own, a better way, or not on it.
    pub fn why_not(&self, word: &str, engine: &str) -> String {
        if SESSION.contains(&word) {
            return "Podėlis runs each call in a transaction of its own".to_string();
        }
        if let Some((_, way)) = self.instead.iter().find(|(w, _)| *w == word) {
            return (*way).to_string();
        }
        format!("agents can run queries and, where allowed, INSERT, UPDATE, DELETE and schema changes on {engine}")
    }

    /// The first of `words` (lowercase) that reaches past the data, and
    /// what it does.
    pub fn reaching_outside<'a>(&self, words: &'a [String]) -> Option<(&'a str, &'static str)> {
        words.iter().find_map(|w| self.outside.iter().find(|(o, _)| o == w).map(|(_, why)| (w.as_str(), *why)))
    }
}

#[cfg(test)]
mod tests {
    use crate::db::Engine;

    /// The statements every engine's agents may start with.
    const COMMON_READS: &[&str] = &["select", "with", "explain"];
    const COMMON_WRITES: &[&str] = &["insert", "update", "delete", "create", "alter", "drop"];

    #[test]
    fn every_engine_lets_agents_read_and_write_the_common_way() {
        for engine in Engine::ALL {
            let rules = engine.dialect().agent_constraints();
            for word in COMMON_READS {
                assert!(rules.reads.contains(word), "{engine:?} reads {word}");
            }
            for word in COMMON_WRITES {
                assert!(rules.writes.contains(word), "{engine:?} writes {word}");
            }
            // A word is a read or a write, not both.
            assert!(rules.reads.iter().all(|r| !rules.writes.contains(r)), "{engine:?}");
        }
    }
}
