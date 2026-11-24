//! Comparing two connections: their schemas (tables, columns, indexes and
//! foreign keys, with the SQL that would make one match the other) and the
//! rows the same query returns on each.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt::Write as _;

use crate::db::{
    ColumnInfo, Engine, ForeignKey, IndexInfo, QueryOutcome, TableDetails, TableInfo, same_type, translate_type,
};

/// One side's table: its columns and, when loaded, indexes and foreign keys.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
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

/// `CREATE INDEX` for `index` on `table`: its own definition on the same
/// engine, else one built from its columns.
fn index_sql(index: &IndexInfo, table: &str, engine: Engine, same_engine: bool) -> String {
    let stmt = match &index.definition {
        Some(def) if same_engine => def.trim_end_matches(';').to_string(),
        _ if index.columns.is_empty() => format!("-- {}: an expression index, copy it by hand", index.name),
        _ => {
            let cols: Vec<String> = index.columns.split(',').map(|c| engine.quote_ident(c.trim())).collect();
            let unique = if index.unique { "UNIQUE " } else { "" };
            format!("CREATE {unique}INDEX {} ON {table} ({})", engine.quote_ident(&index.name), cols.join(", "))
        }
    };
    if stmt.starts_with("--") { stmt } else { format!("{stmt};") }
}

/// A note for a column whose type the other engine has nothing like.
fn untranslatable(column: &ColumnInfo, engine: Engine) -> String {
    format!("-- column {} {}: {} has no such type; add it by hand", column.name, column.data_type, engine.label())
}

/// A foreign key as keys are matched: its column and target.
fn fk_key(fk: &ForeignKey) -> String {
    format!("{}→{}", fk.column.to_lowercase(), fk.target.to_lowercase())
}

/// The differences inside one table that exists on both sides.
struct TableChanges {
    lines: Vec<String>,
    sql: Vec<String>,
}

#[expect(
    clippy::too_many_lines,
    reason = "each kind of difference (columns, indexes, keys) is a few lines; split up they'd need the same half-dozen arguments each"
)]
fn table_changes(here: &TableShape, there: &TableShape, this: Side, other: Side) -> TableChanges {
    let engine = other.engine;
    let same_engine = this.engine == engine;
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
                sql.push(match translate_type(this.engine, &column.data_type, engine) {
                    Some(ty) => format!("ALTER TABLE {table} ADD COLUMN {name} {ty}{null};{note}"),
                    None => untranslatable(column, engine),
                });
            }
            Some(theirs) => {
                // Across engines, the same type under another name isn't a difference.
                let type_differs = if same_engine {
                    !column.data_type.eq_ignore_ascii_case(&theirs.data_type)
                } else {
                    !same_type(this.engine, &column.data_type, engine, &theirs.data_type)
                };
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
                if type_differs || column.nullable != theirs.nullable {
                    let Some(ty) = translate_type(this.engine, &column.data_type, engine) else {
                        sql.push(untranslatable(column, engine));
                        continue;
                    };
                    match dialect.and_then(|d| d.modify_column(&table, &name, &ty, column.nullable)) {
                        Some(stmt) => sql.push(format!("{stmt};")),
                        None => sql.push(format!(
                            "-- {} can't change column {name} in place: rebuild {table}",
                            engine.label()
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
            sql.push(index_sql(index, &table, engine, same_engine));
        }
        for index in b_idx.iter().filter(|i| !a_keys.contains(&index_key(i))) {
            lines.push(format!("{}: only there", index_label(index)));
            sql.push(format!("-- DROP INDEX {};", engine.quote_ident(&index.name)));
        }
        // Keys by column and target; one on both sides whose rules differ is
        // one change, not one only here and one only there.
        let same_key = |x: &ForeignKey, y: &ForeignKey| fk_key(x) == fk_key(y);
        // RESTRICT is NO ACTION where either side says an unsaid rule so;
        // a rule's difference counts only where the other side could keep
        // the rule (DuckDB keeps none, MySQL refuses SET DEFAULT).
        let loose = [this.engine, engine]
            .iter()
            .any(|e| e.dialect().is_some_and(podelis_core::db::SqlDialect::restrict_is_no_action));
        let keeps = |r: &Option<String>| {
            dialect.is_some_and(podelis_core::db::SqlDialect::keeps_fk_rules)
                && r.as_deref().is_none_or(|r| dialect.is_some_and(|d| d.takes_fk_rule(r)))
        };
        for fk in &a.foreign_keys {
            // Its rules as they're compared: with `loose` (a side whose RESTRICT is
            // how it says a rule left unsaid: MySQL), RESTRICT as NO ACTION;
            // elsewhere they differ (PostgreSQL checks RESTRICT at once).
            let rules = fk.actions_where(loose);
            match b.foreign_keys.iter().find(|other| same_key(fk, other)) {
                Some(other) if other.actions_where(loose) == rules => {}
                Some(_) if !(keeps(&fk.on_delete) && keeps(&fk.on_update)) => {}
                Some(other) => {
                    let said = |r: &str| if r.is_empty() { "none".to_string() } else { r.trim().to_string() };
                    lines.push(format!(
                        "foreign key {} → {}: {} here, {} there",
                        fk.column,
                        fk.target,
                        said(&rules),
                        said(&other.actions_where(loose))
                    ));
                    // Its name isn't known here: dropped by hand, added again.
                    sql.push(format!(
                        "-- foreign key {} → {}: drop it on {table} and add it again{}",
                        fk.column,
                        fk.target,
                        fk.actions()
                    ));
                }
                None => {
                    lines.push(format!("foreign key {} → {}{}: only here", fk.column, fk.target, fk.actions()));
                    let target = qualified(engine, &there.info.schema, &fk.target_table);
                    // With its rules (without, a delete would stop cascading),
                    // where the other engine takes them.
                    let takes = |rule: &Option<String>| {
                        rule.as_deref().is_none_or(|r| dialect.is_some_and(|d| d.takes_fk_rule(r)))
                    };
                    let added =
                        fk.target_column.as_ref().filter(|_| takes(&fk.on_delete) && takes(&fk.on_update)).and_then(
                            |tc| {
                                dialect?.add_foreign_key(
                                    &table,
                                    &engine.quote_ident(&fk.column),
                                    &target,
                                    &engine.quote_ident(tc),
                                )
                            },
                        );
                    sql.push(match added {
                        Some(stmt) => format!("{stmt}{};", fk.actions()),
                        None => format!("-- foreign key {} → {}{}: add it by hand", fk.column, fk.target, fk.actions()),
                    });
                }
            }
        }
        for fk in b.foreign_keys.iter().filter(|f| !a.foreign_keys.iter().any(|other| same_key(f, other))) {
            lines.push(format!("foreign key {} → {}{}: only there", fk.column, fk.target, fk.actions()));
            sql.push(format!("-- drop the foreign key on {table} ({}) by hand", fk.column));
        }
    }
    TableChanges { lines, sql }
}

/// `CREATE TABLE` (or `CREATE VIEW`) for a table only on this side, written
/// for the other side's schema and engine.
fn create(table: &TableShape, schema: &str, from: Engine, engine: Engine) -> Vec<String> {
    let name = qualified(engine, schema, &table.info.name);
    let same_engine = from == engine;
    if table.info.is_view {
        return match table.details.as_ref().and_then(|d| d.definition.as_deref()) {
            Some(def) if same_engine => {
                let what = if table.info.materialized { "MATERIALIZED VIEW" } else { "VIEW" };
                vec![format!("CREATE {what} {name} AS {};", def.trim().trim_end_matches(';'))]
            }
            _ => vec![format!("-- view {name}: copy its definition by hand")],
        };
    }
    let mut cols = Vec::new();
    for c in &table.columns {
        let Some(ty) = translate_type(from, &c.data_type, engine) else {
            return vec![format!(
                "-- table {name}: column {} {} has no such type on {}; create it by hand",
                c.name,
                c.data_type,
                engine.label()
            )];
        };
        let null = if c.nullable { "" } else { " NOT NULL" };
        cols.push(format!("  {} {ty}{null}", engine.quote_ident(&c.name)));
    }
    let pk: Vec<String> = table.columns.iter().filter(|c| c.primary_key).map(|c| engine.quote_ident(&c.name)).collect();
    if !pk.is_empty() {
        cols.push(format!("  PRIMARY KEY ({})", pk.join(", ")));
    }
    let mut out = vec![format!("CREATE TABLE {name} (\n{}\n);", cols.join(",\n"))];
    for index in table.details.iter().flat_map(|d| &d.indexes).filter(|i| !i.primary) {
        out.push(index_sql(index, &name, engine, same_engine));
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
                let changes = table_changes(table, other_table, this, other);
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

    let kind = |t: &TableShape| if t.info.is_view { t!("kind.view") } else { t!("kind.table") };
    let mut report = format!("{}\n", t!("compare.report_title", here = this.name, there = other.name));
    if missing.is_empty() && extra.is_empty() && altered.is_empty() {
        let _ = write!(report, "\n{}", t!("compare.report_match", same = same));
        return SchemaDiff { report, sql: String::new(), differences: 0 };
    }
    if !missing.is_empty() {
        let _ = writeln!(report, "\n{}", t!("compare.report_only_here"));
        for (key, table) in &missing {
            let _ = writeln!(report, "  {} {key}", kind(table));
        }
    }
    if !extra.is_empty() {
        let _ = writeln!(report, "\n{}", t!("compare.report_only_there"));
        for (key, table) in &extra {
            let _ = writeln!(report, "  {} {key}", kind(table));
        }
    }
    if !altered.is_empty() {
        let _ = writeln!(report, "\n{}", t!("compare.report_different"));
        for (key, changes) in &altered {
            let _ = writeln!(report, "  {key}");
            for line in &changes.lines {
                let _ = writeln!(report, "    {line}");
            }
        }
    }
    let _ = write!(report, "\n{}", t!("compare.report_same", same = same));

    let mut sql =
        format!("-- Makes {} match {}. Review it first: drops are left commented out.\n", other.name, this.name);
    if !same_engine {
        let _ = writeln!(
            sql,
            "-- From {} to {}: types are written as {} names them; views and some types need you.",
            this.engine.label(),
            other.engine.label(),
            other.engine.label()
        );
    }
    for (_, table) in &missing {
        let schema = if by_name { their_schema.as_str() } else { table.info.schema.as_str() };
        for stmt in create(table, schema, this.engine, other.engine) {
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
        let what = match (table.info.is_view, table.info.materialized) {
            (true, true) => "MATERIALIZED VIEW",
            (true, false) => "VIEW",
            _ => "TABLE",
        };
        let _ = write!(sql, "\n-- DROP {what} {};\n", qualified(other.engine, &table.info.schema, &table.info.name));
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
        return Err(t!("compare.different_columns", here = here.columns.join(", "), there = there.columns.join(", ")));
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
                partition_of: None,
                materialized: false,
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
    fn a_foreign_keys_rules_are_compared_and_written() {
        let fk = |on_delete: Option<&str>| ForeignKey {
            column: "user_id".into(),
            target: "users.id".into(),
            target_schema: "public".into(),
            target_table: "users".into(),
            target_column: Some("id".into()),
            on_delete: on_delete.map(str::to_string),
            on_update: None,
        };
        let orders = |rule| {
            let mut t = table("public", "orders", vec![col("user_id", "integer", true, false)], vec![]);
            t.details.as_mut().unwrap().foreign_keys = vec![fk(rule)];
            t
        };
        // The same key, its rule changed: one change, not a key added beside it.
        let diff = schema_diff(&[orders(Some("CASCADE"))], &[orders(None)], PG, PG_PROD);
        assert!(
            diff.report.contains("foreign key user_id → users.id: ON DELETE CASCADE here, none there"),
            "{}",
            diff.report
        );
        assert!(!diff.sql.contains("ADD FOREIGN KEY"), "{}", diff.sql);
        assert!(diff.sql.contains("drop it on public.orders and add it again ON DELETE CASCADE"), "{}", diff.sql);
        assert_eq!(diff.differences, 1);
        assert_eq!(schema_diff(&[orders(Some("CASCADE"))], &[orders(Some("CASCADE"))], PG, PG_PROD).differences, 0);
        // RESTRICT isn't NO ACTION on PostgreSQL (checked at once)…
        assert_eq!(schema_diff(&[orders(Some("RESTRICT"))], &[orders(None)], PG, PG_PROD).differences, 1);
        // …but is what MySQL says for a rule left unsaid.
        let my_side = Side { name: "prod", engine: Engine::MySql };
        assert_eq!(schema_diff(&[orders(Some("RESTRICT"))], &[orders(None)], PG, my_side).differences, 0);
        // DuckDB keeps no rules: not a difference there, every time.
        let duck = Side { name: "copy", engine: Engine::DuckDb };
        assert_eq!(schema_diff(&[orders(Some("CASCADE"))], &[orders(None)], PG, duck).differences, 0);
        // Only here: added with its rules.
        let mut bare = orders(None);
        bare.details.as_mut().unwrap().foreign_keys.clear();
        let added = schema_diff(&[orders(Some("CASCADE"))], &[bare.clone()], PG, PG_PROD).sql;
        assert!(added.contains("REFERENCES public.users (id) ON DELETE CASCADE;"), "{added}");
        // A rule the other engine doesn't take: by hand.
        let my = Side { name: "prod", engine: Engine::MySql };
        let by_hand = schema_diff(&[orders(Some("SET DEFAULT"))], &[bare], PG, my).sql;
        assert!(by_hand.contains("ON DELETE SET DEFAULT: add it by hand"), "{by_hand}");
    }

    #[test]
    fn a_materialized_view_is_made_and_dropped_as_one() {
        let mv = |schema: &str| {
            let mut t = table(schema, "totals", vec![col("n", "bigint", true, false)], vec![]);
            t.info.is_view = true;
            t.info.materialized = true;
            t.details = Some(TableDetails {
                definition: Some("SELECT count(*) AS n FROM orders".into()),
                ..TableDetails::default()
            });
            t
        };
        let made = schema_diff(&[mv("public")], &[], PG, PG_PROD).sql;
        assert!(made.contains("CREATE MATERIALIZED VIEW totals AS SELECT count(*) AS n FROM orders;"), "{made}");
        let dropped = schema_diff(&[], &[mv("prod")], PG, PG_PROD).sql;
        assert!(dropped.contains("-- DROP MATERIALIZED VIEW ") && dropped.contains("totals;"), "{dropped}");
    }

    #[test]
    fn matching_schemas_and_different_engines() {
        let here = [table("main", "t", vec![col("id", "INTEGER", false, true)], vec![])];
        let there = [table("main", "t", vec![col("id", "integer", false, true)], vec![])];
        let diff = schema_diff(&here, &there, PG, PG_PROD);
        assert_eq!(diff.differences, 0);
        assert!(diff.report.ends_with("The schemas match: 1 tables and views."), "{}", diff.report);
    }

    /// PostgreSQL against MySQL: the same types under other names match, and
    /// the SQL uses MySQL's names (or says when there's none).
    #[test]
    fn schemas_across_engines() {
        let my = Side { name: "mysql", engine: Engine::MySql };
        let mut indexed = index("people_name", "name");
        indexed.definition = Some("CREATE INDEX people_name ON public.people USING btree (name)".into());
        let here = [
            table(
                "public",
                "people",
                vec![
                    col("id", "int4", false, true),
                    col("name", "varchar(120)", true, false),
                    col("born", "timestamp", true, false),
                    col("note", "varchar(40)", true, false),
                    col("tags", "text[]", true, false),
                ],
                vec![indexed],
            ),
            table(
                "public",
                "cities",
                vec![col("id", "int8", false, true), col("seen", "timestamptz", true, false)],
                vec![],
            ),
        ];
        let there = [table(
            "shop",
            "people",
            vec![
                col("id", "int", false, true),
                col("name", "varchar(120)", true, false),
                col("born", "datetime(6)", true, false),
                col("note", "varchar(20)", true, false),
            ],
            vec![],
        )];
        let diff = schema_diff(&here, &there, PG, my);
        assert!(
            diff.report.contains(
                "DIFFERENT\n  people\n    column note: varchar(40) here, varchar(20) there\n    \
                                  column tags text[]: only here\n    index people_name (name): only here\n"
            ),
            "{}",
            diff.report
        );
        for expected in [
            "From PostgreSQL to MySQL",
            "CREATE TABLE shop.cities (\n  id bigint NOT NULL,\n  seen timestamp(6),\n  PRIMARY KEY (id)\n);",
            "ALTER TABLE shop.people MODIFY COLUMN note varchar(40)",
            "-- column tags text[]: MySQL has no such type; add it by hand",
            "CREATE INDEX people_name ON shop.people (name);",
        ] {
            assert!(diff.sql.contains(expected), "missing {expected:?} in\n{}", diff.sql);
        }
        assert!(!diff.sql.contains("USING btree"), "PostgreSQL's own index SQL: {}", diff.sql);
    }

    /// The same CREATE TABLE compares as equal across every engine there
    /// is to test on, and a column's length changed is seen on each.
    #[tokio::test]
    async fn live_schemas_across_engines() {
        let ddl = "CREATE TABLE same_t (id int NOT NULL PRIMARY KEY, big bigint, small smallint, name varchar(120), \
                   code char(3), note text, price numeric(10,2), ratio double precision, ok boolean, \
                   day date, clock time, seen DECIMAL(5,0))";
        let mut shapes = Vec::new();
        for engine in Engine::BUILT {
            let Some((db, _)) = crate::db::tests::own_live_db(engine, "podelis_diff").await else { continue };
            let shape = async |later: &str| {
                db.run("DROP TABLE IF EXISTS same_t", 1).await.unwrap();
                // `later` goes in before the closing parenthesis.
                db.run(&format!("{}{later})", &ddl[..ddl.len() - 1]), 1).await.unwrap();
                let catalog = db.catalog(false).await.unwrap();
                let (info, columns) = catalog.into_iter().find(|(t, _)| t.name == "same_t").unwrap();
                vec![TableShape { info, columns, details: None }]
            };
            let side = |name| Side { name, engine };
            let plain = shape("").await;
            let narrow = shape(", later varchar(10)").await;
            let wide = shape(", later varchar(20)").await;
            db.run("DROP TABLE same_t", 1).await.unwrap();
            let diff = schema_diff(&wide, &narrow, side("here"), side("there"));
            // Where the catalog keeps no lengths (DuckDB's VARCHAR), there's
            // no difference to see.
            let length_kept =
                wide[0].columns.last().map(|c| &c.data_type) != narrow[0].columns.last().map(|c| &c.data_type);
            if !length_kept {
                assert_eq!(diff.differences, 0, "{engine:?}: {}", diff.report);
                shapes.push((engine, plain));
                continue;
            }
            assert!(
                diff.report.contains("column later: varchar(20) here, varchar(10) there"),
                "{engine:?}: {}",
                diff.report
            );
            assert_eq!(diff.differences, 1, "{engine:?}: {}", diff.report);
            shapes.push((engine, plain));
        }
        for (i, (engine, here)) in shapes.iter().enumerate() {
            for (other, there) in &shapes[i + 1..] {
                let diff =
                    schema_diff(here, there, Side { name: "a", engine: *engine }, Side { name: "b", engine: *other });
                assert_eq!(diff.differences, 0, "{engine:?} and {other:?}: {}", diff.report);
            }
        }
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
