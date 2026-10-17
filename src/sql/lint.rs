//! Warnings about SQL that is valid but probably doesn't do what was meant:
//! `UPDATE`/`DELETE` without `WHERE`, `= NULL`, `NOT IN (subquery)`, comma
//! joins with no condition, `LIMIT` without `ORDER BY` and MySQL's `||`. Only statements
//! `sqlparser` understands are checked, so dialect SQL it can't parse gets
//! no warnings rather than false ones.
//!
//! A finding can also carry a risk: then running the statement needs a
//! confirmation too (see `analyze`), even with warnings turned off.

use std::collections::HashSet;
use std::ops::ControlFlow;
use std::ops::Range;

use sqlparser::ast::{BinaryOperator, Expr, LimitClause, Query, SetExpr, Spanned, Statement, Value, Visit, Visitor};
use sqlparser::tokenizer::Span;

use crate::db::Engine;
use crate::sql::analyze::{HintSource, SyntaxHint};
use crate::sql::parse::{ParsedScript, ParsedStatement};
use crate::sql::scope::{self, Scope, Source};

/// One warning: its rule's name, what's wrong, and where in the statement.
struct Finding {
    rule: &'static str,
    message: &'static str,
    span: Span,
    /// Why running it needs a confirmation, e.g. "DELETE without WHERE".
    risk: Option<&'static str>,
}

/// Every warning in `script`, in reading order, each numbered. `sql_mode`
/// is the session's (MySQL), or empty.
pub fn lints(script: &ParsedScript, sql_mode: &str) -> Vec<SyntaxHint> {
    let found: Vec<_> = script.statements.iter().flat_map(|stmt| findings(stmt, script.engine, sql_mode)).collect();
    let total = found.len();
    found
        .into_iter()
        .enumerate()
        .map(|(i, (range, finding))| SyntaxHint {
            message: finding.message.to_string(),
            offset: Some(range.start),
            len: (range.end > range.start).then_some(range.end - range.start),
            source: HintSource::Lint(finding.rule),
            nth: Some((i + 1, total)),
        })
        .collect()
}

/// The first risky finding in `stmt`, for the confirmation before running.
pub fn risk(stmt: &ParsedStatement, engine: Engine) -> Option<&'static str> {
    findings(stmt, engine, "").into_iter().find_map(|(_, f)| f.risk)
}

/// Everything the rules found in `stmt`, in reading order, with byte ranges
/// in the script.
fn findings(stmt: &ParsedStatement, engine: Engine, sql_mode: &str) -> Vec<(Range<usize>, Finding)> {
    let Ok(parsed) = &stmt.ast else { return Vec::new() };
    let mut lints = Lints {
        pipes_are_or: engine.dialect().pipes_are_or(sql_mode),
        findings: Vec::new(),
        any_rows_ok: HashSet::new(),
    };
    let _ = parsed.visit(&mut lints);
    for scope in scope::lower(parsed).iter().flat_map(Scope::walk) {
        lints.cross_join(scope);
    }
    let mut found: Vec<_> =
        lints.findings.into_iter().map(|f| (stmt.range(f.span).unwrap_or(stmt.offset..stmt.offset), f)).collect();
    found.sort_by_key(|(range, _)| range.start);
    found
}

struct Lints {
    /// Facts from the dialect (and session settings) that rules depend on,
    /// rather than which engine it is.
    pipes_are_or: bool,
    findings: Vec<Finding>,
    /// Queries whose `LIMIT` order doesn't matter: the statement's own
    /// result (you see what came back) and `EXISTS` subqueries.
    any_rows_ok: HashSet<*const Query>,
}

impl Lints {
    fn add(&mut self, rule: &'static str, message: &'static str, span: Span) {
        self.findings.push(Finding { rule, message, span, risk: None });
    }

    /// Comma-separated plain tables (not CTEs or table functions) with no
    /// WHERE: every row of one paired with every row of the other. A
    /// one-row CTE in a comma join is the usual harmless case.
    fn cross_join(&mut self, scope: &Scope) {
        if scope.has_where {
            return;
        }
        let mut plain = scope.tables.iter().filter(|t| {
            !t.joined && matches!(&t.source, Source::Table { name, .. } if !scope.is_cte(&name.to_lowercase()))
        });
        if let Some(second) = plain.nth(1) {
            self.add("cross-join", "Tables joined by a comma with no WHERE: a cross join of every row", second.span);
        }
    }

    fn add_risky(&mut self, rule: &'static str, message: &'static str, span: Span, risk: &'static str) {
        self.findings.push(Finding { rule, message, span, risk: Some(risk) });
    }
}

impl Visitor for Lints {
    type Break = ();

    fn pre_visit_statement(&mut self, statement: &Statement) -> ControlFlow<()> {
        match statement {
            Statement::Query(query) => {
                self.any_rows_ok.insert(&raw const **query);
            }
            Statement::Update(u) => {
                if u.selection.is_none() {
                    let message = "UPDATE without WHERE changes every row";
                    self.add_risky("no-where", message, u.update_token.0.span, "UPDATE without WHERE");
                }
                if u.limit.is_some() && u.order_by.is_empty() {
                    self.add("limit-no-order", "LIMIT without ORDER BY: which rows change is undefined", u.span());
                }
            }
            Statement::Delete(d) => {
                if d.selection.is_none() && d.using.is_none() {
                    let message = "DELETE without WHERE removes every row";
                    self.add_risky("no-where", message, d.delete_token.0.span, "DELETE without WHERE");
                }
                if d.limit.is_some() && d.order_by.is_empty() {
                    self.add("limit-no-order", "LIMIT without ORDER BY: which rows go is undefined", d.span());
                }
            }
            _ => {}
        }
        ControlFlow::Continue(())
    }

    fn pre_visit_query(&mut self, query: &Query) -> ControlFlow<()> {
        let limited = match &query.limit_clause {
            Some(LimitClause::LimitOffset { limit, offset, .. }) => limit.is_some() || offset.is_some(),
            Some(LimitClause::OffsetCommaLimit { .. }) => true,
            None => query.fetch.is_some(),
        };
        // `SELECT ... LIMIT 1` with only one possible row (no FROM) is fine.
        let from_tables = matches!(&*query.body, SetExpr::Select(s) if !s.from.is_empty())
            || matches!(&*query.body, SetExpr::SetOperation { .. });
        if limited && query.order_by.is_none() && from_tables && !self.any_rows_ok.contains(&(&raw const *query)) {
            let span = query.limit_clause.as_ref().map_or_else(|| query.span(), Spanned::span);
            self.add("limit-no-order", "LIMIT without ORDER BY: which rows come back is undefined", span);
        }
        ControlFlow::Continue(())
    }

    fn pre_visit_expr(&mut self, expr: &Expr) -> ControlFlow<()> {
        match expr {
            Expr::BinaryOp { left, op: op @ (BinaryOperator::Eq | BinaryOperator::NotEq), right }
                if is_null(left) || is_null(right) =>
            {
                let message = if *op == BinaryOperator::Eq {
                    "= NULL is never true: use IS NULL"
                } else {
                    "<> NULL is never true: use IS NOT NULL"
                };
                self.add("null-compare", message, expr.span());
            }
            // Harmless when the subquery already filters NULLs out.
            Expr::InSubquery { negated: true, subquery, .. }
                if !subquery.to_string().to_ascii_uppercase().contains("IS NOT NULL") =>
            {
                self.add(
                    "not-in-null",
                    "NOT IN (subquery) returns no rows if the subquery yields a NULL: NOT EXISTS is safer",
                    expr.span(),
                );
            }
            Expr::BinaryOp { op: BinaryOperator::StringConcat, .. } if self.pipes_are_or => {
                self.add("pipes-or", "|| means OR in MySQL, not concatenation: use CONCAT()", expr.span());
            }
            Expr::Exists { subquery, .. } => {
                self.any_rows_ok.insert(&raw const **subquery);
            }
            _ => {}
        }
        ControlFlow::Continue(())
    }
}

fn is_null(expr: &Expr) -> bool {
    matches!(expr, Expr::Value(v) if v.value == Value::Null)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sql::parse::parse;

    const PG: Engine = Engine::Postgres;
    const MY: Engine = Engine::MySql;

    /// The rule that fires first and the text it points at.
    fn first(sql: &str, engine: Engine) -> Option<(&'static str, String)> {
        lints(&parse(sql, engine), "").into_iter().next().map(|hint| {
            let HintSource::Lint(rule) = hint.source else { panic!("not a lint: {hint:?}") };
            let start = hint.offset.unwrap();
            (rule, sql[start..start + hint.len.unwrap_or(0)].to_string())
        })
    }

    fn rule(sql: &str, engine: Engine) -> Option<&'static str> {
        first(sql, engine).map(|(rule, _)| rule)
    }

    #[test]
    fn update_and_delete_without_where() {
        assert_eq!(first("select 1;\nupdate t set a = 1", PG), Some(("no-where", "update".into())));
        assert_eq!(first("DELETE FROM t", MY), Some(("no-where", "DELETE".into())));
        assert_eq!(rule("delete from t where id = 1", PG), None);
        assert_eq!(rule("delete from t using u where t.id = u.id", PG), None);
        assert_eq!(rule("update t set a = 1 where id = 2", MY), None);
    }

    #[test]
    fn comparing_with_null() {
        assert_eq!(first("select * from t where a = null", PG), Some(("null-compare", "a = null".into())));
        assert_eq!(rule("select * from t where a <> NULL", MY), Some("null-compare"));
        assert_eq!(rule("select * from t where a is null", PG), None);
        // Assigning NULL is fine.
        assert_eq!(rule("update t set a = null where id = 1", PG), None);
    }

    #[test]
    fn not_in_a_subquery() {
        let sql = "select * from t where id not in (select t_id from u)";
        assert_eq!(rule(sql, PG), Some("not-in-null"));
        assert_eq!(rule("select * from t where id in (select t_id from u)", PG), None);
        assert_eq!(rule("select * from t where id not in (1, 2)", PG), None);
        assert_eq!(rule("select * from t where id not in (select t_id from u where t_id is not null)", PG), None);
    }

    #[test]
    fn comma_joins_without_a_condition() {
        assert_eq!(first("select * from a, b", PG), Some(("cross-join", "b".into())));
        assert_eq!(rule("select * from a, b where a.id = b.a_id", PG), None);
        assert_eq!(rule("select * from a join b on a.id = b.a_id", PG), None);
        // CTEs and table functions are the usual reasons for a comma.
        assert_eq!(rule("with p as (select 1 as n) select * from p, t", PG), None);
        assert_eq!(rule("select * from t, json_each(t.doc)", Engine::Sqlite), None);
        // A CTE only hides tables where it is visible.
        let sql = "select * from t, p, (with p as (select 1 as n) select * from p) x";
        assert_eq!(first(sql, PG), Some(("cross-join", "p".into())));
    }

    #[test]
    fn limit_without_order_by() {
        // Looking at a few rows is fine; picking them inside a query isn't.
        assert_eq!(rule("select * from t limit 10", PG), None);
        let sql = "select * from t where id = (select id from u limit 1)";
        assert_eq!(rule(sql, PG), Some("limit-no-order"));
        assert_eq!(rule("select * from t where id = (select id from u order by id limit 1)", PG), None);
        assert_eq!(rule("insert into t select * from u limit 5", PG), Some("limit-no-order"));
        assert_eq!(rule("select * from t where exists (select 1 from u limit 1)", PG), None);
        assert_eq!(rule("delete from t where a = 1 limit 10", MY), Some("limit-no-order"));
        assert_eq!(rule("delete from t where a = 1 order by id limit 10", MY), None);
    }

    #[test]
    fn pipes_in_mysql() {
        assert_eq!(first("select a || b from t", MY), Some(("pipes-or", "a || b".into())));
        assert_eq!(rule("select a || b from t", PG), None);
        assert_eq!(rule("select a || b from t", Engine::Sqlite), None);
        assert_eq!(rule("select concat(a, b) from t", MY), None);
        // Not when the session makes || concatenate.
        let ansi = "REAL_AS_FLOAT,PIPES_AS_CONCAT,ANSI_QUOTES,IGNORE_SPACE,ONLY_FULL_GROUP_BY,ANSI";
        assert!(lints(&parse("select a || b from t", MY), ansi).is_empty());
    }

    #[test]
    fn every_warning_is_numbered() {
        let sql = "select * from t where a = null; delete from t";
        let all: Vec<_> = lints(&parse(sql, PG), "").into_iter().map(|h| (h.source, h.nth)).collect();
        assert_eq!(
            all,
            [(HintSource::Lint("null-compare"), Some((1, 2))), (HintSource::Lint("no-where"), Some((2, 2)))]
        );
    }

    #[test]
    fn the_first_in_reading_order_wins_and_unparsed_sql_is_skipped() {
        let sql = "select * from t where x = null and id not in (select a from u)";
        assert_eq!(rule(sql, PG), Some("null-compare"));
        let sql = "select * from t where id not in (select a from u) and x = null";
        assert_eq!(rule(sql, PG), Some("not-in-null"));
        assert_eq!(rule("selec * fro t; delete from t", PG), Some("no-where"));
        assert_eq!(rule("select 1; select * from t", PG), None);
    }
}
