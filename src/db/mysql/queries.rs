//! MySQL: catalog, details and session queries.

// Every engine has these functions with one signature, the generated
// dispatch calling them alike (build.rs): some needn't wait for anything.
#![allow(clippy::unused_async)]

use super::super::activity::Activity;
use super::super::catalog::{ColumnRow, TableRow};
use super::super::details::{ForeignKey, IndexInfo};
use super::super::*;
use sqlx::mysql::{MySql, MySqlDatabaseError};
use super::DriverConnection;
use super::super::dialect::Scan;

impl Database {
    /// The MySQL session connection if it's free right now; otherwise (or
    /// after `close()`) a query takes one from the pool.
    pub(in crate::db) fn mysql_session_now(&self) -> Option<MappedMutexGuard<'_, PoolConnection<MySql>>> {
        let session = self.0.session.try_lock().ok()?;
        MutexGuard::try_map(session, |s| match s {
            Session::MySql(c) => Some(c),
            _ => None,
        })
        .ok()
    }
}

/// Runs `query` on the session connection if it's free (so it sees the
/// session's settings), else on one from the pool.
macro_rules! fetch_all_on_session {
    ($db:expr, $pool:expr, $query:expr) => {
        match $db.mysql_session_now() {
            Some(mut c) => $query.fetch_all(&mut **c).await?,
            None => $query.fetch_all($pool).await?,
        }
    };
}

type MySqlActivityRow = (String, String, String, String, String, Option<i64>, String, String);

pub(in crate::db) async fn column_rows(db: &Database, system: bool) -> Result<Vec<ColumnRow>> {
    let Pool::MySql(p) = &db.0.pool else { unreachable!("engine matches pool") };
    Ok({
        let query = sqlx::query_as(
            "SELECT CAST(table_schema AS CHAR), CAST(table_name AS CHAR), CAST(column_name AS CHAR),
                            CAST(column_type AS CHAR), CAST(is_nullable AS CHAR), CAST(column_key AS CHAR)
                     FROM information_schema.columns
                     WHERE table_schema = DATABASE()
                        OR (? AND table_schema IN ('information_schema', 'performance_schema', 'mysql', 'sys'))
                     ORDER BY table_schema, table_name, ordinal_position",
        )
        .bind(system);
        fetch_all_on_session!(db, p, query)
    })
}

pub(in crate::db) async fn table_rows(db: &Database, system: bool) -> Result<Vec<TableRow>> {
    let Pool::MySql(p) = &db.0.pool else { unreachable!("engine matches pool") };
    Ok({
        let query = sqlx::query_as(
            "SELECT CAST(table_schema AS CHAR), CAST(table_name AS CHAR),
                            CAST(table_type AS CHAR), CAST(table_rows AS SIGNED),
                            CAST(data_length + index_length AS SIGNED)
                     FROM information_schema.tables
                     WHERE table_schema = DATABASE()
                        OR (? AND table_schema IN ('information_schema', 'performance_schema', 'mysql', 'sys'))
                     ORDER BY table_schema, table_name",
        )
        .bind(system);
        // DATABASE() must reflect any `USE` the user ran, so ask the
        // session connection — unless a query is running on it.
        fetch_all_on_session!(db, p, query)
    })
}

#[cfg(test)]
pub(in crate::db) async fn column_info_rows(
    db: &Database,
    schema: &str,
    table: &str,
) -> Result<Vec<(String, String, String)>> {
    let Pool::MySql(p) = &db.0.pool else { unreachable!("engine matches pool") };
    Ok({
        sqlx::query_as(
            "SELECT CAST(column_name AS CHAR), CAST(column_type AS CHAR),
                            CAST(is_nullable AS CHAR)
                     FROM information_schema.columns
                     WHERE table_schema = ? AND table_name = ?
                     ORDER BY ordinal_position",
        )
        .bind(schema)
        .bind(table)
        .fetch_all(p)
        .await?
    })
}

pub(in crate::db) async fn routine_rows(db: &Database) -> Result<Vec<(String, String, String, String)>> {
    let Pool::MySql(p) = &db.0.pool else { unreachable!("engine matches pool") };
    Ok({
        let query = sqlx::query_as(
            "SELECT CAST(routine_schema AS CHAR), CAST(routine_name AS CHAR),
                            CAST(LOWER(routine_type) AS CHAR), CAST(routine_name AS CHAR)
                     FROM information_schema.routines
                     WHERE routine_schema = DATABASE()
                     ORDER BY routine_name",
        );
        // DATABASE() must reflect a `USE`, as in `tables`.
        fetch_all_on_session!(db, p, query)
    })
}

pub(in crate::db) async fn routine_definition(db: &Database, routine: &RoutineInfo) -> Result<String> {
    let Pool::MySql(p) = &db.0.pool else { unreachable!("engine matches pool") };
    {
        let kind = if routine.kind == "procedure" { "PROCEDURE" } else { "FUNCTION" };
        let sql = format!(
            "SHOW CREATE {kind} {}.{}",
            Engine::MySql.quote_ident(&routine.schema),
            Engine::MySql.quote_ident(&routine.id)
        );
        let row = sqlx::query(AssertSqlSafe(sql)).fetch_one(p).await?;
        // Column 3 is the CREATE statement; NULL without the privilege.
        let def: Option<String> = row
            .try_get::<Option<String>, _>(2)
            .or_else(|_| row.try_get::<Option<Vec<u8>>, _>(2).map(|b| b.map(|b| String::from_utf8_lossy(&b).into())))?;
        def.context("The server didn't show the definition (missing privilege?)")
    }
}

/// The foreign keys of the current database (the session's, after a USE).
pub(in crate::db) async fn link_rows(
    db: &Database,
) -> Result<Vec<(String, String, String, String, String, Option<String>)>> {
    let Pool::MySql(p) = &db.0.pool else { unreachable!("engine matches pool") };
    let query = sqlx::query_as(
        "SELECT CAST(table_schema AS CHAR), CAST(table_name AS CHAR), CAST(column_name AS CHAR),
                    CAST(referenced_table_schema AS CHAR), CAST(referenced_table_name AS CHAR),
                    CAST(referenced_column_name AS CHAR)
             FROM information_schema.key_column_usage
             WHERE table_schema = DATABASE() AND referenced_table_name IS NOT NULL
             ORDER BY table_name, constraint_name, ordinal_position",
    );
    Ok(fetch_all_on_session!(db, p, query))
}

/// Table and column comments of the current database (a view's
/// `TABLE_COMMENT` is just "VIEW", so views are left out).
pub(in crate::db) async fn comment_rows(db: &Database) -> Result<Vec<(String, String, Option<String>, String)>> {
    let Pool::MySql(p) = &db.0.pool else { unreachable!("engine matches pool") };
    let query = sqlx::query_as(
        "SELECT CAST(table_schema AS CHAR), CAST(table_name AS CHAR), CAST(NULL AS CHAR),
                    CAST(table_comment AS CHAR)
             FROM information_schema.tables
             WHERE table_schema = DATABASE() AND table_type <> 'VIEW' AND table_comment <> ''
         UNION ALL
         SELECT CAST(table_schema AS CHAR), CAST(table_name AS CHAR), CAST(column_name AS CHAR),
                    CAST(column_comment AS CHAR)
             FROM information_schema.columns
             WHERE table_schema = DATABASE() AND column_comment <> ''
         ORDER BY 2, 3",
    );
    Ok(fetch_all_on_session!(db, p, query))
}

pub(in crate::db) async fn table_details(db: &Database, schema: &str, table: &str) -> Result<TableDetails> {
    let Pool::MySql(p) = &db.0.pool else { unreachable!("engine matches pool") };
    let indexes: Vec<(String, i64, Option<String>)> = sqlx::query_as(
        "SELECT CAST(index_name AS CHAR), CAST(MAX(non_unique) AS SIGNED),
                    CAST(GROUP_CONCAT(column_name ORDER BY seq_in_index SEPARATOR ', ') AS CHAR)
             FROM information_schema.statistics
             WHERE table_schema = ? AND table_name = ?
             GROUP BY index_name
             ORDER BY index_name = 'PRIMARY' DESC, index_name",
    )
    .bind(schema)
    .bind(table)
    .fetch_all(p)
    .await?;
    let fks: Vec<(String, String, String, String)> = sqlx::query_as(
        "SELECT CAST(column_name AS CHAR), CAST(referenced_table_schema AS CHAR),
                    CAST(referenced_table_name AS CHAR), CAST(referenced_column_name AS CHAR)
             FROM information_schema.key_column_usage
             WHERE table_schema = ? AND table_name = ? AND referenced_table_name IS NOT NULL
             ORDER BY constraint_name, ordinal_position",
    )
    .bind(schema)
    .bind(table)
    .fetch_all(p)
    .await?;
    let definition: Option<(Option<String>,)> = sqlx::query_as(
        "SELECT CAST(view_definition AS CHAR) FROM information_schema.views
             WHERE table_schema = ? AND table_name = ?",
    )
    .bind(schema)
    .bind(table)
    .fetch_optional(p)
    .await?;
    Ok(TableDetails {
        indexes: indexes
            .into_iter()
            .map(|(name, non_unique, columns)| IndexInfo {
                primary: name == "PRIMARY",
                name,
                columns: columns.unwrap_or_default(),
                unique: non_unique == 0,
                definition: None,
            })
            .collect(),
        foreign_keys: fks
            .into_iter()
            .map(|(column, s, t, c)| ForeignKey {
                column,
                target: super::super::details::fk_target(schema, &s, &t, &c),
                target_schema: s,
                target_table: t,
                target_column: Some(c),
            })
            .collect(),
        definition: definition.and_then(|(d,)| d),
    })
}

pub(in crate::db) async fn activity(db: &Database) -> Result<Vec<Activity>> {
    let Pool::MySql(p) = &db.0.pool else { unreachable!("engine matches pool") };
    {
        let rows: Vec<MySqlActivityRow> = sqlx::query_as(
            "SELECT CAST(id AS CHAR), CAST(user AS CHAR), CAST(coalesce(db, '') AS CHAR),
                            CAST(host AS CHAR), CAST(command AS CHAR), CAST(time AS SIGNED),
                            CAST(coalesce(state, '') AS CHAR), CAST(coalesce(info, '') AS CHAR)
                     FROM information_schema.processlist
                     WHERE id <> CONNECTION_ID() AND command <> 'Daemon'
                     ORDER BY command = 'Sleep', time DESC",
        )
        .fetch_all(p)
        .await?;
        Ok(rows
            .into_iter()
            .map(|(id, user, database, client, command, seconds, state, query)| Activity {
                id,
                user,
                database,
                client,
                state: command.to_lowercase(),
                seconds,
                wait: state,
                query,
                blocked_by: Vec::new(),
                own: false,
            })
            .collect())
    }
}

pub(in crate::db) async fn stop_session(db: &Database, id: i64, terminate: bool) -> Result<()> {
    let Pool::MySql(p) = &db.0.pool else { unreachable!("engine matches pool") };
    let sql = if terminate { format!("KILL {id}") } else { format!("KILL QUERY {id}") };
    sqlx::raw_sql(AssertSqlSafe(sql)).execute(p).await?;
    Ok(())
}

/// The session-info query and the name of each column it returns.
pub(in crate::db) const SESSION_INFO: (&str, &[&str]) = (
    "SELECT CURRENT_USER(), DATABASE(), @@time_zone, @@character_set_client,
                        @@transaction_isolation, CAST(@@transaction_read_only AS CHAR),
                        CAST(CONNECTION_ID() AS CHAR), CONCAT(@@hostname, ':', @@port),
                        CAST(@@max_execution_time AS CHAR)",
    &[
        "User",
        "Database",
        "Time zone",
        "Encoding",
        "Isolation",
        "Read-only",
        "Connection ID",
        "Server",
        "Max execution time (ms)",
    ],
);

/// The session's TLS version and cipher, if it's encrypted.
pub(in crate::db) async fn tls_info(db: &Database) -> Option<String> {
    let o = db.run("SHOW SESSION STATUS WHERE Variable_name IN ('Ssl_version', 'Ssl_cipher')", 2).await.ok()?;
    let get = |name: &str| {
        o.rows.iter().find(|r| r[0].as_deref() == Some(name)).and_then(|r| r[1].clone()).filter(|v| !v.is_empty())
    };
    Some(format!("{}, {}", get("Ssl_version")?, get("Ssl_cipher").unwrap_or_default()))
}

/// What a failed prepare means: a syntax error (`Some(true)`), an unknown
/// object (`Some(false)`), or nothing worth a hint (`None`).
pub(in crate::db) fn prepare_problem(err: &dyn sqlx::error::DatabaseError) -> Option<bool> {
    match err.try_downcast_ref::<MySqlDatabaseError>()?.number() {
        1064 | 1149 => Some(true),
        // unknown table/column/function, no database selected
        1146 | 1054 | 1305 | 1046 | 1109 => Some(false),
        // 1295: statement can't be prepared; anything else: not a hint
        _ => None,
    }
}

/// Cancels connection `id`'s running query.
pub(in crate::db) async fn cancel(db: &Database, id: i64) -> Result<()> {
    let Pool::MySql(p) = &db.0.pool else { unreachable!("engine matches pool") };
    sqlx::raw_sql(AssertSqlSafe(format!("KILL QUERY {id}"))).execute(p).await?;
    Ok(())
}

/// Its plans are made on the session (as the user's statements run).
pub(in crate::db) async fn full_scans_of(db: &Database, statement: &str) -> Option<Vec<Scan>> {
    db.scans_by_explain(statement).await
}

pub(in crate::db) async fn estimated_rows(db: &Database, scans: Vec<Scan>) -> Vec<(String, f64)> {
    db.estimates_from_catalog(scans).await
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
