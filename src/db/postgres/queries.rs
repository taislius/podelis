//! PostgreSQL: catalog, details and session queries.

use super::super::activity::Activity;
use super::super::catalog::{ColumnRow, TableRow};
use super::super::details::{ForeignKey, IndexInfo};
use super::super::*;

type PgActivityRow = (String, String, String, String, String, Option<i64>, String, String, String);

pub(in crate::db) async fn column_rows(db: &Database, system: bool) -> Result<Vec<ColumnRow>> {
    let Pool::Postgres(p) = &db.0.pool else { unreachable!("engine matches pool") };
    let rows: Vec<ColumnRow> = {
        sqlx::query_as(
            "SELECT c.table_schema::text, c.table_name::text, c.column_name::text, c.udt_name::text,
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
    // Array types are named `_int4` etc.; show them as `int4[]`.
    Ok(rows
        .into_iter()
        .map(|(schema, table, column, ty, nullable, key)| {
            let ty = ty.strip_prefix('_').map_or_else(|| ty.clone(), |element| format!("{element}[]"));
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

pub(in crate::db) async fn table_details(p: &PgPool, schema: &str, table: &str) -> Result<TableDetails> {
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
pub(in crate::db) async fn cancel(p: &PgPool, id: i64) -> Result<()> {
    sqlx::query("SELECT pg_cancel_backend($1)").bind(id as i32).execute(p).await?;
    Ok(())
}
