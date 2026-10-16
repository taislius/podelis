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
