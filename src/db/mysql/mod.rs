//! MySQL (and MariaDB).

mod agent_constraints;
pub(super) mod cells;
pub(super) mod connect;
mod plan;
pub(super) mod queries;

use super::dialect::{Capabilities, Dialect, LexerRules, Placeholders, Plan, hex_digits};

pub struct MySql;

impl Dialect for MySql {
    fn agent_constraints(&self) -> &'static crate::db::agent_constraints::AgentConstraints {
        &agent_constraints::AGENT_CONSTRAINTS
    }

    fn label(&self) -> &'static str {
        "MySQL"
    }
    fn url_schemes(&self) -> &'static [&'static str] {
        &["mysql", "mariadb"]
    }
    fn default_port(&self) -> &'static str {
        "3306"
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            file_based: false,
            cancel: true,
            activity: true,
            schemas: false,
            routines: true,
            server_txn_state: false,
            ddl_commits: true,
            stores_create_sql: false,
        }
    }
    fn lexer(&self) -> LexerRules {
        LexerRules {
            hash_comments: true,
            dash_comment_needs_space: true,
            nested_comments: false,
            backslash_escapes: true,
            double_quoted_strings: true,
            backtick_identifiers: true,
            dollar_quotes: false,
        }
    }
    fn placeholders(&self) -> Placeholders {
        Placeholders { dollar: false, question: true, sqlite: false }
    }
    fn is_system_table(&self, schema: &str, _table: &str) -> bool {
        ["information_schema", "performance_schema", "mysql", "sys"].iter().any(|s| s.eq_ignore_ascii_case(schema))
    }
    fn parser(&self) -> Box<dyn sqlparser::dialect::Dialect> {
        Box::new(sqlparser::dialect::MySqlDialect {})
    }
    /// Unless the session's `sql_mode` has `PIPES_AS_CONCAT` (`ANSI` lists it
    /// too).
    fn pipes_are_or(&self, sql_mode: &str) -> bool {
        !sql_mode.split(',').any(|mode| mode.trim().eq_ignore_ascii_case("PIPES_AS_CONCAT"))
    }
    fn sql_mode_query(&self) -> Option<&'static str> {
        Some("SELECT @@SESSION.sql_mode")
    }
    fn identifier_quote(&self) -> char {
        '`'
    }
    fn string_literal(&self, value: &str) -> String {
        format!("'{}'", value.replace('\\', "\\\\").replace('\'', "''"))
    }
    /// `0xDEADBEEF`, as the `mysql` client shows it.
    fn hex(&self, bytes: &[u8]) -> String {
        format!("0x{}", hex_digits(bytes, true))
    }
    fn is_binary_type(&self, ty: &str) -> bool {
        ["BINARY", "VARBINARY", "TINYBLOB", "BLOB", "MEDIUMBLOB", "LONGBLOB", "GEOMETRY"]
            .iter()
            .any(|t| ty.eq_ignore_ascii_case(t))
    }
    fn binary_literal(&self, shown: &str) -> Option<String> {
        let digits = shown.strip_prefix("0x")?;
        if digits.is_empty() {
            return Some("X''".into());
        }
        digits.chars().all(|c| c.is_ascii_hexdigit()).then(|| shown.to_string())
    }
    fn version_query(&self) -> &'static str {
        "SELECT VERSION()"
    }
    /// FORMAT=TREE where the server has it (MySQL 8.0.16+); older MySQL
    /// and MariaDB get the classic table.
    fn explain(&self, stmt: &str, version: &str) -> String {
        match Server::of(version) {
            Server::MySql(v) if v < (8, 0, 16) => format!("EXPLAIN {stmt}"),
            Server::MariaDb => format!("EXPLAIN {stmt}"),
            Server::MySql(_) => format!("EXPLAIN FORMAT=TREE {stmt}"),
        }
    }
    /// MySQL 8.0.18+ has EXPLAIN ANALYZE; MariaDB's is ANALYZE (a table
    /// with the actual rows next to the estimates).
    fn explain_analyze(&self, stmt: &str, version: &str) -> Option<String> {
        match Server::of(version) {
            Server::MySql(v) if v < (8, 0, 18) => None,
            Server::MariaDb => Some(format!("ANALYZE {stmt}")),
            Server::MySql(_) => Some(format!("EXPLAIN ANALYZE {stmt}")),
        }
    }
    /// FORMAT=TREE is already a readable tree; the classic table is lined
    /// up. Notes are added to both.
    fn format_plan(&self, columns: &[String], rows: &[Vec<Option<String>>]) -> Option<Plan> {
        if columns.len() > 1 {
            return (!rows.is_empty()).then(|| plan::analyze_table(columns, rows));
        }
        Some(plan::analyze(rows.first()?.first()?.clone()?.trim_end().to_string()))
    }
    fn full_scans(&self, columns: &[String], rows: &[Vec<Option<String>>]) -> Vec<crate::db::dialect::Scan> {
        if columns.len() > 1 {
            return plan::table_full_scans(columns, rows);
        }
        rows.first().and_then(|r| r.first()).and_then(|c| c.as_deref()).map(plan::full_scans).unwrap_or_default()
    }
    fn insert_defaults(&self, table: &str) -> String {
        format!("INSERT INTO {table} () VALUES ()")
    }
    fn real_type(&self) -> &'static str {
        "DOUBLE"
    }
    /// With microseconds: a plain `DATETIME` would round fractions away.
    fn timestamp_type(&self) -> &'static str {
        "DATETIME(6)"
    }
    fn modify_column(&self, table: &str, column: &str, data_type: &str, nullable: bool) -> Option<String> {
        let null = if nullable { "NULL" } else { "NOT NULL" };
        Some(format!("ALTER TABLE {table} MODIFY COLUMN {column} {data_type} {null}"))
    }
}

/// The kind of server, from its version string (`8.0.36`, `5.7.44-log`,
/// `10.11.6-MariaDB-0+deb12u1`).
#[derive(Debug, PartialEq)]
enum Server {
    MySql((u32, u32, u32)),
    MariaDb,
}

impl Server {
    /// A version that can't be read is taken to be a current MySQL.
    fn of(version: &str) -> Self {
        if version.to_ascii_lowercase().contains("mariadb") {
            return Server::MariaDb;
        }
        let mut parts = version.split(|c: char| !c.is_ascii_digit()).map(|p| p.parse::<u32>().ok());
        match (parts.next().flatten(), parts.next().flatten(), parts.next().flatten()) {
            (Some(major), Some(minor), patch) => Server::MySql((major, minor, patch.unwrap_or(0))),
            _ => Server::MySql((u32::MAX, 0, 0)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mysql_tree_is_passed_through() {
        let rows = vec![vec![Some("-> Table scan on t  (cost=0.35 rows=1)\n".to_string())]];
        let columns = ["EXPLAIN".to_string()];
        assert_eq!(MySql.format_plan(&columns, &rows).unwrap().text, "-> Table scan on t  (cost=0.35 rows=1)");
    }

    #[test]
    fn older_servers_get_the_classic_explain() {
        let explain = |version| MySql.explain("SELECT 1", version);
        assert_eq!(explain("8.0.36"), "EXPLAIN FORMAT=TREE SELECT 1");
        assert_eq!(explain("8.0.16"), "EXPLAIN FORMAT=TREE SELECT 1");
        assert_eq!(explain("9.1.0"), "EXPLAIN FORMAT=TREE SELECT 1");
        assert_eq!(explain(""), "EXPLAIN FORMAT=TREE SELECT 1");
        assert_eq!(explain("8.0.15"), "EXPLAIN SELECT 1");
        assert_eq!(explain("5.7.44-log"), "EXPLAIN SELECT 1");
        assert_eq!(explain("10.11.6-MariaDB-0+deb12u1"), "EXPLAIN SELECT 1");

        let analyze = |version| MySql.explain_analyze("SELECT 1", version);
        assert_eq!(analyze("8.0.18").as_deref(), Some("EXPLAIN ANALYZE SELECT 1"));
        assert_eq!(analyze("8.0.17"), None);
        assert_eq!(analyze("5.6.51"), None);
        assert_eq!(analyze("11.4.2-MariaDB").as_deref(), Some("ANALYZE SELECT 1"));
    }
}
