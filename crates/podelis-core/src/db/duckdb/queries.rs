//! DuckDB: catalog, details and session queries, through its
//! `information_schema` and `duckdb_*()` functions, on the session.

#![expect(clippy::unused_async, reason = "every engine has these functions with one signature, the generated dispatch calling them alike (build.rs): some needn't wait for anything")]

use super::super::activity::Activity;
use super::super::catalog::{ColumnRow, TableRow};
use super::super::details::{ForeignKey, IndexInfo};
use super::super::dialect::Scan;
use super::super::*;
use super::DriverConnection;
use super::{cells, driver};

/// Text cells of a catalog query's rows.
type TextRows = Vec<Vec<Option<String>>>;

/// `sql`'s rows as text, on the session, its `?`s bound to `params`.
async fn rows(db: &Database, sql: &str, params: &[&str]) -> Result<TextRows> {
    let session = db.0.session.lock().await;
    db.clear_interrupt();
    let Session::DuckDb(c) = &*session else { anyhow::bail!("the connection is closed") };
    let sql = sql.to_string();
    let params: Vec<String> = params.iter().map(ToString::to_string).collect();
    driver::blocking(c, move |conn| {
        let mut stmt = conn.prepare(&sql)?;
        let arrow = stmt.query_arrow(::duckdb::params_from_iter(params.iter()))?;
        Ok(arrow.flat_map(|batch| (0..batch.num_rows()).map(move |r| cells::row(&batch, r)).collect::<Vec<_>>()).collect())
    })
    .await
}

/// Cell `i` as text (empty for NULL).
fn text(row: &[Option<String>], i: usize) -> String {
    row.get(i).cloned().flatten().unwrap_or_default()
}

/// A filter keeping out the system schemas, unless they're wanted.
fn user_schemas(column: &str, system: bool) -> String {
    if system { "TRUE".into() } else { format!("{column} NOT IN ('information_schema', 'pg_catalog')") }
}

pub(in crate::db) async fn column_rows(db: &Database, system: bool) -> Result<Vec<ColumnRow>> {
    let sql = format!(
        "SELECT c.table_schema, c.table_name, c.column_name, c.data_type, c.is_nullable,
                CASE WHEN EXISTS (SELECT 1 FROM duckdb_constraints() k
                                  WHERE k.constraint_type = 'PRIMARY KEY' AND k.schema_name = c.table_schema
                                    AND k.table_name = c.table_name
                                    AND list_contains(k.constraint_column_names, c.column_name))
                     THEN 'PRI' ELSE '' END
         FROM information_schema.columns c
         WHERE c.table_catalog = current_database() AND {}
         ORDER BY c.table_schema, c.table_name, c.ordinal_position",
        user_schemas("c.table_schema", system)
    );
    Ok(rows(db, &sql, &[])
        .await?
        .iter()
        .map(|r| (text(r, 0), text(r, 1), text(r, 2), text(r, 3), text(r, 4), text(r, 5)))
        .collect())
}

/// Tables and views, each table's rows as DuckDB estimates them. It
/// keeps no sizes.
pub(in crate::db) async fn table_rows(db: &Database, system: bool) -> Result<Vec<TableRow>> {
    let sql = format!(
        "SELECT t.table_schema, t.table_name, t.table_type, CAST(d.estimated_size AS VARCHAR)
         FROM information_schema.tables t
         LEFT JOIN duckdb_tables() d ON d.database_name = t.table_catalog AND d.schema_name = t.table_schema
                                     AND d.table_name = t.table_name
         WHERE t.table_catalog = current_database() AND {}
         ORDER BY 1, 2",
        user_schemas("t.table_schema", system)
    );
    Ok(rows(db, &sql, &[])
        .await?
        .iter()
        .map(|r| (text(r, 0), text(r, 1), text(r, 2), r.get(3).cloned().flatten().and_then(|n| n.parse().ok()), None))
        .collect())
}

#[cfg(test)]
pub(in crate::db) async fn column_info_rows(
    db: &Database,
    schema: &str,
    table: &str,
) -> Result<Vec<(String, String, String)>> {
    let sql = "SELECT column_name, data_type, is_nullable FROM information_schema.columns
               WHERE table_catalog = current_database() AND table_schema = ? AND table_name = ?
               ORDER BY ordinal_position";
    Ok(rows(db, sql, &[schema, table]).await?.iter().map(|r| (text(r, 0), text(r, 1), text(r, 2))).collect())
}

/// Each foreign key's columns, paired with the ones they point at.
pub(in crate::db) async fn link_rows(
    db: &Database,
) -> Result<Vec<(String, String, String, String, String, Option<String>)>> {
    let sql = "SELECT schema_name, table_name, UNNEST(constraint_column_names), schema_name, referenced_table,
                      UNNEST(referenced_column_names)
               FROM duckdb_constraints()
               WHERE constraint_type = 'FOREIGN KEY' AND database_name = current_database()
               ORDER BY 1, 2";
    Ok(rows(db, sql, &[])
        .await?
        .iter()
        .map(|r| (text(r, 0), text(r, 1), text(r, 2), text(r, 3), text(r, 4), r.get(5).cloned().flatten()))
        .collect())
}

pub(in crate::db) async fn table_details(db: &Database, schema: &str, table: &str) -> Result<TableDetails> {
    let keys = rows(
        db,
        "SELECT constraint_type, array_to_string(constraint_column_names, ', '), constraint_text
         FROM duckdb_constraints()
         WHERE database_name = current_database() AND schema_name = ? AND table_name = ?
           AND constraint_type IN ('PRIMARY KEY', 'UNIQUE')
         ORDER BY constraint_type = 'PRIMARY KEY' DESC, constraint_index",
        &[schema, table],
    )
    .await?;
    let indexes = rows(
        db,
        "SELECT index_name, CAST(is_unique AS VARCHAR), expressions, sql FROM duckdb_indexes()
         WHERE database_name = current_database() AND schema_name = ? AND table_name = ?
         ORDER BY index_name",
        &[schema, table],
    )
    .await?;
    let fks = rows(
        db,
        "SELECT UNNEST(constraint_column_names), referenced_table, UNNEST(referenced_column_names)
         FROM duckdb_constraints()
         WHERE database_name = current_database() AND schema_name = ? AND table_name = ?
           AND constraint_type = 'FOREIGN KEY'",
        &[schema, table],
    )
    .await?;
    let definition = rows(
        db,
        "SELECT sql FROM duckdb_tables() WHERE database_name = current_database() AND schema_name = ? AND table_name = ?
         UNION ALL
         SELECT sql FROM duckdb_views() WHERE database_name = current_database() AND schema_name = ? AND view_name = ?",
        &[schema, table, schema, table],
    )
    .await?;
    let keys = keys.iter().map(|r| IndexInfo {
        name: text(r, 0),
        columns: text(r, 1),
        unique: true,
        primary: text(r, 0) == "PRIMARY KEY",
        definition: r.get(2).cloned().flatten(),
    });
    let indexes = indexes.iter().map(|r| IndexInfo {
        name: text(r, 0),
        columns: text(r, 2).trim_matches(['[', ']']).to_string(),
        unique: text(r, 1) == "true",
        primary: false,
        definition: r.get(3).cloned().flatten(),
    });
    Ok(TableDetails {
        indexes: keys.chain(indexes).collect(),
        foreign_keys: fks
            .iter()
            .map(|r| {
                let (column, target_table, target_column) = (text(r, 0), text(r, 1), r.get(2).cloned().flatten());
                ForeignKey {
                    target: format!("{target_table}.{}", target_column.as_deref().unwrap_or("(primary key)")),
                    column,
                    target_schema: schema.to_string(),
                    target_table,
                    target_column,
                }
            })
            .collect(),
        definition: definition.first().and_then(|r| r.first().cloned().flatten()),
    })
}

/// The session-info query and the name of each column it returns.
pub(in crate::db) const SESSION_INFO: (&str, &[&str]) = (
    "SELECT current_database(), current_setting('access_mode'), CAST(current_setting('threads') AS VARCHAR),
            current_setting('memory_limit')",
    &["Database", "Access mode", "Threads", "Memory limit"],
);

/// DuckDB is a file: no server sessions to list, or stop.
pub(in crate::db) async fn activity(_db: &Database) -> Result<Vec<Activity>> {
    anyhow::bail!("DuckDB has no server sessions to show")
}

pub(in crate::db) async fn stop_session(_db: &Database, _id: i64, _terminate: bool) -> Result<()> {
    anyhow::bail!("DuckDB has no server sessions")
}

/// … and no network to encrypt.
pub(in crate::db) async fn tls_info(_db: &Database) -> Option<String> {
    None
}

/// `COMMENT ON` descriptions of tables, views and their columns (the
/// column's name is null for a table's own).
pub(in crate::db) async fn comment_rows(db: &Database) -> Result<Vec<(String, String, Option<String>, String)>> {
    let sql = "SELECT schema_name, table_name, NULL, comment FROM duckdb_tables()
               WHERE database_name = current_database() AND comment IS NOT NULL
               UNION ALL
               SELECT schema_name, view_name, NULL, comment FROM duckdb_views()
               WHERE database_name = current_database() AND comment IS NOT NULL AND NOT internal
               UNION ALL
               SELECT schema_name, table_name, column_name, comment FROM duckdb_columns()
               WHERE database_name = current_database() AND comment IS NOT NULL
               ORDER BY 1, 2";
    Ok(rows(db, sql, &[]).await?.iter().map(|r| (text(r, 0), text(r, 1), r.get(2).cloned().flatten(), text(r, 3))).collect())
}

/// Its macros aren't listed yet.
pub(in crate::db) async fn routine_rows(_db: &Database) -> Result<Vec<(String, String, String, String)>> {
    Ok(Vec::new())
}

pub(in crate::db) async fn routine_definition(_db: &Database, _routine: &RoutineInfo) -> Result<String> {
    anyhow::bail!("DuckDB's macros aren't listed")
}

/// The sequential scans of its drawn plan, but those its filters narrow
/// by the table's key (`id=5`): on a small table DuckDB scans rather than
/// use the key's index, skipping blocks by their least and greatest values.
pub(in crate::db) async fn full_scans_of(db: &Database, statement: &str) -> Option<Vec<Scan>> {
    let dialect = db.engine().dialect();
    let plan = db.run(&dialect.explain(statement, db.version()), 10_000).await.ok()?;
    let text: Vec<&str> = plan.rows.iter().filter_map(|r| r.last()?.as_deref()).collect();
    let text = text.join("\n");
    let found = super::scans_with_filters(&text);
    // One table read under a limit that stops it (not a TOP_N, which sorts
    // all of it, or a join's side, which is read whole): not read in full.
    if found.len() == 1 && text.contains("STREAMING_LIMIT") && !text.contains("JOIN") {
        return Some(Vec::new());
    }
    let mut scans = Vec::new();
    for (scan, filters) in found {
        let schema = scan.schema.clone().unwrap_or_else(|| "main".into());
        let keys = rows(
            db,
            "SELECT constraint_column_names[1] FROM duckdb_constraints()
             WHERE database_name = current_database() AND schema_name = ? AND table_name = ?
               AND constraint_type IN ('PRIMARY KEY', 'UNIQUE') AND len(constraint_column_names) = 1",
            &[&schema, &scan.table],
        )
        .await
        .unwrap_or_default();
        let by_key = filters.iter().any(|f| {
            f.split_once('=').is_some_and(|(column, _)| keys.iter().any(|k| k.first().cloned().flatten().is_some_and(|k| k.eq_ignore_ascii_case(column.trim()))))
        });
        if !by_key {
            scans.push(scan);
        }
    }
    Some(scans)
}

pub(in crate::db) async fn estimated_rows(db: &Database, scans: Vec<Scan>) -> Vec<(String, f64)> {
    db.estimates_from_catalog(scans).await
}

/// Interrupts the session's running query.
pub(in crate::db) async fn cancel(db: &Database, _id: i64) -> Result<()> {
    let Pool::DuckDb(p) = &db.0.pool else { unreachable!("engine matches pool") };
    p.interrupt();
    Ok(())
}

/// The transaction state isn't asked: Podėlis tracks BEGIN and COMMIT.
pub(in crate::db) async fn transaction_state(_c: &mut DriverConnection) -> Option<TxnState> {
    None
}

/// No `COPY … FROM STDIN` (`Capabilities::copy_in` is off): imports insert
/// rows.
pub(in crate::db) async fn copy_in(
    db: &Database,
    _statement: &str,
    _first_line: u64,
    _chunks: impl Iterator<Item = Result<Vec<u8>>>,
) -> Result<u64> {
    anyhow::bail!("{} has no COPY from Podėlis", db.engine().label())
}
