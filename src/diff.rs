//! Comparing two connections: their schemas (tables, columns, indexes and
//! foreign keys, with the SQL that would make one match the other) and the
//! rows the same query returns on each.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt::Write as _;

use crate::db::{ColumnInfo, Engine, ForeignKey, IndexInfo, QueryOutcome, TableDetails, TableInfo};

/// One side's table: its columns and, when loaded, indexes and foreign keys.
pub struct TableShape {
    pub info: TableInfo,
    pub columns: Vec<ColumnInfo>,
    pub details: Option<TableDetails>,
}

/// A connection being compared: its name and engine.
#[derive(Clone, Copy)]
pub struct Side<'a> {
    pub name: &'a str,
    pub engine: Engine,
}

pub struct SchemaDiff {
    /// What differs, for reading.
    pub report: String,
    /// Statements that make the other side match this one (drops left
    /// commented out); empty when nothing differs.
    pub sql: String,
    pub differences: usize,
}

/// How tables are matched up: by name when each side has one schema (a
/// staging and a production database are usually named differently), else
/// by `schema.name`.
fn keys(tables: &[TableShape], by_name: bool) -> BTreeMap<String, &TableShape> {
    tables
        .iter()
        .map(|t| (if by_name { t.info.name.clone() } else { format!("{}.{}", t.info.schema, t.info.name) }, t))
        .collect()
}

fn one_schema(tables: &[TableShape]) -> bool {
    tables.iter().map(|t| &t.info.schema).collect::<HashSet<_>>().len() <= 1
}

fn qualified(engine: Engine, schema: &str, name: &str) -> String {
    if schema.is_empty() {
        engine.quote_ident(name)
    } else {
        format!("{}.{}", engine.quote_ident(schema), engine.quote_ident(name))
    }
}

/// What makes an index the same on both sides: its columns and uniqueness
/// (names often differ between environments).
fn index_key(index: &IndexInfo) -> String {
    let what =
        if index.columns.is_empty() { index.definition.as_deref().unwrap_or(&index.name) } else { &index.columns };
    format!("{}{}", if index.unique { "unique " } else { "" }, what.to_lowercase().replace(' ', ""))
}

fn index_label(index: &IndexInfo) -> String {
    let unique = if index.unique { "unique " } else { "" };
    format!("{unique}index {} ({})", index.name, index.columns)
}

fn fk_key(fk: &ForeignKey) -> String {
    format!("{}→{}", fk.column.to_lowercase(), fk.target.to_lowercase())
}

/// The differences inside one table that exists on both sides.
struct TableChanges {
    lines: Vec<String>,
    sql: Vec<String>,
}

// Each kind of difference (columns, indexes, keys) is a few lines; split up
// they'd need the same half-dozen arguments each.
#[allow(clippy::too_many_lines)]
fn table_changes(here: &TableShape, there: &TableShape, other: Side, same_engine: bool) -> TableChanges {
    let engine = other.engine;
    let dialect = engine.dialect();
    let table = qualified(engine, &there.info.schema, &there.info.name);
    let mut lines = Vec::new();
    let mut sql = Vec::new();
    let theirs: HashMap<String, &ColumnInfo> = there.columns.iter().map(|c| (c.name.to_lowercase(), c)).collect();
    let ours: HashSet<String> = here.columns.iter().map(|c| c.name.to_lowercase()).collect();
    for column in &here.columns {
        let name = engine.quote_ident(&column.name);
        match theirs.get(&column.name.to_lowercase()) {
            None => {
                lines.push(format!("column {} {}: only here", column.name, column.data_type));
                let null = if column.nullable { "" } else { " NOT NULL" };
                let note = if column.nullable { "" } else { "  -- needs a DEFAULT if the table has rows" };
                sql.push(format!("ALTER TABLE {table} ADD COLUMN {name} {}{null};{note}", column.data_type));
            }
            Some(theirs) => {
                let type_differs = !column.data_type.eq_ignore_ascii_case(&theirs.data_type);
                if type_differs {
                    lines
                        .push(format!("column {}: {} here, {} there", column.name, column.data_type, theirs.data_type));
                }
                if column.nullable != theirs.nullable {
                    let null = |n: bool| if n { "nullable" } else { "NOT NULL" };
                    lines.push(format!(
                        "column {}: {} here, {} there",
                        column.name,
                        null(column.nullable),
                        null(theirs.nullable)
                    ));
                }
                if column.primary_key != theirs.primary_key {
                    let pk = |p: bool| if p { "in the primary key" } else { "not in the primary key" };
                    lines.push(format!(
                        "column {}: {} here, {} there",
                        column.name,
                        pk(column.primary_key),
                        pk(theirs.primary_key)
                    ));
                    sql.push(format!("-- the primary key of {table} differs: change it by hand"));
                }
                if (type_differs || column.nullable != theirs.nullable) && same_engine {
                    match dialect.modify_column(&table, &name, &column.data_type, column.nullable) {
                        Some(stmt) => sql.push(format!("{stmt};")),
                        None => sql.push(format!(
                            "-- {} can't change column {name} in place: rebuild {table}",
                            dialect.label()
                        )),
                    }
                }
            }
        }
    }
    for column in there.columns.iter().filter(|c| !ours.contains(&c.name.to_lowercase())) {
        lines.push(format!("column {} {}: only there", column.name, column.data_type));
        sql.push(format!("-- ALTER TABLE {table} DROP COLUMN {};", engine.quote_ident(&column.name)));
    }

    if let (Some(a), Some(b)) = (&here.details, &there.details) {
        let secondary = |d: &TableDetails| d.indexes.iter().filter(|i| !i.primary).cloned().collect::<Vec<_>>();
        let (a_idx, b_idx) = (secondary(a), secondary(b));
        let b_keys: HashSet<String> = b_idx.iter().map(index_key).collect();
        let a_keys: HashSet<String> = a_idx.iter().map(index_key).collect();
        for index in a_idx.iter().filter(|i| !b_keys.contains(&index_key(i))) {
            lines.push(format!("{}: only here", index_label(index)));
            let stmt = match &index.definition {
                Some(def) if same_engine => def.trim_end_matches(';').to_string(),
                _ if index.columns.is_empty() => format!("-- {}: an expression index, copy it by hand", index.name),
                _ => {
                    let cols: Vec<String> = index.columns.split(',').map(|c| engine.quote_ident(c.trim())).collect();
                    let unique = if index.unique { "UNIQUE " } else { "" };
                    format!("CREATE {unique}INDEX {} ON {table} ({})", engine.quote_ident(&index.name), cols.join(", "))
                }
            };
            sql.push(if stmt.starts_with("--") { stmt } else { format!("{stmt};") });
        }
        for index in b_idx.iter().filter(|i| !a_keys.contains(&index_key(i))) {
            lines.push(format!("{}: only there", index_label(index)));
            sql.push(format!("-- DROP INDEX {};", engine.quote_ident(&index.name)));
        }
        let b_fks: HashSet<String> = b.foreign_keys.iter().map(fk_key).collect();
        let a_fks: HashSet<String> = a.foreign_keys.iter().map(fk_key).collect();
        for fk in a.foreign_keys.iter().filter(|f| !b_fks.contains(&fk_key(f))) {
            lines.push(format!("foreign key {} → {}: only here", fk.column, fk.target));
            let target = qualified(engine, &there.info.schema, &fk.target_table);
            let added = fk.target_column.as_ref().and_then(|tc| {
                dialect.add_foreign_key(&table, &engine.quote_ident(&fk.column), &target, &engine.quote_ident(tc))
            });
            sql.push(match added {
                Some(stmt) => format!("{stmt};"),
                None => format!("-- foreign key {} → {}: add it by hand", fk.column, fk.target),
            });
        }
        for fk in b.foreign_keys.iter().filter(|f| !a_fks.contains(&fk_key(f))) {
            lines.push(format!("foreign key {} → {}: only there", fk.column, fk.target));
            sql.push(format!("-- drop the foreign key on {table} ({}) by hand", fk.column));
        }
    }
    TableChanges { lines, sql }
}

/// `CREATE TABLE` (or `CREATE VIEW`) for a table only on this side, written
/// for the other side's schema.
fn create(table: &TableShape, schema: &str, engine: Engine) -> Vec<String> {
    let name = qualified(engine, schema, &table.info.name);
    if table.info.is_view {
        return match table.details.as_ref().and_then(|d| d.definition.as_deref()) {
            Some(def) => vec![format!("CREATE VIEW {name} AS {};", def.trim().trim_end_matches(';'))],
            None => vec![format!("-- view {name}: copy its definition by hand")],
        };
    }
    let mut cols: Vec<String> = table
        .columns
        .iter()
        .map(|c| {
            let null = if c.nullable { "" } else { " NOT NULL" };
            format!("  {} {}{null}", engine.quote_ident(&c.name), c.data_type)
        })
        .collect();
    let pk: Vec<String> = table.columns.iter().filter(|c| c.primary_key).map(|c| engine.quote_ident(&c.name)).collect();
    if !pk.is_empty() {
        cols.push(format!("  PRIMARY KEY ({})", pk.join(", ")));
    }
    let mut out = vec![format!("CREATE TABLE {name} (\n{}\n);", cols.join(",\n"))];
    for index in table.details.iter().flat_map(|d| &d.indexes).filter(|i| !i.primary) {
        out.push(match &index.definition {
            Some(def) => format!("{};", def.trim_end_matches(';')),
            None => format!("-- {}: create it by hand", index_label(index)),
        });
    }
    out
}

/// What differs between `here` (this tab's connection) and `there`, and the
/// SQL that makes `there` match `here`.
pub fn schema_diff(here: &[TableShape], there: &[TableShape], this: Side, other: Side) -> SchemaDiff {
    let by_name = one_schema(here) && one_schema(there);
    let (ours, theirs) = (keys(here, by_name), keys(there, by_name));
    let same_engine = this.engine == other.engine;
    // Where tables only here would be created on the other side.
    let their_schema = there.first().map_or_else(String::new, |t| t.info.schema.clone());
    let mut missing = Vec::new();
    let mut extra = Vec::new();
    let mut altered: Vec<(String, TableChanges)> = Vec::new();
    let mut same = 0;
    for (key, table) in &ours {
        match theirs.get(key) {
            None => missing.push((key.clone(), *table)),
            Some(other_table) => {
                let changes = table_changes(table, other_table, other, same_engine);
                if changes.lines.is_empty() {
                    same += 1;
                } else {
                    altered.push((key.clone(), changes));
                }
            }
        }
    }
    for (key, table) in &theirs {
        if !ours.contains_key(key) {
            extra.push((key.clone(), *table));
        }
    }

    let kind = |t: &TableShape| if t.info.is_view { "view" } else { "table" };
    let mut report = format!("{} (this tab) compared with {}\n", this.name, other.name);
    if missing.is_empty() && extra.is_empty() && altered.is_empty() {
        let _ = write!(report, "\nThe schemas match: {same} tables and views.");
        return SchemaDiff { report, sql: String::new(), differences: 0 };
    }
    if !missing.is_empty() {
        let _ = writeln!(report, "\nONLY HERE");
        for (key, table) in &missing {
            let _ = writeln!(report, "  {} {key}", kind(table));
        }
    }
    if !extra.is_empty() {
        let _ = writeln!(report, "\nONLY THERE");
        for (key, table) in &extra {
            let _ = writeln!(report, "  {} {key}", kind(table));
        }
    }
    if !altered.is_empty() {
        let _ = writeln!(report, "\nDIFFERENT");
        for (key, changes) in &altered {
            let _ = writeln!(report, "  {key}");
            for line in &changes.lines {
                let _ = writeln!(report, "    {line}");
            }
        }
    }
    let _ = write!(report, "\nThe same: {same} tables and views.");

    let mut sql =
        format!("-- Makes {} match {}. Review it first: drops are left commented out.\n", other.name, this.name);
    if same_engine {
        for (_, table) in &missing {
            let schema = if by_name { their_schema.as_str() } else { table.info.schema.as_str() };
            for stmt in create(table, schema, other.engine) {
                let _ = writeln!(sql, "\n{stmt}");
            }
        }
        for (_, changes) in &altered {
            let _ = writeln!(sql);
            for stmt in &changes.sql {
                let _ = writeln!(sql, "{stmt}");
            }
        }
        for (_, table) in &extra {
            let what = if table.info.is_view { "VIEW" } else { "TABLE" };
            let _ =
                write!(sql, "\n-- DROP {what} {};\n", qualified(other.engine, &table.info.schema, &table.info.name));
        }
    } else {
        let _ = writeln!(
            sql,
            "-- The connections use different engines ({} and {}): align them by hand.",
            this.engine.label(),
            other.engine.label()
        );
    }
    let differences = missing.len() + extra.len() + altered.iter().map(|(_, c)| c.lines.len()).sum::<usize>();
    SchemaDiff { report, sql, differences }
}

/// The rows of two results side by side.
pub struct RowDiff {
    /// The differing rows, with a first `diff` column saying where each is
    /// from: `− here`, `+ there`, or `≠ here` / `≠ there` for a changed row.
    pub outcome: QueryOutcome,
    pub only_here: usize,
    pub only_there: usize,
    pub changed: usize,
    pub same: usize,
    /// The column rows were matched by, if one identifies them.
    pub key: Option<String>,
}

type Row = Vec<Option<String>>;

/// Compares two results of the same query. Rows are matched by the first
/// column when it identifies them (no NULLs or repeats on either side), so
/// a changed row shows as a pair; otherwise as whole rows.
pub fn row_diff(here: &QueryOutcome, there: &QueryOutcome) -> Result<RowDiff, String> {
    let lower = |cols: &[String]| cols.iter().map(|c| c.to_lowercase()).collect::<Vec<_>>();
    if lower(&here.columns) != lower(&there.columns) {
        return Err(format!(
            "The results have different columns: ({}) here, ({}) there",
            here.columns.join(", "),
            there.columns.join(", ")
        ));
    }
    let identifies = |rows: &[Row]| {
        let mut seen = HashSet::new();
        rows.iter().all(|r| r.first().cloned().flatten().is_some_and(|k| seen.insert(k)))
    };
    let keyed = !here.columns.is_empty() && identifies(&here.rows) && identifies(&there.rows);
    let mut out: Vec<Row> = Vec::new();
    let tagged = |tag: &str, row: &Row| std::iter::once(Some(tag.to_string())).chain(row.iter().cloned()).collect();
    let (mut missing, mut extra, mut changed, mut same) = (0, 0, 0, 0);
    if keyed {
        let theirs: HashMap<&str, &Row> = there.rows.iter().filter_map(|r| Some((r.first()?.as_deref()?, r))).collect();
        let mut matched = HashSet::new();
        for row in &here.rows {
            let key = row[0].as_deref().unwrap_or_default();
            match theirs.get(key) {
                None => {
                    missing += 1;
                    out.push(tagged("− here", row));
                }
                Some(other) => {
                    matched.insert(key);
                    if row == *other {
                        same += 1;
                    } else {
                        changed += 1;
                        out.push(tagged("≠ here", row));
                        out.push(tagged("≠ there", other));
                    }
                }
            }
        }
        for row in there.rows.iter().filter(|r| !matched.contains(r[0].as_deref().unwrap_or_default())) {
            extra += 1;
            out.push(tagged("+ there", row));
        }
    } else {
        // Whole rows, counting repeats.
        let mut counts: HashMap<&Row, i64> = HashMap::new();
        for row in &there.rows {
            *counts.entry(row).or_default() += 1;
        }
        for row in &here.rows {
            match counts.get_mut(row) {
                Some(n) if *n > 0 => {
                    *n -= 1;
                    same += 1;
                }
                _ => {
                    missing += 1;
                    out.push(tagged("− here", row));
                }
            }
        }
        for row in &there.rows {
            if let Some(n) = counts.get_mut(row)
                && *n > 0
            {
                *n -= 1;
                extra += 1;
                out.push(tagged("+ there", row));
            }
        }
    }
    let columns = std::iter::once("diff".to_string()).chain(here.columns.iter().cloned()).collect();
    let column_types = std::iter::once(String::new()).chain(here.column_types.iter().cloned()).collect();
    let outcome = QueryOutcome {
        columns,
        column_types,
        total_rows: out.len() as u64,
        rows: out,
        statements: 1,
        elapsed: here.elapsed.max(there.elapsed),
        ..QueryOutcome::default()
    };
    let key = keyed.then(|| here.columns[0].clone());
    Ok(RowDiff { outcome, only_here: missing, only_there: extra, changed, same, key })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn col(name: &str, ty: &str, nullable: bool, pk: bool) -> ColumnInfo {
        ColumnInfo { name: name.into(), data_type: ty.into(), nullable, primary_key: pk }
    }

    fn table(schema: &str, name: &str, columns: Vec<ColumnInfo>, indexes: Vec<IndexInfo>) -> TableShape {
        TableShape {
            info: TableInfo {
                schema: schema.into(),
                name: name.into(),
                is_view: false,
                rows: None,
                bytes: None,
                system: false,
            },
            columns,
            details: Some(TableDetails { indexes, ..TableDetails::default() }),
        }
    }

    fn index(name: &str, columns: &str) -> IndexInfo {
        IndexInfo { name: name.into(), columns: columns.into(), unique: false, primary: false, definition: None }
    }

    const PG: Side = Side { name: "staging", engine: Engine::Postgres };
    const PG_PROD: Side = Side { name: "prod", engine: Engine::Postgres };

    #[test]
    fn schemas_differ_in_tables_columns_and_indexes() {
        let users = |extra: bool, email: &str| {
            let mut cols = vec![col("id", "integer", false, true), col("email", email, true, false)];
            if extra {
                cols.push(col("plan", "text", false, false));
            }
            cols
        };
        let here = [
            table("app", "users", users(true, "varchar(255)"), vec![index("users_email", "email")]),
            table("app", "audit", vec![col("id", "integer", false, true)], vec![]),
            table("app", "same", vec![col("id", "integer", false, true)], vec![]),
        ];
        // Another schema name, as a separate database would have.
        let there = [
            table("prod", "users", users(false, "text"), vec![index("users_old", "created_at")]),
            table("prod", "legacy", vec![col("id", "integer", false, true)], vec![]),
            table("prod", "same", vec![col("id", "integer", false, true)], vec![]),
        ];
        let diff = schema_diff(&here, &there, PG, PG_PROD);
        assert_eq!(
            diff.report,
            "staging (this tab) compared with prod\n\n\
             ONLY HERE\n  table audit\n\n\
             ONLY THERE\n  table legacy\n\n\
             DIFFERENT\n  users\n    column email: varchar(255) here, text there\n    \
             column plan text: only here\n    index users_email (email): only here\n    \
             index users_old (created_at): only there\n\n\
             The same: 1 tables and views."
        );
        assert_eq!(diff.differences, 6);
        for expected in [
            "CREATE TABLE prod.audit (\n  id integer NOT NULL,\n  PRIMARY KEY (id)\n);",
            "ALTER TABLE prod.users ALTER COLUMN email TYPE varchar(255), ALTER COLUMN email DROP NOT NULL;",
            "ALTER TABLE prod.users ADD COLUMN plan text NOT NULL;  -- needs a DEFAULT",
            "CREATE INDEX users_email ON prod.users (email);",
            "-- DROP INDEX users_old;",
            "-- DROP TABLE prod.legacy;",
        ] {
            assert!(diff.sql.contains(expected), "missing {expected:?} in\n{}", diff.sql);
        }
    }

    #[test]
    fn matching_schemas_and_different_engines() {
        let here = [table("main", "t", vec![col("id", "INTEGER", false, true)], vec![])];
        let there = [table("main", "t", vec![col("id", "integer", false, true)], vec![])];
        let diff = schema_diff(&here, &there, PG, PG_PROD);
        assert_eq!(diff.differences, 0);
        assert!(diff.report.ends_with("The schemas match: 1 tables and views."), "{}", diff.report);

        let my = Side { name: "mysql", engine: Engine::MySql };
        let there = [table("main", "u", vec![], vec![])];
        let diff = schema_diff(&here, &there, PG, my);
        assert!(diff.sql.contains("different engines"), "{}", diff.sql);
        assert!(!diff.sql.contains("CREATE TABLE"), "{}", diff.sql);
    }

    fn result(columns: &[&str], rows: &[&[&str]]) -> QueryOutcome {
        QueryOutcome {
            columns: columns.iter().map(ToString::to_string).collect(),
            column_types: columns.iter().map(|_| String::new()).collect(),
            rows: rows.iter().map(|r| r.iter().map(|c| Some(c.to_string())).collect()).collect(),
            ..QueryOutcome::default()
        }
    }

    fn tags(diff: &RowDiff) -> Vec<String> {
        diff.outcome
            .rows
            .iter()
            .map(|r| r.iter().map(|c| c.clone().unwrap_or_default()).collect::<Vec<_>>().join(" "))
            .collect()
    }

    #[test]
    fn rows_matched_by_their_first_column() {
        let here = result(&["id", "name"], &[&["1", "ana"], &["2", "ben"], &["3", "cleo"]]);
        let there = result(&["ID", "name"], &[&["1", "ana"], &["2", "benas"], &["4", "dan"]]);
        let diff = row_diff(&here, &there).unwrap();
        assert_eq!(diff.key.as_deref(), Some("id"));
        assert_eq!((diff.only_here, diff.only_there, diff.changed, diff.same), (1, 1, 1, 1));
        assert_eq!(tags(&diff), ["≠ here 2 ben", "≠ there 2 benas", "− here 3 cleo", "+ there 4 dan"]);
        assert_eq!(diff.outcome.columns, ["diff", "id", "name"]);
    }

    #[test]
    fn rows_without_a_key_compare_whole() {
        let here = result(&["n"], &[&["a"], &["a"], &["b"]]);
        let there = result(&["n"], &[&["a"], &["c"]]);
        let diff = row_diff(&here, &there).unwrap();
        assert_eq!(diff.key, None);
        assert_eq!(tags(&diff), ["− here a", "− here b", "+ there c"]);
        assert_eq!(diff.same, 1);
        assert!(row_diff(&here, &result(&["m"], &[])).is_err());
    }
}
