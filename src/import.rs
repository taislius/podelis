//! `--import file.csv`: loads a CSV/TSV file into a table, creating it with
//! guessed column types if it doesn't exist. Rows go in batches inside one
//! transaction, so a failure leaves the table as it was.

use std::path::Path;

use anyhow::{Context, Result};

use crate::db::{Database, Engine, sql_literal};

const BATCH: usize = 500;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ColType {
    Integer,
    Real,
    Text,
}

impl ColType {
    fn sql(self, engine: Engine) -> &'static str {
        match self {
            ColType::Integer => engine.dialect().integer_type(),
            ColType::Real => engine.dialect().real_type(),
            ColType::Text => "TEXT",
        }
    }

    /// The narrowest type that still fits `value` (empty fits anything).
    fn widen(self, value: &str) -> ColType {
        if value.is_empty() || self == ColType::Text {
            return self;
        }
        // "007" or "0123" are codes (zip codes, ids), not numbers.
        let digits = value.strip_prefix('-').unwrap_or(value);
        if digits.len() > 1 && digits.starts_with('0') && !digits.starts_with("0.") {
            return ColType::Text;
        }
        let int = value.parse::<i64>().is_ok();
        match self {
            ColType::Integer if int => ColType::Integer,
            _ if value.parse::<f64>().is_ok_and(f64::is_finite) && !value.starts_with('.') => ColType::Real,
            _ => ColType::Text,
        }
    }
}

fn reader(path: &Path) -> Result<csv::Reader<std::fs::File>> {
    let tab = path.extension().is_some_and(|e| e.eq_ignore_ascii_case("tsv") || e.eq_ignore_ascii_case("tab"));
    csv::ReaderBuilder::new()
        .delimiter(if tab { b'\t' } else { b',' })
        .from_path(path)
        .with_context(|| format!("can't read {}", path.display()))
}

/// A table name from a file name: `sales 2026.csv` → `sales_2026`.
pub fn table_name_for(path: &Path) -> String {
    let stem = path.file_stem().map(|s| s.to_string_lossy().to_lowercase()).unwrap_or_default();
    let name: String = stem.chars().map(|c| if c.is_alphanumeric() { c } else { '_' }).collect();
    let name = name.trim_matches('_').to_string();
    if name.is_empty() || name.starts_with(|c: char| c.is_ascii_digit()) { format!("t_{name}") } else { name }
}

/// What an import did.
#[derive(Debug, PartialEq, Eq)]
pub struct Imported {
    pub rows: u64,
    pub created: bool,
}

pub async fn import(db: &Database, path: &Path, table: &str) -> Result<Imported> {
    let engine = db.engine();
    // First pass: the header and each column's type.
    let mut rdr = reader(path)?;
    let header: Vec<String> = rdr.headers()?.iter().map(|h| h.trim().to_string()).collect();
    anyhow::ensure!(
        !header.is_empty() && header.iter().all(|h| !h.is_empty()),
        "the first line must name every column"
    );
    let mut types = vec![ColType::Integer; header.len()];
    let mut seen = vec![false; header.len()];
    for (n, record) in rdr.records().enumerate() {
        let record = record.with_context(|| format!("line {}", n + 2))?;
        for (i, value) in record.iter().enumerate().take(header.len()) {
            types[i] = types[i].widen(value);
            seen[i] |= !value.is_empty();
        }
    }
    // Columns with no values at all are text.
    for (t, s) in types.iter_mut().zip(&seen) {
        if !s {
            *t = ColType::Text;
        }
    }

    let exists = db.tables(false).await?.iter().any(|t| t.name.eq_ignore_ascii_case(table));
    let quoted = engine.quote_ident(table);
    let cols: Vec<String> = header.iter().map(|h| engine.quote_ident(h)).collect();
    db.run("BEGIN", 1).await?;
    let result = async {
        if !exists {
            let defs: Vec<String> = cols.iter().zip(&types).map(|(c, t)| format!("{c} {}", t.sql(engine))).collect();
            db.run(&format!("CREATE TABLE {quoted} ({})", defs.join(", ")), 1).await?;
        }
        // Second pass: the rows, in batches.
        let mut rdr = reader(path)?;
        let mut batch = Vec::with_capacity(BATCH);
        let mut rows = 0u64;
        for (n, record) in rdr.records().enumerate() {
            let record = record.with_context(|| format!("line {}", n + 2))?;
            let values: Vec<String> = (0..header.len())
                .map(|i| match record.get(i).unwrap_or("") {
                    "" => "NULL".to_string(),
                    v if types[i] != ColType::Text => v.to_string(),
                    v => sql_literal(v, engine),
                })
                .collect();
            batch.push(format!("({})", values.join(", ")));
            if batch.len() == BATCH {
                rows += insert(db, &quoted, &cols, &mut batch)
                    .await
                    .with_context(|| format!("rows up to line {}", n + 2))?;
            }
        }
        rows += insert(db, &quoted, &cols, &mut batch).await?;
        anyhow::Ok(rows)
    }
    .await;
    match result {
        Ok(rows) => {
            db.run("COMMIT", 1).await?;
            Ok(Imported { rows, created: !exists })
        }
        Err(e) => {
            let _ = db.run("ROLLBACK", 1).await;
            Err(e)
        }
    }
}

async fn insert(db: &Database, table: &str, cols: &[String], batch: &mut Vec<String>) -> Result<u64> {
    if batch.is_empty() {
        return Ok(0);
    }
    let sql = format!("INSERT INTO {table} ({}) VALUES {}", cols.join(", "), batch.join(", "));
    let n = batch.len() as u64;
    batch.clear();
    db.run(&sql, 1).await?;
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guesses_types_and_names() {
        let t = |values: &[&str]| values.iter().fold(ColType::Integer, |t, v| t.widen(v));
        assert_eq!(t(&["1", "", "42"]), ColType::Integer);
        assert_eq!(t(&["1", "2.5"]), ColType::Real);
        assert_eq!(t(&["1", "x"]), ColType::Text);
        assert_eq!(t(&["007"]), ColType::Text, "leading zeros are codes, not numbers");
        assert_eq!(table_name_for(Path::new("/data/Sales 2026.csv")), "sales_2026");
        assert_eq!(table_name_for(Path::new("2026.csv")), "t_2026");
    }

    #[tokio::test]
    async fn imports_into_a_new_then_an_existing_table() {
        let dir = tempfile::tempdir().unwrap();
        let csv = dir.path().join("pets.csv");
        std::fs::write(&csv, "id,name,weight\n1,Rex,12.5\n2,\"Žąsis, the goose\",\n3,Kit,3\n").unwrap();
        let params = crate::db::ConnectParams {
            engine: Engine::Sqlite,
            host: dir.path().join("i.db").to_string_lossy().into_owned(),
            port: 0,
            user: String::new(),
            password: String::new(),
            database: String::new(),
            ssl_mode: crate::db::SslMode::Disable,
            read_only: false,
            ssh: None,
            password_command: Vec::new(),
        };
        let db = Database::connect(params).await.unwrap();
        assert_eq!(import(&db, &csv, "pets").await.unwrap(), Imported { rows: 3, created: true });
        assert_eq!(import(&db, &csv, "pets").await.unwrap(), Imported { rows: 3, created: false });
        let o = db.run("SELECT typeof(id), typeof(weight), name FROM pets WHERE id = 2 LIMIT 1", 10).await.unwrap();
        assert_eq!(o.rows[0], vec![Some("integer".into()), Some("null".into()), Some("Žąsis, the goose".into())]);
        // A bad file changes nothing.
        let bad = dir.path().join("bad.csv");
        std::fs::write(&bad, "id,name\n1,a\n2,b,extra\n").unwrap();
        assert!(import(&db, &bad, "pets").await.is_err());
        assert_eq!(db.run("SELECT count(*) FROM pets", 1).await.unwrap().rows[0][0].as_deref(), Some("6"));
    }
}
