//! PostgreSQL.

mod agent_constraints;
mod plan;

pub(super) mod cells;
pub(super) mod connect;
mod copy;
pub(super) mod passwords;
pub(super) mod pgpass;
pub(super) mod queries;

use super::dialect::{Capabilities, Dialect, LexerRules, Placeholders, Plan};

pub struct Postgres;

/// A row's value as text: what `db::run` reads results with.
pub(super) use cells::pg_cell as cell;

/// What `db::Pool` and `db::Session` hold for this engine (see build.rs).
pub(super) type DriverPool = sqlx::PgPool;
pub(super) type DriverConnection = sqlx::pool::PoolConnection<sqlx::Postgres>;

/// This engine's dialect, for `Engine::dialect`.
pub(super) static DIALECT: Postgres = Postgres;

impl Dialect for Postgres {
    fn begin(&self, read_only: bool) -> &'static str {
        if read_only { "BEGIN READ ONLY" } else { "BEGIN" }
    }

    fn hidden_columns(&self) -> &'static [&'static str] {
        &["ctid", "xmin", "xmax", "cmin", "cmax", "tableoid"]
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

    /// `real` is a 4-byte float here (MySQL's is a double).
    fn type_kind(&self, base: &str) -> Option<crate::db::TypeKind> {
        (base == "real").then_some(crate::db::TypeKind::Float4)
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

    fn database_error_message(&self, err: &dyn sqlx::error::DatabaseError) -> Option<String> {
        let pg = err.try_downcast_ref::<sqlx::postgres::PgDatabaseError>()?;
        let mut msg = pg.message().to_string();
        if let Some(detail) = pg.detail() {
            msg = format!("{msg}. {detail}");
        }
        if let Some(hint) = pg.hint() {
            msg = format!("{msg} (hint: {hint})");
        }
        Some(msg)
    }

    fn database_error_position(&self, err: &dyn sqlx::error::DatabaseError) -> Option<super::ErrorPos> {
        let pg = err.try_downcast_ref::<sqlx::postgres::PgDatabaseError>()?;
        match pg.position() {
            Some(sqlx::postgres::PgErrorPosition::Original(p)) => Some(super::ErrorPos::Char(p.saturating_sub(1))),
            _ => None,
        }
    }

    /// Admin and crash shutdowns (57P01–57P03): said just before the
    /// connection goes.
    fn closes_connection(&self, err: &dyn sqlx::error::DatabaseError) -> bool {
        err.try_downcast_ref::<sqlx::postgres::PgDatabaseError>().is_some()
            && err.code().is_some_and(|c| matches!(c.as_ref(), "57P01" | "57P02" | "57P03"))
    }

    fn agent_constraints(&self) -> &'static crate::db::agent_constraints::AgentConstraints {
        &agent_constraints::AGENT_CONSTRAINTS
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
        }
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
    fn placeholders(&self) -> Placeholders {
        Placeholders { dollar: true, question: false, sqlite: false }
    }
    fn is_system_table(&self, schema: &str, _table: &str) -> bool {
        matches!(schema, "pg_catalog" | "information_schema")
    }
    fn parser(&self) -> Box<dyn sqlparser::dialect::Dialect> {
        Box::new(sqlparser::dialect::PostgreSqlDialect {})
    }
    fn formatter(&self) -> sqlformat::Dialect {
        sqlformat::Dialect::PostgreSql
    }
    /// Postgres sockets are a folder holding `.s.PGSQL.<port>`.
    fn socket_file(&self, path: &str, port: u16) -> String {
        if std::path::Path::new(path).is_dir() {
            format!("{}/.s.PGSQL.{port}", path.trim_end_matches('/'))
        } else {
            path.to_string()
        }
    }
    fn version_query(&self) -> &'static str {
        "SHOW server_version"
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
