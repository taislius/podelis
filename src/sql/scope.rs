//! Lowering: a small tree of the names each `SELECT` can see, built from
//! `sqlparser`'s full syntax tree, so rules about names don't each walk the
//! whole tree. See docs/architecture.md ("SQL checks").
//!
//! One `Scope` per `SELECT`. Names come from its `FROM` and from `WITH`
//! clauses around it; subqueries inside it are its children and see the
//! same CTEs. Tables only so far: aliases and columns come with the first
//! rule that needs them.

use std::ops::ControlFlow;

use sqlparser::ast::{ObjectNamePart, Query, Select, SetExpr, Spanned, Statement, TableFactor, Visit, Visitor};
use sqlparser::tokenizer::Span;

#[derive(Debug)]
pub struct Scope {
    /// CTE names (lowercased) visible here: from this query's `WITH` and
    /// every enclosing one.
    pub ctes: Vec<String>,
    /// `FROM` entries and their joins, in order.
    pub tables: Vec<TableRef>,
    pub has_where: bool,
    /// Subqueries in this `SELECT`: derived tables, `IN (…)`, `EXISTS`…
    pub children: Vec<Scope>,
}

#[derive(Debug)]
pub struct TableRef {
    pub source: Source,
    /// Joined with `JOIN`, rather than listed after a comma.
    pub joined: bool,
    pub span: Span,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Source {
    /// A named table, view or CTE.
    Table { schema: Option<String>, name: String },
    /// A table function, e.g. `json_each(doc)`.
    Function,
    /// `(SELECT …) x`: its own scope is among the children.
    Subquery,
    /// UNNEST, nested joins and the rest.
    Other,
}

impl Scope {
    /// Whether `name` (lowercased) is a CTE here rather than a table.
    pub fn is_cte(&self, name: &str) -> bool {
        self.ctes.iter().any(|c| c == name)
    }

    /// This scope and every scope inside it.
    pub fn walk(&self) -> Box<dyn Iterator<Item = &Scope> + '_> {
        Box::new(std::iter::once(self).chain(self.children.iter().flat_map(Scope::walk)))
    }
}

/// The scopes of every query in `statements`, outermost first.
pub fn lower(statements: &[Statement]) -> Vec<Scope> {
    statements.iter().flat_map(|stmt| outermost_queries(stmt, &[])).collect()
}

fn lower_query(query: &Query, outer: &[String]) -> Vec<Scope> {
    let mut visible = outer.to_vec();
    let mut scopes = Vec::new();
    if let Some(with) = &query.with {
        // All of them, so recursive CTEs can see themselves.
        visible.extend(with.cte_tables.iter().map(|cte| cte.alias.name.value.to_lowercase()));
        for cte in &with.cte_tables {
            scopes.extend(lower_query(&cte.query, &visible));
        }
    }
    lower_body(&query.body, &visible, &mut scopes);
    scopes
}

fn lower_body(body: &SetExpr, visible: &[String], scopes: &mut Vec<Scope>) {
    match body {
        SetExpr::Select(select) => scopes.push(lower_select(select, visible)),
        SetExpr::SetOperation { left, right, .. } => {
            lower_body(left, visible, scopes);
            lower_body(right, visible, scopes);
        }
        SetExpr::Query(query) => scopes.extend(lower_query(query, visible)),
        // VALUES, and data-modifying CTE bodies: no FROM of their own here.
        _ => {}
    }
}

fn lower_select(select: &Select, visible: &[String]) -> Scope {
    let mut tables = Vec::new();
    for from in &select.from {
        tables.push(table_ref(&from.relation, false));
        tables.extend(from.joins.iter().map(|join| table_ref(&join.relation, true)));
    }
    Scope {
        ctes: visible.to_vec(),
        tables,
        has_where: select.selection.is_some(),
        children: outermost_queries(select, visible),
    }
}

fn table_ref(factor: &TableFactor, joined: bool) -> TableRef {
    let source = match factor {
        TableFactor::Table { name, args, .. } => {
            if args.is_some() {
                Source::Function
            } else {
                let mut parts: Vec<String> = name
                    .0
                    .iter()
                    .map(|part| match part {
                        ObjectNamePart::Identifier(id) => id.value.clone(),
                        ObjectNamePart::Function(f) => f.name.value.clone(),
                    })
                    .collect();
                let name = parts.pop().unwrap_or_default();
                Source::Table { schema: parts.pop(), name }
            }
        }
        TableFactor::Derived { .. } => Source::Subquery,
        TableFactor::Function { .. } | TableFactor::TableFunction { .. } => Source::Function,
        _ => Source::Other,
    };
    TableRef { source, joined, span: factor.span() }
}

/// Lowers the queries in `node` that aren't inside another query in it:
/// those are reached through their parent's scope.
fn outermost_queries<V: Visit + ?Sized>(node: &V, visible: &[String]) -> Vec<Scope> {
    struct Outermost<'v> {
        depth: usize,
        visible: &'v [String],
        scopes: Vec<Scope>,
    }
    impl Visitor for Outermost<'_> {
        type Break = ();
        fn pre_visit_query(&mut self, query: &Query) -> ControlFlow<()> {
            if self.depth == 0 {
                self.scopes.extend(lower_query(query, self.visible));
            }
            self.depth += 1;
            ControlFlow::Continue(())
        }
        fn post_visit_query(&mut self, _: &Query) -> ControlFlow<()> {
            self.depth -= 1;
            ControlFlow::Continue(())
        }
    }
    let mut outermost = Outermost { depth: 0, visible, scopes: Vec::new() };
    let _ = node.visit(&mut outermost);
    outermost.scopes
}

#[cfg(test)]
mod tests {
    use std::fmt::Write as _;

    use insta::assert_snapshot;

    use super::*;
    use crate::db::Engine;
    use crate::parse::parse;

    /// The scopes of `sql` as an indented outline.
    fn outline(sql: &str) -> String {
        fn write(out: &mut String, scope: &Scope, depth: usize) {
            let tables: Vec<String> = scope
                .tables
                .iter()
                .map(|t| {
                    let s = match &t.source {
                        Source::Table { schema: Some(schema), name } => format!("{schema}.{name}"),
                        Source::Table { schema: None, name } => name.clone(),
                        Source::Function => "fn()".into(),
                        Source::Subquery => "(subquery)".into(),
                        Source::Other => "?".into(),
                    };
                    if t.joined { format!("join {s}") } else { s }
                })
                .collect();
            let where_ = if scope.has_where { " where" } else { "" };
            let _ = writeln!(
                out,
                "{}select from [{}]{where_} ctes [{}]",
                "  ".repeat(depth),
                tables.join(", "),
                scope.ctes.join(", ")
            );
            for child in &scope.children {
                write(out, child, depth + 1);
            }
        }
        let parsed = parse(sql, Engine::Postgres);
        let mut out = String::new();
        for stmt in &parsed.statements {
            for scope in lower(stmt.ast.as_ref().unwrap()) {
                write(&mut out, &scope, 0);
            }
        }
        out
    }

    #[test]
    fn joins_and_schemas() {
        assert_snapshot!(outline("select * from public.orders o join customers c on o.cid = c.id, items where true"));
    }

    #[test]
    fn subqueries_are_children() {
        assert_snapshot!(outline(
            "select * from (select id from a) x where x.id in (select a_id from b where exists (select 1 from c))"
        ));
    }

    #[test]
    fn ctes_are_visible_inside_and_not_outside() {
        assert_snapshot!(outline(
            "with p as (select 1 as n), q as (select * from p) select * from q, (with r as (select 2) select * from r) s; \
             select * from p"
        ));
    }

    #[test]
    fn set_operations_and_other_statements() {
        assert_snapshot!(outline(
            "select a from t union select b from u; \
             insert into t select * from u; \
             delete from t where id in (select t_id from v); \
             select * from t, json_each(t.doc)"
        ));
    }
}
