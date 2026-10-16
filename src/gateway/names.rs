//! Checking a model's SQL against the real schema before anyone sees it:
//! every table it reads must exist, and (where every table it reads is a
//! real one) every column it names. What isn't there goes back to the
//! model, so it fixes the query instead of the user finding out when it
//! runs.
//!
//! It never decides alone: the SQL is shown either way, with the names
//! still unknown said beside it. A parser can't follow every dialect's
//! corner, so a valid query flagged here is a nuisance, not a block.

use std::collections::BTreeSet;
use std::ops::ControlFlow;

use sqlparser::ast::{Expr, Query, SelectItem, SetExpr, Statement, Visit, Visitor};

use super::context::Schema;
use crate::db::Engine;
use crate::scope::{self, Source};

/// What's wrong with `sql` for `schema`: names it uses that aren't
/// there, or why it couldn't be read at all.
pub fn problems(sql: &str, engine: Engine, schema: &Schema) -> Vec<String> {
    let script = crate::parse::parse(sql, engine);
    let mut statements: Vec<Statement> = Vec::new();
    for s in &script.statements {
        if let Ok(parsed) = &s.ast {
            statements.extend(parsed.iter().cloned());
        } else {
            let (message, _) = s.error().unwrap_or_default();
            return vec![format!("it doesn't parse as {}: {message}", engine.label())];
        }
    }
    if statements.is_empty() {
        return vec!["there's no SQL in it".to_string()];
    }
    let lower = |s: &str| s.to_lowercase();
    let find = |schema_name: Option<&str>, name: &str| {
        schema
            .tables
            .iter()
            .find(|(t, _)| lower(&t.name) == lower(name) && schema_name.is_none_or(|s| lower(&t.schema) == lower(s)))
    };
    let scopes = scope::lower(&statements);
    let mut problems = Vec::new();
    let mut columns: BTreeSet<String> = BTreeSet::new();
    // Columns are checked only where every source is a known table.
    let mut plain = true;
    for scope in scopes.iter().flat_map(scope::Scope::walk) {
        plain &= scope.ctes.is_empty();
        for table in &scope.tables {
            match &table.source {
                Source::Table { name, .. } if scope.is_cte(&lower(name)) => plain = false,
                Source::Table { schema: s, name } => {
                    if let Some((_, cols)) = find(s.as_deref(), name) {
                        columns.extend(cols.iter().map(|c| lower(&c.name)));
                    } else {
                        let named = s.as_ref().map_or_else(|| name.clone(), |s| format!("{s}.{name}"));
                        let problem = format!("there's no table `{named}`");
                        if !problems.contains(&problem) {
                            problems.push(problem);
                        }
                        plain = false;
                    }
                }
                Source::Function | Source::Subquery | Source::Other => plain = false,
            }
        }
    }
    // Columns every table has without listing them.
    let builtin = engine.dialect().hidden_columns();
    let reads_tables = !columns.is_empty();
    columns.extend(builtin.iter().map(|c| (*c).to_string()));
    if plain && reads_tables {
        let mut names = Names::default();
        for s in &statements {
            let _ = s.visit(&mut names);
        }
        let unknown: Vec<String> =
            names.used.into_iter().filter(|n| !columns.contains(n) && !names.aliases.contains(n)).collect();
        problems.extend(unknown.into_iter().map(|c| format!("no table it reads has a column `{c}`")));
    }
    problems
}

/// The column names a statement uses, and the names its `SELECT`s give
/// (`AS total`), which later clauses may use too.
#[derive(Default)]
struct Names {
    used: BTreeSet<String>,
    aliases: BTreeSet<String>,
}

impl Visitor for Names {
    type Break = ();

    fn pre_visit_query(&mut self, query: &Query) -> ControlFlow<()> {
        self.aliases_in(&query.body);
        ControlFlow::Continue(())
    }

    fn pre_visit_expr(&mut self, expr: &Expr) -> ControlFlow<()> {
        match expr {
            Expr::Identifier(id) => {
                self.used.insert(id.value.to_lowercase());
            }
            // `o.total`: the column is the last part; the first is a table
            // or its alias, which the table check covers.
            Expr::CompoundIdentifier(parts) if parts.len() >= 2 => {
                if let Some(last) = parts.last() {
                    self.used.insert(last.value.to_lowercase());
                }
            }
            _ => {}
        }
        ControlFlow::Continue(())
    }
}

impl Names {
    fn aliases_in(&mut self, body: &SetExpr) {
        match body {
            SetExpr::Select(select) => {
                for item in &select.projection {
                    if let SelectItem::ExprWithAlias { alias, .. } = item {
                        self.aliases.insert(alias.value.to_lowercase());
                    }
                }
            }
            SetExpr::SetOperation { left, right, .. } => {
                self.aliases_in(left);
                self.aliases_in(right);
            }
            SetExpr::Query(q) => self.aliases_in(&q.body),
            _ => {}
        }
    }
}

/// The SQL in a model's answer: its fenced `sql` block (or the first
/// fenced block), else the whole answer if it reads as SQL. And what it
/// said besides.
pub fn split_answer(answer: &str) -> (Option<String>, String) {
    // A block labelled sql first (any case), else the first block.
    let lower = answer.to_ascii_lowercase();
    let start = lower.find("```sql").or_else(|| answer.find("```"));
    // The rest of the fence's line is its label (`sql`, `postgresql`…),
    // not SQL; on one line (```SELECT 1```) there's none.
    let fence = start.map(|start| {
        let after = start + 3;
        let line_end = answer[after..].find('\n').map(|i| after + i);
        let label = line_end.map(|end| &answer[after..end]);
        match (label, line_end) {
            (Some(l), Some(end)) if l.trim().chars().all(|c| c.is_alphanumeric() || "+-_#.".contains(c)) => {
                (start, end + 1)
            }
            _ => (start, after),
        }
    });
    if let Some((start, body)) = fence
        && let Some(len) = answer[body..].find("```")
    {
        let sql = answer[body..body + len].trim().to_string();
        // A line apart: `FACT:` lines after the block stay lines of their own.
        let said = format!("{}\n{}", answer[..start].trim(), answer[body + len + 3..].trim());
        return ((!sql.is_empty()).then_some(sql), said.trim().to_string());
    }
    let first = answer.split_whitespace().next().unwrap_or_default().to_ascii_uppercase();
    if matches!(first.as_str(), "SELECT" | "WITH") {
        return (Some(answer.trim().to_string()), String::new());
    }
    (None, answer.trim().to_string())
}

#[cfg(test)]
mod tests;
