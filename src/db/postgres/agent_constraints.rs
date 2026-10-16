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
