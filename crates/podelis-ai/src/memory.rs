//! What a project's team has said about its data, kept for models: facts
//! as "thing, relation, value" (`orders.status` · means · `3 = cancelled`),
//! each with who said it, when, and whether it's confirmed.
//!
//! One SQLite file per project (`memory/<project-id>.db` in the config
//! folder), owned by the gateway. Each fact is about one database of the
//! project (`database`: its name, as connections to it share it, so dev
//! and prod have one memory), or, with none, about every database in the
//! project (as all facts were before). Only confirmed facts are shown to
//! models: a fact the user adds is confirmed (they said it); one a model
//! suggests waits for the user. A fact is shown when it's about a table
//! the question needs, or matches the question's words (full-text search).

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use sqlx::sqlite::{SqliteConnectOptions, SqliteConnection};
use sqlx::{Connection as _, Row as _};

/// What went wrong with a project's memory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemoryError {
    /// Its file couldn't be opened, read or written (it may have been
    /// moved or replaced): one kept open is opened again next time.
    Storage(String),
    /// What was given doesn't make a fact.
    NotAFact,
}

impl std::fmt::Display for MemoryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MemoryError::Storage(e) => f.write_str(e),
            MemoryError::NotAFact => f.write_str("a fact needs what it's about and what's said about it"),
        }
    }
}

impl std::error::Error for MemoryError {}

impl From<sqlx::Error> for MemoryError {
    fn from(e: sqlx::Error) -> Self {
        MemoryError::Storage(format!("memory: {e}"))
    }
}

impl From<MemoryError> for String {
    fn from(e: MemoryError) -> Self {
        e.to_string()
    }
}

/// The most facts shown to a model for one question.
const MAX_SHOWN: usize = 30;

/// A fact's id: its row in the memory file. What the gateway, the memory
/// window and the ontology call it by, written as the number it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct FactId(i64);

impl FactId {
    /// As it's written: in JSON, and for `podelis memory forget <id>`.
    pub const fn get(self) -> i64 {
        self.0
    }

    /// One read back from where it was written (a gateway request, the
    /// command line). Whether a fact has it is the memory's to say.
    pub const fn from_written(id: i64) -> Self {
        Self(id)
    }
}

impl std::fmt::Display for FactId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// As the gateway sends it: its fields by name.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Fact {
    pub id: FactId,
    /// What it's about: a table, `table.column`, or a word (`revenue`).
    pub subject: String,
    /// How the value relates to it: `means` by default.
    pub relation: String,
    pub value: String,
    /// Who said it: `user`, `model:<name>`, `file:<path>`.
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub confirmed: bool,
    /// When it was said (RFC 3339, local time).
    #[serde(default)]
    pub at: String,
    /// The database it's about (its key: `shop`), or "" for every
    /// database in the project.
    #[serde(default)]
    pub database: String,
}

impl Fact {
    /// As a model is shown it: one line.
    pub fn line(&self) -> String {
        format!("{} {} {}", self.subject, self.relation, self.value)
    }
}

/// A fact as typed (`orders.status 3 = cancelled`, `revenue is orders.total
/// minus refunds`): what it's about, then what's said; first words say how
/// it relates (else `means`):
/// - `is`, `are`, `means`, `equals`;
/// - `also called` / `aka`: another name people use for it (`customer also
///   called client`): a question saying "clients" finds `customer`;
/// - `defined as`: a business term's formula or filter (`revenue defined as
///   sum(payment.amount) minus refunds`), used for that term;
/// - `excluded by default`: rows left out unless asked for (`customer
///   excluded by default deleted_at is not null`: soft deletes, test
///   accounts).
pub fn parse_fact(text: &str) -> Option<(String, String, String)> {
    let mut words = text.split_whitespace();
    let subject = words.next()?;
    let rest: Vec<&str> = words.collect();
    let lower: Vec<String> = rest.iter().map(|w| w.to_lowercase()).collect();
    let starts = |seq: &[&str]| lower.len() > seq.len() && lower.iter().zip(seq).all(|(a, b)| a == b);
    let (relation, value) = if starts(&["also", "called"]) {
        (ALSO_CALLED, &rest[2..])
    } else if starts(&["aka"]) || starts(&["a.k.a."]) {
        (ALSO_CALLED, &rest[1..])
    } else if starts(&["is", "defined", "as"]) {
        (DEFINED_AS, &rest[3..])
    } else if starts(&["defined", "as"]) {
        (DEFINED_AS, &rest[2..])
    } else if starts(&["excluded", "by", "default"]) {
        (EXCLUDED_BY_DEFAULT, &rest[3..])
    } else {
        match rest.split_first() {
            Some((first @ &("is" | "are" | "means" | "equals"), value)) if !value.is_empty() => (*first, value),
            _ => ("means", rest.as_slice()),
        }
    };
    (!value.is_empty()).then(|| (subject.to_string(), relation.to_string(), value.join(" ")))
}

/// Another name people use for a table, column or term.
pub const ALSO_CALLED: &str = "also called";
/// A business term's formula or filter.
pub const DEFINED_AS: &str = "defined as";
/// Rows of a table left out unless the question asks for them.
pub const EXCLUDED_BY_DEFAULT: &str = "excluded by default";
/// What happened to a table or column, from migrations.
pub const HISTORY: &str = "history";

/// The relations Podėlis knows a fact by: what its subject means, its
/// other names, a term's formula, rows left out by default, its history.
/// A fact may have any other word (`note`): it's read as what it means.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Relation {
    Means,
    Is,
    Are,
    Equals,
    AlsoCalled,
    DefinedAs,
    ExcludedByDefault,
    History,
}

impl Relation {
    pub const ALL: [Relation; 8] = [
        Relation::Means,
        Relation::Is,
        Relation::Are,
        Relation::Equals,
        Relation::AlsoCalled,
        Relation::DefinedAs,
        Relation::ExcludedByDefault,
        Relation::History,
    ];

    /// As a fact writes it.
    pub fn as_str(self) -> &'static str {
        match self {
            Relation::Means => "means",
            Relation::Is => "is",
            Relation::Are => "are",
            Relation::Equals => "equals",
            Relation::AlsoCalled => ALSO_CALLED,
            Relation::DefinedAs => DEFINED_AS,
            Relation::ExcludedByDefault => EXCLUDED_BY_DEFAULT,
            Relation::History => HISTORY,
        }
    }

    /// The one `relation` names (ignoring case and spaces around it), if
    /// it's one of these.
    pub fn of(relation: &str) -> Option<Self> {
        let relation = relation.trim();
        Self::ALL.into_iter().find(|r| r.as_str().eq_ignore_ascii_case(relation))
    }

    /// Said only of a table (or its columns), never of a word: a fact of
    /// one word with it names a table, here or gone.
    pub fn is_of_tables(self) -> bool {
        matches!(self, Relation::ExcludedByDefault | Relation::History)
    }
}

pub fn path(config_dir: &Path, project_id: &str) -> PathBuf {
    config_dir.join("memory").join(format!("{project_id}.db"))
}

/// The project's memory, created on first use, only the user's to read.
pub async fn open(config_dir: &Path, project_id: &str) -> Result<SqliteConnection, MemoryError> {
    let file = path(config_dir, project_id);
    let dir = file.parent().ok_or_else(|| MemoryError::Storage("no memory folder".into()))?;
    std::fs::create_dir_all(dir)
        .map_err(|e| MemoryError::Storage(format!("couldn't create {}: {e}", dir.display())))?;
    restrict(dir, 0o700);
    // By path, not a URL: a `?` or `%20` in the folder's name is just a name.
    let options = SqliteConnectOptions::new().filename(&file).create_if_missing(true);
    let mut conn = SqliteConnection::connect_with(&options).await?;
    restrict(&file, 0o600);
    for statement in SCHEMA.iter().copied() {
        sqlx::query(statement).execute(&mut conn).await?;
    }
    // A memory from before facts had a database: theirs is every database.
    let columns: Vec<String> =
        sqlx::query_scalar("SELECT name FROM pragma_table_info('facts')").fetch_all(&mut conn).await?;
    if !columns.iter().any(|c| c == "database") {
        sqlx::query("ALTER TABLE facts ADD COLUMN database TEXT NOT NULL DEFAULT ''").execute(&mut conn).await?;
    }
    Ok(conn)
}

fn restrict(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode));
}

/// The tables, and a full-text index kept in step by triggers.
const SCHEMA: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS facts (
        id INTEGER PRIMARY KEY,
        subject TEXT NOT NULL,
        relation TEXT NOT NULL,
        value TEXT NOT NULL,
        source TEXT NOT NULL,
        confirmed INTEGER NOT NULL DEFAULT 0,
        at TEXT NOT NULL,
        database TEXT NOT NULL DEFAULT ''
    )",
    "CREATE VIRTUAL TABLE IF NOT EXISTS facts_text
        USING fts5(subject, relation, value, content='facts', content_rowid='id')",
    "CREATE TRIGGER IF NOT EXISTS facts_added AFTER INSERT ON facts BEGIN
        INSERT INTO facts_text(rowid, subject, relation, value) VALUES (new.id, new.subject, new.relation, new.value);
    END",
    // A model's suggestion the user forgot: not suggested again.
    "CREATE TABLE IF NOT EXISTS rejected (subject TEXT NOT NULL, value TEXT NOT NULL)",
    "CREATE TRIGGER IF NOT EXISTS facts_removed AFTER DELETE ON facts BEGIN
        INSERT INTO facts_text(facts_text, rowid, subject, relation, value)
            VALUES ('delete', old.id, old.subject, old.relation, old.value);
    END",
    // A fact edited: its words in the index change with it.
    "CREATE TRIGGER IF NOT EXISTS facts_edited AFTER UPDATE OF subject, relation, value ON facts BEGIN
        INSERT INTO facts_text(facts_text, rowid, subject, relation, value)
            VALUES ('delete', old.id, old.subject, old.relation, old.value);
        INSERT INTO facts_text(rowid, subject, relation, value) VALUES (new.id, new.subject, new.relation, new.value);
    END",
];

fn row(r: &sqlx::sqlite::SqliteRow) -> Fact {
    Fact {
        id: FactId(r.get("id")),
        subject: r.get("subject"),
        relation: r.get("relation"),
        value: r.get("value"),
        source: r.get("source"),
        confirmed: r.get::<i64, _>("confirmed") != 0,
        at: r.get("at"),
        database: r.get("database"),
    }
}

/// Adds a fact; returns it.
pub async fn add(
    conn: &mut SqliteConnection,
    subject: &str,
    relation: &str,
    value: &str,
    source: &str,
    confirmed: bool,
    database: &str,
) -> Result<Fact, MemoryError> {
    let (subject, relation, value) = (subject.trim(), relation.trim(), value.trim());
    if subject.is_empty() || value.is_empty() {
        return Err(MemoryError::NotAFact);
    }
    let relation = if relation.is_empty() { "means" } else { relation };
    let at = chrono::Local::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, false);
    let r = sqlx::query(
        "INSERT INTO facts (subject, relation, value, source, confirmed, at, database) VALUES (?, ?, ?, ?, ?, ?, ?)
         RETURNING *",
    )
    .bind(subject)
    .bind(relation)
    .bind(value)
    .bind(source)
    .bind(i64::from(confirmed))
    .bind(at)
    .bind(database.trim().to_lowercase())
    .fetch_one(&mut *conn)
    .await?;
    Ok(row(&r))
}

/// Every fact, oldest first; with `text`, those matching it. With a
/// `database`, only those about it and those about every database.
pub async fn list(
    conn: &mut SqliteConnection,
    text: Option<&str>,
    database: Option<&str>,
) -> Result<Vec<Fact>, MemoryError> {
    // A database's facts and those about every database; all of them for
    // none (`?1` NULL).
    let database = database.map(|d| d.trim().to_lowercase());
    let rows = match text.map(fts_query).filter(|q| !q.is_empty()) {
        Some(q) => {
            sqlx::query(
                "SELECT f.* FROM facts f JOIN facts_text t ON t.rowid = f.id WHERE facts_text MATCH ?2 \
             AND (?1 IS NULL OR f.database = '' OR f.database = ?1) ORDER BY f.id",
            )
            .bind(database)
            .bind(q)
            .fetch_all(&mut *conn)
            .await
        }
        None => {
            sqlx::query("SELECT * FROM facts WHERE (?1 IS NULL OR database = '' OR database = ?1) ORDER BY id")
                .bind(database)
                .fetch_all(&mut *conn)
                .await
        }
    };
    Ok(rows?.iter().map(row).collect())
}

/// Replaces what was learned from `source` (a folder of migrations), in
/// whatever scope, with `facts` about `database` ("" for every one), all
/// confirmed (the team wrote them), in one transaction.
/// Returns how many there are now.
pub async fn replace_source(
    conn: &mut SqliteConnection,
    source: &str,
    facts: &[super::migrations::Learned],
    database: &str,
) -> Result<usize, MemoryError> {
    let failed = MemoryError::from;
    let database = database.trim().to_lowercase();
    let mut tx = conn.begin().await.map_err(failed)?;
    // What the folder taught before, in any scope: learning it again (for
    // this database or every one) replaces it, rather than adding a copy.
    sqlx::query("DELETE FROM facts WHERE source = ?").bind(source).execute(&mut *tx).await.map_err(failed)?;
    let at = chrono::Local::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, false);
    for f in facts {
        sqlx::query(
            "INSERT INTO facts (subject, relation, value, source, confirmed, at, database) VALUES (?, ?, ?, ?, 1, ?, ?)",
        )
        .bind(&f.subject)
        .bind(&f.relation)
        .bind(&f.value)
        .bind(source)
        .bind(&at)
        .bind(&database)
            .execute(&mut *tx)
            .await
            .map_err(failed)?;
    }
    tx.commit().await.map_err(failed)?;
    Ok(facts.len())
}

/// Removes a fact; false if there was none with that id. A model's
/// suggestion forgotten is remembered as rejected, so the same guess
/// isn't kept again.
pub async fn forget(conn: &mut SqliteConnection, id: FactId) -> Result<bool, MemoryError> {
    let failed = MemoryError::from;
    sqlx::query(
        "INSERT INTO rejected (subject, value) SELECT subject, value FROM facts WHERE id = ? AND source LIKE 'model:%'",
    )
    .bind(id.0)
    .execute(&mut *conn)
    .await
    .map_err(failed)?;
    let done = sqlx::query("DELETE FROM facts WHERE id = ?").bind(id.0).execute(&mut *conn).await.map_err(failed)?;
    Ok(done.rows_affected() > 0)
}

/// Whether the user forgot this suggestion before.
pub async fn rejected(conn: &mut SqliteConnection, subject: &str, value: &str) -> Result<bool, MemoryError> {
    let found: Option<i64> = sqlx::query_scalar(
        "SELECT 1 FROM rejected WHERE lower(subject) = lower(?) AND lower(value) = lower(?) LIMIT 1",
    )
    .bind(subject)
    .bind(value)
    .fetch_optional(&mut *conn)
    .await?;
    Ok(found.is_some())
}

/// Reads an edit of a fact whose relation was `relation`: when the text
/// still has that relation after the subject (`orders history created …`,
/// `x note …`, a relation [`parse_fact`] doesn't know), it stays; otherwise
/// the text is read as when adding.
pub fn parse_edit(text: &str, relation: &str) -> Option<(String, String, String)> {
    let words: Vec<&str> = text.split_whitespace().collect();
    let kept: Vec<String> = relation.split_whitespace().map(str::to_lowercase).collect();
    let (subject, rest) = words.split_first()?;
    let same =
        !kept.is_empty() && rest.len() > kept.len() && rest.iter().zip(&kept).all(|(w, k)| w.to_lowercase() == *k);
    if same {
        return Some(((*subject).to_string(), relation.to_string(), rest[kept.len()..].join(" ")));
    }
    parse_fact(text)
}

/// Fact `id` now says `text` (the user's edit, read by [`parse_edit`]):
/// the user's from then on (confirmed, its source `user`, so learning a
/// folder again doesn't replace it), dated now. A model's guess changed is
/// remembered as rejected, as when forgotten, so it isn't suggested again
/// beside the correction. None if there's no such fact.
pub async fn update(conn: &mut SqliteConnection, id: FactId, text: &str) -> Result<Option<Fact>, MemoryError> {
    let failed = MemoryError::from;
    let Some(old) = sqlx::query("SELECT * FROM facts WHERE id = ?")
        .bind(id.0)
        .fetch_optional(&mut *conn)
        .await
        .map_err(failed)?
        .as_ref()
        .map(row)
    else {
        return Ok(None);
    };
    let Some((subject, relation, value)) = parse_edit(text, &old.relation) else {
        return Err(MemoryError::NotAFact);
    };
    if old.source.starts_with("model:") && (old.subject != subject || old.value != value) {
        sqlx::query("INSERT INTO rejected (subject, value) VALUES (?, ?)")
            .bind(&old.subject)
            .bind(&old.value)
            .execute(&mut *conn)
            .await
            .map_err(failed)?;
    }
    let at = chrono::Local::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, false);
    let r = sqlx::query(
        "UPDATE facts SET subject = ?, relation = ?, value = ?, source = 'user', confirmed = 1, at = ? WHERE id = ?
         RETURNING *",
    )
    .bind(subject)
    .bind(relation)
    .bind(value)
    .bind(at)
    .bind(id.0)
    .fetch_optional(&mut *conn)
    .await
    .map_err(failed)?;
    Ok(r.as_ref().map(row))
}

/// Moves a fact to `database` (its key), or with "" to every database in
/// the project. False if there's no such fact.
pub async fn set_database(conn: &mut SqliteConnection, id: FactId, database: &str) -> Result<bool, MemoryError> {
    let done = sqlx::query("UPDATE facts SET database = ? WHERE id = ?")
        .bind(database.trim().to_lowercase())
        .bind(id.0)
        .execute(&mut *conn)
        .await?;
    Ok(done.rows_affected() > 0)
}

/// Marks a fact confirmed (one a model suggested, now the user's).
pub async fn confirm(conn: &mut SqliteConnection, id: FactId) -> Result<bool, MemoryError> {
    let done = sqlx::query("UPDATE facts SET confirmed = 1 WHERE id = ?").bind(id.0).execute(&mut *conn).await?;
    Ok(done.rows_affected() > 0)
}

/// `text`'s words as an FTS5 query: any of them, each quoted (so nothing
/// in it is read as FTS syntax).
fn fts_query(text: &str) -> String {
    let words: Vec<String> = text
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|w| w.chars().count() >= 2)
        .map(|w| {
            let w = w.to_lowercase();
            // The word, and its stem as a prefix: "clients" finds a fact
            // about a "client", "rented" one about "rental".
            match stem(&w) {
                Some(stem) => format!("\"{w}\" OR \"{stem}\"*"),
                None => format!("\"{w}\""),
            }
        })
        .collect();
    words.join(" OR ")
}

/// `w` without a plural ending, `-ed`, `-ing` or `-al`, if what's left is
/// still a word (four letters or more).
fn stem(w: &str) -> Option<String> {
    ["ies", "es", "s", "ed", "ing", "al"]
        .iter()
        .find_map(|end| w.strip_suffix(end).filter(|rest| rest.chars().count() >= 4))
        .map(str::to_string)
}

/// The confirmed facts for one question, read once: all of them, and
/// those matching its words.
#[derive(Default)]
pub struct ForQuestion {
    all: Vec<Fact>,
    matched: Vec<Fact>,
}

/// For a question about `database` (its key; `None`: every fact).
pub async fn for_question(
    conn: &mut SqliteConnection,
    question: &str,
    database: Option<&str>,
) -> Result<ForQuestion, MemoryError> {
    let confirmed = |facts: Vec<Fact>| facts.into_iter().filter(|f| f.confirmed).collect();
    Ok(ForQuestion {
        all: confirmed(list(conn, None, database).await?),
        matched: confirmed(list(conn, Some(question), database).await?),
    })
}

impl ForQuestion {
    /// Every confirmed fact about the database.
    pub fn all(&self) -> &[Fact] {
        &self.all
    }

    /// What the facts matching the question say, for choosing tables:
    /// "revenue is payment.amount" brings `payment`. Not histories: a
    /// table's past names other tables and columns.
    pub fn words(&self) -> String {
        let mut out = String::new();
        for f in self.matched.iter().filter(|f| Relation::of(&f.relation) != Some(Relation::History)) {
            let _ = write!(out, " {} {}", f.subject, f.value);
        }
        out
    }

    /// Those to show a model, about `tables` (names, as the context chose
    /// them) or their columns first, as `ontology` reads their subjects
    /// (`orders`, `Orders.status`, `public.orders` are one table), then
    /// those matching the question.
    pub fn about(&self, ontology: &crate::ontology::Ontology, tables: &[String]) -> Vec<Fact> {
        let chosen: Vec<_> = tables.iter().filter_map(|t| ontology.table_of(t)).collect();
        let mut shown: Vec<Fact> = self
            .all
            .iter()
            .filter(|f| ontology.table_of(&f.subject).is_some_and(|t| chosen.contains(&t)))
            .cloned()
            .collect();
        for f in &self.matched {
            if !shown.iter().any(|s| s.id == f.id) {
                shown.push(f.clone());
            }
        }
        // What people said first, then notes from files, then histories: when
        // there's no room for all, the history goes.
        shown.sort_by_key(|f| match (f.source.as_str(), Relation::of(&f.relation)) {
            ("user", _) => 0,
            (_, Some(Relation::History)) => 2,
            _ => 1,
        });
        shown.truncate(MAX_SHOWN);
        shown
    }
}

/// The confirmed facts to show a model for `question`, about `tables`
/// (names, as the context chose them): those about the tables first,
/// then those matching the question's words.
#[cfg(test)]
pub async fn relevant(
    conn: &mut SqliteConnection,
    ontology: &crate::ontology::Ontology,
    tables: &[String],
    question: &str,
) -> Result<Vec<Fact>, MemoryError> {
    Ok(for_question(conn, question, None).await?.about(ontology, tables))
}

/// The facts as a text file, to read, fix, or keep in git: a line each,
/// `subject | relation | value | source · when` (unconfirmed ones marked).
pub fn export(project: &str, facts: &[Fact]) -> String {
    let mut out = format!(
        "# Podėlis memory · project {project} · {}\n# subject | relation | value | who said it · when\n",
        chrono::Local::now().format("%Y-%m-%d %H:%M")
    );
    for f in facts {
        let unconfirmed = if f.confirmed { "" } else { " · not confirmed" };
        let _ = writeln!(out, "{} | {} | {} | {} · {}{unconfirmed}", f.subject, f.relation, f.value, f.source, f.at);
    }
    out
}

#[cfg(test)]
#[path = "memory_tests.rs"]
mod tests;
