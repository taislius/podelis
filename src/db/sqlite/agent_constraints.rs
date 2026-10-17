//! What AI agents may send to SQLite (see `db::agent_constraints`).

use crate::db::agent_constraints::AgentConstraints;

pub(in crate::db) static AGENT_CONSTRAINTS: AgentConstraints = AgentConstraints {
    // Not PRAGMA: it reads or changes the connection's settings
    // (`journal_mode`, `writable_schema`…); agents have `describe_table`
    // for what the reporting ones would tell them.
    reads: &["select", "with", "explain", "values"],
    writes: &["insert", "update", "delete", "create", "alter", "drop", "replace"],
    outside: &[("attach", "opens another database file"), ("load_extension", "loads code into the database")],
    instead: &[
        ("pragma", "describe_table tells a table's columns, keys and indexes"),
        ("attach", "a connection is one database file: another one is a connection of its own"),
    ],
};

/// Its own statements, in its syntax; the rules every engine shares are
/// tested in `crate::agents`.
#[cfg(test)]
mod tests {
    use crate::agents::{Verdict, Wrap, verdict};
    use crate::db::{AgentAccess, ConnectionMode, Engine};

    fn verdict_on(sql: &str) -> Verdict {
        verdict(AgentAccess::Write, ConnectionMode::Normal, sql, Engine::Sqlite)
    }

    /// Why it's refused.
    fn refused(sql: &str) -> String {
        match verdict_on(sql) {
            Verdict::Refuse(said) => said,
            Verdict::Run(wrap) => panic!("ran {sql}: {wrap:?}"),
        }
    }

    /// How it runs.
    fn run(sql: &str) -> Wrap {
        match verdict_on(sql) {
            Verdict::Run(wrap) => wrap,
            Verdict::Refuse(said) => panic!("refused {sql}: {said}"),
        }
    }

    #[test]
    fn its_own_statements() {
        for (sql, why) in [
            ("select load_extension('x')", "loads code into the database"),
            ("attach 'other.db' as o", "not ATTACH: a connection is one database file"),
            ("pragma writable_schema = 1", "not PRAGMA: describe_table tells"),
        ] {
            assert!(refused(sql).contains(why), "{sql}: {}", refused(sql));
        }
        assert_eq!(run("replace into t values (1)"), Wrap::Commit);
        assert_eq!(run("values (1), (2)"), Wrap::Commit);
    }
}
