//! PostgreSQL.

mod agent_constraints;
mod plan;

#[cfg(feature = "postgres")]
pub(super) mod cells;
#[cfg(feature = "postgres")]
pub(super) mod connect;
#[cfg(all(test, feature = "postgres"))]
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

use super::dialect::{Capabilities, Dialect, LexerRules, Placeholders, Plan};

pub struct Postgres;

/// A row's value as text: what `db::run` reads results with.
#[cfg(feature = "postgres")]
pub(super) use cells::pg_cell as cell;

/// What `db::Pool` and `db::Session` hold for this engine (see build.rs).
#[cfg(feature = "postgres")]
pub(super) type DriverPool = sqlx::PgPool;
#[cfg(feature = "postgres")]
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

    #[cfg(feature = "postgres")]
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

    #[cfg(feature = "postgres")]
    fn database_error_position(&self, err: &dyn sqlx::error::DatabaseError) -> Option<super::ErrorPos> {
        let pg = err.try_downcast_ref::<sqlx::postgres::PgDatabaseError>()?;
        match pg.position() {
            Some(sqlx::postgres::PgErrorPosition::Original(p)) => Some(super::ErrorPos::Char(p.saturating_sub(1))),
            _ => None,
        }
    }

    /// Admin and crash shutdowns (57P01–57P03): said just before the
    /// connection goes.
    #[cfg(feature = "postgres")]
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
        let d = Engine::Postgres.dialect();
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
        let pg = |plan: &str| Engine::Postgres.dialect().full_scans(&["QUERY PLAN".into()], &rows(plan));
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
