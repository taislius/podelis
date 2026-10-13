//! `--import file.csv`: loads a CSV/TSV file into a table, creating it with
//! guessed column types if it doesn't exist. It all happens in one
//! transaction, so a failure leaves the table as it was. PostgreSQL gets
//! the rows through `COPY`; the others in batches of `INSERT`s.

use std::path::Path;

use anyhow::{Context, Result};
use chrono::{DateTime, NaiveDate, NaiveDateTime};

use crate::db::{Database, Engine, sql_literal};

const BATCH: usize = 500;

/// How much `COPY` data to send at a time.
const COPY_CHUNK: usize = 1 << 20;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ColType {
    Integer,
    Real,
    Boolean,
    Date,
    Timestamp,
    /// A timestamp with a time zone offset (`Z`, `+02:00`).
    TimestampTz,
    Text,
}

impl ColType {
    fn sql(self, engine: Engine) -> &'static str {
        let dialect = engine.dialect();
        match self {
            ColType::Integer => dialect.integer_type(),
            ColType::Real => dialect.real_type(),
            ColType::Boolean => dialect.boolean_type(),
            ColType::Date => "DATE",
            ColType::Timestamp => dialect.timestamp_type(),
            ColType::TimestampTz => dialect.timestamptz_type(),
            ColType::Text => "TEXT",
        }
    }

    /// The narrowest type for one value (not empty).
    fn of(value: &str) -> ColType {
        // "007" or "0123" are codes (zip codes, ids), not numbers.
        let digits = value.strip_prefix('-').unwrap_or(value);
        if digits.len() > 1 && digits.starts_with('0') && !digits.starts_with("0.") {
            return ColType::Text;
        }
        if value.parse::<i64>().is_ok() {
            ColType::Integer
        } else if value.parse::<f64>().is_ok_and(f64::is_finite) && !value.starts_with('.') {
            ColType::Real
        } else if value.eq_ignore_ascii_case("true") || value.eq_ignore_ascii_case("false") {
            ColType::Boolean
        } else if NaiveDate::parse_from_str(value, "%Y-%m-%d").is_ok() {
            ColType::Date
        } else if ["%Y-%m-%d %H:%M:%S%.f", "%Y-%m-%dT%H:%M:%S%.f", "%Y-%m-%d %H:%M", "%Y-%m-%dT%H:%M"]
            .iter()
            .any(|f| NaiveDateTime::parse_from_str(value, f).is_ok())
        {
            ColType::Timestamp
        } else if is_timestamptz(value) {
            ColType::TimestampTz
        } else {
            ColType::Text
        }
    }

    /// A type that fits values of both. Timestamps with and without a time
    /// zone don't mix: which zone the others are in is anyone's guess.
    fn join(self, other: ColType) -> ColType {
        match (self, other) {
            (a, b) if a == b => a,
            (ColType::Integer | ColType::Real, ColType::Integer | ColType::Real) => ColType::Real,
            (ColType::Date | ColType::Timestamp, ColType::Date | ColType::Timestamp) => ColType::Timestamp,
            _ => ColType::Text,
        }
    }

    /// Written into SQL as it is, not as a string.
    fn bare(self) -> bool {
        matches!(self, ColType::Integer | ColType::Real | ColType::Boolean)
    }
}

/// `2026-10-13 10:00:00+02:00`, `2026-10-13T10:00Z`: with seconds or not,
/// fractions, a space or a `T`, and the offset as `Z`, `+02`, `+0200` or
/// `+02:00`.
fn is_timestamptz(value: &str) -> bool {
    let value = value.strip_suffix('Z').map_or_else(|| value.to_string(), |v| format!("{v}+00:00"));
    ["%Y-%m-%d %H:%M:%S%.f%#z", "%Y-%m-%dT%H:%M:%S%.f%#z", "%Y-%m-%d %H:%M%#z", "%Y-%m-%dT%H:%M%#z"]
        .iter()
        .any(|f| DateTime::parse_from_str(&value, f).is_ok())
}

fn is_tsv(path: &Path) -> bool {
    path.extension().is_some_and(|e| e.eq_ignore_ascii_case("tsv") || e.eq_ignore_ascii_case("tab"))
}

/// TSV files don't quote: a `"` is part of the value (film titles in
/// published data sets, `COPY`'s output).
fn reader(path: &Path) -> Result<csv::Reader<std::fs::File>> {
    let tsv = is_tsv(path);
    csv::ReaderBuilder::new()
        .delimiter(if tsv { b'\t' } else { b',' })
        .quoting(!tsv)
        .from_path(path)
        .with_context(|| format!("can't read {}", path.display()))
}

/// An empty field is NULL, and in a TSV file so is `\N` (how PostgreSQL's
/// `COPY` and many published data sets write it).
fn is_null(value: &str, tsv: bool) -> bool {
    value.is_empty() || (tsv && value == "\\N")
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

/// Which pass an import is in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    /// Reading the file to guess the column types.
    Reading,
    /// Sending the rows.
    Loading,
}

/// How far an import has come.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Progress {
    pub stage: Stage,
    /// How much of the file has been read, of `size` bytes.
    pub bytes: u64,
    pub size: u64,
    pub rows: u64,
}

/// `reading 45% · 2.7M rows`.
impl std::fmt::Display for Progress {
    #[allow(clippy::cast_precision_loss)] // display only, one decimal
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let stage = match self.stage {
            Stage::Reading => "reading",
            Stage::Loading => "loading",
        };
        let percent = (self.bytes * 100).checked_div(self.size).unwrap_or(100).min(100);
        let rows = if self.rows == 1 { "row" } else { "rows" };
        write!(f, "{stage} {percent}% · {} {rows}", crate::db::human_rows(self.rows as f64))
    }
}

/// Progress is reported every this many rows, and at the end of each pass.
const PROGRESS_ROWS: u64 = 10_000;

/// A file's rows after the header, with how far into the file they are.
struct Rows {
    reader: csv::Reader<std::fs::File>,
    record: csv::StringRecord,
    /// Rows read so far.
    count: u64,
}

impl Rows {
    fn open(path: &Path) -> Result<(Vec<String>, Rows)> {
        let mut reader = reader(path)?;
        let header = reader.headers()?.iter().map(|h| h.trim().to_string()).collect();
        Ok((header, Rows { reader, record: csv::StringRecord::new(), count: 0 }))
    }

    /// The next row, or `None` at the end of the file. An error names the
    /// row's line.
    fn next(&mut self) -> Option<Result<&csv::StringRecord>> {
        match self.reader.read_record(&mut self.record) {
            Ok(true) => {
                self.count += 1;
                Some(Ok(&self.record))
            }
            Ok(false) => None,
            Err(e) => Some(Err(anyhow::Error::from(e).context(format!("line {}", self.count + 2)))),
        }
    }

    /// The line the last row was on (the header is line 1).
    fn line(&self) -> u64 {
        self.count + 1
    }

    fn progress(&self, stage: Stage, size: u64) -> Progress {
        Progress { stage, bytes: self.reader.position().byte(), size, rows: self.count }
    }
}

pub async fn import(
    db: &Database,
    path: &Path,
    table: &str,
    progress: &mut (dyn FnMut(Progress) + Send),
) -> Result<Imported> {
    let engine = db.engine();
    let tsv = is_tsv(path);
    let size = std::fs::metadata(path).map_or(0, |m| m.len());
    // First pass: the header and each column's type.
    let (header, mut rows) = Rows::open(path)?;
    anyhow::ensure!(
        !header.is_empty() && header.iter().all(|h| !h.is_empty()),
        "the first line must name every column"
    );
    let mut guessed: Vec<Option<ColType>> = vec![None; header.len()];
    while let Some(record) = rows.next() {
        for (i, value) in record?.iter().enumerate().take(header.len()) {
            if !is_null(value, tsv) {
                let t = ColType::of(value);
                guessed[i] = Some(guessed[i].map_or(t, |g| g.join(t)));
            }
        }
        if rows.count % PROGRESS_ROWS == 0 {
            progress(rows.progress(Stage::Reading, size));
        }
    }
    progress(rows.progress(Stage::Reading, size));
    let exists = db.tables(false).await?.iter().any(|t| t.name.eq_ignore_ascii_case(table));
    // Columns with no values at all are text. Into an existing table, only
    // numbers go in bare: its columns may not be what the file suggests.
    let types: Vec<ColType> = guessed
        .iter()
        .map(|g| match g {
            Some(t @ (ColType::Integer | ColType::Real)) => *t,
            Some(t) if !exists => *t,
            _ => ColType::Text,
        })
        .collect();
    let quoted = engine.quote_ident(table);
    let cols: Vec<String> = header.iter().map(|h| engine.quote_ident(h)).collect();
    db.run("BEGIN", 1).await?;
    let result = async {
        if !exists {
            let defs: Vec<String> = cols.iter().zip(&types).map(|(c, t)| format!("{c} {}", t.sql(engine))).collect();
            db.run(&format!("CREATE TABLE {quoted} ({})", defs.join(", ")), 1).await?;
        }
        // Second pass: the rows.
        let (_, mut rows) = Rows::open(path)?;
        if engine == Engine::Postgres {
            let width = header.len();
            let progress = &mut *progress;
            let chunks = std::iter::from_fn(move || {
                let mut chunk = Vec::new();
                while chunk.len() < COPY_CHUNK {
                    match rows.next() {
                        None => break,
                        Some(Ok(record)) => copy_line(record, width, tsv, &mut chunk),
                        Some(Err(e)) => return Some(Err(e)),
                    }
                }
                progress(rows.progress(Stage::Loading, size));
                (!chunk.is_empty()).then_some(Ok(chunk))
            });
            let sql = format!("COPY {quoted} ({}) FROM STDIN", cols.join(", "));
            return db.copy_in(&sql, 2, chunks).await;
        }
        let mut batch = Vec::with_capacity(BATCH);
        let mut done = 0u64;
        while let Some(record) = rows.next() {
            let record = record?;
            let values: Vec<String> = (0..header.len())
                .map(|i| match record.get(i).unwrap_or("") {
                    v if is_null(v, tsv) => "NULL".to_string(),
                    v if types[i].bare() => v.to_string(),
                    v => sql_literal(v, engine),
                })
                .collect();
            batch.push(format!("({})", values.join(", ")));
            if batch.len() == BATCH {
                let line = rows.line();
                done +=
                    insert(db, &quoted, &cols, &mut batch).await.with_context(|| format!("rows up to line {line}"))?;
                if rows.count % PROGRESS_ROWS == 0 {
                    progress(rows.progress(Stage::Loading, size));
                }
            }
        }
        done += insert(db, &quoted, &cols, &mut batch).await?;
        progress(rows.progress(Stage::Loading, size));
        anyhow::Ok(done)
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

/// A row in `COPY`'s text format: tab-separated, `\N` for NULL, and
/// backslashes, tabs and line breaks escaped.
fn copy_line(record: &csv::StringRecord, width: usize, tsv: bool, out: &mut Vec<u8>) {
    for i in 0..width {
        if i > 0 {
            out.push(b'\t');
        }
        match record.get(i).unwrap_or("") {
            v if is_null(v, tsv) => out.extend_from_slice(b"\\N"),
            v => {
                for &b in v.as_bytes() {
                    match b {
                        b'\\' => out.extend_from_slice(b"\\\\"),
                        b'\t' => out.extend_from_slice(b"\\t"),
                        b'\n' => out.extend_from_slice(b"\\n"),
                        b'\r' => out.extend_from_slice(b"\\r"),
                        b => out.push(b),
                    }
                }
            }
        }
    }
    out.push(b'\n');
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

    async fn import_quietly(db: &Database, path: &Path, table: &str) -> Result<Imported> {
        import(db, path, table, &mut |_| {}).await
    }

    #[test]
    fn guesses_types_and_names() {
        let t = |values: &[&str]| values.iter().map(|v| ColType::of(v)).reduce(ColType::join).unwrap();
        assert_eq!(t(&["1", "42"]), ColType::Integer);
        assert_eq!(t(&["1", "2.5"]), ColType::Real);
        assert_eq!(t(&["1", "x"]), ColType::Text);
        assert_eq!(t(&["007"]), ColType::Text, "leading zeros are codes, not numbers");
        assert_eq!(t(&["true", "FALSE"]), ColType::Boolean);
        assert_eq!(t(&["true", "1"]), ColType::Text);
        assert_eq!(t(&["2026-10-13", "2020-02-29"]), ColType::Date);
        assert_eq!(t(&["2026-10-13", "2026-10-13 10:00:00.5", "2026-10-13T10:00"]), ColType::Timestamp);
        assert_eq!(t(&["2026-02-30"]), ColType::Text, "not a real date");
        assert_eq!(
            t(&[
                "2026-10-13T10:00:00Z",
                "2026-10-13 10:00:00.5+02",
                "2026-10-13T10:00+0200",
                "2026-10-13 10:00:00-05:30"
            ]),
            ColType::TimestampTz
        );
        assert_eq!(t(&["2026-10-13 10:00:00+02:00", "2026-10-13 10:00:00"]), ColType::Text, "zones don't mix");
        assert_eq!(t(&["2026-10-13 10:00:00+25:00"]), ColType::Text, "not a real offset");
        assert_eq!(t(&["2026-10-13", "13/10/2026"]), ColType::Text);
        assert_eq!(table_name_for(Path::new("/data/Sales 2026.csv")), "sales_2026");
        assert_eq!(table_name_for(Path::new("2026.csv")), "t_2026");
    }

    #[test]
    fn progress_reads_as_a_line() {
        let p = |stage, bytes, size, rows| Progress { stage, bytes, size, rows }.to_string();
        assert_eq!(p(Stage::Reading, 45, 100, 2_700_000), "reading 45% · 2.7M rows");
        assert_eq!(p(Stage::Loading, 100, 100, 1), "loading 100% · 1 row");
        assert_eq!(p(Stage::Loading, 0, 0, 0), "loading 100% · 0 rows", "an empty file");
    }

    #[test]
    fn copy_lines_escape_and_mark_nulls() {
        let mut out = Vec::new();
        let record = csv::StringRecord::from(vec!["a\\b", "tab\there", "", "\\N", "line\nbreak\r"]);
        copy_line(&record, 6, true, &mut out);
        assert_eq!(String::from_utf8(out).unwrap(), "a\\\\b\ttab\\there\t\\N\t\\N\tline\\nbreak\\r\t\\N\n");
    }

    /// A file with every guessed type, into PostgreSQL (`COPY`) and MySQL
    /// (`INSERT`s), then a bad value: nothing changes, and the error names
    /// its line.
    #[tokio::test]
    async fn live_imports_guess_types_on_servers() {
        let dir = tempfile::tempdir().unwrap();
        let csv = dir.path().join("pets.csv");
        std::fs::write(
            &csv,
            "id,name,active,born,seen,score,at\n\
             1,\"Rex\tthe \\ dog\",true,2020-01-02,2026-10-13 10:00:00.123456,1.5,2026-10-13T10:00:00Z\n\
             2,Žąsis,FALSE,2019-12-31,2026-10-13T11:30,2,2026-10-13 12:00:00+02:00\n3,,,,,,\n",
        )
        .unwrap();
        let bad = dir.path().join("bad.csv");
        std::fs::write(&bad, "id,born\n4,2020-01-01\n5,not a date\n").unwrap();
        for engine in [Engine::Postgres, Engine::MySql] {
            let Some(db) = crate::db::tests::live_db(engine).await else { continue };
            if engine == Engine::MySql {
                db.run("CREATE DATABASE IF NOT EXISTS podelis_import; USE podelis_import", 1).await.unwrap();
            }
            db.run("DROP TABLE IF EXISTS pets_import", 1).await.unwrap();
            assert_eq!(import_quietly(&db, &csv, "pets_import").await.unwrap(), Imported { rows: 3, created: true });
            let o = db.run("SELECT * FROM pets_import ORDER BY id", 10).await.unwrap();
            let types: Vec<String> = o.column_types.iter().map(|t| t.to_lowercase()).collect();
            assert_eq!(o.rows[0][1].as_deref(), Some("Rex\tthe \\ dog"), "{engine:?}");
            assert_eq!(o.rows[1][1].as_deref(), Some("Žąsis"), "{engine:?}");
            assert_eq!(o.rows[2][1..], [None, None, None, None, None, None], "{engine:?}");
            if engine == Engine::Postgres {
                assert_eq!(types[1..], ["text", "bool", "date", "timestamp", "float8", "timestamptz"]);
                db.run("SET TIME ZONE 'UTC'", 1).await.unwrap();
                let at = db.run("SELECT at FROM pets_import ORDER BY id", 10).await.unwrap();
                assert_eq!(at.rows[0][0], at.rows[1][0], "the same moment: {:?}", at.rows);
                assert_eq!(o.rows[1][2].as_deref(), Some("false"));
                assert_eq!(o.rows[0][4].as_deref(), Some("2026-10-13 10:00:00.123456"));
            } else {
                assert_eq!(o.rows[1][2].as_deref(), Some("0"), "BOOLEAN is TINYINT(1)");
                assert_eq!(o.rows[0][4].as_deref(), Some("2026-10-13 10:00:00.123456"));
                assert_eq!(o.rows[1][4].as_deref(), Some("2026-10-13 11:30:00.000000"));
                assert_eq!(o.rows[0][6].as_deref(), Some("2026-10-13T10:00:00Z"), "kept as written");
            }
            // A bad value in an existing table: nothing goes in.
            let err = import_quietly(&db, &bad, "pets_import").await.unwrap_err();
            if engine == Engine::Postgres {
                assert!(format!("{err:#}").starts_with("line 3: "), "{err:#}");
            }
            let count = db.run("SELECT count(*) FROM pets_import", 1).await.unwrap();
            assert_eq!(count.rows[0][0].as_deref(), Some("3"), "{engine:?}");
            db.run("DROP TABLE pets_import", 1).await.unwrap();
        }
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
        assert_eq!(import_quietly(&db, &csv, "pets").await.unwrap(), Imported { rows: 3, created: true });
        assert_eq!(import_quietly(&db, &csv, "pets").await.unwrap(), Imported { rows: 3, created: false });
        let o = db.run("SELECT typeof(id), typeof(weight), name FROM pets WHERE id = 2 LIMIT 1", 10).await.unwrap();
        assert_eq!(o.rows[0], vec![Some("integer".into()), Some("null".into()), Some("Žąsis, the goose".into())]);
        // A bad file changes nothing.
        let bad = dir.path().join("bad.csv");
        std::fs::write(&bad, "id,name\n1,a\n2,b,extra\n").unwrap();
        assert!(import_quietly(&db, &bad, "pets").await.is_err());
        assert_eq!(db.run("SELECT count(*) FROM pets", 1).await.unwrap().rows[0][0].as_deref(), Some("6"));

        // TSV as IMDb writes it: quotes are part of the value, `\N` is NULL.
        let tsv = dir.path().join("movies.tsv");
        std::fs::write(&tsv, "id\ttitle\tyear\n1\tThe \"Best\" Movie\t1999\n2\t\"Quoted start\t\\N\n3\tAfter\t2002\n")
            .unwrap();
        assert_eq!(import_quietly(&db, &tsv, "movies").await.unwrap(), Imported { rows: 3, created: true });
        let o = db.run("SELECT title, year, typeof(year) FROM movies ORDER BY id", 10).await.unwrap();
        let col = |c: usize| o.rows.iter().map(|r| r[c].clone()).collect::<Vec<_>>();
        assert_eq!(col(0), [Some("The \"Best\" Movie".into()), Some("\"Quoted start".into()), Some("After".into())]);
        assert_eq!(col(1), [Some("1999".into()), None, Some("2002".into())]);
        assert_eq!(col(2)[0].as_deref(), Some("integer"), "a `\\N` doesn't make the column text");
    }
}
