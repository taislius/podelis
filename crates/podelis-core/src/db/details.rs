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
    /// Its CHECK constraints (PostgreSQL, MySQL 8.0.16 on, MariaDB).
    #[serde(default)]
    pub checks: Vec<CheckInfo>,
}

/// A CHECK constraint: its name and `CHECK (…)` as the server writes it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckInfo {
    pub name: String,
    pub definition: String,
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
    /// What deleting the row it points at does (`CASCADE`, `SET NULL`,
    /// `RESTRICT`…); `None` for NO ACTION, the default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_delete: Option<String>,
    /// And updating its key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_update: Option<String>,
}

impl ForeignKey {
    /// Its rules as SQL writes them, after the target: ` ON DELETE CASCADE`.
    pub fn actions(&self) -> String {
        self.actions_where(false)
    }

    /// Its rules, RESTRICT left out with `restrict_is_no_action` (where
    /// RESTRICT is how the engine says a rule left unsaid: MySQL).
    pub fn actions_where(&self, restrict_is_no_action: bool) -> String {
        use std::fmt::Write as _;
        let said = |r: &Option<String>| r.clone().filter(|r| !restrict_is_no_action || r != "RESTRICT");
        let mut out = String::new();
        if let Some(rule) = said(&self.on_delete) {
            let _ = write!(out, " ON DELETE {rule}");
        }
        if let Some(rule) = said(&self.on_update) {
            let _ = write!(out, " ON UPDATE {rule}");
        }
        out
    }
}

/// A foreign key's rule as the catalog says it: `None` for NO ACTION
/// (what an unsaid rule is), else upper case.
/// Its words upper case, a column list (`SET NULL (a, b)`) as it is.
pub fn fk_rule(rule: Option<&str>) -> Option<String> {
    let rule = rule?.trim();
    let (words, columns) = rule.split_at(rule.find('(').unwrap_or(rule.len()));
    let rule = format!("{} {columns}", words.trim().to_ascii_uppercase());
    let rule = rule.trim().to_string();
    (!rule.is_empty() && rule != "NO ACTION").then_some(rule)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoutineInfo {
    pub schema: String,
    /// Name, with the argument types on Postgres (overloads differ by them).
    pub name: String,
    /// "function" or "procedure"; or another object kept with them:
    /// "trigger", "sequence", "enum", "domain", "type" (composite), "event".
    pub kind: String,
    /// How to look the definition up again (an oid on Postgres).
    pub(super) id: String,
}

impl RoutineInfo {
    /// A function or a procedure, not another object listed with them.
    pub fn is_function(&self) -> bool {
        matches!(self.kind.as_str(), "function" | "procedure")
    }

    /// Its name as SQL writes it on `engine`: a function's with its
    /// argument types, as listed (Postgres's overloads differ by them); a
    /// trigger's alone, quoted (it's listed with its table: `audit
    /// (orders)`); anything else's quoted, with its schema.
    pub fn sql_name(&self, engine: Engine) -> String {
        match self.kind.as_str() {
            "function" | "procedure" => self.name.clone(),
            "trigger" => {
                let name = self.name.rsplit_once(" (").map_or(self.name.as_str(), |(name, _)| name);
                engine.quote_ident(name)
            }
            _ => format!("{}.{}", engine.quote_ident(&self.schema), engine.quote_ident(&self.name)),
        }
    }
}

/// A comment the database keeps on a table or view (`column` is `None`)
/// or on one of its columns: what the schema's authors said it means.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Comment {
    pub schema: String,
    pub table: String,
    pub column: Option<String>,
    pub text: String,
}

/// A foreign key's column and what it points to: one row of the whole
/// database's links, for showing related tables.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
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
    /// 20 statements. A read whose plan fails is an error: how much it
    /// reads isn't known (a guard asks, rather than let it through).
    ///
    /// Each engine plans them its own way (`full_scans_of`: PostgreSQL on
    /// a connection of its own, so one that fails can't abort the
    /// session's open transaction).
    pub async fn full_scans(&self, sql: &str) -> Result<Vec<(String, f64)>> {
        let engine = self.engine();
        let mut scans = Vec::new();
        for statement in crate::language::split_statements(sql, engine).into_iter().take(20) {
            let infos = crate::language::statements::analyze(&crate::sql::parse::parse(statement, engine));
            let starts = statement.trim_start().get(..7).unwrap_or_default().to_ascii_lowercase();
            if infos.iter().any(|i| i.kind != crate::language::statements::Kind::Read)
                || starts.starts_with("explain")
                || starts.starts_with("show")
            {
                continue;
            }
            // A plan that couldn't be had: how much it reads isn't known.
            let found = on_driver!(engine, engine::queries::full_scans_of(self, statement).await);
            let found = match found {
                Ok(found) => found,
                // The statement itself is wrong (a syntax error, a table not
                // there): its run says so, reading nothing.
                Err(e) if errors::server_error(&e).is_some_and(|s| s.check.is_some()) => continue,
                Err(e) => {
                    let what = infos.first().map_or_else(|| statement.to_string(), |i| i.summary.clone());
                    return Err(e.context(format!("couldn't plan {what}")));
                }
            };
            scans.extend(found);
        }
        if scans.iter().all(|s| s.rows.is_some()) {
            return Ok(scans.into_iter().filter_map(|s| Some((s.name(), s.rows?))).collect());
        }
        Ok(on_driver!(engine, engine::queries::estimated_rows(self, scans).await))
    }

    /// A statement's full scans from its EXPLAIN on the session (for
    /// engines whose plans don't need a connection of their own).
    pub(super) async fn scans_by_explain(&self, statement: &str) -> Result<Vec<Scan>> {
        let Some(dialect) = self.engine().dialect() else { return Ok(Vec::new()) };
        let plan = self.run_own(&dialect.explain(statement, self.version()), 10_000).await?;
        Ok(dialect.full_scans(&plan.columns, &plan.rows))
    }

    /// Runs Podėlis's own `sql` (a plan: it changes nothing), on a new
    /// session again if the old one had been given up on.
    pub(super) async fn run_own(&self, sql: &str, limit: usize) -> Result<QueryOutcome> {
        match self.run(sql, limit).await {
            Err(e) if errors::is_reconnected(&e) => self.run(sql, limit).await,
            result => result,
        }
    }

    /// Scans' rows: the plan's, else the catalog's estimate (all tables
    /// listed once).
    pub(super) async fn estimates_from_catalog(&self, scans: Vec<Scan>) -> Vec<(String, f64)> {
        let tables = self.tables(false).await.unwrap_or_default();
        let mut out = Vec::new();
        for scan in scans {
            #[expect(clippy::cast_precision_loss, reason = "an estimate")]
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

    /// User-defined functions and procedures, and the objects kept beside
    /// them: triggers, sequences and types (PostgreSQL), triggers and
    /// events (MySQL). Postgres leaves out those that belong to extensions.
    pub async fn routines(&self) -> Result<Vec<RoutineInfo>> {
        let rows: Vec<(String, String, String, String)> =
            on_driver!(self.0.engine, engine::queries::routine_rows(self).await?);
        Ok(rows.into_iter().map(|(schema, name, kind, id)| RoutineInfo { schema, name, kind, id }).collect())
    }

    /// The table as the server writes it, every detail (defaults, keys
    /// with their ON DELETE, triggers…): MySQL's `SHOW CREATE TABLE`,
    /// PostgreSQL's from its own functions. `None` for a view, or where the
    /// engine has none (SQLite's CREATE is its details' `definition`).
    /// Asked for by itself (`d`), not with the details: it's several
    /// queries, which schema compare and agents don't need.
    pub async fn table_ddl(&self, schema: &str, table: &str) -> Result<Option<String>> {
        on_driver!(self.0.engine, engine::queries::table_ddl(self, schema, table).await)
    }

    /// The CREATE statement of a function, procedure or other object.
    pub async fn routine_definition(&self, routine: &RoutineInfo) -> Result<String> {
        on_driver!(self.0.engine, engine::queries::routine_definition(self, routine).await)
    }
}

#[cfg(test)]
mod routine_tests {
    use super::*;

    #[test]
    fn an_objects_name_as_sql_writes_it() {
        let routine = |kind: &str, name: &str| RoutineInfo {
            schema: "App".into(),
            name: name.into(),
            kind: kind.into(),
            id: String::new(),
        };
        assert_eq!(routine("function", "f(x integer)").sql_name(Engine::Postgres), "f(x integer)");
        assert_eq!(routine("trigger", "audit (orders)").sql_name(Engine::Postgres), "audit");
        assert_eq!(routine("sequence", "Tickets").sql_name(Engine::Postgres), "\"App\".\"Tickets\"");
    }
}
