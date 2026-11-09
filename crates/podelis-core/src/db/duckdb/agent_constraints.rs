//! What AI agents may send to DuckDB (see `db::agent_constraints`).

use crate::db::agent_constraints::AgentConstraints;

pub(in crate::db) static AGENT_CONSTRAINTS: AgentConstraints = AgentConstraints {
    // Not PRAGMA, SUMMARIZE or DESCRIBE's cousins that change settings:
    // agents have `describe_table` for a table's shape.
    reads: &["select", "with", "explain", "values", "from", "describe", "summarize", "show"],
    writes: &["insert", "update", "delete", "create", "alter", "drop"],
    // DuckDB reads and writes files and URLs from SQL itself.
    outside: &[
        ("attach", "opens another database file"),
        ("install", "downloads an extension"),
        ("load", "loads code into the database"),
        ("copy", "reads or writes files on this machine"),
        ("export", "writes the database out to files"),
        ("import", "reads a database in from files"),
        ("read_csv", "reads files on this machine"),
        ("read_csv_auto", "reads files on this machine"),
        ("read_parquet", "reads files on this machine"),
        ("read_json", "reads files on this machine"),
        ("read_json_auto", "reads files on this machine"),
        ("read_text", "reads files on this machine"),
        ("read_blob", "reads files on this machine"),
        ("glob", "lists files on this machine"),
    ],
    instead: &[
        ("pragma", "describe_table tells a table's columns, keys and indexes"),
        ("attach", "a connection is one database file: another one is a connection of its own"),
    ],
    executable_comments: &[],
    quotes_by_server_mode: false,
};
