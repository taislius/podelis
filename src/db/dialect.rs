//! What differs between engines without talking to a server: names,
//! quoting, lexing, placeholders, EXPLAIN, and what each one supports.
//! Each engine implements [`Dialect`] in its own module; everything outside
//! `db` asks `engine.dialect()` or its [`Capabilities`] instead of matching
//! on the engine.

use super::Engine;

/// An EXPLAIN's output made readable: the plan as an indented tree, and
/// what stands out in it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Plan {
    pub text: String,
    pub notes: Vec<PlanNote>,
}

/// Something worth a look in a plan, e.g. a full scan of a big table.
#[derive(Clone, Debug, PartialEq)]
pub struct PlanNote {
    pub text: String,
    /// The line of `Plan::text` it's about.
    pub line: Option<usize>,
}

/// Rows above which reading a whole table is worth pointing out.
pub const BIG_SCAN_ROWS: f64 = 10_000.0;

/// An estimate this many times off (either way) is worth pointing out.
pub const MISESTIMATE_FACTOR: f64 = 10.0;

/// "1.2k", "3.4M": row counts in notes.
pub fn human_rows(n: f64) -> String {
    match n {
        n if n >= 1e6 => format!("{:.1}M", n / 1e6),
        n if n >= 1e3 => format!("{:.1}k", n / 1e3),
        n => format!("{n:.0}"),
    }
}

/// Whether `estimated` and `actual` rows are far enough apart to matter.
pub fn misestimated(estimated: f64, actual: f64) -> bool {
    let (lo, hi) = if estimated < actual { (estimated, actual) } else { (actual, estimated) };
    hi >= 1000.0 && hi >= lo.max(1.0) * MISESTIMATE_FACTOR
}

/// What an engine can do, for the app to decide what to offer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools)]
pub struct Capabilities {
    /// A local file: no host, port, user, TLS or SSH.
    pub file_based: bool,
    /// A running query can be cancelled (and so time out).
    pub cancel: bool,
    /// Sessions on the server can be listed and stopped.
    pub activity: bool,
    /// Several schemas, shown as `schema.table`.
    pub schemas: bool,
    /// Stored functions and procedures.
    pub routines: bool,
    /// The server says whether a transaction is open.
    pub server_txn_state: bool,
    /// DDL commits an open transaction (MySQL).
    pub ddl_commits: bool,
    /// The catalog keeps each table's CREATE statement (SQLite).
    pub stores_create_sql: bool,
}

/// How the lexer splits this engine's SQL.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools)]
pub struct LexerRules {
    /// `#` starts a comment.
    pub hash_comments: bool,
    /// `--` needs a space after it to start a comment.
    pub dash_comment_needs_space: bool,
    /// Block comments nest.
    pub nested_comments: bool,
    /// Backslash escapes in '…' strings.
    pub backslash_escapes: bool,
    /// "…" is a string, not an identifier.
    pub double_quoted_strings: bool,
    /// `…` quotes identifiers.
    pub backtick_identifiers: bool,
    /// $tag$…$tag$ strings.
    pub dollar_quotes: bool,
}

/// Placeholders besides `:name` (which works everywhere).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Placeholders {
    /// `$1`, `$2`…
    pub dollar: bool,
    /// `?`
    pub question: bool,
}

/// An engine's SQL, apart from talking to the server.
pub trait Dialect: Sync {
    fn label(&self) -> &'static str;
    /// URL schemes that select this engine.
    fn url_schemes(&self) -> &'static [&'static str];
    fn default_port(&self) -> &'static str {
        ""
    }
    fn capabilities(&self) -> Capabilities;
    fn lexer(&self) -> LexerRules;
    fn placeholders(&self) -> Placeholders;
    /// For statement analysis.
    fn parser(&self) -> Box<dyn sqlparser::dialect::Dialect>;
    /// Part of the database's own catalog: listed in the navigator only when
    /// system tables are shown.
    fn is_system_table(&self, schema: &str, table: &str) -> bool;
    /// `||` is logical OR, not string concatenation, under the session's
    /// `sql_mode` (see `sql_mode_query`). `sqlparser` reads it as
    /// concatenation either way, so lint rules ask this.
    fn pipes_are_or(&self, _sql_mode: &str) -> bool {
        false
    }
    /// Reads session settings that change what SQL text means.
    fn sql_mode_query(&self) -> Option<&'static str> {
        None
    }
    /// For "format SQL".
    fn formatter(&self) -> sqlformat::Dialect {
        sqlformat::Dialect::Generic
    }

    /// The character that quotes identifiers.
    fn identifier_quote(&self) -> char {
        '"'
    }

    /// An identifier, quoted unless it's a plain lowercase name.
    fn quote_ident(&self, ident: &str) -> String {
        let plain = ident.chars().next().is_some_and(|c| c.is_ascii_lowercase() || c == '_')
            && ident.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
        if plain {
            return ident.to_string();
        }
        let q = self.identifier_quote();
        format!("{q}{}{q}", ident.replace(q, &format!("{q}{q}")))
    }

    /// A string literal.
    fn string_literal(&self, value: &str) -> String {
        format!("'{}'", value.replace('\'', "''"))
    }

    /// The socket file to connect to for a Unix socket `path` given in the
    /// form (Postgres takes the folder that holds it).
    fn socket_file(&self, path: &str, _port: u16) -> String {
        path.to_string()
    }

    /// A query returning the server's version string.
    fn version_query(&self) -> &'static str;

    /// The EXPLAIN statement for `stmt` (without its `;`).
    fn explain(&self, stmt: &str) -> String;
    /// EXPLAIN ANALYZE (which runs the statement), if the engine has it.
    fn explain_analyze(&self, _stmt: &str) -> Option<String> {
        None
    }
    /// The rows an EXPLAIN returned, as a readable tree with notes.
    fn format_plan(&self, rows: &[Vec<Option<String>>]) -> Option<Plan>;

    /// An INSERT of a row with every column at its default.
    fn insert_defaults(&self, table: &str) -> String {
        format!("INSERT INTO {table} DEFAULT VALUES")
    }
    /// Column types for imported integers and decimals.
    fn integer_type(&self) -> &'static str {
        "BIGINT"
    }
    fn real_type(&self) -> &'static str;

    /// A statement changing a column's type and nullability to these, if
    /// the engine can do that in place (SQLite can't). Names are quoted.
    fn modify_column(&self, _table: &str, _column: &str, _data_type: &str, _nullable: bool) -> Option<String> {
        None
    }
    /// A statement adding a foreign key, if the engine can add one to an
    /// existing table (SQLite can't). Names are quoted.
    fn add_foreign_key(&self, table: &str, column: &str, target_table: &str, target_column: &str) -> Option<String> {
        Some(format!("ALTER TABLE {table} ADD FOREIGN KEY ({column}) REFERENCES {target_table} ({target_column})"))
    }
}

static POSTGRES: super::postgres::Postgres = super::postgres::Postgres;
static MYSQL: super::mysql::MySql = super::mysql::MySql;
static SQLITE: super::sqlite::Sqlite = super::sqlite::Sqlite;

impl Engine {
    pub fn dialect(self) -> &'static dyn Dialect {
        match self {
            Engine::Postgres => &POSTGRES,
            Engine::MySql => &MYSQL,
            Engine::Sqlite => &SQLITE,
        }
    }

    pub fn capabilities(self) -> Capabilities {
        self.dialect().capabilities()
    }

    /// The engine a URL scheme selects.
    pub fn from_scheme(scheme: &str) -> Option<Engine> {
        Engine::ALL.into_iter().find(|e| e.dialect().url_schemes().iter().any(|s| s.eq_ignore_ascii_case(scheme)))
    }
}
