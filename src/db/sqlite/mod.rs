//! SQLite.

use std::fmt::Write as _;

mod agent_constraints;
pub(super) mod cells;
pub(super) mod connect;
pub(super) mod queries;

use super::dialect::{Capabilities, Dialect, LexerRules, Placeholders, Plan, PlanNote, hex_digits};

pub struct Sqlite;

impl Dialect for Sqlite {
    fn agent_constraints(&self) -> &'static crate::db::agent_constraints::AgentConstraints {
        &agent_constraints::AGENT_CONSTRAINTS
    }

    /// `X'DEADBEEF'`, as SQLite's `quote()` writes it.
    fn hex(&self, bytes: &[u8]) -> String {
        format!("X'{}'", hex_digits(bytes, true))
    }
    fn is_binary_type(&self, ty: &str) -> bool {
        ty.eq_ignore_ascii_case("BLOB")
    }
    fn binary_literal(&self, shown: &str) -> Option<String> {
        let digits = shown.strip_prefix("X'")?.strip_suffix('\'')?;
        (digits.len() % 2 == 0 && digits.chars().all(|c| c.is_ascii_hexdigit())).then(|| shown.to_string())
    }
    fn label(&self) -> &'static str {
        "SQLite"
    }
    fn url_schemes(&self) -> &'static [&'static str] {
        &["sqlite", "sqlite3"]
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            file_based: true,
            cancel: true,
            activity: false,
            schemas: false,
            routines: false,
            server_txn_state: false,
            ddl_commits: false,
            stores_create_sql: true,
        }
    }
    fn lexer(&self) -> LexerRules {
        LexerRules {
            hash_comments: false,
            dash_comment_needs_space: false,
            nested_comments: false,
            backslash_escapes: false,
            double_quoted_strings: false,
            backtick_identifiers: true,
            dollar_quotes: false,
        }
    }
    fn placeholders(&self) -> Placeholders {
        Placeholders { dollar: false, question: true, sqlite: true }
    }
    fn is_system_table(&self, _schema: &str, table: &str) -> bool {
        table.starts_with("sqlite_")
    }
    fn parser(&self) -> Box<dyn sqlparser::dialect::Dialect> {
        Box::new(sqlparser::dialect::SQLiteDialect {})
    }
    fn version_query(&self) -> &'static str {
        "SELECT sqlite_version()"
    }
    fn explain(&self, stmt: &str, _version: &str) -> String {
        format!("EXPLAIN QUERY PLAN {stmt}")
    }
    /// `SCAN t` (or `SCAN TABLE t`) without an index.
    fn full_scans(&self, _columns: &[String], rows: &[Vec<Option<String>>]) -> Vec<crate::db::dialect::Scan> {
        rows.iter()
            .filter_map(|r| r.get(3).cloned().flatten())
            .filter(|d| d.starts_with("SCAN ") && !d.contains(" USING "))
            .filter_map(|d| {
                let rest = d.strip_prefix("SCAN ")?;
                let rest = rest.strip_prefix("TABLE ").unwrap_or(rest);
                Some(crate::db::dialect::Scan::new(rest.split_whitespace().next()?, None))
            })
            .collect()
    }
    fn format_plan(&self, _columns: &[String], rows: &[Vec<Option<String>>]) -> Option<Plan> {
        let notes = plan_notes(rows.iter().map(|r| r.get(3).cloned().flatten().unwrap_or_default()));
        Some(Plan { text: plan_tree(rows), notes })
    }
    fn integer_type(&self) -> &'static str {
        "INTEGER"
    }
    fn real_type(&self) -> &'static str {
        "REAL"
    }
    fn add_foreign_key(&self, _table: &str, _column: &str, _target: &str, _target_column: &str) -> Option<String> {
        None
    }
}

/// EXPLAIN QUERY PLAN rows (id, parent, _, detail), nested by parent.
fn plan_tree(rows: &[Vec<Option<String>>]) -> String {
    let mut depth_of: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut out = String::new();
    for row in rows {
        let field = |i: usize| row.get(i).cloned().flatten().unwrap_or_default();
        let (id, parent, detail) = (field(0), field(1), field(3));
        let depth = depth_of.get(&parent).map_or(0, |d| d + 1);
        depth_of.insert(id, depth);
        let _ = writeln!(out, "{}{}{detail}", "   ".repeat(depth), if depth == 0 { "" } else { "└─ " });
    }
    out.trim_end().to_string()
}

/// What stands out in a plan, by its detail lines (one per plan line).
/// SQLite reports no row counts, so a lone full scan isn't flagged: only a
/// table scanned again for every row of an outer loop.
fn plan_notes(details: impl Iterator<Item = String>) -> Vec<PlanNote> {
    let mut notes = Vec::new();
    let mut loops = 0;
    for (line, detail) in details.enumerate() {
        let note = |text: String| PlanNote { text, line: Some(line) };
        let table_loop = detail.starts_with("SCAN ") || detail.starts_with("SEARCH ");
        if let Some(table) = detail.strip_prefix("SCAN ")
            && loops > 0
            && !detail.contains(" USING ")
            && !table.starts_with('(')
            && !table.starts_with("CONSTANT")
        {
            let table = table.split_whitespace().next().unwrap_or(table);
            notes.push(note(format!(
                "Full scan of {table} for every row of the outer loop: an index on its join columns may help"
            )));
        }
        if let Some(what) = detail.strip_prefix("USE TEMP B-TREE FOR ") {
            notes.push(note(format!("Sorts in a temporary B-tree for {what}: an index matching it would avoid that")));
        }
        if detail.starts_with("CORRELATED ") {
            notes.push(note("Correlated subquery: it runs again for every outer row".to_string()));
        }
        loops += usize::from(table_loop);
    }
    notes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sqlite_plan_nests_by_parent() {
        let row = |id: &str, parent: &str, detail: &str| {
            vec![Some(id.to_string()), Some(parent.to_string()), Some("0".into()), Some(detail.to_string())]
        };
        let rows = vec![
            row("2", "0", "SCAN orders"),
            row("5", "2", "SEARCH users USING INTEGER PRIMARY KEY"),
            row("9", "0", "USE TEMP B-TREE"),
        ];
        assert_eq!(plan_tree(&rows), "SCAN orders\n   └─ SEARCH users USING INTEGER PRIMARY KEY\nUSE TEMP B-TREE");
    }

    #[test]
    fn notes_nested_scans_temp_sorts_and_correlated_subqueries() {
        let notes = |details: &[&str]| -> Vec<(String, Option<usize>)> {
            plan_notes(details.iter().map(ToString::to_string)).into_iter().map(|n| (n.text, n.line)).collect()
        };
        // One scan on its own is fine; scanning b for every row of a isn't.
        assert!(notes(&["SCAN a"]).is_empty());
        assert!(notes(&["SCAN a", "SEARCH b USING INDEX b_a (a_id=?)"]).is_empty());
        let found = notes(&["SCAN a", "SCAN b", "USE TEMP B-TREE FOR ORDER BY", "CORRELATED SCALAR SUBQUERY 1"]);
        let lines: Vec<Option<usize>> = found.iter().map(|(_, line)| *line).collect();
        assert_eq!(lines, [Some(1), Some(2), Some(3)]);
        assert!(found[0].0.starts_with("Full scan of b for every row"));
    }
}
