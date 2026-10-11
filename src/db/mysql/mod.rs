//! MySQL (and MariaDB).

pub(super) mod cells;
pub(super) mod connect;
mod plan;
pub(super) mod queries;

use super::dialect::{Capabilities, Dialect, LexerRules, Placeholders, Plan};

pub struct MySql;

impl Dialect for MySql {
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
        Placeholders { dollar: false, question: true }
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
    fn version_query(&self) -> &'static str {
        "SELECT VERSION()"
    }
    fn explain(&self, stmt: &str) -> String {
        format!("EXPLAIN FORMAT=TREE {stmt}")
    }
    fn explain_analyze(&self, stmt: &str) -> Option<String> {
        Some(format!("EXPLAIN ANALYZE {stmt}"))
    }
    /// FORMAT=TREE is already a readable tree; notes are added.
    fn format_plan(&self, rows: &[Vec<Option<String>>]) -> Option<Plan> {
        Some(plan::analyze(rows.first()?.first()?.clone()?.trim_end().to_string()))
    }
    fn insert_defaults(&self, table: &str) -> String {
        format!("INSERT INTO {table} () VALUES ()")
    }
    fn real_type(&self) -> &'static str {
        "DOUBLE"
    }
    fn modify_column(&self, table: &str, column: &str, data_type: &str, nullable: bool) -> Option<String> {
        let null = if nullable { "NULL" } else { "NOT NULL" };
        Some(format!("ALTER TABLE {table} MODIFY COLUMN {column} {data_type} {null}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mysql_tree_is_passed_through() {
        let rows = vec![vec![Some("-> Table scan on t  (cost=0.35 rows=1)\n".to_string())]];
        assert_eq!(MySql.format_plan(&rows).unwrap().text, "-> Table scan on t  (cost=0.35 rows=1)");
    }
}
