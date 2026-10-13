//! PostgreSQL.

mod plan;

pub(super) mod cells;
pub(super) mod connect;
pub(super) mod pgpass;
pub(super) mod queries;

use super::dialect::{Capabilities, Dialect, LexerRules, Placeholders, Plan};

pub struct Postgres;

impl Dialect for Postgres {
    fn label(&self) -> &'static str {
        "PostgreSQL"
    }
    fn url_schemes(&self) -> &'static [&'static str] {
        &["postgres", "postgresql"]
    }
    fn default_port(&self) -> &'static str {
        "5432"
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            file_based: false,
            cancel: true,
            activity: true,
            schemas: true,
            routines: true,
            server_txn_state: true,
            ddl_commits: false,
            stores_create_sql: false,
        }
    }
    fn lexer(&self) -> LexerRules {
        LexerRules {
            hash_comments: false,
            dash_comment_needs_space: false,
            nested_comments: true,
            backslash_escapes: false,
            double_quoted_strings: false,
            backtick_identifiers: false,
            dollar_quotes: true,
        }
    }
    fn placeholders(&self) -> Placeholders {
        Placeholders { dollar: true, question: false, sqlite: false }
    }
    fn is_system_table(&self, schema: &str, _table: &str) -> bool {
        matches!(schema, "pg_catalog" | "information_schema")
    }
    fn parser(&self) -> Box<dyn sqlparser::dialect::Dialect> {
        Box::new(sqlparser::dialect::PostgreSqlDialect {})
    }
    fn formatter(&self) -> sqlformat::Dialect {
        sqlformat::Dialect::PostgreSql
    }
    /// Postgres sockets are a folder holding `.s.PGSQL.<port>`.
    fn socket_file(&self, path: &str, port: u16) -> String {
        if std::path::Path::new(path).is_dir() {
            format!("{}/.s.PGSQL.{port}", path.trim_end_matches('/'))
        } else {
            path.to_string()
        }
    }
    fn version_query(&self) -> &'static str {
        "SHOW server_version"
    }
    fn explain(&self, stmt: &str, _version: &str) -> String {
        format!("EXPLAIN (FORMAT JSON) {stmt}")
    }
    fn explain_analyze(&self, stmt: &str, _version: &str) -> Option<String> {
        Some(format!("EXPLAIN (ANALYZE, BUFFERS, FORMAT JSON) {stmt}"))
    }
    fn format_plan(&self, _columns: &[String], rows: &[Vec<Option<String>>]) -> Option<Plan> {
        plan::format(rows.first()?.first()?.as_deref()?)
    }
    fn full_scans(&self, _columns: &[String], rows: &[Vec<Option<String>>]) -> Vec<crate::db::dialect::Scan> {
        rows.first().and_then(|r| r.first()).and_then(|c| c.as_deref()).map(plan::full_scans).unwrap_or_default()
    }
    fn real_type(&self) -> &'static str {
        "DOUBLE PRECISION"
    }
    fn timestamptz_type(&self) -> &'static str {
        "TIMESTAMPTZ"
    }
    fn modify_column(&self, table: &str, column: &str, data_type: &str, nullable: bool) -> Option<String> {
        let null = if nullable { "DROP NOT NULL" } else { "SET NOT NULL" };
        Some(format!("ALTER TABLE {table} ALTER COLUMN {column} TYPE {data_type}, ALTER COLUMN {column} {null}"))
    }
}
