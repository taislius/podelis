//! SQLite: catalog, details and session queries.

// Every engine has these functions with one signature, the generated
// dispatch calling them alike (build.rs): some needn't wait for anything.
#![allow(clippy::unused_async)]

use super::super::catalog::{ColumnRow, TableRow};
use super::super::details::{ForeignKey, IndexInfo};
use super::super::*;
use sqlx::sqlite::Sqlite;
use super::DriverConnection;
use super::super::dialect::Scan;

impl Database {
    /// The SQLite session connection, waited for (SQLite has one).
    pub(in crate::db) async fn sqlite_session(&self) -> Result<MappedMutexGuard<'_, PoolConnection<Sqlite>>> {
        let session = self.0.session.lock().await;
        self.clear_interrupt();
        MutexGuard::try_map(session, |s| match s {
            Session::Sqlite(c) => Some(c),
            _ => None,
        })
        .map_err(|_| anyhow::anyhow!("the connection is closed"))
    }
}

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

/// Tables and views; each table's rows where `ANALYZE` counted them (the
/// first number of its `sqlite_stat1` rows), for the heavy-query guard and
/// the navigator's order by rows. SQLite keeps no sizes.
pub(in crate::db) async fn table_rows(db: &Database, system: bool) -> Result<Vec<TableRow>> {
    let mut c = db.sqlite_session().await?;
    let counted: Option<i64> = sqlx::query_scalar("SELECT 1 FROM sqlite_master WHERE name = 'sqlite_stat1'")
        .fetch_optional(&mut **c)
        .await?;
    macro_rules! tables {
        ($rows:literal) => {
            concat!(
                "SELECT 'main', m.name, m.type, ",
                $rows,
                ", CAST(NULL AS INTEGER) FROM sqlite_master m
                 WHERE m.type IN ('table', 'view') AND (?1 OR m.name NOT LIKE 'sqlite_%')
                 UNION ALL SELECT 'main', 'sqlite_schema', 'table', NULL, NULL WHERE ?1
                 ORDER BY 2"
            )
        };
    }
    let sql = if counted.is_some() {
        tables!(
            "(SELECT CAST(substr(s.stat, 1, instr(s.stat || ' ', ' ') - 1) AS INTEGER)
              FROM sqlite_stat1 s WHERE s.tbl = m.name LIMIT 1)"
        )
    } else {
        tables!("CAST(NULL AS INTEGER)")
    };
    Ok(sqlx::query_as(sql)
    .bind(system)
    .fetch_all(&mut **c)
    .await?)
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

pub(in crate::db) async fn link_rows(
    db: &Database,
) -> Result<Vec<(String, String, String, String, String, Option<String>)>> {
    let mut c = db.sqlite_session().await?;
    let c: &mut sqlx::SqliteConnection = &mut c;
    Ok(sqlx::query_as(
        "SELECT 'main', m.name, p.\"from\", 'main', p.\"table\", p.\"to\"
             FROM sqlite_schema m JOIN pragma_foreign_key_list(m.name) p
             WHERE m.type = 'table'
             ORDER BY m.name, p.id, p.seq",
    )
    .fetch_all(&mut *c)
    .await?)
}

pub(in crate::db) async fn table_details(db: &Database, _schema: &str, table: &str) -> Result<TableDetails> {
    let mut c = db.sqlite_session().await?;
    let c: &mut sqlx::SqliteConnection = &mut c;
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

/// SQLite is a file: no server sessions to list, or stop.
pub(in crate::db) async fn activity(_db: &Database) -> Result<Vec<Activity>> {
    anyhow::bail!("SQLite has no server sessions to show")
}

pub(in crate::db) async fn stop_session(_db: &Database, _id: i64, _terminate: bool) -> Result<()> {
    anyhow::bail!("SQLite has no server sessions")
}

/// … and no network to encrypt.
pub(in crate::db) async fn tls_info(_db: &Database) -> Option<String> {
    None
}

/// SQLite keeps no comments on tables or columns.
pub(in crate::db) async fn comment_rows(_db: &Database) -> Result<Vec<(String, String, Option<String>, String)>> {
    Ok(Vec::new())
}

/// … nor functions or procedures of its own.
pub(in crate::db) async fn routine_rows(_db: &Database) -> Result<Vec<(String, String, String, String)>> {
    Ok(Vec::new())
}

pub(in crate::db) async fn routine_definition(_db: &Database, _routine: &RoutineInfo) -> Result<String> {
    anyhow::bail!("SQLite has no stored functions")
}

/// Its plans are made on the session (it has one connection). Its plan
/// doesn't show a LIMIT: a plain read of one table that a LIMIT stops
/// early (nothing that needs every row first) reads no table in full.
pub(in crate::db) async fn full_scans_of(db: &Database, statement: &str) -> Option<Vec<Scan>> {
    let scans = db.scans_by_explain(statement).await?;
    Some(if stopped_by_limit(statement) && scans.len() == 1 { Vec::new() } else { scans })
}

/// A LIMIT, and nothing that needs every row before it: no WHERE, ORDER
/// BY, GROUP BY, HAVING, DISTINCT, JOIN or second table.
fn stopped_by_limit(statement: &str) -> bool {
    use crate::sql::TokenKind;
    let tokens = crate::sql::tokenize(statement, crate::db::Engine::Sqlite);
    let words: Vec<String> = tokens
        .iter()
        .filter(|t| t.kind == TokenKind::Word)
        .map(|t| statement[t.range.clone()].to_ascii_lowercase())
        .collect();
    let has = |w: &str| words.iter().any(|x| x == w);
    let commas = tokens.iter().any(|t| t.kind == TokenKind::Punct && &statement[t.range.clone()] == ",");
    has("limit")
        && !["where", "order", "group", "having", "distinct", "join", "union", "intersect", "except"]
            .iter()
            .any(|w| has(w))
        // A comma between tables joins them; between columns it's harmless,
        // but can't be told apart here: only `SELECT *` or one column.
        && !commas
}

pub(in crate::db) async fn estimated_rows(db: &Database, scans: Vec<Scan>) -> Vec<(String, f64)> {
    db.estimates_from_catalog(scans).await
}

/// The connection's progress handler stops the query once it sees this.
pub(in crate::db) async fn cancel(db: &Database, _id: i64) -> Result<()> {
    db.0.interrupt.store(true, Ordering::SeqCst);
    Ok(())
}

/// The transaction state isn't asked: Podėlis tracks BEGIN and COMMIT.
pub(in crate::db) async fn transaction_state(_c: &mut DriverConnection) -> Option<TxnState> {
    None
}

/// It has no `COPY` (`Capabilities::copy_in` is off): imports insert rows.
pub(in crate::db) async fn copy_in(
    db: &Database,
    _statement: &str,
    _first_line: u64,
    _chunks: impl Iterator<Item = Result<Vec<u8>>>,
) -> Result<u64> {
    anyhow::bail!("{} has no COPY", db.engine().label())
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_limit_stops_only_a_plain_read() {
        for sql in ["SELECT * FROM t LIMIT 5", "select x from t limit 10 offset 5"] {
            assert!(super::stopped_by_limit(sql), "{sql}");
        }
        for sql in [
            "SELECT * FROM t",
            "SELECT * FROM t WHERE x = 1 LIMIT 5",
            "SELECT * FROM t ORDER BY x LIMIT 5",
            "SELECT DISTINCT x FROM t LIMIT 5",
            "SELECT * FROM t, u LIMIT 5",
            "SELECT * FROM t JOIN u ON t.id = u.id LIMIT 5",
            "SELECT x, count(*) FROM t GROUP BY x LIMIT 5",
        ] {
            assert!(!super::stopped_by_limit(sql), "{sql}");
        }
    }
}
