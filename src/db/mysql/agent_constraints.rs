//! What AI agents may send to MySQL and MariaDB (see
//! `db::agent_constraints`).

use crate::db::agent_constraints::AgentConstraints;

pub(in crate::db) static AGENT_CONSTRAINTS: AgentConstraints = AgentConstraints {
    // `TABLE t` and `VALUES ROW(…)` are queries (8.0.19+); `SHOW`,
    // `DESCRIBE` and `DESC` read the catalog.
    reads: &["select", "with", "explain", "values", "table", "show", "describe", "desc"],
    writes: &["insert", "update", "delete", "create", "alter", "drop", "replace", "truncate"],
    instead: &[],
    outside: &[
        ("outfile", "writes a file on the server"),
        ("dumpfile", "writes a file on the server"),
        ("load_file", "reads a file on the server"),
        ("get_lock", "holds a lock past the transaction"),
    ],
};

/// Its own statements, in its syntax; the rules every engine shares are
/// tested in `crate::agents`.
#[cfg(test)]
mod tests {
    use crate::agents::{Verdict, Wrap, verdict};
    use crate::db::{AgentAccess, ConnectionMode, Engine};

    fn verdict_on(sql: &str) -> Verdict {
        verdict(AgentAccess::Write, ConnectionMode::Normal, sql, Engine::MySql)
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
            ("select * from users into outfile '/tmp/x'", "writes a file on the server"),
            ("select 1 into dumpfile '/tmp/x'", "writes a file on the server"),
            ("select load_file('/etc/passwd')", "reads a file on the server"),
            ("select get_lock('x', 10)", "holds a lock past the transaction"),
            ("set autocommit = 1", "not SET: Podėlis runs each call in a transaction of its own"),
            ("lock tables t write", "transaction of its own"),
        ] {
            assert!(refused(sql).contains(why), "{sql}: {}", refused(sql));
        }
        assert_eq!(run("replace into t values (1)"), Wrap::Commit);
        assert_eq!(run("describe t"), Wrap::Commit);
    }
}
