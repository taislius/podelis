//! Details loaded on demand for the schema navigator: a table's indexes and
//! foreign keys, view definitions, and stored functions/procedures.

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
        match &self.0.pool {
            Pool::Postgres(p) => postgres::queries::table_details(p, schema, table).await,
            Pool::MySql(p) => mysql::queries::table_details(p, schema, table).await,
            Pool::Sqlite(_) => {
                let mut c = self.sqlite_session().await?;
                sqlite::queries::table_details(&mut c, table).await
            }
        }
    }

    /// The tables `sql`'s reads would read in full, with about how many
    /// rows each (the plan's estimate, else the catalog's): what a guard
    /// against heavy queries asks about. Only reads are planned, at most
    /// 20 statements; one that can't be planned is left out.
    ///
    /// On PostgreSQL the plans are made on a connection of their own: one
    /// that fails would otherwise abort the session's open transaction.
    pub async fn full_scans(&self, sql: &str) -> Result<Vec<(String, f64)>> {
        let engine = self.engine();
        let dialect = engine.dialect();
        let mut scans = Vec::new();
        for statement in crate::sql::split_statements(sql, engine).into_iter().take(20) {
            let infos = crate::analyze::analyze(&crate::parse::parse(statement, engine));
            let starts = statement.trim_start().get(..7).unwrap_or_default().to_ascii_lowercase();
            if infos.iter().any(|i| i.kind != crate::analyze::Kind::Read)
                || starts.starts_with("explain")
                || starts.starts_with("show")
            {
                continue;
            }
            if let Pool::Postgres(p) = &self.0.pool {
                let Some(json) = postgres::queries::guard_plan(p, statement).await else { continue };
                let rows = vec![vec![Some(json)]];
                scans.extend(dialect.full_scans(&[], &rows));
                continue;
            }
            let Ok(plan) = self.run(&dialect.explain(statement, self.version()), 10_000).await else { continue };
            scans.extend(dialect.full_scans(&plan.columns, &plan.rows));
        }
        if scans.iter().all(|s| s.rows.is_some()) {
            return Ok(scans.into_iter().filter_map(|s| Some((s.name(), s.rows?))).collect());
        }
        let mut out = Vec::new();
        if let Pool::Postgres(p) = &self.0.pool {
            // Only the tables scanned, by schema: a catalog can be huge.
            for scan in scans {
                let rows = match scan.rows {
                    Some(rows) => Some(rows),
                    None => postgres::queries::estimated_rows(p, scan.schema.as_deref(), &scan.table).await,
                };
                if let Some(rows) = rows {
                    out.push((scan.name(), rows));
                }
            }
            return Ok(out);
        }
        let tables = self.tables(false).await.unwrap_or_default();
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
        Ok(out)
    }

    /// Every foreign key of the database (the system catalogs left out), in
    /// one query: what points where, both ways.
    pub async fn links(&self) -> Result<Vec<Link>> {
        let rows: Vec<(String, String, String, String, String, Option<String>)> = match &self.0.pool {
            Pool::Postgres(p) => postgres::queries::link_rows(p).await?,
            Pool::MySql(_) => mysql::queries::link_rows(self).await?,
            Pool::Sqlite(_) => {
                let mut c = self.sqlite_session().await?;
                sqlite::queries::link_rows(&mut c).await?
            }
        };
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
        let rows = match &self.0.pool {
            Pool::Postgres(p) => postgres::queries::comment_rows(p).await?,
            Pool::MySql(_) => mysql::queries::comment_rows(self).await?,
            Pool::Sqlite(_) => Vec::new(),
        };
        Ok(rows
            .into_iter()
            .filter(|(_, _, _, text)| !text.trim().is_empty())
            .map(|(schema, table, column, text)| Comment { schema, table, column, text })
            .collect())
    }

    /// User-defined functions and procedures (none on SQLite). Postgres
    /// leaves out those that belong to extensions.
    pub async fn routines(&self) -> Result<Vec<RoutineInfo>> {
        let rows: Vec<(String, String, String, String)> = match &self.0.pool {
            Pool::Postgres(_) => postgres::queries::routine_rows(self).await?,
            Pool::MySql(_) => mysql::queries::routine_rows(self).await?,
            Pool::Sqlite(_) => Vec::new(),
        };
        Ok(rows.into_iter().map(|(schema, name, kind, id)| RoutineInfo { schema, name, kind, id }).collect())
    }

    /// The CREATE statement of a function or procedure.
    pub async fn routine_definition(&self, routine: &RoutineInfo) -> Result<String> {
        match &self.0.pool {
            Pool::Postgres(_) => postgres::queries::routine_definition(self, routine).await,
            Pool::MySql(_) => mysql::queries::routine_definition(self, routine).await,
            Pool::Sqlite(_) => anyhow::bail!("SQLite has no stored functions"),
        }
    }
}
