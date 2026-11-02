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
