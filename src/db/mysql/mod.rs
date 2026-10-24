//! MySQL (and MariaDB).

mod agent_constraints;
#[cfg(feature = "mysql")]
pub(super) mod cells;
#[cfg(feature = "mysql")]
pub(super) mod connect;
#[cfg(all(test, feature = "mysql"))]
pub(in crate::db) mod db_tests;
#[cfg(feature = "mysql")]
pub(super) mod passwords;
mod plan;
#[cfg(feature = "mysql")]
pub(super) mod queries;
mod versions;

use super::dialect::{Capabilities, Dialect, LexerRules, Placeholders, Plan, hex_digits};
use crate::sql::TokenKind;

pub struct MySql;

/// A row's value as text: what `db::run` reads results with.
#[cfg(feature = "mysql")]
pub(super) use cells::mysql_cell as cell;

/// What `db::Pool` and `db::Session` hold for this engine (see build.rs).
#[cfg(feature = "mysql")]
pub(super) type DriverPool = sqlx::MySqlPool;
#[cfg(feature = "mysql")]
pub(super) type DriverConnection = sqlx::pool::PoolConnection<sqlx::MySql>;

/// This engine's dialect, for `Engine::dialect`.
pub(super) static DIALECT: MySql = MySql;

impl Dialect for MySql {
    fn begin(&self, read_only: bool) -> &'static str {
        if read_only { "START TRANSACTION READ ONLY" } else { "START TRANSACTION" }
    }

    fn tables_without_rollback(&self) -> Option<&'static str> {
        Some(
            "SELECT CAST(GROUP_CONCAT(table_name SEPARATOR ', ') AS CHAR) FROM information_schema.tables \
             WHERE table_schema = DATABASE() AND table_type = 'BASE TABLE' AND engine <> 'InnoDB'",
        )
    }

    fn password_files_note(&self) -> &'static str {
        " (or ~/.my.cnf)"
    }

    fn container_images(&self) -> &'static [&'static str] {
        &["mysql", "mariadb", "percona-server", "mysql-server"]
    }

    /// The official mysql and mariadb images': their own user if one was
    /// made, else root.
    fn container_login(&self, var: &dyn Fn(&str) -> Option<String>) -> Option<crate::db::ContainerLogin> {
        use crate::db::ContainerSecret as Secret;
        let either = |mysql: &'static str, mariadb: &'static str| {
            var(mysql).map(|v| (mysql, v)).or_else(|| var(mariadb).map(|v| (mariadb, v)))
        };
        let database = either("MYSQL_DATABASE", "MARIADB_DATABASE").map(|(_, v)| v).unwrap_or_default();
        if let Some((_, user)) = either("MYSQL_USER", "MARIADB_USER") {
            let password = match either("MYSQL_PASSWORD", "MARIADB_PASSWORD") {
                Some((name, _)) => Some(Secret::Var(name)),
                None => either("MYSQL_PASSWORD_FILE", "MARIADB_PASSWORD_FILE").map(|(_, f)| Secret::File(f)),
            };
            if password.is_some() {
                return Some(crate::db::ContainerLogin { user, database, password });
            }
        }
        let password = match either("MYSQL_ROOT_PASSWORD", "MARIADB_ROOT_PASSWORD") {
            Some((name, _)) => Some(Secret::Var(name)),
            None => either("MYSQL_ROOT_PASSWORD_FILE", "MARIADB_ROOT_PASSWORD_FILE").map(|(_, f)| Secret::File(f)),
        };
        Some(crate::db::ContainerLogin { user: "root".to_string(), database, password })
    }

    /// `float` is a 4-byte float here; `timestamp` a moment in time (stored
    /// in UTC, shown in the session's zone).
    fn type_kind(&self, base: &str) -> Option<crate::db::TypeKind> {
        match base {
            "float" => Some(crate::db::TypeKind::Float4),
            "timestamp" => Some(crate::db::TypeKind::TimestampTz),
            _ => None,
        }
    }

    fn type_name(&self, kind: &crate::db::TypeKind) -> Option<String> {
        use crate::db::TypeKind as Kind;
        use crate::db::types::{decimal, sized};
        Some(match kind {
            Kind::Int(1) => "tinyint".into(),
            Kind::Int(2) => "smallint".into(),
            Kind::Int(3) => "mediumint".into(),
            Kind::Int(4) => "int".into(),
            Kind::Int(_) => "bigint".into(),
            Kind::Bool => "boolean".into(),
            // Unconstrained `numeric` keeps any scale; MySQL's default
            // (10,0) would round to whole numbers.
            Kind::Decimal(ps) => decimal("decimal", *ps).unwrap_or_else(|| "decimal(65,30)".into()),
            Kind::Float4 => "float".into(),
            Kind::Float8 => "double".into(),
            // MySQL's varchar needs a length; text holds as much as any.
            Kind::Varchar(Some(n)) => format!("varchar({n})"),
            Kind::Varchar(None) | Kind::Text => "longtext".into(),
            Kind::Char(n) if n.is_none_or(|n| n <= 255) => sized("char", *n),
            Kind::Char(_) | Kind::Other(_) => return None,
            Kind::Date => "date".into(),
            // Microseconds, as PostgreSQL keeps them.
            Kind::Time => "time(6)".into(),
            Kind::Timestamp => "datetime(6)".into(),
            Kind::TimestampTz => "timestamp(6)".into(),
            Kind::Binary => "longblob".into(),
            Kind::Json => "json".into(),
            Kind::Uuid => "char(36)".into(),
        })
    }

    /// Its syntax errors start with a long boilerplate sentence.
    #[cfg(feature = "mysql")]
    fn database_error_message(&self, err: &dyn sqlx::error::DatabaseError) -> Option<String> {
        let msg = err.try_downcast_ref::<sqlx::mysql::MySqlDatabaseError>()?.message();
        Some(match msg.find("for the right syntax to use ") {
            Some(i) => format!("syntax error {}", &msg[i + "for the right syntax to use ".len()..]),
            None => msg.to_string(),
        })
    }

    /// "… at line N", and "near 'rest of the statement' at line N": the
    /// quoted text is what follows the error, which pins down the column.
    #[cfg(feature = "mysql")]
    fn missing_feature_error(&self, err: &dyn sqlx::error::DatabaseError) -> bool {
        // a syntax error, FUNCTION … does not exist
        err.try_downcast_ref::<sqlx::mysql::MySqlDatabaseError>().is_some_and(|e| matches!(e.number(), 1064 | 1305))
    }
    #[cfg(feature = "mysql")]
    fn database_error_position(&self, err: &dyn sqlx::error::DatabaseError) -> Option<super::ErrorPos> {
        use super::ErrorPos;
        let msg = err.try_downcast_ref::<sqlx::mysql::MySqlDatabaseError>()?.message();
        let at = msg.rfind(" at line ")?;
        let digits: String = msg[at + " at line ".len()..].chars().take_while(char::is_ascii_digit).collect();
        let line = digits.parse().ok()?;
        match msg[..at].find("near '") {
            Some(i) if msg[..at].ends_with('\'') => {
                let near = &msg[i + "near '".len()..at - 1];
                Some(if near.is_empty() { ErrorPos::Line(line) } else { ErrorPos::Near { line, text: near.to_string() } })
            }
            _ => Some(ErrorPos::Line(line)),
        }
    }

    fn agent_constraints(&self) -> &'static crate::db::agent_constraints::AgentConstraints {
        &agent_constraints::AGENT_CONSTRAINTS
    }

    fn label(&self) -> &'static str {
        "MySQL"
    }
    fn url_schemes(&self) -> &'static [&'static str] {
        &["mysql", "mariadb"]
    }
    fn default_port(&self) -> &'static str {
        "3306"
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            file_based: false,
            cancel: true,
            activity: true,
            schemas: false,
            routines: true,
            server_txn_state: false,
            ddl_commits: true,
            stores_create_sql: false,
            table_sizes: true,
            plan_as_grid: true,
            copy_in: false,
        }
    }
    fn lexer(&self) -> LexerRules {
        LexerRules {
            hash_comments: true,
            dash_comment_needs_space: true,
            nested_comments: false,
            backslash_escapes: true,
            double_quoted_strings: true,
            backtick_identifiers: true,
            dollar_quotes: false,
        }
    }
    fn placeholders(&self) -> Placeholders {
        Placeholders { dollar: false, question: true, sqlite: false }
    }
    fn is_system_table(&self, schema: &str, _table: &str) -> bool {
        ["information_schema", "performance_schema", "mysql", "sys"].iter().any(|s| s.eq_ignore_ascii_case(schema))
    }
    fn parser(&self) -> Box<dyn sqlparser::dialect::Dialect> {
        Box::new(sqlparser::dialect::MySqlDialect {})
    }
    /// Unless the session's `sql_mode` has `PIPES_AS_CONCAT` (`ANSI` lists it
    /// too).
    fn pipes_are_or(&self, sql_mode: &str) -> bool {
        !sql_mode.split(',').any(|mode| mode.trim().eq_ignore_ascii_case("PIPES_AS_CONCAT"))
    }
    fn sql_mode_query(&self) -> Option<&'static str> {
        Some("SELECT @@SESSION.sql_mode")
    }
    fn identifier_quote(&self) -> char {
        '`'
    }
    fn string_literal(&self, value: &str) -> String {
        format!("'{}'", value.replace('\\', "\\\\").replace('\'', "''"))
    }
    /// `0xDEADBEEF`, as the `mysql` client shows it.
    fn hex(&self, bytes: &[u8]) -> String {
        format!("0x{}", hex_digits(bytes, true))
    }
    fn is_binary_type(&self, ty: &str) -> bool {
        ["BINARY", "VARBINARY", "TINYBLOB", "BLOB", "MEDIUMBLOB", "LONGBLOB", "GEOMETRY"]
            .iter()
            .any(|t| ty.eq_ignore_ascii_case(t))
    }
    fn binary_literal(&self, shown: &str) -> Option<String> {
        let digits = shown.strip_prefix("0x")?;
        if digits.is_empty() {
            return Some("X''".into());
        }
        digits.chars().all(|c| c.is_ascii_hexdigit()).then(|| shown.to_string())
    }
    fn version_query(&self) -> &'static str {
        "SELECT VERSION()"
    }
    /// FORMAT=TREE where the server has it (MySQL 8.0.16+); older MySQL
    /// and MariaDB get the classic table.
    fn explain(&self, stmt: &str, version: &str) -> String {
        match Server::of(version) {
            Server::MySql(v) if v < (8, 0, 16) => format!("EXPLAIN {stmt}"),
            Server::MariaDb => format!("EXPLAIN {stmt}"),
            Server::MySql(_) => format!("EXPLAIN FORMAT=TREE {stmt}"),
        }
    }
    fn too_new(&self, sql: &str, version: &str) -> Option<crate::db::dialect::TooNew> {
        versions::too_new(sql, version)
    }
    /// MySQL 8.0.18+ has EXPLAIN ANALYZE, for the statements it can run
    /// that way; MariaDB's is ANALYZE (a table with the actual rows next to
    /// the estimates).
    fn explain_analyze(&self, stmt: &str, version: &str) -> Option<String> {
        match Server::of(version) {
            Server::MySql(v) if v < (8, 0, 18) => None,
            Server::MariaDb => Some(format!("ANALYZE {stmt}")),
            Server::MySql(_) => runs_under_explain_analyze(stmt).then(|| format!("EXPLAIN ANALYZE {stmt}")),
        }
    }
    /// FORMAT=TREE is already a readable tree; the classic table is lined
    /// up. Notes are added to both.
    fn format_plan(&self, columns: &[String], rows: &[Vec<Option<String>>]) -> Option<Plan> {
        if columns.len() > 1 {
            return (!rows.is_empty()).then(|| plan::analyze_table(columns, rows));
        }
        Some(plan::analyze(rows.first()?.first()?.clone()?.trim_end().to_string()))
    }
    fn full_scans(&self, columns: &[String], rows: &[Vec<Option<String>>]) -> Vec<crate::db::dialect::Scan> {
        if columns.len() > 1 {
            return plan::table_full_scans(columns, rows);
        }
        rows.first().and_then(|r| r.first()).and_then(|c| c.as_deref()).map(plan::full_scans).unwrap_or_default()
    }
    fn insert_defaults(&self, table: &str) -> String {
        format!("INSERT INTO {table} () VALUES ()")
    }
    fn real_type(&self) -> &'static str {
        "DOUBLE"
    }
    /// With microseconds: a plain `DATETIME` would round fractions away.
    fn timestamp_type(&self) -> &'static str {
        "DATETIME(6)"
    }
    fn modify_column(&self, table: &str, column: &str, data_type: &str, nullable: bool) -> Option<String> {
        let null = if nullable { "NULL" } else { "NOT NULL" };
        Some(format!("ALTER TABLE {table} MODIFY COLUMN {column} {data_type} {null}"))
    }
}

/// The kind of server, from its version string (`8.0.36`, `5.7.44-log`,
/// `10.11.6-MariaDB-0+deb12u1`).
#[derive(Debug, PartialEq)]
enum Server {
    MySql((u32, u32, u32)),
    MariaDb,
}

impl Server {
    /// A version that can't be read is taken to be a current MySQL.
    fn of(version: &str) -> Self {
        if version.to_ascii_lowercase().contains("mariadb") {
            return Server::MariaDb;
        }
        let mut parts = version.split(|c: char| !c.is_ascii_digit()).map(|p| p.parse::<u32>().ok());
        match (parts.next().flatten(), parts.next().flatten(), parts.next().flatten()) {
            (Some(major), Some(minor), patch) => Server::MySql((major, minor, patch.unwrap_or(0))),
            _ => Server::MySql((u32::MAX, 0, 0)),
        }
    }
}

/// Whether MySQL's EXPLAIN ANALYZE runs `stmt`: reads, and UPDATE and
/// DELETE of several tables (a JOIN, a comma, `DELETE t FROM`, `USING`).
/// Of one table, and INSERT or REPLACE, it shows the plan without running
/// them ("not executable by iterator executor").
fn runs_under_explain_analyze(stmt: &str) -> bool {
    let tokens: Vec<(TokenKind, String)> = crate::sql::tokenize(stmt, crate::db::Engine::MySql)
        .into_iter()
        .filter(|t| !matches!(t.kind, TokenKind::Whitespace | TokenKind::Comment))
        .map(|t| (t.kind, stmt[t.range].to_ascii_lowercase()))
        .collect();
    let word = |i: usize| tokens.get(i).filter(|(k, _)| *k == TokenKind::Word).map(|(_, w)| w.as_str());
    match word(0) {
        Some("insert" | "replace") => false,
        Some("update") => tokens[1..]
            .iter()
            .take_while(|(k, w)| !(*k == TokenKind::Word && w == "set"))
            .any(|(k, w)| (*k == TokenKind::Word && w == "join") || (*k == TokenKind::Punct && w == ",")),
        Some("delete") => word(1) != Some("from") || tokens.iter().any(|(k, w)| *k == TokenKind::Word && w == "using"),
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use crate::db::Engine;
    use crate::db::dialect::Scan;

    #[test]
    fn quote_ident() {
        assert_eq!(Engine::MySql.quote_ident("order items"), "`order items`");
        assert_eq!(Engine::MySql.quote_ident("1abc"), "`1abc`");
    }

    #[test]
    fn explain_statements() {
        assert_eq!(Engine::MySql.dialect().explain("select 1", ""), "EXPLAIN FORMAT=TREE select 1");
    }

    #[test]
    fn binary_literals_only_for_binary_columns_in_hex() {
        use crate::db::value_literal;
        assert_eq!(value_literal("0xDEADBEEF", "BLOB", Engine::MySql), "0xDEADBEEF");
        assert_eq!(value_literal("0x", "VARBINARY", Engine::MySql), "X''");
        assert_eq!(value_literal("0xDEADBEEF", "TEXT", Engine::MySql), "'0xDEADBEEF'", "text that looks like hex");
        assert_eq!(value_literal("hello", "BLOB", Engine::MySql), "'hello'", "typed in by hand");
    }

    #[test]
    fn full_scans_in_plans() {
        let scan = |table: &str, rows| Scan::new(table, rows);
        let rows = |text: &str| vec![vec![Some(text.to_string())]];
        let my = |plan: &str| Engine::MySql.dialect().full_scans(&["EXPLAIN".into()], &rows(plan));
        let tree = "-> Filter: (t.x = 1)  (cost=10.5 rows=2)\n    -> Table scan on t  (cost=10.5 rows=5200)";
        assert_eq!(my(tree), [scan("t", Some(5200.0))]);
        assert!(my("-> Limit: 5 row(s)  (cost=1 rows=5)\n    -> Table scan on t  (cost=1 rows=5200)").is_empty());
        let limited_filter = "-> Limit: 5 row(s)\n    -> Filter: (t.x = 1)\n        -> Table scan on t  (cost=1 rows=5200)";
        assert_eq!(my(limited_filter), [scan("t", Some(5200.0))]);
        let limited_sort = "-> Limit: 5 row(s)\n    -> Sort: t.x, limit input to 5 row(s) per chunk\n        -> Table scan on t  (cost=1 rows=5200)";
        assert_eq!(my(limited_sort), [scan("t", Some(5200.0))]);
        let cols: Vec<String> = ["id", "select_type", "table", "type", "rows"].map(String::from).to_vec();
        let classic =
            vec![vec![Some("1".into()), Some("SIMPLE".into()), Some("t".into()), Some("ALL".into()), Some("99".into())]];
        assert_eq!(Engine::MySql.dialect().full_scans(&cols, &classic), [scan("t", Some(99.0))]);
    }

    #[test]
    fn explain_analyze_only_what_it_runs() {
        let runs = |sql| super::runs_under_explain_analyze(sql);
        for sql in [
            "select * from a",
            "with c as (select 1) select * from c",
            "table a",
            "update a join b on a.id = b.id set a.x = 3",
            "update a, b set a.x = 4 where a.id = b.id",
            "delete a from a join b on a.id = b.id",
            "delete from a using a join b on a.id = b.id",
        ] {
            assert!(runs(sql), "{sql}");
        }
        for sql in ["update a set x = 2 where id = 1", "delete from a where id = 9", "insert into a values (1)", "replace into a values (1)"] {
            assert!(!runs(sql), "{sql}");
        }
        // A comma after SET is the next column, not another table.
        assert!(!runs("update a set x = 1, y = 2"));
    }
    use super::*;

    #[test]
    fn mysql_tree_is_passed_through() {
        let rows = vec![vec![Some("-> Table scan on t  (cost=0.35 rows=1)\n".to_string())]];
        let columns = ["EXPLAIN".to_string()];
        assert_eq!(MySql.format_plan(&columns, &rows).unwrap().text, "-> Table scan on t  (cost=0.35 rows=1)");
    }

    #[test]
    fn older_servers_get_the_classic_explain() {
        let explain = |version| MySql.explain("SELECT 1", version);
        assert_eq!(explain("8.0.36"), "EXPLAIN FORMAT=TREE SELECT 1");
        assert_eq!(explain("8.0.16"), "EXPLAIN FORMAT=TREE SELECT 1");
        assert_eq!(explain("9.1.0"), "EXPLAIN FORMAT=TREE SELECT 1");
        assert_eq!(explain(""), "EXPLAIN FORMAT=TREE SELECT 1");
        assert_eq!(explain("8.0.15"), "EXPLAIN SELECT 1");
        assert_eq!(explain("5.7.44-log"), "EXPLAIN SELECT 1");
        assert_eq!(explain("10.11.6-MariaDB-0+deb12u1"), "EXPLAIN SELECT 1");

        let analyze = |version| MySql.explain_analyze("SELECT 1", version);
        assert_eq!(analyze("8.0.18").as_deref(), Some("EXPLAIN ANALYZE SELECT 1"));
        assert_eq!(analyze("8.0.17"), None);
        assert_eq!(analyze("5.6.51"), None);
        assert_eq!(analyze("11.4.2-MariaDB").as_deref(), Some("ANALYZE SELECT 1"));
    }
}
