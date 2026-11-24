//! MySQL: catalog, details and session queries.

#![expect(clippy::unused_async, reason = "every engine has these functions with one signature, the generated dispatch calling them alike (build.rs): some needn't wait for anything")]

use super::super::activity::Activity;
use super::super::catalog::{ColumnRow, TableRow};
use super::super::details::{ForeignKey, IndexInfo};
use super::super::*;
use sqlx::{AssertSqlSafe, Row};
use super::DriverConnection;
use super::super::dialect::Scan;

/// How long a catalog query waits for a session whose settings changed.
const CHANGED_SESSION_WAIT: std::time::Duration = std::time::Duration::from_secs(5);

/// Runs `query` on the session connection if it's free (so it sees the
/// session's settings: `DATABASE()` after a `USE`), taken at once so a run
/// can't get in before it; else on one from the pool, while the session's
/// settings are the connection's own. Once they've changed (`USE`, `SET`),
/// a pool connection could be in another database: it waits for the
/// session's turn instead, `CHANGED_SESSION_WAIT` at most (then it says
/// the session's busy); not for a session given up on (the next one opens
/// at the connection's own database, the pool's). `$query` builds the query
/// (it's moved into the session's work, so another is built for each try).
macro_rules! fetch_all_on_session {
    ($db:expr, $pool:expr, $query:expr) => {{
        macro_rules! on_session {
            () => {{
                let query = ($query)();
                move |session: &mut Session| -> SessionFuture<'_, _> {
                    Box::pin(async move {
                        match session {
                            Session::MySql(c) => Some(query.fetch_all(&mut **c).await),
                            _ => None,
                        }
                    })
                }
            }};
        }
        let rows = match $db.with_free_session(on_session!()).await {
            Some(rows) => rows?,
            // Not a session given up on (it'd wait on a dead link): the
            // next one opens at the connection's own database, the pool's.
            // Not for long: an open result's rows hold the session until
            // they're read or closed.
            None if $db.0.session_changed.load(Ordering::SeqCst) && !$db.0.stale.load(Ordering::SeqCst) => {
                match $db.with_session_within(CHANGED_SESSION_WAIT, on_session!()).await? {
                    Some(rows) => rows,
                    None => anyhow::bail!(
                        "the session is busy (an open result holds it), and after USE or SET only it shows the \
                         right database: load again once it's free"
                    ),
                }
            }
            None => None,
        };
        match rows {
            Some(rows) => rows?,
            None => ($query)().fetch_all($pool).await?,
        }
    }};
}

type MySqlActivityRow = (String, String, String, String, String, Option<i64>, String, String);

pub(in crate::db) async fn column_rows(db: &Database, system: bool) -> Result<Vec<ColumnRow>> {
    let Pool::MySql(p) = &db.0.pool else { unreachable!("engine matches pool") };
    Ok({
        let query = move || sqlx::query_as(
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
        let query = move || sqlx::query_as(
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

/// Routines of the session's database (`DATABASE()` follows a `USE`).
const ROUTINES: &str = "SELECT CAST(routine_schema AS CHAR), CAST(routine_name AS CHAR),
            CAST(LOWER(routine_type) AS CHAR), CAST(routine_name AS CHAR)
     FROM information_schema.routines
     WHERE routine_schema = DATABASE()";

/// Its triggers, listed with their tables.
const TRIGGERS: &str = "SELECT CAST(trigger_schema AS CHAR),
            CAST(CONCAT(trigger_name, ' (', event_object_table, ')') AS CHAR),
            'trigger', CAST(trigger_name AS CHAR)
     FROM information_schema.triggers
     WHERE trigger_schema = DATABASE()";

/// Its events.
const EVENTS: &str = "SELECT CAST(event_schema AS CHAR), CAST(event_name AS CHAR), 'event', CAST(event_name AS CHAR)
     FROM information_schema.events
     WHERE event_schema = DATABASE()";

pub(in crate::db) async fn routine_rows(db: &Database) -> Result<Vec<(String, String, String, String)>> {
    let Pool::MySql(p) = &db.0.pool else { unreachable!("engine matches pool") };
    // Each kind by itself: one a server or a user can't read (events
    // without the EVENT privilege, a server without them) leaves only that
    // kind out. DATABASE() must reflect a `USE`, as in `tables`.
    let mut rows: Vec<(String, String, String, String)> = {
        let query = move || sqlx::query_as(ROUTINES);
        fetch_all_on_session!(db, p, query)
    };
    for (kind, sql) in [("triggers", TRIGGERS), ("events", EVENTS)] {
        let query = move || sqlx::query_as(sql);
        let more: Result<Vec<(String, String, String, String)>> = async { Ok(fetch_all_on_session!(db, p, query)) }.await;
        match more {
            Ok(more) => rows.extend(more),
            Err(e) => tracing::debug!(error = %e, kind, "mysql: not listed"),
        }
    }
    rows.sort_by(|a, b| a.1.cmp(&b.1));
    Ok(rows)
}

pub(in crate::db) async fn routine_definition(db: &Database, routine: &RoutineInfo) -> Result<String> {
    let Pool::MySql(p) = &db.0.pool else { unreachable!("engine matches pool") };
    {
        // Which column of SHOW CREATE … holds the statement.
        let (kind, column) = match routine.kind.as_str() {
            "procedure" => ("PROCEDURE", 2),
            "trigger" => ("TRIGGER", 2),
            "event" => ("EVENT", 3),
            _ => ("FUNCTION", 2),
        };
        let sql = format!(
            "SHOW CREATE {kind} {}.{}",
            Engine::MySql.quote_ident(&routine.schema),
            Engine::MySql.quote_ident(&routine.id)
        );
        let row = sqlx::query(AssertSqlSafe(sql)).fetch_one(p).await?;
        // The CREATE statement; NULL without the privilege.
        let def: Option<String> = row
            .try_get::<Option<String>, _>(column)
            .or_else(|_| row.try_get::<Option<Vec<u8>>, _>(column).map(|b| b.map(|b| String::from_utf8_lossy(&b).into())))?;
        def.context("The server didn't show the definition (missing privilege?)")
    }
}

/// The foreign keys of the current database (the session's, after a USE).
pub(in crate::db) async fn link_rows(
    db: &Database,
) -> Result<Vec<(String, String, String, String, String, Option<String>)>> {
    let Pool::MySql(p) = &db.0.pool else { unreachable!("engine matches pool") };
    let query = move || sqlx::query_as(
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
    let query = move || sqlx::query_as(
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

/// A table's CHECK constraints, MySQL's way: constraint names are the
/// schema's own.
const CHECKS_MYSQL: &str = "SELECT CAST(cc.constraint_name AS CHAR), CAST(cc.check_clause AS CHAR)
     FROM information_schema.table_constraints tc
     JOIN information_schema.check_constraints cc
       ON cc.constraint_schema = tc.constraint_schema AND cc.constraint_name = tc.constraint_name
     WHERE tc.constraint_type = 'CHECK' AND tc.table_schema = ? AND tc.table_name = ?
     ORDER BY 1";

/// MariaDB's: a name may repeat in other tables, and `check_constraints`
/// has the table's.
const CHECKS_MARIADB: &str = "SELECT CAST(constraint_name AS CHAR), CAST(check_clause AS CHAR)
     FROM information_schema.check_constraints
     WHERE constraint_schema = ? AND table_name = ?
     ORDER BY 1";

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
    // Column, its target's schema, table and column, ON DELETE, ON UPDATE.
    // The rules' table named by its schema and table as constants too:
    // MySQL 5.7 reads only that table's then, not every schema's.
    type FkRow = (String, String, String, String, Option<String>, Option<String>);
    let fks: Vec<FkRow> = sqlx::query_as(
        "SELECT CAST(k.column_name AS CHAR), CAST(k.referenced_table_schema AS CHAR),
                    CAST(k.referenced_table_name AS CHAR), CAST(k.referenced_column_name AS CHAR),
                    CAST(r.delete_rule AS CHAR), CAST(r.update_rule AS CHAR)
             FROM information_schema.key_column_usage k
             LEFT JOIN information_schema.referential_constraints r
               ON r.constraint_schema = ? AND r.table_name = ?
              AND r.constraint_schema = k.constraint_schema AND r.constraint_name = k.constraint_name
              AND r.table_name = k.table_name
             WHERE k.table_schema = ? AND k.table_name = ? AND k.referenced_table_name IS NOT NULL
             ORDER BY k.constraint_name, k.ordinal_position",
    )
    .bind(schema)
    .bind(table)
    .bind(schema)
    .bind(table)
    .fetch_all(p)
    .await?;
    // MySQL before 8.0.16 has no CHECK constraints (nor the table): none.
    // (A server that refuses the query all the same lists none either.)
    let server = super::server::Server::of(db.version());
    // MariaDB names a column's CHECK after the column, so names repeat from
    // table to table: its check_constraints says whose (MySQL's doesn't;
    // its names are the schema's own).
    let checks_sql = if server.is_mariadb() { CHECKS_MARIADB } else { CHECKS_MYSQL };
    let checks: Vec<(String, String)> = if server.has(super::server::Feature::CheckConstraints) {
        sqlx::query_as(checks_sql).bind(schema).bind(table).fetch_all(p).await.unwrap_or_default()
    } else {
        Vec::new()
    };
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
            .map(|(column, s, t, c, on_delete, on_update)| ForeignKey {
                column,
                target: super::super::details::fk_target(schema, &s, &t, &c),
                target_schema: s,
                target_table: t,
                target_column: Some(c),
                on_delete: super::super::details::fk_rule(on_delete.as_deref()),
                on_update: super::super::details::fk_rule(on_update.as_deref()),
            })
            .collect(),
        definition: definition.and_then(|(d,)| d),
        checks: checks.into_iter().map(|(name, clause)| CheckInfo { name, definition: check_sql(&clause) }).collect(),
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
            .filter_map(|(id, user, database, client, command, seconds, state, query)| {
                Some(Activity {
                    id: crate::db::SessionId::parse(&id)?,
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
            })
            .collect())
    }
}

/// Whether `KILL` found no such session (`Unknown thread id`, 1094): it
/// ended meanwhile (closed, or reconnected under another id), so there's
/// nothing left to stop rather than an error to show.
fn session_gone(e: &sqlx::Error) -> bool {
    e.as_database_error()
        .and_then(|d| d.try_downcast_ref::<sqlx::mysql::MySqlDatabaseError>())
        .is_some_and(|m| m.number() == 1094)
}

/// `KILL` (or `KILL QUERY`) `id`, done if the session is gone already.
async fn kill(db: &Database, sql: String) -> Result<()> {
    let Pool::MySql(p) = &db.0.pool else { unreachable!("engine matches pool") };
    match sqlx::raw_sql(AssertSqlSafe(sql)).execute(p).await {
        Err(e) if !session_gone(&e) => Err(e.into()),
        _ => Ok(()),
    }
}

pub(in crate::db) async fn stop_session(db: &Database, id: i64, terminate: bool) -> Result<()> {
    kill(db, if terminate { format!("KILL {id}") } else { format!("KILL QUERY {id}") }).await
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

/// Connections in use, and how many may be open: the server's limit, and
/// each user's where it's set (MySQL's one more for administrators isn't
/// said). `SHOW GLOBAL STATUS` works without `performance_schema` and on
/// MariaDB alike; on a pool connection, not the session.
pub(in crate::db) async fn connections(db: &Database) -> Option<String> {
    let Pool::MySql(p) = &db.0.pool else { unreachable!("engine matches pool") };
    let (_, in_use): (String, String) =
        sqlx::query_as("SHOW GLOBAL STATUS LIKE 'Threads_connected'").fetch_one(p).await.ok()?;
    let (max, per_user): (i64, i64) =
        sqlx::query_as("SELECT CAST(@@max_connections AS SIGNED), CAST(@@max_user_connections AS SIGNED)")
            .fetch_one(p)
            .await
            .ok()?;
    let each = if per_user > 0 { format!("; each user: up to {per_user}") } else { String::new() };
    Some(format!("{in_use} in use, up to {max}{each}"))
}

/// The session's TLS version and cipher, if it's encrypted.
pub(in crate::db) async fn tls_info(db: &Database) -> Option<String> {
    let o = db.run("SHOW SESSION STATUS WHERE Variable_name IN ('Ssl_version', 'Ssl_cipher')", 2).await.ok()?;
    let get = |name: &str| {
        o.rows.iter().find(|r| r[0].as_deref() == Some(name)).and_then(|r| r[1].clone()).filter(|v| !v.is_empty())
    };
    Some(format!("{}, {}", get("Ssl_version")?, get("Ssl_cipher").unwrap_or_default()))
}

/// The databases this user can see (MySQL lists only those), on a pool
/// connection: the session may be busy.
pub(in crate::db) async fn databases(db: &Database) -> Result<Option<Vec<String>>> {
    let Pool::MySql(p) = &db.0.pool else { unreachable!("engine matches pool") };
    let names: Vec<String> =
        sqlx::query_scalar("SELECT CAST(schema_name AS CHAR) FROM information_schema.schemata ORDER BY 1")
            .fetch_all(p)
            .await?;
    Ok(Some(names))
}

/// Cancels connection `id`'s running query.
/// (A session gone already has nothing running: done, not "Unknown thread
/// id".)
pub(in crate::db) async fn cancel(db: &Database, id: i64) -> Result<()> {
    kill(db, format!("KILL QUERY {id}")).await
}

/// Its plans are made on the session (as the user's statements run).
pub(in crate::db) async fn full_scans_of(db: &Database, statement: &str) -> Result<Vec<Scan>> {
    if super::server::Server::of(db.version()).has(super::server::Feature::ExplainTree) {
        // The tree says where a LIMIT stops reading early.
        return db.scans_by_explain(statement).await;
    }
    // The classic table (MariaDB, MySQL before 8.0.16) doesn't: one table
    // read as it comes (no sort, no temporary table) under the statement's
    // own LIMIT stops early, as the tree would show it.
    let dialect = super::sql();
    let plan = db.run_own(&dialect.explain(statement, db.version()), 10_000).await?;
    let scans = super::plan::table_full_scans(&plan.columns, &plan.rows);
    let extra = plan.columns.iter().position(|c| c.eq_ignore_ascii_case("Extra"));
    let as_it_comes = plan.rows.len() == 1
        && extra.and_then(|e| plan.rows[0].get(e).cloned().flatten()).is_none_or(|x| {
            let x = x.to_ascii_lowercase();
            !x.contains("filesort") && !x.contains("temporary")
        });
    Ok(if as_it_comes && stops_early(statement) { Vec::new() } else { scans })
}

/// Whether `statement` stops reading once its LIMIT has its rows: a LIMIT
/// with nothing that needs every row first (ORDER BY, GROUP BY, DISTINCT,
/// an aggregate, a UNION), nor a WHERE: the rows it keeps may be far apart
/// or none, so the whole table may be read (the tree's `Filter` says so);
/// nor an OFFSET, whose rows are read before the LIMIT's.
fn stops_early(statement: &str) -> bool {
    let words = crate::db::dialect::code_words(statement, Engine::MySql);
    let has = |w: &str| words.iter().any(|x| x == w);
    let aggregate = words.windows(2).any(|w| matches!(w[0].as_str(), "count" | "sum" | "avg" | "min" | "max") && w[1] == "(");
    // `LIMIT 10 OFFSET n` and `LIMIT n, 10` read the n rows first.
    let offset = has("offset") || words.windows(3).any(|w| w[0] == "limit" && w[2] == ",");
    has("limit")
        && !offset
        && !["where", "order", "group", "distinct", "union", "having"].iter().any(|w| has(w))
        && !aggregate
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

pub(in crate::db) async fn table_ddl(db: &Database, schema: &str, table: &str) -> Result<Option<String>> {
    let Pool::MySql(p) = &db.0.pool else { unreachable!("engine matches pool") };
    // Given up on by the server after 5 s behind a lock (an ALTER's), not
    // a year (its default). Set on a connection of its own, taken out of
    // the pool: closed after (or if it's given up on while it asks), it
    // never goes back to the pool with the setting. A server (a proxy)
    // that refuses the setting is asked all the same.
    let mut c = p.acquire().await?.detach();
    let _ = sqlx::query("SET SESSION lock_wait_timeout = 5").execute(&mut c).await;
    let ddl = show_create_table(&mut c, schema, table).await;
    let _ = sqlx::Connection::close(c).await;
    ddl
}

/// The table as MySQL writes it (`SHOW CREATE TABLE`): every column with
/// its default, every key with its ON DELETE, CHECKs, the engine, the
/// character set, comments. `None` for a view (its query is `definition`);
/// an error the server gives (a privilege it lacks) is said, not taken for
/// "none".
async fn show_create_table(c: &mut sqlx::MySqlConnection, schema: &str, table: &str) -> Result<Option<String>> {
    use sqlx::{Column as _, Row as _};
    let sql = format!("SHOW CREATE TABLE {}.{}", Engine::MySql.quote_ident(schema), Engine::MySql.quote_ident(table));
    let row = sqlx::query(AssertSqlSafe(sql)).fetch_one(&mut *c).await?;
    // A view's says `Create View`: not a table's.
    if row.columns().get(1).is_none_or(|c| !c.name().eq_ignore_ascii_case("Create Table")) {
        return Ok(None);
    }
    let ddl: String = row
        .try_get::<String, _>(1)
        .or_else(|_| row.try_get::<Vec<u8>, _>(1).map(|b| String::from_utf8_lossy(&b).into_owned()))?;
    Ok(Some(format!("{};", ddl.trim_end_matches(';'))))
}

/// `CHECK (…)` for a CHECK constraint's clause: MySQL's comes in brackets
/// (`(`total` > 0)`), MariaDB's without (`` `total` > 0 ``).
fn check_sql(clause: &str) -> String {
    let clause = clause.trim();
    // Brackets round all of it, not `(a) OR (b)`.
    let mut depth = 0i32;
    let wrapped = clause.starts_with('(')
        && clause.char_indices().all(|(i, c)| {
            depth += match c {
                '(' => 1,
                ')' => -1,
                _ => 0,
            };
            depth > 0 || i + c.len_utf8() == clause.len()
        });
    if wrapped { format!("CHECK {clause}") } else { format!("CHECK ({clause})") }
}

#[cfg(test)]
mod check_tests {
    #[test]
    fn a_limit_stops_early_unless_every_row_is_needed_first() {
        assert!(super::stops_early("SELECT * FROM scan_big LIMIT 5"));
        assert!(!super::stops_early("SELECT * FROM scan_big ORDER BY note LIMIT 5"));
        assert!(!super::stops_early("SELECT * FROM scan_big WHERE note = 'rare' LIMIT 1"), "may read it all");
        assert!(!super::stops_early("SELECT * FROM scan_big LIMIT 10 OFFSET 50000000"));
        assert!(!super::stops_early("SELECT * FROM scan_big LIMIT 50000000, 10"));
        assert!(!super::stops_early("SELECT count(*) FROM scan_big LIMIT 5"));
        assert!(!super::stops_early("SELECT * FROM scan_big"));
        assert!(!super::stops_early("SELECT 'limit' FROM scan_big"), "a string isn't the word");
    }

    #[test]
    fn a_check_clause_is_bracketed_once() {
        assert_eq!(super::check_sql("(`total` > 0)"), "CHECK (`total` > 0)");
        assert_eq!(super::check_sql("`total` > 0"), "CHECK (`total` > 0)");
        assert_eq!(super::check_sql("(a > 0) or (b > 0)"), "CHECK ((a > 0) or (b > 0))");
    }
}
