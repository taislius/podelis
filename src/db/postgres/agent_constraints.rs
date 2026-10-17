//! What AI agents may send to PostgreSQL (see `db::agent_constraints`).

use crate::db::agent_constraints::AgentConstraints;

const LOCKS: &str = "holds a lock past the transaction";
const FILES: &str = "reaches the server's files";

pub(in crate::db) static AGENT_CONSTRAINTS: AgentConstraints = AgentConstraints {
    // `TABLE t` and `VALUES (…)` are queries; `SHOW` reads a setting.
    reads: &["select", "with", "explain", "values", "table", "show"],
    writes: &["insert", "update", "delete", "create", "alter", "drop", "merge", "truncate", "comment"],
    instead: &[],
    outside: &[
        ("pg_read_file", FILES),
        ("pg_read_binary_file", FILES),
        ("pg_ls_dir", FILES),
        ("pg_stat_file", FILES),
        ("lo_import", FILES),
        ("lo_export", FILES),
        ("pg_terminate_backend", "stops other sessions"),
        ("pg_cancel_backend", "stops other sessions' queries"),
        ("pg_reload_conf", "changes how the server runs"),
        ("pg_rotate_logfile", "changes how the server runs"),
        ("pg_promote", "changes how the server runs"),
        ("pg_switch_wal", "changes how the server runs"),
        ("pg_advisory_lock", LOCKS),
        ("pg_advisory_lock_shared", LOCKS),
        ("pg_try_advisory_lock", LOCKS),
        ("pg_try_advisory_lock_shared", LOCKS),
        ("dblink", "reaches other servers"),
        ("dblink_exec", "reaches other servers"),
        ("dblink_connect", "reaches other servers"),
    ],
};

/// Its own statements, in its syntax; the rules every engine shares are
/// tested in `crate::agents`.
#[cfg(test)]
mod tests {
    use crate::agents::{Verdict, Wrap, verdict};
    use crate::db::{AgentAccess, ConnectionMode, Engine};

    fn verdict_on(sql: &str) -> Verdict {
        verdict(AgentAccess::Write, ConnectionMode::Normal, sql, Engine::Postgres)
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
            ("do $$ begin delete from t; end $$", "not DO"),
            ("copy t to program 'rm -rf /'", "not COPY"),
            ("select pg_terminate_backend(pid) from pg_stat_activity", "stops other sessions"),
            ("select lo_export(1, '/tmp/x')", "reaches the server's files"),
            ("select pg_advisory_lock(1)", "holds a lock past the transaction"),
            ("SET default_transaction_read_only = off", "transaction of its own"),
            ("BEGIN READ WRITE; insert into t values (1); COMMIT", "transaction of its own"),
            // MySQL's and SQLite's, not PostgreSQL's.
            ("replace into t values (1)", "schema changes on PostgreSQL"),
        ] {
            assert!(refused(sql).contains(why), "{sql}: {}", refused(sql));
        }
        assert_eq!(run("merge into t using u on t.id = u.id when matched then update set x = 1"), Wrap::Commit);
        assert_eq!(run("table t"), Wrap::Commit);
    }
}
