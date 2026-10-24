//! What a project's team has said about its data, kept for models: facts
//! as "thing, relation, value" (`orders.status` · means · `3 = cancelled`),
//! each with who said it, when, and whether it's confirmed.
//!
//! One SQLite file per project (`memory/<project-id>.db` in the config
//! folder), owned by the gateway. Only confirmed facts are shown to
//! models: a fact the user adds is confirmed (they said it); one a model
//! suggests waits for the user. A fact is shown when it's about a table
//! the question needs, or matches the question's words (full-text search).

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use sqlx::sqlite::{SqliteConnectOptions, SqliteConnection};
use sqlx::{Connection as _, Row as _};

/// The most facts shown to a model for one question.
const MAX_SHOWN: usize = 30;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fact {
    pub id: i64,
    /// What it's about: a table, `table.column`, or a word (`revenue`).
    pub subject: String,
    /// How the value relates to it: `means` by default.
    pub relation: String,
    pub value: String,
    /// Who said it: `user`, `model:<name>`, `file:<path>`.
    pub source: String,
    pub confirmed: bool,
    /// When it was said (RFC 3339, local time).
    pub at: String,
}

impl Fact {
    pub fn to_json(&self) -> Value {
        json!({
            "id": self.id,
            "subject": self.subject,
            "relation": self.relation,
            "value": self.value,
            "source": self.source,
            "confirmed": self.confirmed,
            "at": self.at,
        })
    }

    /// As a model is shown it: one line.
    pub fn line(&self) -> String {
        format!("{} {} {}", self.subject, self.relation, self.value)
    }

    /// Whether it's about `table` (as the context names it, `schema.table`
    /// or `table`) or one of its columns: `orders`, `orders.status`,
    /// `public.orders`, `public.orders.status`.
    fn is_about(&self, table: &str) -> bool {
        let subject = self.subject.trim().to_lowercase();
        let table = table.to_lowercase();
        let bare = table.rsplit('.').next().unwrap_or(&table).to_string();
        [table, bare].iter().any(|t| subject == *t || subject.starts_with(&format!("{t}.")))
    }
}

/// A fact as typed (`orders.status 3 = cancelled`, `revenue is orders.total
/// minus refunds`): what it's about, then what's said; first words say how
/// it relates (else `means`):
/// - `is`, `are`, `means`, `equals`;
/// - `also called` / `aka`: another name people use for it (`customer also
///   called client`): a question saying "clients" finds `customer`;
/// - `defined as`: a business term's formula or filter (`revenue defined as
///   sum(payment.amount) minus refunds`), used for that term.
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

pub fn path(config_dir: &Path, project_id: &str) -> PathBuf {
    config_dir.join("memory").join(format!("{project_id}.db"))
}

/// The project's memory, created on first use, only the user's to read.
pub async fn open(config_dir: &Path, project_id: &str) -> Result<SqliteConnection, String> {
    let file = path(config_dir, project_id);
    let dir = file.parent().ok_or("no memory folder")?;
    std::fs::create_dir_all(dir).map_err(|e| format!("couldn't create {}: {e}", dir.display()))?;
    restrict(dir, 0o700);
    // By path, not a URL: a `?` or `%20` in the folder's name is just a name.
    let options = SqliteConnectOptions::new().filename(&file).create_if_missing(true);
    let mut conn = SqliteConnection::connect_with(&options).await.map_err(|e| format!("memory: {e}"))?;
    restrict(&file, 0o600);
    for statement in SCHEMA.iter().copied() {
        sqlx::query(statement).execute(&mut conn).await.map_err(|e| format!("memory: {e}"))?;
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
        at TEXT NOT NULL
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
];

fn row(r: &sqlx::sqlite::SqliteRow) -> Fact {
    Fact {
        id: r.get("id"),
        subject: r.get("subject"),
        relation: r.get("relation"),
        value: r.get("value"),
        source: r.get("source"),
        confirmed: r.get::<i64, _>("confirmed") != 0,
        at: r.get("at"),
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
) -> Result<Fact, String> {
    let (subject, relation, value) = (subject.trim(), relation.trim(), value.trim());
    if subject.is_empty() || value.is_empty() {
        return Err("a fact needs what it's about and what's said about it".into());
    }
    let relation = if relation.is_empty() { "means" } else { relation };
    let at = chrono::Local::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, false);
    let r = sqlx::query(
        "INSERT INTO facts (subject, relation, value, source, confirmed, at) VALUES (?, ?, ?, ?, ?, ?)
         RETURNING id, subject, relation, value, source, confirmed, at",
    )
    .bind(subject)
    .bind(relation)
    .bind(value)
    .bind(source)
    .bind(i64::from(confirmed))
    .bind(at)
    .fetch_one(&mut *conn)
    .await
    .map_err(|e| format!("memory: {e}"))?;
    Ok(row(&r))
}

/// Every fact, oldest first; with `text`, those matching it.
pub async fn list(conn: &mut SqliteConnection, text: Option<&str>) -> Result<Vec<Fact>, String> {
    let rows =
        match text.map(fts_query).filter(|q| !q.is_empty()) {
            Some(q) => sqlx::query(
                "SELECT f.* FROM facts f JOIN facts_text t ON t.rowid = f.id WHERE facts_text MATCH ? ORDER BY f.id",
            )
            .bind(q)
            .fetch_all(&mut *conn)
            .await,
            None => sqlx::query("SELECT * FROM facts ORDER BY id").fetch_all(&mut *conn).await,
        };
    Ok(rows.map_err(|e| format!("memory: {e}"))?.iter().map(row).collect())
}

/// Replaces what was learned from `source` (a folder of migrations) with
/// `facts`, all confirmed (the team wrote them), in one transaction.
/// Returns how many there are now.
pub async fn replace_source(
    conn: &mut SqliteConnection,
    source: &str,
    facts: &[super::migrations::Learned],
) -> Result<usize, String> {
    let failed = |e: sqlx::Error| format!("memory: {e}");
    let mut tx = conn.begin().await.map_err(failed)?;
    sqlx::query("DELETE FROM facts WHERE source = ?").bind(source).execute(&mut *tx).await.map_err(failed)?;
    let at = chrono::Local::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, false);
    for f in facts {
        sqlx::query("INSERT INTO facts (subject, relation, value, source, confirmed, at) VALUES (?, ?, ?, ?, 1, ?)")
            .bind(&f.subject)
            .bind(&f.relation)
            .bind(&f.value)
            .bind(source)
            .bind(&at)
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
pub async fn forget(conn: &mut SqliteConnection, id: i64) -> Result<bool, String> {
    let failed = |e: sqlx::Error| format!("memory: {e}");
    sqlx::query(
        "INSERT INTO rejected (subject, value) SELECT subject, value FROM facts WHERE id = ? AND source LIKE 'model:%'",
    )
    .bind(id)
    .execute(&mut *conn)
    .await
    .map_err(failed)?;
    let done = sqlx::query("DELETE FROM facts WHERE id = ?").bind(id).execute(&mut *conn).await.map_err(failed)?;
    Ok(done.rows_affected() > 0)
}

/// Whether the user forgot this suggestion before.
pub async fn rejected(conn: &mut SqliteConnection, subject: &str, value: &str) -> Result<bool, String> {
    let found: Option<i64> = sqlx::query_scalar(
        "SELECT 1 FROM rejected WHERE lower(subject) = lower(?) AND lower(value) = lower(?) LIMIT 1",
    )
    .bind(subject)
    .bind(value)
    .fetch_optional(&mut *conn)
    .await
    .map_err(|e| format!("memory: {e}"))?;
    Ok(found.is_some())
}

/// Marks a fact confirmed (one a model suggested, now the user's).
pub async fn confirm(conn: &mut SqliteConnection, id: i64) -> Result<bool, String> {
    let done = sqlx::query("UPDATE facts SET confirmed = 1 WHERE id = ?")
        .bind(id)
        .execute(&mut *conn)
        .await
        .map_err(|e| format!("memory: {e}"))?;
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

pub async fn for_question(conn: &mut SqliteConnection, question: &str) -> Result<ForQuestion, String> {
    let confirmed = |facts: Vec<Fact>| facts.into_iter().filter(|f| f.confirmed).collect();
    Ok(ForQuestion { all: confirmed(list(conn, None).await?), matched: confirmed(list(conn, Some(question)).await?) })
}

impl ForQuestion {
    /// What the facts matching the question say, for choosing tables:
    /// "revenue is payment.amount" brings `payment`. Not histories: a
    /// table's past names other tables and columns.
    pub fn words(&self) -> String {
        let mut out = String::new();
        for f in self.matched.iter().filter(|f| f.relation != "history") {
            let _ = write!(out, " {} {}", f.subject, f.value);
        }
        out
    }

    /// Those to show a model, about `tables` (names, as the context chose
    /// them) first, then those matching the question.
    pub fn about(&self, tables: &[String]) -> Vec<Fact> {
        let mut shown: Vec<Fact> = self.all.iter().filter(|f| tables.iter().any(|t| f.is_about(t))).cloned().collect();
        for f in &self.matched {
            if !shown.iter().any(|s| s.id == f.id) {
                shown.push(f.clone());
            }
        }
        // What people said first, then notes from files, then histories: when
        // there's no room for all, the history goes.
        shown.sort_by_key(|f| match (f.source.as_str(), f.relation.as_str()) {
            ("user", _) => 0,
            (_, "history") => 2,
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
pub async fn relevant(conn: &mut SqliteConnection, tables: &[String], question: &str) -> Result<Vec<Fact>, String> {
    Ok(for_question(conn, question).await?.about(tables))
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
mod tests;
