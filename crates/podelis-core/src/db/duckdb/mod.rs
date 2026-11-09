//! DuckDB: an analytical database in a file (or in memory), its SQL close
//! to PostgreSQL's. Its driver is the `duckdb` crate, not sqlx: sync, so
//! each call runs on a blocking thread (`driver.rs`).

mod agent_constraints;
#[cfg(feature = "duckdb")]
pub(super) mod cells;
#[cfg(feature = "duckdb")]
pub(super) mod connect;
#[cfg(all(any(test, feature = "test-support"), feature = "duckdb"))]
pub(in crate::db) mod db_tests;
#[cfg(feature = "duckdb")]
pub(super) mod driver;
#[cfg(feature = "duckdb")]
pub(super) mod passwords;
#[cfg(feature = "duckdb")]
pub(super) mod queries;

use super::backend::{Backend, Capabilities};
use super::dialect::{LexerRules, Placeholders, Plan, SqlDialect};

pub struct DuckDb;

/// What `db::Pool` and `db::Session` hold for this engine (see build.rs).
#[cfg(feature = "duckdb")]
pub(super) type DriverPool = driver::DuckPool;
#[cfg(feature = "duckdb")]
pub(super) type DriverConnection = driver::DuckConnection;

/// Its SQL dialect, for its own modules (`Engine::dialect` is an `Option`
/// for the rest).
#[cfg(any(test, feature = "duckdb"))]
fn sql() -> &'static dyn SqlDialect {
    &DIALECT
}

/// This engine's dialect, for `Engine::dialect`.
pub(super) static DIALECT: DuckDb = DuckDb;

/// Words a name can't be without quotes: DuckDB's reserved keywords and
/// its type-or-function ones (`duckdb_keywords()`, categories `reserved`
/// and `type_function`), sorted.
const RESERVED: &[&str] = &[
    "ALL", "ANALYSE", "ANALYZE", "AND", "ANTI", "ANY", "ARRAY", "AS", "ASC", "ASOF", "ASYMMETRIC", "AT",
    "AUTHORIZATION", "BINARY", "BOTH", "BY", "CASE", "CAST", "CHECK", "COLLATE", "COLLATION", "COLUMN",
    "COLUMNS", "CONCURRENTLY", "CONSTRAINT", "CREATE", "CROSS", "DEFAULT", "DEFERRABLE", "DESC", "DESCRIBE",
    "DISTINCT", "DO", "ELSE", "END", "EXCEPT", "FALSE", "FETCH", "FOR", "FOREIGN", "FREEZE", "FROM", "FULL",
    "GENERATED", "GLOB", "GROUP", "HAVING", "ILIKE", "IN", "INITIALLY", "INNER", "INTERSECT", "INTO", "IS",
    "ISNULL", "JOIN", "LAMBDA", "LATERAL", "LEADING", "LEFT", "LIKE", "LIMIT", "MAP", "NATURAL", "NOT",
    "NOTNULL", "NULL", "OFFSET", "ON", "ONLY", "OR", "ORDER", "OUTER", "OVERLAPS", "PIVOT", "PIVOT_LONGER",
    "PIVOT_WIDER", "PLACING", "POSITIONAL", "PRIMARY", "QUALIFY", "REFERENCES", "RETURNING", "RIGHT",
    "SELECT", "SEMI", "SHOW", "SIMILAR", "SOME", "STRUCT", "SUMMARIZE", "SYMMETRIC", "TABLE", "TABLESAMPLE",
    "THEN", "TO", "TRAILING", "TRUE", "TRY_CAST", "UNION", "UNIQUE", "UNPACK", "UNPIVOT", "USING", "VARIADIC",
    "VERBOSE", "WHEN", "WHERE", "WINDOW", "WITH",
];

impl Backend for DuckDb {
    /// Its own names beside the standard's (`DESCRIBE` writes them in
    /// capitals; `db::types` lowercases them first).
    fn type_kind(&self, ty: &str) -> Option<crate::db::TypeKind> {
        use crate::db::TypeKind as Kind;
        use crate::db::types::base;
        if ty.ends_with("[]") || ty.ends_with(']') {
            return Some(Kind::Other(ty.to_string()));
        }
        Some(match base(ty) {
            "tinyint" | "int1" => Kind::Int(1),
            "int2" | "short" => Kind::Int(2),
            "int4" | "signed" => Kind::Int(4),
            "int8" | "long" => Kind::Int(8),
            "bool" | "logical" => Kind::Bool,
            "float" | "float4" => Kind::Float4,
            "double" | "float8" => Kind::Float8,
            "string" | "bpchar" => Kind::Text,
            "timestamptz" => Kind::TimestampTz,
            "bytea" | "varbinary" => Kind::Binary,
            "uuid" => Kind::Uuid,
            // Wider or unsigned integers hold what nothing else does.
            "hugeint" | "uhugeint" | "utinyint" | "usmallint" | "uinteger" | "ubigint" => {
                Kind::Other(ty.to_string())
            }
            _ => return None,
        })
    }
    /// "v1.4.1".
    fn version_query(&self) -> &'static str {
        "SELECT version()"
    }
    fn is_system_table(&self, schema: &str, _table: &str) -> bool {
        matches!(schema, "information_schema" | "pg_catalog")
    }
    fn agent_constraints(&self) -> &'static crate::db::agent_constraints::AgentConstraints {
        &agent_constraints::AGENT_CONSTRAINTS
    }
    fn language(&self) -> crate::language::Language {
        crate::language::Language::Sql(&DIALECT)
    }
    fn label(&self) -> &'static str {
        "DuckDB"
    }
    fn url_schemes(&self) -> &'static [&'static str] {
        &["duckdb"]
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            file_based: true,
            cancel: true,
            activity: false,
            schemas: true,
            routines: false,
            server_txn_state: false,
            ddl_commits: false,
            stores_create_sql: true,
            table_sizes: false,
            plan_as_grid: false,
            copy_in: false,
            transactions: crate::db::Transactions::Scripts,
        }
    }
    fn connect_hint(&self, message: &str) -> Option<&'static str> {
        // One process may write a database file at a time.
        message.contains("could not set lock on file").then_some(
            "another program has this database file open (DuckDB lets one write it): close that, or connect read-only",
        )
    }
}

impl SqlDialect for DuckDb {
    fn lexer(&self) -> LexerRules {
        LexerRules {
            hash_comments: false,
            dash_comment_needs_space: false,
            nested_comments: false,
            backslash_escapes: false,
            double_quoted_strings: false,
            backtick_identifiers: false,
            dollar_quotes: true,
        }
    }
    fn placeholders(&self) -> Placeholders {
        Placeholders { dollar_number: true, question: true, dollar_name: true, ..Placeholders::default() }
    }
    fn keywords(&self) -> &'static [&'static str] {
        &[
            "ATTACH", "DESCRIBE", "DETACH", "EXPORT", "ILIKE", "IMPORT", "INSTALL", "LOAD", "MACRO", "PIVOT", "PRAGMA",
            "QUALIFY", "SUMMARIZE", "UNPIVOT",
        ]
    }
    /// Quoted unless plain, as anywhere, and also when it's a word DuckDB
    /// keeps (`at`, `order`).
    fn quote_ident(&self, ident: &str) -> String {
        let plain = ident.chars().next().is_some_and(|c| c.is_ascii_lowercase() || c == '_')
            && ident.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
        let kept = RESERVED.binary_search(&ident.to_ascii_uppercase().as_str()).is_ok();
        if plain && !kept { ident.to_string() } else { format!("\"{}\"", ident.replace('"', "\"\"")) }
    }
    fn parser(&self) -> Box<dyn sqlparser::dialect::Dialect> {
        Box::new(sqlparser::dialect::DuckDbDialect {})
    }
    fn formatter(&self) -> sqlformat::Dialect {
        sqlformat::Dialect::PostgreSql
    }
    fn explain(&self, stmt: &str, _version: &str) -> String {
        format!("EXPLAIN {stmt}")
    }
    fn explain_analyze(&self, stmt: &str, _version: &str) -> Option<String> {
        Some(format!("EXPLAIN ANALYZE {stmt}"))
    }
    /// Its EXPLAIN is one row (`explain_key`, `explain_value`): the plan
    /// drawn already.
    fn format_plan(&self, _columns: &[String], rows: &[Vec<Option<String>>]) -> Option<Plan> {
        let text: Vec<&str> = rows.iter().filter_map(|r| r.last()?.as_deref()).collect();
        (!text.is_empty()).then(|| Plan { text: text.join("\n").trim_end().to_string(), notes: Vec::new() })
    }
    /// Each scan box of its drawn plan that says `Type: Sequential Scan`,
    /// with its table. Its `~2 rows` are what comes out after the filters,
    /// not what's read: the catalog says how big the table is.
    fn full_scans(&self, _columns: &[String], rows: &[Vec<Option<String>>]) -> Vec<crate::db::dialect::Scan> {
        let text: Vec<&str> = rows.iter().filter_map(|r| r.last()?.as_deref()).collect();
        full_scans_in(&text.join("\n"))
    }
    fn type_name(&self, kind: &crate::db::TypeKind) -> Option<String> {
        use crate::db::TypeKind as Kind;
        use crate::db::types::{decimal, sized};
        Some(match kind {
            Kind::Int(1) => "TINYINT".into(),
            Kind::Int(2) => "SMALLINT".into(),
            Kind::Int(3 | 4) => "INTEGER".into(),
            Kind::Int(_) => "BIGINT".into(),
            Kind::Bool => "BOOLEAN".into(),
            // Unconstrained `numeric` keeps any scale; DuckDB's default
            // (18,3) would round it.
            Kind::Decimal(ps) => decimal("DECIMAL", *ps).unwrap_or_else(|| "DECIMAL(38,10)".into()),
            Kind::Float4 => "FLOAT".into(),
            Kind::Float8 => "DOUBLE".into(),
            Kind::Varchar(n) => sized("VARCHAR", *n),
            // No fixed-length strings: VARCHAR keeps them.
            Kind::Char(_) | Kind::Text => "VARCHAR".into(),
            Kind::Date => "DATE".into(),
            Kind::Time => "TIME".into(),
            Kind::Timestamp => "TIMESTAMP".into(),
            Kind::TimestampTz => "TIMESTAMPTZ".into(),
            Kind::Binary => "BLOB".into(),
            Kind::Json => "JSON".into(),
            Kind::Uuid => "UUID".into(),
            Kind::Other(_) => return None,
        })
    }
    /// `VARCHAR(n)` is `VARCHAR`, and `CHAR(n)` too.
    fn keeps_string_lengths(&self) -> bool {
        false
    }
    /// Shown as hex (`00ff275c`, as Arrow writes a blob), which goes back
    /// as `unhex('00ff275c')`.
    fn is_binary_type(&self, ty: &str) -> bool {
        ty.eq_ignore_ascii_case("BLOB")
    }
    fn binary_literal(&self, shown: &str) -> Option<String> {
        (shown.len().is_multiple_of(2) && shown.chars().all(|c| c.is_ascii_hexdigit())).then(|| format!("unhex('{shown}')"))
    }
    fn integer_type(&self) -> &'static str {
        "BIGINT"
    }
    fn real_type(&self) -> &'static str {
        "DOUBLE"
    }
    fn timestamptz_type(&self) -> &'static str {
        "TIMESTAMPTZ"
    }
}

/// A box of a drawn plan, as it's read: its table, and whether it reads
/// all of it.
#[derive(Default)]
struct PlanBox {
    table: Option<String>,
    /// Its pushed-down filters (`id=5`), each as drawn.
    filters: Vec<String>,
    /// `Filters:` came alone: they're on the lines after it.
    filters_next: bool,
    /// `Table:` came alone: the name is on the lines after it, up to a
    /// blank one (a long one wraps).
    table_next: bool,
    sequential: bool,
}

/// The full scans in a plan DuckDB drew.
fn full_scans_in(plan: &str) -> Vec<crate::db::dialect::Scan> {
    scans_with_filters(plan).into_iter().map(|(scan, _)| scan).collect()
}

/// The sequential scans in a plan DuckDB drew, each with its pushed-down
/// filters: boxes side by side are told apart by the column their left
/// edge is at.
pub(super) fn scans_with_filters(plan: &str) -> Vec<(crate::db::dialect::Scan, Vec<String>)> {
    use std::collections::HashMap;
    let mut boxes: HashMap<usize, PlanBox> = HashMap::new();
    let mut scans = Vec::new();
    for line in plan.lines() {
        let chars: Vec<char> = line.chars().collect();
        let mut i = 0;
        while i < chars.len() {
            match chars[i] {
                '┌' => {
                    boxes.insert(i, PlanBox::default());
                }
                '└' => {
                    if let Some(PlanBox { table: Some(table), sequential: true, filters, .. }) = boxes.remove(&i) {
                        // `database.schema.table`: the last two.
                        let mut parts = table.rsplitn(3, '.');
                        let name = parts.next().unwrap_or_default();
                        let mut scan = crate::db::dialect::Scan::new(name, None);
                        scan.schema = parts.next().map(str::to_string);
                        scans.push((scan, filters));
                    }
                }
                '│' => {
                    let end = chars[i + 1..].iter().position(|&c| c == '│').map_or(chars.len(), |p| i + 1 + p);
                    let cell: String = chars[i + 1..end].iter().collect();
                    let cell = cell.trim();
                    if let Some(open) = boxes.get_mut(&i) {
                        if open.filters_next {
                            if cell.is_empty() {
                                open.filters_next = false;
                            } else {
                                open.filters.push(cell.to_string());
                            }
                        } else if let Some(filter) = cell.strip_prefix("Filters:") {
                            let filter = filter.trim();
                            open.filters_next = filter.is_empty();
                            if !filter.is_empty() {
                                open.filters.push(filter.to_string());
                            }
                        } else if open.table_next {
                            if cell.is_empty() {
                                open.table_next = false;
                            } else {
                                open.table.get_or_insert_default().push_str(cell);
                            }
                        } else if let Some(table) = cell.strip_prefix("Table:") {
                            let table = table.trim();
                            open.table_next = table.is_empty();
                            open.table = (!table.is_empty()).then(|| table.to_string());
                        } else if cell == "Type: Sequential Scan" {
                            open.sequential = true;
                        }
                    }
                    i = end;
                    continue;
                }
                _ => {}
            }
            i += 1;
        }
    }
    scans
}

#[cfg(test)]
mod tests {
    use crate::db::Engine;

    #[test]
    fn full_scans_in_its_drawn_plans() {
        let plan = "\
┌─────────────┴─────────────┐┌─────────────┴─────────────┐
│          SEQ_SCAN         ││         INDEX_SCAN        │
│    ────────────────────   ││    ────────────────────   │
│      Table: p.main.ea     ││      Table: p.main.eb     │
│   Type: Sequential Scan   ││      Type: Index Scan     │
│        Filters: x=2       ││                           │
│           ~1 row          ││          ~5 rows          │
└───────────────────────────┘└───────────────────────────┘";
        let scans = super::full_scans_in(plan);
        assert_eq!(scans.len(), 1);
        assert_eq!((scans[0].table.as_str(), scans[0].schema.as_deref(), scans[0].rows), ("ea", Some("main"), None));
        // A long name wraps onto the next line.
        let wrapped = r#"┌─────────────┴─────────────┐
│          SEQ_SCAN         │
│           Table:          │
│  "podelis_agents-duckdb"  │
│       .main.agent_t       │
│                           │
│   Type: Sequential Scan   │
└───────────────────────────┘"#;
        let scans = super::full_scans_in(wrapped);
        assert_eq!(scans.iter().map(|s| s.table.as_str()).collect::<Vec<_>>(), ["agent_t"]);
        let filtered = super::scans_with_filters(plan);
        assert_eq!(filtered[0].1, ["x=2"]);
    }

    #[test]
    fn its_kept_words_are_quoted() {
        let d = super::sql();
        assert_eq!((d.quote_ident("at"), d.quote_ident("x"), d.quote_ident("Name")), ("\"at\"".into(), "x".into(), "\"Name\"".into()));
        assert!(super::RESERVED.windows(2).all(|w| w[0] < w[1]), "sorted, for the binary search");
    }

    #[test]
    fn its_plan_is_drawn_already() {
        let d = super::sql();
        assert_eq!(d.explain("SELECT 1", ""), "EXPLAIN SELECT 1");
        let rows = vec![vec![Some("physical_plan".into()), Some("┌───┐\n│ X │\n└───┘\n".into())]];
        assert_eq!(d.format_plan(&[], &rows).unwrap().text, "┌───┐\n│ X │\n└───┘");
        assert!(d.format_plan(&[], &[]).is_none());
        assert_eq!(crate::db::value_literal("00ff", "BLOB", Engine::DuckDb), "unhex('00ff')");
        assert_eq!(crate::db::value_literal("0ff", "BLOB", Engine::DuckDb), "'0ff'", "odd digits: text");
    }

    #[test]
    fn its_type_names() {
        use crate::db::{same_type, translate_type};
        assert!(same_type(Engine::DuckDb, "INTEGER", Engine::Postgres, "int4"));
        assert!(same_type(Engine::DuckDb, "DOUBLE", Engine::Postgres, "float8"));
        assert!(same_type(Engine::DuckDb, "TIMESTAMP WITH TIME ZONE", Engine::Postgres, "timestamptz"));
        assert_eq!(translate_type(Engine::Postgres, "text", Engine::DuckDb).as_deref(), Some("VARCHAR"));
        assert_eq!(translate_type(Engine::DuckDb, "HUGEINT", Engine::Postgres), None);
        assert_eq!(translate_type(Engine::DuckDb, "INTEGER[]", Engine::MySql), None);
        // It keeps no string lengths: any string is its VARCHAR.
        assert!(same_type(Engine::Postgres, "varchar(120)", Engine::DuckDb, "VARCHAR"));
        assert!(same_type(Engine::MySql, "char(3)", Engine::DuckDb, "VARCHAR"));
        assert!(!same_type(Engine::Postgres, "varchar(120)", Engine::MySql, "varchar(40)"), "elsewhere they count");
    }

    #[test]
    fn a_locked_file_is_explained() {
        let hint = crate::db::Engine::DuckDb.backend().connect_hint("io error: could not set lock on file \"x.duckdb\"");
        assert!(hint.unwrap().contains("read-only"));
    }
}
