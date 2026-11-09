//! PostgreSQL.

mod agent_constraints;
mod plan;

#[cfg(feature = "postgres")]
pub(super) mod cells;
#[cfg(feature = "postgres")]
pub(super) mod connect;
#[cfg(feature = "postgres")]
pub(super) mod driver;
#[cfg(all(any(test, feature = "test-support"), feature = "postgres"))]
pub(in crate::db) mod db_tests;
#[cfg(feature = "postgres")]
mod copy;
#[cfg(feature = "postgres")]
pub(super) mod passwords;
#[cfg(feature = "postgres")]
pub(super) mod pgpass;
#[cfg(feature = "postgres")]
pub(super) mod queries;
mod versions;

use super::backend::{Backend, Capabilities};
use super::dialect::{SqlDialect, LexerRules, Placeholders, Plan};

pub struct Postgres;

/// A row's value as text: what `db::run` reads results with.
#[cfg(feature = "postgres")]
pub(super) use cells::pg_cell as cell;

/// What `db::Pool` and `db::Session` hold for this engine (see build.rs).
#[cfg(feature = "postgres")]
pub(super) type DriverPool = sqlx::PgPool;
#[cfg(feature = "postgres")]
pub(super) type DriverConnection = sqlx::pool::PoolConnection<sqlx::Postgres>;

/// Its SQL dialect, for its own modules (`Engine::dialect` is an `Option`
/// for the rest).
#[cfg(any(test, feature = "postgres"))]
fn sql() -> &'static dyn SqlDialect {
    &DIALECT
}

/// This engine's dialect, for `Engine::dialect`.
pub(super) static DIALECT: Postgres = Postgres;

impl Backend for Postgres {
    /// `real` is a 4-byte float here (MySQL's is a double).
    fn type_kind(&self, ty: &str) -> Option<crate::db::TypeKind> {
        use crate::db::TypeKind as Kind;
        use crate::db::types::{arguments, base};
        if ty.ends_with("[]") {
            return Some(Kind::Other(ty.to_string()));
        }
        Some(match base(ty) {
            "int2" => Kind::Int(2),
            "int4" => Kind::Int(4),
            "int8" => Kind::Int(8),
            "bool" => Kind::Bool,
            "float4" | "real" => Kind::Float4,
            "float8" => Kind::Float8,
            "bpchar" => Kind::Char(arguments(ty).first().copied()),
            "timestamptz" => Kind::TimestampTz,
            "bytea" => Kind::Binary,
            "json" | "jsonb" => Kind::Json,
            "uuid" => Kind::Uuid,
            _ => return None,
        })
    }
    fn auth_failed(&self, message: &str) -> bool {
        message.contains("password authentication failed")
    }
    fn version_query(&self) -> &'static str {
        "SHOW server_version"
    }
    fn is_system_table(&self, schema: &str, _table: &str) -> bool {
        matches!(schema, "pg_catalog" | "information_schema")
    }
    fn agent_constraints(&self) -> &'static crate::db::agent_constraints::AgentConstraints {
        &agent_constraints::AGENT_CONSTRAINTS
    }
    fn language(&self) -> crate::language::Language {
        crate::language::Language::Sql(&DIALECT)
    }
    fn password_files_note(&self) -> &'static str {
        " (or ~/.pgpass)"
    }

    fn container_images(&self) -> &'static [&'static str] {
        &["postgres", "postgis", "timescaledb", "timescaledb-ha"]
    }

    /// The official postgres image's user, database and password.
    fn container_login(&self, var: &dyn Fn(&str) -> Option<String>) -> Option<crate::db::ContainerLogin> {
        use crate::db::ContainerSecret as Secret;
        let user = var("POSTGRES_USER").unwrap_or_else(|| "postgres".to_string());
        let database = var("POSTGRES_DB").unwrap_or_else(|| user.clone());
        let password = if var("POSTGRES_HOST_AUTH_METHOD").as_deref() == Some("trust") {
            None
        } else if var("POSTGRES_PASSWORD").is_some() {
            Some(Secret::Var("POSTGRES_PASSWORD"))
        } else {
            var("POSTGRES_PASSWORD_FILE").map(Secret::File)
        };
        Some(crate::db::ContainerLogin { user, database, password })
    }

    fn label(&self) -> &'static str {
        "PostgreSQL"
    }
    fn url_schemes(&self) -> &'static [&'static str] {
        &["postgres", "postgresql"]
    }
    fn default_port(&self) -> &'static str {
        "5432"
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            file_based: false,
            cancel: true,
            activity: true,
            schemas: true,
            routines: true,
            server_txn_state: true,
            ddl_commits: false,
            stores_create_sql: false,
            table_sizes: true,
            plan_as_grid: false,
            copy_in: true,
            transactions: crate::db::Transactions::Scripts,
        }
    }
    fn connect_hint(&self, message: &str) -> Option<&'static str> {
        let has = |s: &str| message.contains(s);
        Some(if has("password authentication failed") {
            "wrong or missing password for this user: from the form (or the keychain), the password command, ~/.pgpass or PGPASSWORD"
        } else if has("no pg_hba.conf entry") {
            "the server doesn't let this user/database in from your address: it needs a pg_hba.conf entry (or SSL, if only hostssl is allowed)"
        } else if has("role") && has("does not exist") {
            "there's no such user on the server"
        } else if has("database") && has("does not exist") {
            "no database by that name: check it, or leave Database empty for the default"
        } else if has("remaining connection slots") {
            "the server has no free connections: close some, or raise its connection limit"
        } else if has("starting up") || has("shutting down") || has("in recovery mode") {
            "the server is starting up or shutting down: try again in a moment"
        } else {
            return None;
        })
    }
    /// Postgres sockets are a folder holding `.s.PGSQL.<port>`.
    fn socket_file(&self, path: &str, port: u16) -> String {
        if std::path::Path::new(path).is_dir() {
            format!("{}/.s.PGSQL.{port}", path.trim_end_matches('/'))
        } else {
            path.to_string()
        }
    }
}

impl SqlDialect for Postgres {
    fn load_queries(&self) -> &'static [(&'static str, &'static str)] {
        &[
            (
                "Connections",
                "SELECT count(*) || ' of ' || current_setting('max_connections') FROM pg_stat_activity",
            ),
            (
                "Running queries",
                "SELECT count(*) FROM pg_stat_activity WHERE state = 'active' AND pid <> pg_backend_pid()",
            ),
            ("Waiting on locks", "SELECT count(*) FROM pg_stat_activity WHERE wait_event_type = 'Lock'"),
            (
                "Longest running",
                "SELECT coalesce(date_trunc('second', max(now() - query_start))::text, 'none') FROM pg_stat_activity \
                 WHERE state = 'active' AND pid <> pg_backend_pid()",
            ),
            (
                "Idle in transaction",
                "SELECT count(*) FROM pg_stat_activity WHERE state LIKE 'idle in transaction%'",
            ),
            (
                "Cache hit ratio",
                "SELECT coalesce(round(100.0 * sum(blks_hit) / nullif(sum(blks_hit) + sum(blks_read), 0), 1) || ' %', \
                 'none yet') FROM pg_stat_database",
            ),
            (
                "Transactions (commit / rollback)",
                "SELECT sum(xact_commit) || ' / ' || sum(xact_rollback) FROM pg_stat_database",
            ),
            ("Deadlocks", "SELECT sum(deadlocks) FROM pg_stat_database"),
            ("Database size", "SELECT pg_size_pretty(pg_database_size(current_database()))"),
        ]
    }
    fn begin(&self, read_only: bool) -> &'static str {
        if read_only { "BEGIN READ ONLY" } else { "BEGIN" }
    }

    fn hidden_columns(&self) -> &'static [&'static str] {
        &["ctid", "xmin", "xmax", "cmin", "cmax", "tableoid"]
    }


    fn type_name(&self, kind: &crate::db::TypeKind) -> Option<String> {
        use crate::db::TypeKind as Kind;
        use crate::db::types::{decimal, sized};
        Some(match kind {
            Kind::Int(1 | 2) => "smallint".into(),
            Kind::Int(3 | 4) => "integer".into(),
            Kind::Int(_) => "bigint".into(),
            Kind::Bool => "boolean".into(),
            Kind::Decimal(ps) => decimal("numeric", *ps).unwrap_or_else(|| "numeric".into()),
            Kind::Float4 => "real".into(),
            Kind::Float8 => "double precision".into(),
            Kind::Varchar(n) => sized("varchar", *n),
            Kind::Char(n) => sized("char", *n),
            Kind::Text => "text".into(),
            Kind::Date => "date".into(),
            Kind::Time => "time".into(),
            Kind::Timestamp => "timestamp".into(),
            Kind::TimestampTz => "timestamptz".into(),
            Kind::Binary => "bytea".into(),
            Kind::Json => "jsonb".into(),
            Kind::Uuid => "uuid".into(),
            Kind::Other(_) => return None,
        })
    }





    fn lexer(&self) -> LexerRules {
        LexerRules {
            hash_comments: false,
            dash_comment_needs_space: false,
            nested_comments: true,
            backslash_escapes: false,
            double_quoted_strings: false,
            backtick_identifiers: false,
            dollar_quotes: true,
        }
    }

    fn keywords(&self) -> &'static [&'static str] {
        &["FETCH", "FULL", "FUNCTION", "GRANT", "ILIKE", "LATERAL", "RETURNING", "REVOKE", "SCHEMA", "SHOW", "TRUNCATE"]
    }
    fn placeholders(&self) -> Placeholders {
        Placeholders { dollar_number: true, ..Placeholders::default() }
    }
    fn parser(&self) -> Box<dyn sqlparser::dialect::Dialect> {
        Box::new(sqlparser::dialect::PostgreSqlDialect {})
    }
    fn formatter(&self) -> sqlformat::Dialect {
        sqlformat::Dialect::PostgreSql
    }
    fn locks_held_query(&self) -> Option<&'static str> {
        Some(
            "SELECT count(*) FROM pg_locks WHERE pid = pg_backend_pid() AND granted \
             AND locktype IN ('relation', 'tuple', 'transactionid')",
        )
    }
    fn too_new(&self, sql: &str, version: &str) -> Option<crate::db::dialect::TooNew> {
        versions::too_new(sql, version)
    }
    fn explain(&self, stmt: &str, _version: &str) -> String {
        format!("EXPLAIN (FORMAT JSON) {stmt}")
    }
    fn explain_analyze(&self, stmt: &str, _version: &str) -> Option<String> {
        Some(format!("EXPLAIN (ANALYZE, BUFFERS, FORMAT JSON) {stmt}"))
    }
    fn format_plan(&self, _columns: &[String], rows: &[Vec<Option<String>>]) -> Option<Plan> {
        plan::format(rows.first()?.first()?.as_deref()?)
    }
    fn plans_rows(&self, _version: &str) -> bool {
        true
    }
    fn plan_rows(&self, _columns: &[String], rows: &[Vec<Option<String>>]) -> Option<f64> {
        plan::top_rows(rows.first()?.first()?.as_deref()?)
    }
    fn full_scans(&self, _columns: &[String], rows: &[Vec<Option<String>>]) -> Vec<crate::db::dialect::Scan> {
        rows.first().and_then(|r| r.first()).and_then(|c| c.as_deref()).map(plan::full_scans).unwrap_or_default()
    }
    fn real_type(&self) -> &'static str {
        "DOUBLE PRECISION"
    }
    fn timestamptz_type(&self) -> &'static str {
        "TIMESTAMPTZ"
    }
    fn modify_column(&self, table: &str, column: &str, data_type: &str, nullable: bool) -> Option<String> {
        let null = if nullable { "DROP NOT NULL" } else { "SET NOT NULL" };
        Some(format!("ALTER TABLE {table} ALTER COLUMN {column} TYPE {data_type}, ALTER COLUMN {column} {null}"))
    }
}

#[cfg(test)]
mod tests {
    use crate::db::Engine;
    use crate::db::dialect::Scan;

    fn scan(schema: Option<&str>, table: &str, rows: Option<f64>) -> Scan {
        Scan { schema: schema.map(str::to_string), ..Scan::new(table, rows) }
    }

    #[test]
    fn quote_ident() {
        assert_eq!(Engine::Postgres.quote_ident("users"), "users");
        assert_eq!(Engine::Postgres.quote_ident("Users"), "\"Users\"");
        assert_eq!(Engine::Postgres.quote_ident("a\"b"), "\"a\"\"b\"");
    }

    #[test]
    fn explain_statements() {
        let d = super::sql();
        assert_eq!(d.explain_analyze("select 1", "").unwrap(), "EXPLAIN (ANALYZE, BUFFERS, FORMAT JSON) select 1");
        assert_eq!(d.explain("select 1", ""), "EXPLAIN (FORMAT JSON) select 1");
    }

    #[test]
    fn binary_literals() {
        assert_eq!(crate::db::value_literal(r"\x00ff", "BYTEA", Engine::Postgres), r"'\x00ff'");
    }

    #[test]
    fn full_scans_in_plans() {
        let rows = |text: &str| vec![vec![Some(text.to_string())]];
        let pg = |plan: &str| super::sql().full_scans(&["QUERY PLAN".into()], &rows(plan));
        let join = r#"[{"Plan": {"Node Type": "Hash Join", "Plans": [
            {"Node Type": "Seq Scan", "Relation Name": "orders", "Schema": "shop", "Plan Rows": 5000},
            {"Node Type": "Index Scan", "Relation Name": "customers"}]}}]"#;
        assert_eq!(pg(join), [scan(Some("shop"), "orders", Some(5000.0))]);
        // A LIMIT stops a plain scan early; not one with a filter, nor one
        // under a sort, nor the inner side of a nested loop.
        let limited = r#"[{"Plan": {"Node Type": "Limit", "Plans": [{"Node Type": "Seq Scan", "Relation Name": "t", "Plan Rows": 9}]}}]"#;
        assert!(pg(limited).is_empty());
        let filtered = r#"[{"Plan": {"Node Type": "Limit", "Plans": [{"Node Type": "Seq Scan", "Relation Name": "t", "Filter": "(x = 1)", "Plan Rows": 9}]}}]"#;
        assert_eq!(pg(filtered), [scan(None, "t", None)], "rows to be looked up: the filter's aren't the table's");
        let sorted = r#"[{"Plan": {"Node Type": "Limit", "Plans": [{"Node Type": "Sort", "Plans": [{"Node Type": "Seq Scan", "Relation Name": "t", "Plan Rows": 9}]}]}}]"#;
        assert_eq!(pg(sorted), [scan(None, "t", Some(9.0))]);
        let nested = r#"[{"Plan": {"Node Type": "Limit", "Plans": [{"Node Type": "Nested Loop", "Plans": [
            {"Node Type": "Seq Scan", "Relation Name": "a", "Plan Rows": 9},
            {"Node Type": "Seq Scan", "Relation Name": "b", "Plan Rows": 9}]}]}}]"#;
        assert_eq!(pg(nested), [scan(None, "b", Some(9.0))]);
    }
}
