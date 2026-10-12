//! SQLite: catalog, details and session queries.

use super::super::catalog::{ColumnRow, TableRow};
use super::super::details::{ForeignKey, IndexInfo};
use super::super::*;

/// (name, unique, origin, columns, CREATE INDEX)
type SqliteIndexRow = (String, i64, String, Option<String>, Option<String>);

pub(in crate::db) async fn column_rows(db: &Database, system: bool) -> Result<Vec<ColumnRow>> {
    Ok({
        let mut c = db.sqlite_session().await?;
        let rows: Vec<(String, String, String, i64, i64)> = sqlx::query_as(
            "SELECT m.name, p.name, p.type, p.\"notnull\", p.pk
                     FROM (SELECT name FROM sqlite_master
                           WHERE type IN ('table', 'view') AND (?1 OR name NOT LIKE 'sqlite_%')
                           UNION ALL SELECT 'sqlite_schema' WHERE ?1) m
                     JOIN pragma_table_info(m.name) p
                     ORDER BY m.name, p.cid",
        )
        .bind(system)
        .fetch_all(&mut **c)
        .await?;
        rows.into_iter()
            .map(|(t, c, ty, nn, pk)| {
                let nullable = if nn == 0 { "YES" } else { "NO" };
                ("main".into(), t, c, ty, nullable.into(), if pk > 0 { "PRI" } else { "" }.into())
            })
            .collect()
    })
}

pub(in crate::db) async fn table_rows(db: &Database, system: bool) -> Result<Vec<TableRow>> {
    Ok({
        let mut c = db.sqlite_session().await?;
        sqlx::query_as(
            "SELECT 'main', name, type, CAST(NULL AS INTEGER), CAST(NULL AS INTEGER) FROM sqlite_master
                     WHERE type IN ('table', 'view') AND (?1 OR name NOT LIKE 'sqlite_%')
                     UNION ALL SELECT 'main', 'sqlite_schema', 'table', NULL, NULL WHERE ?1
                     ORDER BY 2",
        )
        .bind(system)
        .fetch_all(&mut **c)
        .await?
    })
}

#[cfg(test)]
pub(in crate::db) async fn column_info_rows(
    db: &Database,
    _schema: &str,
    table: &str,
) -> Result<Vec<(String, String, String)>> {
    Ok({
        let mut c = db.sqlite_session().await?;
        let rows: Vec<(String, String, i64)> =
            sqlx::query_as("SELECT name, type, \"notnull\" FROM pragma_table_info(?)")
                .bind(table)
                .fetch_all(&mut **c)
                .await?;
        rows.into_iter()
            .map(|(name, ty, notnull)| (name, ty, if notnull == 0 { "YES" } else { "NO" }.to_string()))
            .collect()
    })
}

pub(in crate::db) async fn table_details(c: &mut sqlx::SqliteConnection, table: &str) -> Result<TableDetails> {
    let indexes: Vec<SqliteIndexRow> = sqlx::query_as(
        "SELECT il.name, il.\"unique\", il.origin,
                    (SELECT group_concat(name, ', ') FROM pragma_index_info(il.name)),
                    (SELECT sql FROM sqlite_master WHERE type = 'index' AND name = il.name)
             FROM pragma_index_list(?1) il
             ORDER BY il.origin = 'pk' DESC, il.name",
    )
    .bind(table)
    .fetch_all(&mut *c)
    .await?;
    let fks: Vec<(String, String, Option<String>)> =
        sqlx::query_as("SELECT \"from\", \"table\", \"to\" FROM pragma_foreign_key_list(?1) ORDER BY id, seq")
            .bind(table)
            .fetch_all(&mut *c)
            .await?;
    let definition: Option<(Option<String>,)> =
        sqlx::query_as("SELECT sql FROM sqlite_master WHERE type IN ('table', 'view') AND name = ?1")
            .bind(table)
            .fetch_optional(&mut *c)
            .await?;
    Ok(TableDetails {
        indexes: indexes
            .into_iter()
            .map(|(name, unique, origin, columns, def)| IndexInfo {
                name,
                columns: columns.unwrap_or_default(),
                unique: unique != 0,
                primary: origin == "pk",
                definition: def,
            })
            .collect(),
        foreign_keys: fks
            .into_iter()
            .map(|(column, t, to)| ForeignKey {
                column,
                // No target column means the target's primary key.
                target: format!("{t}.{}", to.as_deref().unwrap_or("(primary key)")),
                target_schema: "main".into(),
                target_table: t,
                target_column: to,
            })
            .collect(),
        definition: definition.and_then(|(d,)| d),
    })
}

/// The session-info query and the name of each column it returns.
pub(in crate::db) const SESSION_INFO: (&str, &[&str]) = (
                "SELECT (SELECT journal_mode FROM pragma_journal_mode),
                        CAST((SELECT foreign_keys FROM pragma_foreign_keys) AS TEXT),
                        CAST((SELECT page_count FROM pragma_page_count) * (SELECT page_size FROM pragma_page_size) AS TEXT),
                        CAST((SELECT query_only FROM pragma_query_only) AS TEXT)",
                &["Journal mode", "Foreign keys", "Size (bytes)", "Read-only"],
            );

/// What a failed prepare means: a syntax error (`Some(true)`), an unknown
/// object (`Some(false)`), or nothing worth a hint (`None`).
pub(in crate::db) fn prepare_problem(err: &dyn sqlx::error::DatabaseError) -> Option<bool> {
    let msg = err.message();
    if msg.contains("syntax error") || msg.contains("incomplete input") || msg.contains("unrecognized token") {
        Some(true)
    } else if msg.starts_with("no such") {
        Some(false)
    } else {
        None
    }
}
