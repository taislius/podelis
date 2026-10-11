//! Details loaded on demand for the schema navigator: a table's indexes and
//! foreign keys, view definitions, and stored functions/procedures.

use super::*;

#[derive(Clone, Debug, Default)]
pub struct TableDetails {
    pub indexes: Vec<IndexInfo>,
    pub foreign_keys: Vec<ForeignKey>,
    /// A view's query, or (SQLite) the table's CREATE statement.
    pub definition: Option<String>,
}

#[derive(Clone, Debug)]
pub struct IndexInfo {
    pub name: String,
    /// Comma-separated columns (empty for expression indexes).
    pub columns: String,
    pub unique: bool,
    pub primary: bool,
    /// The CREATE INDEX statement, where the server provides one.
    pub definition: Option<String>,
}

#[derive(Clone, Debug)]
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
