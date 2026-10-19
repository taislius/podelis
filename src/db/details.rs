//! Details loaded on demand for the schema navigator: a table's indexes and
//! foreign keys, view definitions, and stored functions/procedures.

use super::dialect::Scan;
use super::*;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct TableDetails {
    pub indexes: Vec<IndexInfo>,
    pub foreign_keys: Vec<ForeignKey>,
    /// A view's query, or (SQLite) the table's CREATE statement.
    pub definition: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct IndexInfo {
    pub name: String,
    /// Comma-separated columns (empty for expression indexes).
    pub columns: String,
    pub unique: bool,
    pub primary: bool,
    /// The CREATE INDEX statement, where the server provides one.
    pub definition: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ForeignKey {
    pub column: String,
    /// `table.column`, or `schema.table.column` for another schema.
    pub target: String,
    pub target_schema: String,
    pub target_table: String,
    /// `None` when it's the target's primary key, unnamed (SQLite).
    pub target_column: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoutineInfo {
    pub schema: String,
    /// Name, with the argument types on Postgres (overloads differ by them).
    pub name: String,
    /// "function" or "procedure".
    pub kind: String,
    /// How to look the definition up again (an oid on Postgres).
    pub(super) id: String,
}

/// A comment the database keeps on a table or view (`column` is `None`)
/// or on one of its columns: what the schema's authors said it means.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Comment {
    pub schema: String,
    pub table: String,
    pub column: Option<String>,
    pub text: String,
}

/// A foreign key's column and what it points to: one row of the whole
/// database's links, for showing related tables.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Link {
    pub schema: String,
    pub table: String,
    pub column: String,
    pub target_schema: String,
    pub target_table: String,
    /// `None` when it's the target's primary key, unnamed (SQLite).
    pub target_column: Option<String>,
}

pub(super) fn fk_target(own_schema: &str, schema: &str, table: &str, column: &str) -> String {
    if schema == own_schema { format!("{table}.{column}") } else { format!("{schema}.{table}.{column}") }
}

impl Database {
    pub async fn table_details(&self, schema: &str, table: &str) -> Result<TableDetails> {
        on_driver!(self.0.engine, engine::queries::table_details(self, schema, table).await)
    }

    /// The tables `sql`'s reads would read in full, with about how many
    /// rows each (the plan's estimate, else the catalog's): what a guard
    /// against heavy queries asks about. Only reads are planned, at most
    /// 20 statements; one that can't be planned is left out.
    ///
    /// Each engine plans them its own way (`full_scans_of`: PostgreSQL on
    /// a connection of its own, so one that fails can't abort the
    /// session's open transaction).
    pub async fn full_scans(&self, sql: &str) -> Result<Vec<(String, f64)>> {
        let engine = self.engine();
        let mut scans = Vec::new();
        for statement in crate::sql::split_statements(sql, engine).into_iter().take(20) {
            let infos = crate::sql::analyze::analyze(&crate::sql::parse::parse(statement, engine));
            let starts = statement.trim_start().get(..7).unwrap_or_default().to_ascii_lowercase();
            if infos.iter().any(|i| i.kind != crate::sql::analyze::Kind::Read)
                || starts.starts_with("explain")
                || starts.starts_with("show")
            {
                continue;
            }
            let found = on_driver!(engine, engine::queries::full_scans_of(self, statement).await);
            scans.extend(found.unwrap_or_default());
        }
        if scans.iter().all(|s| s.rows.is_some()) {
            return Ok(scans.into_iter().filter_map(|s| Some((s.name(), s.rows?))).collect());
        }
        Ok(on_driver!(engine, engine::queries::estimated_rows(self, scans).await))
    }

    /// A statement's full scans from its EXPLAIN on the session (for
    /// engines whose plans don't need a connection of their own).
    pub(super) async fn scans_by_explain(&self, statement: &str) -> Option<Vec<Scan>> {
        let dialect = self.engine().dialect();
        let plan = self.run(&dialect.explain(statement, self.version()), 10_000).await.ok()?;
        Some(dialect.full_scans(&plan.columns, &plan.rows))
    }

    /// Scans' rows: the plan's, else the catalog's estimate (all tables
    /// listed once).
    pub(super) async fn estimates_from_catalog(&self, scans: Vec<Scan>) -> Vec<(String, f64)> {
        let tables = self.tables(false).await.unwrap_or_default();
        let mut out = Vec::new();
        for scan in scans {
            #[allow(clippy::cast_precision_loss)] // an estimate
            let estimate = || {
                tables
                    .iter()
                    .find(|t| {
                        t.name.eq_ignore_ascii_case(&scan.table)
                            && scan.schema.as_ref().is_none_or(|s| t.schema.eq_ignore_ascii_case(s))
                    })
                    .and_then(|t| t.rows)
                    .map(|r| r as f64)
            };
            if let Some(rows) = scan.rows.or_else(estimate) {
                out.push((scan.name(), rows));
            }
        }
        out
    }

    /// Every foreign key of the database (the system catalogs left out), in
    /// one query: what points where, both ways.
    pub async fn links(&self) -> Result<Vec<Link>> {
        let rows: Vec<(String, String, String, String, String, Option<String>)> =
            on_driver!(self.0.engine, engine::queries::link_rows(self).await?);
        Ok(rows
            .into_iter()
            .map(|(schema, table, column, target_schema, target_table, target_column)| Link {
                schema,
                table,
                column,
                target_schema,
                target_table,
                target_column,
            })
            .collect())
    }

    /// Every table, view and column comment (the system catalogs left
    /// out); SQLite has none.
    pub async fn comments(&self) -> Result<Vec<Comment>> {
        let rows = on_driver!(self.0.engine, engine::queries::comment_rows(self).await?);
        Ok(rows
            .into_iter()
            .filter(|(_, _, _, text)| !text.trim().is_empty())
            .map(|(schema, table, column, text)| Comment { schema, table, column, text })
            .collect())
    }

    /// User-defined functions and procedures (none on SQLite). Postgres
    /// leaves out those that belong to extensions.
    pub async fn routines(&self) -> Result<Vec<RoutineInfo>> {
        let rows: Vec<(String, String, String, String)> =
            on_driver!(self.0.engine, engine::queries::routine_rows(self).await?);
        Ok(rows.into_iter().map(|(schema, name, kind, id)| RoutineInfo { schema, name, kind, id }).collect())
    }

    /// The CREATE statement of a function or procedure.
    pub async fn routine_definition(&self, routine: &RoutineInfo) -> Result<String> {
        on_driver!(self.0.engine, engine::queries::routine_definition(self, routine).await)
    }
}
