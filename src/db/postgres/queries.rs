//! PostgreSQL: catalog, details and session queries.

use super::super::activity::Activity;
use super::super::catalog::{ColumnRow, TableRow};
use super::super::details::{ForeignKey, IndexInfo};
use super::super::*;
use sqlx::PgPool;
use sqlx::postgres::PgDatabaseError;

use super::cells::pg_cell;
use super::DriverConnection;
use super::super::dialect::Scan;

type PgActivityRow = (String, String, String, String, String, Option<i64>, String, String, String);

pub(in crate::db) async fn column_rows(db: &Database, system: bool) -> Result<Vec<ColumnRow>> {
    let Pool::Postgres(p) = &db.0.pool else { unreachable!("engine matches pool") };
    let rows: Vec<ColumnRow> = {
        sqlx::query_as(
            "SELECT c.table_schema::text, c.table_name::text, c.column_name::text,
                            c.udt_name::text || CASE
                                WHEN c.character_maximum_length IS NOT NULL
                                    THEN '(' || c.character_maximum_length || ')'
                                WHEN c.udt_name = 'numeric' AND c.numeric_precision IS NOT NULL
                                    THEN '(' || c.numeric_precision || ',' || c.numeric_scale || ')'
                                ELSE '' END,
                            c.is_nullable::text,
                            CASE WHEN pk.column_name IS NULL THEN '' ELSE 'PRI' END
                     FROM information_schema.columns c
                     LEFT JOIN (
                         SELECT k.table_schema, k.table_name, k.column_name
                         FROM information_schema.table_constraints tc
                         JOIN information_schema.key_column_usage k
                           ON k.constraint_schema = tc.constraint_schema
                          AND k.constraint_name = tc.constraint_name
                          AND k.table_name = tc.table_name
                         WHERE tc.constraint_type = 'PRIMARY KEY'
                     ) pk ON pk.table_schema = c.table_schema AND pk.table_name = c.table_name
                         AND pk.column_name = c.column_name
                     WHERE ($1 OR c.table_schema NOT IN ('pg_catalog', 'information_schema'))
                       AND c.table_schema NOT LIKE 'pg_toast%'
                     ORDER BY c.table_schema, c.table_name, c.ordinal_position",
        )
        .bind(system)
        .fetch_all(p)
        .await?
    };
    // Array types are named `_int4` etc.; show them as `int4[]`. Lengths and
    // precision come along (`varchar(120)`, `numeric(10,2)`), so comparing
    // schemas sees them change, and `char(3)` is called that, not `bpchar`.
    Ok(rows
        .into_iter()
        .map(|(schema, table, column, ty, nullable, key)| {
            let ty = ty.strip_prefix('_').map_or_else(|| ty.clone(), |element| format!("{element}[]"));
            let ty = ty.strip_prefix("bpchar").map_or_else(|| ty.clone(), |rest| format!("char{rest}"));
            (schema, table, column, ty, nullable, key)
        })
        .collect())
}

/// Sizes come from `relpages` (the table, its TOAST table and indexes),
/// which the catalog keeps: reading each table's files instead would be
/// slow with tens of thousands of tables.
pub(in crate::db) async fn table_rows(db: &Database, system: bool) -> Result<Vec<TableRow>> {
    let Pool::Postgres(p) = &db.0.pool else { unreachable!("engine matches pool") };
    Ok({
        sqlx::query_as(
            "SELECT t.table_schema::text, t.table_name::text, t.table_type::text, c.reltuples::bigint,
                            ((c.relpages::bigint
                              + COALESCE((SELECT tc.relpages FROM pg_class tc WHERE tc.oid = c.reltoastrelid), 0)
                              + COALESCE((SELECT sum(i.relpages) FROM pg_index x
                                          JOIN pg_class i ON i.oid = x.indexrelid
                                          WHERE x.indrelid = c.oid), 0))
                             * current_setting('block_size')::bigint)::bigint
                     FROM information_schema.tables t
                     LEFT JOIN pg_namespace n ON n.nspname = t.table_schema
                     LEFT JOIN pg_class c ON c.relnamespace = n.oid AND c.relname = t.table_name
                     WHERE ($1 OR t.table_schema NOT IN ('pg_catalog', 'information_schema'))
                       AND t.table_schema NOT LIKE 'pg_toast%'
                     ORDER BY t.table_schema, t.table_name",
        )
        .bind(system)
        .fetch_all(p)
        .await?
    })
}

#[cfg(test)]
pub(in crate::db) async fn column_info_rows(
    db: &Database,
    schema: &str,
    table: &str,
) -> Result<Vec<(String, String, String)>> {
    let Pool::Postgres(p) = &db.0.pool else { unreachable!("engine matches pool") };
    Ok({
        sqlx::query_as(
            "SELECT column_name::text, data_type::text, is_nullable::text
                     FROM information_schema.columns
                     WHERE table_schema = $1 AND table_name = $2
                     ORDER BY ordinal_position",
        )
        .bind(schema)
        .bind(table)
        .fetch_all(p)
        .await?
    })
}

pub(in crate::db) async fn routine_rows(db: &Database) -> Result<Vec<(String, String, String, String)>> {
    let Pool::Postgres(p) = &db.0.pool else { unreachable!("engine matches pool") };
    Ok({
        sqlx::query_as(
            "SELECT n.nspname::text,
                            p.proname::text || '(' || pg_get_function_identity_arguments(p.oid) || ')',
                            CASE p.prokind WHEN 'p' THEN 'procedure' ELSE 'function' END,
                            p.oid::text
                     FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace
                     WHERE n.nspname NOT IN ('pg_catalog', 'information_schema')
                       AND n.nspname NOT LIKE 'pg_toast%'
                       AND p.prokind IN ('f', 'p')
                       AND NOT EXISTS (SELECT 1 FROM pg_depend d WHERE d.objid = p.oid AND d.deptype = 'e')
                     ORDER BY 1, 2",
        )
        .fetch_all(p)
        .await?
    })
}

pub(in crate::db) async fn routine_definition(db: &Database, routine: &RoutineInfo) -> Result<String> {
    let Pool::Postgres(p) = &db.0.pool else { unreachable!("engine matches pool") };
    {
        let (def,): (String,) =
            sqlx::query_as("SELECT pg_get_functiondef($1::text::oid)").bind(&routine.id).fetch_one(p).await?;
        Ok(def)
    }
}

pub(in crate::db) async fn link_rows(
    db: &Database,
) -> Result<Vec<(String, String, String, String, String, Option<String>)>> {
    let Pool::Postgres(p) = &db.0.pool else { unreachable!("engine matches pool") };
    Ok(sqlx::query_as(
        "SELECT n.nspname::text, t.relname::text, a.attname::text, fn.nspname::text, ft.relname::text, fa.attname::text
             FROM pg_constraint c
             JOIN pg_class t ON t.oid = c.conrelid
             JOIN pg_namespace n ON n.oid = t.relnamespace
             JOIN pg_class ft ON ft.oid = c.confrelid
             JOIN pg_namespace fn ON fn.oid = ft.relnamespace
             CROSS JOIN LATERAL unnest(c.conkey, c.confkey) AS k(col, fcol)
             JOIN pg_attribute a ON a.attrelid = c.conrelid AND a.attnum = k.col
             JOIN pg_attribute fa ON fa.attrelid = c.confrelid AND fa.attnum = k.fcol
             WHERE c.contype = 'f' AND n.nspname NOT IN ('pg_catalog', 'information_schema')
             ORDER BY n.nspname, t.relname, c.conname",
    )
    .fetch_all(p)
    .await?)
}

/// `COMMENT ON` descriptions of tables, views and their columns (the
/// column's name is null for a table's own).
pub(in crate::db) async fn comment_rows(db: &Database) -> Result<Vec<(String, String, Option<String>, String)>> {
    let Pool::Postgres(p) = &db.0.pool else { unreachable!("engine matches pool") };
    Ok(sqlx::query_as(
        "SELECT n.nspname::text, c.relname::text, a.attname::text, d.description
             FROM pg_description d
             JOIN pg_class c ON c.oid = d.objoid AND d.classoid = 'pg_class'::regclass
             JOIN pg_namespace n ON n.oid = c.relnamespace
             LEFT JOIN pg_attribute a ON a.attrelid = c.oid AND a.attnum = d.objsubid AND d.objsubid > 0
             WHERE c.relkind IN ('r', 'p', 'v', 'm', 'f')
               AND n.nspname NOT IN ('pg_catalog', 'information_schema') AND n.nspname NOT LIKE 'pg_toast%'
               AND (d.objsubid = 0 OR a.attname IS NOT NULL)
             ORDER BY n.nspname, c.relname, d.objsubid",
    )
    .fetch_all(p)
    .await?)
}

pub(in crate::db) async fn table_details(db: &Database, schema: &str, table: &str) -> Result<TableDetails> {
    let Pool::Postgres(p) = &db.0.pool else { unreachable!("engine matches pool") };
    let indexes: Vec<(String, bool, bool, String, String)> = sqlx::query_as(
        "SELECT i.relname::text, ix.indisunique, ix.indisprimary,
                    coalesce(string_agg(a.attname::text, ', ' ORDER BY k.ord), ''),
                    pg_get_indexdef(ix.indexrelid)
             FROM pg_index ix
             JOIN pg_class t ON t.oid = ix.indrelid
             JOIN pg_namespace n ON n.oid = t.relnamespace
             JOIN pg_class i ON i.oid = ix.indexrelid
             LEFT JOIN LATERAL unnest(ix.indkey) WITH ORDINALITY k(attnum, ord) ON true
             LEFT JOIN pg_attribute a ON a.attrelid = t.oid AND a.attnum = k.attnum
             WHERE n.nspname = $1 AND t.relname = $2
             GROUP BY i.relname, ix.indisunique, ix.indisprimary, ix.indexrelid
             ORDER BY ix.indisprimary DESC, i.relname",
    )
    .bind(schema)
    .bind(table)
    .fetch_all(p)
    .await?;
    let fks: Vec<(String, String, String, String)> = sqlx::query_as(
        "SELECT a.attname::text, fn.nspname::text, ft.relname::text, fa.attname::text
             FROM pg_constraint c
             JOIN pg_class t ON t.oid = c.conrelid
             JOIN pg_namespace n ON n.oid = t.relnamespace
             JOIN pg_class ft ON ft.oid = c.confrelid
             JOIN pg_namespace fn ON fn.oid = ft.relnamespace
             CROSS JOIN LATERAL unnest(c.conkey, c.confkey) AS k(col, fcol)
             JOIN pg_attribute a ON a.attrelid = c.conrelid AND a.attnum = k.col
             JOIN pg_attribute fa ON fa.attrelid = c.confrelid AND fa.attnum = k.fcol
             WHERE c.contype = 'f' AND n.nspname = $1 AND t.relname = $2
             ORDER BY c.conname",
    )
    .bind(schema)
    .bind(table)
    .fetch_all(p)
    .await?;
    let definition: Option<(Option<String>,)> = sqlx::query_as(
        "SELECT pg_get_viewdef(c.oid, true)
             FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
             WHERE n.nspname = $1 AND c.relname = $2 AND c.relkind IN ('v', 'm')",
    )
    .bind(schema)
    .bind(table)
    .fetch_optional(p)
    .await?;
    Ok(TableDetails {
        indexes: indexes
            .into_iter()
            .map(|(name, unique, primary, columns, def)| IndexInfo {
                name,
                columns,
                unique,
                primary,
                definition: Some(def),
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
        definition: definition.and_then(|(d,)| d).map(|d| d.trim().to_string()),
    })
}

pub(in crate::db) async fn activity(db: &Database) -> Result<Vec<Activity>> {
    let Pool::Postgres(p) = &db.0.pool else { unreachable!("engine matches pool") };
    {
        let rows: Vec<PgActivityRow> = sqlx::query_as(
                    "SELECT pid::text, coalesce(usename::text, ''), coalesce(datname::text, ''),
                            coalesce(host(client_addr), 'local'), coalesce(state, ''),
                            extract(epoch FROM now() - coalesce(state_change, query_start, backend_start))::bigint,
                            CASE WHEN state = 'idle' THEN '' ELSE coalesce(wait_event_type || ': ' || wait_event, '') END,
                            coalesce(query, ''),
                            array_to_string(pg_blocking_pids(pid), ',')
                     FROM pg_stat_activity
                     WHERE backend_type = 'client backend' AND pid <> pg_backend_pid()
                     ORDER BY state = 'idle', state_change",
                )
                .fetch_all(p)
                .await?;
        Ok(rows
            .into_iter()
            .map(|(id, user, database, client, state, seconds, wait, query, blocked)| Activity {
                id,
                user,
                database,
                client,
                state,
                seconds,
                wait,
                query,
                blocked_by: blocked.split(',').filter(|s| !s.is_empty()).map(str::to_string).collect(),
                own: false,
            })
            .collect())
    }
}

pub(in crate::db) async fn stop_session(db: &Database, id: i64, terminate: bool) -> Result<()> {
    let Pool::Postgres(p) = &db.0.pool else { unreachable!("engine matches pool") };
    let sql = if terminate { "SELECT pg_terminate_backend($1)" } else { "SELECT pg_cancel_backend($1)" };
    let (done,): (bool,) = sqlx::query_as(sql).bind(id as i32).fetch_one(p).await?;
    anyhow::ensure!(done, "the server didn't stop it (no permission, or it already ended)");
    Ok(())
}

/// The session-info query and the name of each column it returns.
pub(in crate::db) const SESSION_INFO: (&str, &[&str]) = (
    "SELECT current_user::text, session_user::text, current_database()::text,
                        array_to_string(current_schemas(false), ', '),
                        current_setting('TimeZone'), current_setting('client_encoding'),
                        current_setting('transaction_isolation'), current_setting('statement_timeout'),
                        current_setting('default_transaction_read_only'), pg_backend_pid()::text,
                        coalesce(host(inet_server_addr()) || ':' || inet_server_port()::text, 'Unix socket'),
                        to_char(pg_postmaster_start_time(), 'YYYY-MM-DD HH24:MI')",
    &[
        "User",
        "Session user",
        "Database",
        "Schemas",
        "Time zone",
        "Encoding",
        "Isolation",
        "Statement timeout",
        "Read-only",
        "Backend PID",
        "Server address",
        "Server started",
    ],
);

/// The session's TLS version and cipher, if it's encrypted.
pub(in crate::db) async fn tls_info(db: &Database) -> Option<String> {
    let o = db.run("SELECT version, cipher FROM pg_stat_ssl WHERE pid = pg_backend_pid() AND ssl", 1).await.ok()?;
    let row = o.rows.first()?;
    Some(format!("{}, {}", row[0].clone()?, row[1].clone().unwrap_or_default()))
}

/// What a failed prepare means: a syntax error (`Some(true)`), an unknown
/// object (`Some(false)`), or nothing worth a hint (`None`).
pub(in crate::db) fn prepare_problem(err: &dyn sqlx::error::DatabaseError) -> Option<bool> {
    match err.try_downcast_ref::<PgDatabaseError>()?.code() {
        "42601" => Some(true),
        // undefined table/column/function/object, wrong argument types
        "42P01" | "42703" | "42883" | "42704" | "42P02" | "42804" => Some(false),
        _ => None,
    }
}

/// Cancels backend `id`'s running query.
pub(in crate::db) async fn cancel(db: &Database, id: i64) -> Result<()> {
    let Pool::Postgres(p) = &db.0.pool else { unreachable!("engine matches pool") };
    sqlx::query("SELECT pg_cancel_backend($1)").bind(id as i32).execute(p).await?;
    Ok(())
}

/// The JSON plan of `statement` for the heavy-query guard, made on a pool
/// connection (a failure there can't abort the session's transaction),
/// with schemas (`VERBOSE`).
/// The full scans `statement` would make, planned on a pool connection of
/// its own: one that fails would otherwise abort the session's open
/// transaction.
pub(in crate::db) async fn full_scans_of(db: &Database, statement: &str) -> Option<Vec<Scan>> {
    let Pool::Postgres(p) = &db.0.pool else { unreachable!("engine matches pool") };
    let json = guard_plan(p, statement).await?;
    Some(db.engine().dialect().full_scans(&[], &[vec![Some(json)]]))
}

/// Each scan's rows: the plan's, else the catalog's estimate for that
/// table alone (a catalog can be huge).
pub(in crate::db) async fn estimated_rows(db: &Database, scans: Vec<Scan>) -> Vec<(String, f64)> {
    let Pool::Postgres(p) = &db.0.pool else { unreachable!("engine matches pool") };
    let mut out = Vec::new();
    for scan in scans {
        let rows = match scan.rows {
            Some(rows) => Some(rows),
            None => table_estimate(p, scan.schema.as_deref(), &scan.table).await,
        };
        if let Some(rows) = rows {
            out.push((scan.name(), rows));
        }
    }
    out
}

async fn guard_plan(p: &PgPool, statement: &str) -> Option<String> {
    let sql = format!("EXPLAIN (FORMAT JSON, VERBOSE) {statement}");
    let row = sqlx::raw_sql(AssertSqlSafe(sql)).fetch_one(p).await.ok()?;
    row.try_get::<serde_json::Value, _>(0).ok().map(|v| v.to_string())
}

/// A table's row count as the planner estimates it (none before it was
/// first analyzed). Without a schema, as the search path finds it.
async fn table_estimate(p: &PgPool, schema: Option<&str>, table: &str) -> Option<f64> {
    let query = match schema {
        Some(schema) => sqlx::query_scalar::<_, f64>(
            "SELECT c.reltuples::float8 FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
             WHERE n.nspname = $1 AND c.relname = $2",
        )
        .bind(schema)
        .bind(table),
        None => sqlx::query_scalar::<_, f64>(
            "SELECT reltuples::float8 FROM pg_class WHERE oid = to_regclass(quote_ident($1))",
        )
        .bind(table),
    };
    query.fetch_optional(p).await.ok().flatten().filter(|rows| *rows >= 0.0)
}

/// Whether the session is inside a transaction, asked: `now()` is frozen
/// at the transaction's start.
pub(in crate::db) async fn transaction_state(c: &mut DriverConnection) -> Option<TxnState> {
    // Simple protocol, so the "statement" is exactly this query.
    let probe = sqlx::raw_sql("SELECT now() <> statement_timestamp()").fetch_one(&mut **c).await;
    match probe {
        Ok(row) => Some(if pg_cell(&row, 0).as_deref() == Some("true") { TxnState::Open } else { TxnState::Idle }),
        Err(sqlx::Error::Database(e)) if e.code().as_deref() == Some("25P02") => Some(TxnState::Aborted),
        Err(_) => None,
    }
}
