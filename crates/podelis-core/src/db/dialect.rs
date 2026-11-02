//! How engines' SQL differs, without talking to a server: quoting, lexing,
//! placeholders, EXPLAIN, statements, types. Each SQL engine implements
//! [`SqlDialect`] in its own module, and everything outside `db` asks
//! `engine.dialect()` instead of matching on the engine. What isn't about
//! SQL (names, ports, containers, what's offered) is `backend.rs`'s, so a
//! database without SQL needs only that.

use super::Engine;
use super::backend::Backend;

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
/// `bytes` as hex digits, two per byte.
pub fn hex_digits(bytes: &[u8], upper: bool) -> String {
    use std::fmt::Write;
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = if upper { write!(out, "{b:02X}") } else { write!(out, "{b:02x}") };
    }
    out
}

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

/// How the lexer splits this engine's SQL.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[expect(clippy::struct_excessive_bools, reason = "independent lexer switches")]
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

/// The parameter forms an engine's SQL takes, each declared by its dialect,
/// besides `:name` (Podėlis fills that in itself, so it works everywhere).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Placeholders {
    /// `$1`, `$2`…
    pub dollar_number: bool,
    /// `?`, numbered in order.
    pub question: bool,
    /// `?3`: a `?` with its number.
    pub question_number: bool,
    /// `@name`
    pub at_name: bool,
    /// `$name`
    pub dollar_name: bool,
}

/// How a transaction ends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TxEnd {
    Commit,
    Rollback,
}

/// An engine's SQL, apart from talking to the server: how it's lexed,
/// parsed, quoted and planned, its statements for transactions, rows and
/// columns, its types and its errors. What any database has, SQL or not,
/// is its [`Backend`]'s.
pub trait SqlDialect: Backend {
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

    /// How a binary value is shown, in the engine's own notation:
    /// PostgreSQL's `\xdeadbeef`, which is also what its server sends.
    fn hex(&self, bytes: &[u8]) -> String {
        format!("\\x{}", hex_digits(bytes, false))
    }
    /// Whether a result column of type `ty` (as the driver names it) holds
    /// binary data.
    fn is_binary_type(&self, ty: &str) -> bool {
        ty.eq_ignore_ascii_case("BYTEA")
    }
    /// A binary value as `hex` shows it, as a literal, when a string
    /// literal won't do (PostgreSQL reads `'\x…'` as `bytea`).
    fn binary_literal(&self, _shown: &str) -> Option<String> {
        None
    }

    /// A query returning the server's version string.
    fn version_query(&self) -> &'static str;
    /// The statement that starts a transaction, read-only if `read_only`
    /// where the engine can.
    fn begin(&self, _read_only: bool) -> &'static str {
        "BEGIN"
    }
    /// The statement that ends a transaction, `how`.
    fn end(&self, how: TxEnd) -> &'static str {
        match how {
            TxEnd::Commit => "COMMIT",
            TxEnd::Rollback => "ROLLBACK",
        }
    }
    /// A savepoint `name`, within a transaction.
    fn savepoint(&self, name: &str) -> String {
        format!("SAVEPOINT {name}")
    }
    /// Back to savepoint `name`, undoing what ran since.
    fn rollback_to_savepoint(&self, name: &str) -> String {
        format!("ROLLBACK TO SAVEPOINT {name}")
    }
    /// The engine's own keywords besides SQL's (`sql::KEYWORDS`), for
    /// highlighting and completion: words its SQL has and not every
    /// engine's. Upper case, sorted.
    fn keywords(&self) -> &'static [&'static str] {
        &[]
    }
    /// The cheapest query that shows the connection answers.
    fn ping(&self) -> &'static str {
        "SELECT 1"
    }
    /// What a session runs before a sandbox's transaction and after it ends,
    /// where the engine needs it (MySQL: autocommit off, so a statement
    /// that ends the transaction leaves the next in a new one, not
    /// committing as it goes); `None` elsewhere.
    fn sandbox_session(&self) -> Option<(&'static str, &'static str)> {
        None
    }
    /// The statement that copies a file database to `path` (a sandbox works
    /// on the copy); `None` where the engine has none.
    fn copy_into(&self, _path: &str) -> Option<String> {
        None
    }
    /// A query listing the tables a rollback doesn't undo (MySQL's
    /// non-InnoDB ones), as one text value; `None` where every table rolls
    /// back.
    fn tables_without_rollback(&self) -> Option<&'static str> {
        None
    }
    /// A query counting the locks this session holds (for a sandbox's
    /// window), as one number; `None` where there's no such view.
    fn locks_held_query(&self) -> Option<&'static str> {
        None
    }
    /// Columns every table has without listing them (`ctid`, `rowid`).
    fn hidden_columns(&self) -> &'static [&'static str] {
        &[]
    }
    /// What type `ty` (as the catalog reports it, lowercase) means here:
    /// this engine's own names, and the standard's it reads otherwise;
    /// `None` leaves it to the standard's meaning (`db::types`).
    fn type_kind(&self, _ty: &str) -> Option<super::TypeKind> {
        None
    }
    /// How this engine writes a type of `kind`; `None` if it has nothing
    /// like it.
    fn type_name(&self, kind: &super::TypeKind) -> Option<String>;
    /// Whether it keeps only a type's affinity: beside it any
    /// integer is any other, and lengths don't count.
    fn keeps_only_affinity(&self) -> bool {
        false
    }
    /// A position only the message's text gives (SQLite's `near "x"`), for
    /// the background syntax check.
    fn position_in_message(&self, _message: &str) -> Option<super::ErrorPos> {
        None
    }
    /// What AI agents may send (`db::agent_constraints`).
    fn agent_constraints(&self) -> &'static super::agent_constraints::AgentConstraints;

    /// Something in `sql` that this server's `version` (as `version_query`
    /// returned it) is too old for: what to say when it fails, so a syntax
    /// error that is really a missing feature says so.
    fn too_new(&self, _sql: &str, _version: &str) -> Option<TooNew> {
        None
    }

    /// The EXPLAIN statement for `stmt` (without its `;`), for a server
    /// of `version` (as `version_query` returned it).
    fn explain(&self, stmt: &str, version: &str) -> String;
    /// EXPLAIN ANALYZE (which runs the statement), if the server has it.
    fn explain_analyze(&self, _stmt: &str, _version: &str) -> Option<String> {
        None
    }
    /// The result an EXPLAIN returned, as a readable tree with notes.
    fn format_plan(&self, columns: &[String], rows: &[Vec<Option<String>>]) -> Option<Plan>;
    /// The tables a plan (as `explain` gives it) reads in full, with the
    /// rows it expects to read where it says. A scan a `LIMIT` stops early
    /// (nothing in between that needs every row) doesn't count.
    fn full_scans(&self, _columns: &[String], _rows: &[Vec<Option<String>>]) -> Vec<Scan> {
        Vec::new()
    }

    /// An INSERT of a row with every column at its default.
    fn insert_defaults(&self, table: &str) -> String {
        format!("INSERT INTO {table} DEFAULT VALUES")
    }
    // The statements Podėlis writes for the rows it shows: names and values
    // come quoted and as literals already (`quote_ident`, `sql_literal`).
    /// Rows of `table`: all, or those `filter` keeps, at most `limit`.
    fn select_rows(&self, table: &str, filter: Option<&str>, limit: Option<usize>) -> String {
        let filter = filter.map(|f| format!(" WHERE {f}")).unwrap_or_default();
        let limit = limit.map(|n| format!(" LIMIT {n}")).unwrap_or_default();
        format!("SELECT * FROM {table}{filter}{limit}")
    }
    /// An INSERT of `rows` (each `(value, …)`) into `columns` of `table`.
    fn insert(&self, table: &str, columns: &[String], rows: &[String]) -> String {
        format!("INSERT INTO {table} ({}) VALUES {}", columns.join(", "), rows.join(", "))
    }
    /// An UPDATE of `table` setting each column to its value, where `filter`.
    fn update(&self, table: &str, set: &[(String, String)], filter: &str) -> String {
        let set: Vec<String> = set.iter().map(|(column, value)| format!("{column} = {value}")).collect();
        format!("UPDATE {table} SET {} WHERE {filter}", set.join(", "))
    }
    /// A DELETE from `table` of the rows `filter` keeps.
    fn delete(&self, table: &str, filter: &str) -> String {
        format!("DELETE FROM {table} WHERE {filter}")
    }
    /// Column types for imported integers, decimals, booleans and
    /// timestamps (dates are `DATE` everywhere).
    fn integer_type(&self) -> &'static str {
        "BIGINT"
    }
    fn real_type(&self) -> &'static str;
    fn boolean_type(&self) -> &'static str {
        "BOOLEAN"
    }
    fn timestamp_type(&self) -> &'static str {
        "TIMESTAMP"
    }
    /// Text unless the engine keeps the offset (PostgreSQL); converting
    /// would change the value.
    fn timestamptz_type(&self) -> &'static str {
        "TEXT"
    }

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

impl Engine {
    /// Its engine module's SQL dialect (`DIALECT`).
    pub fn dialect(self) -> &'static dyn SqlDialect {
        on_engine!(self, &engine::DIALECT)
    }
}

/// A table a plan reads in full: its schema where the plan says, and the
/// rows it expects to read where it says.
#[derive(Clone, Debug, PartialEq)]
pub struct Scan {
    pub schema: Option<String>,
    pub table: String,
    pub rows: Option<f64>,
}

impl Scan {
    pub fn new(table: &str, rows: Option<f64>) -> Self {
        Scan { schema: None, table: table.to_string(), rows }
    }

    /// `schema.table`, or the table.
    pub fn name(&self) -> String {
        self.schema.as_ref().map_or_else(|| self.table.clone(), |s| format!("{s}.{}", self.table))
    }
}

/// A feature a statement uses that the server is too old for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TooNew {
    /// What it is, as written: "EXPLAIN ANALYZE", "MERGE".
    pub feature: &'static str,
    /// What has it: "MySQL 8.0.18 or newer".
    pub needs: &'static str,
    /// The server, as it says it is: "MySQL 5.7.44".
    pub server: String,
}

/// `sql`'s code as lowercase words and punctuation, without spaces,
/// comments or what's in strings: what [`SqlDialect::too_new`] looks through.
pub fn code_words(sql: &str, engine: crate::db::Engine) -> Vec<String> {
    use crate::sql::TokenKind;
    crate::sql::tokenize(sql, engine)
        .into_iter()
        .filter(|t| !matches!(t.kind, TokenKind::Whitespace | TokenKind::Comment | TokenKind::String))
        .map(|t| sql[t.range].to_ascii_lowercase())
        .collect()
}

/// Whether `words` has `seq` in a row somewhere.
pub fn has_seq(words: &[String], seq: &[&str]) -> bool {
    words.windows(seq.len()).any(|w| same(w, seq))
}

fn same(words: &[String], seq: &[&str]) -> bool {
    words.len() == seq.len() && words.iter().zip(seq).all(|(a, b)| a == b)
}

/// Whether `words` starts a statement with `seq` (the first statement, or
/// one after a `;`).
pub fn starts_with(words: &[String], seq: &[&str]) -> bool {
    let mut starts = std::iter::once(0).chain(words.iter().enumerate().filter(|(_, w)| *w == ";").map(|(i, _)| i + 1));
    starts.any(|i| words.get(i..i + seq.len()).is_some_and(|w| same(w, seq)))
}
