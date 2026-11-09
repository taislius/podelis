//! Results as Parquet, from any engine: each column typed by what its
//! engine calls its type (`db::type_kind`), so integers, floats, booleans,
//! decimals, dates and timestamps are Parquet's own, and anything else is
//! text. Every value is read before a column's type is settled: one whose
//! values don't all read as its type takes the next that they do (an
//! integer column holding a 1.5 is floats; moments without a zone, as
//! MySQL's TIMESTAMP gives them, are plain timestamps), else text, and a
//! column with no value at all is text. Nothing is lost or made up.
//!
//! Every row of a query (`Pages`) goes to a scratch file beside the export
//! as it arrives, its values read for their types meanwhile; the Parquet
//! file is written once they're all in, the scratch file then removed. So
//! the rows shown and every row type the same result alike.
//!
//! The types come back from the text the grid shows, by each engine's
//! names for them. Carrying the drivers' own types through instead would
//! be sturdier (podelis-planai, `parquet.md`).

use std::io::Write;
use std::sync::Arc;

use arrow_array::{
    ArrayRef, BooleanArray, Date32Array, Decimal128Array, Float64Array, Int64Array, RecordBatch, StringArray,
    TimestampMicrosecondArray,
};
use arrow_schema::{DataType, Field, Schema, TimeUnit};
use parquet::arrow::ArrowWriter;
use parquet::basic::Compression;
use parquet::file::properties::WriterProperties;

use crate::db::{Engine, QueryOutcome, TypeKind};

/// Rows a row group holds at most: what a writer keeps in memory before it
/// writes them out.
const ROW_GROUP: usize = 65_536;

/// Parquet's decimals hold up to 38 digits.
const MAX_DIGITS: usize = 38;

/// How a column is written: Parquet's type for its kind, or text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Column {
    Int,
    Float,
    Bool,
    /// Precision and scale.
    Decimal(u8, i8),
    /// A decimal its type gives no scale for (`numeric`, as drivers name
    /// it): the values' widest scale.
    AnyDecimal,
    Date,
    Timestamp,
    TimestampUtc,
    Text,
}

impl Column {
    /// What a column of `kind` is written as, if its values read so.
    fn of(kind: &TypeKind) -> Column {
        match kind {
            TypeKind::Int(_) => Column::Int,
            TypeKind::Float4 | TypeKind::Float8 => Column::Float,
            TypeKind::Bool => Column::Bool,
            TypeKind::Decimal(Some((p, s))) if (*p as usize) <= MAX_DIGITS && *s <= *p => {
                Column::Decimal(u8::try_from(*p).unwrap_or(38), i8::try_from(*s).unwrap_or(0))
            }
            TypeKind::Decimal(_) => Column::AnyDecimal,
            TypeKind::Date => Column::Date,
            TypeKind::Timestamp => Column::Timestamp,
            TypeKind::TimestampTz => Column::TimestampUtc,
            _ => Column::Text,
        }
    }

    fn data_type(self) -> DataType {
        match self {
            Column::Int => DataType::Int64,
            Column::Float => DataType::Float64,
            Column::Bool => DataType::Boolean,
            Column::Decimal(p, s) => DataType::Decimal128(p, s),
            Column::Date => DataType::Date32,
            Column::Timestamp => DataType::Timestamp(TimeUnit::Microsecond, None),
            Column::TimestampUtc => DataType::Timestamp(TimeUnit::Microsecond, Some("UTC".into())),
            Column::AnyDecimal | Column::Text => DataType::Utf8,
        }
    }
}

fn bool_of(v: &str) -> Option<bool> {
    let is = |words: &[&str]| words.iter().any(|w| v.eq_ignore_ascii_case(w));
    if is(&["t", "true", "1", "yes", "on"]) {
        Some(true)
    } else if is(&["f", "false", "0", "no", "off"]) {
        Some(false)
    } else {
        None
    }
}

/// A decimal's sign and its digits before and after its point
/// (`-12.30` → negative, `12`, `30`); `None` if it isn't one.
fn decimal_parts(v: &str) -> Option<(bool, &str, &str)> {
    let negative = v.starts_with('-');
    let digits = v.strip_prefix(['-', '+']).unwrap_or(v);
    let (whole, frac) = digits.split_once('.').unwrap_or((digits, ""));
    let all = |s: &str| s.chars().all(|c| c.is_ascii_digit());
    (!(whole.is_empty() && frac.is_empty()) && all(whole) && all(frac)).then_some((negative, whole, frac))
}

/// `12.30` at precision 4 and scale 2 → 1230; `None` past either, or not
/// a number (so a value never overflows the column it's written in).
fn decimal_of(v: &str, precision: u8, scale: i8) -> Option<i128> {
    let (negative, whole, frac) = decimal_parts(v)?;
    let scale = usize::try_from(scale).ok()?;
    let whole = whole.trim_start_matches('0');
    if frac.len() > scale || whole.len() + scale > usize::from(precision) {
        return None;
    }
    let n: i128 = format!("{whole}{frac:0<scale$}").parse().unwrap_or(0);
    Some(if negative { -n } else { n })
}

fn date_of(v: &str) -> Option<i32> {
    let day = chrono::NaiveDate::parse_from_str(v, "%Y-%m-%d").ok()?;
    let epoch = chrono::NaiveDate::from_ymd_opt(1970, 1, 1)?;
    i32::try_from((day - epoch).num_days()).ok()
}

fn timestamp_of(v: &str) -> Option<i64> {
    ["%Y-%m-%d %H:%M:%S%.f", "%Y-%m-%dT%H:%M:%S%.f"]
        .iter()
        .find_map(|f| chrono::NaiveDateTime::parse_from_str(v, f).ok())
        .map(|t| t.and_utc().timestamp_micros())
}

/// A moment with its zone, in UTC: RFC 3339, PostgreSQL's `+02`, or
/// ` UTC` written out (PostgreSQL's values read in binary). One without a
/// zone (MySQL's TIMESTAMP, in the session's) isn't one.
fn moment_of(v: &str) -> Option<i64> {
    if let Ok(t) = chrono::DateTime::parse_from_rfc3339(v) {
        return Some(t.timestamp_micros());
    }
    if let Some(t) = ["%Y-%m-%d %H:%M:%S%.f%#z", "%Y-%m-%dT%H:%M:%S%.f%#z"]
        .iter()
        .find_map(|f| chrono::DateTime::parse_from_str(v, f).ok())
    {
        return Some(t.timestamp_micros());
    }
    timestamp_of(v.strip_suffix(" UTC")?)
}

/// Whether `v` reads as `column`'s type.
fn fits(column: Column, v: &str) -> bool {
    match column {
        Column::Int => v.parse::<i64>().is_ok(),
        Column::Float => v.parse::<f64>().is_ok(),
        Column::Bool => bool_of(v).is_some(),
        Column::Decimal(p, s) => decimal_of(v, p, s).is_some(),
        Column::AnyDecimal => decimal_parts(v).is_some(),
        Column::Date => date_of(v).is_some(),
        Column::Timestamp => timestamp_of(v).is_some(),
        Column::TimestampUtc => moment_of(v).is_some(),
        Column::Text => true,
    }
}

/// A column's values read so far: the types they all still read as, its
/// kind's first, then the wider ones (always ending in text), and for a
/// decimal of no given scale, its values' widest.
struct Fit {
    candidates: Vec<Column>,
    seen: bool,
    whole: usize,
    scale: usize,
    /// An integer past what a float holds exactly (2^53): as a float it
    /// would change, so such a column is text, not floats.
    past_float: bool,
}

impl Fit {
    fn new(kind: &TypeKind) -> Fit {
        let first = Column::of(kind);
        let mut candidates = vec![first];
        match first {
            Column::Int => candidates.push(Column::Float),
            // MySQL's `tinyint(1)` may hold a 2.
            Column::Bool => candidates.push(Column::Int),
            Column::TimestampUtc => candidates.push(Column::Timestamp),
            _ => {}
        }
        if first != Column::Text {
            candidates.push(Column::Text);
        }
        Fit { candidates, seen: false, whole: 1, scale: 0, past_float: false }
    }

    fn read(&mut self, v: &str) {
        self.seen = true;
        // Text takes anything: nothing more to learn.
        if self.candidates.first() == Some(&Column::Text) {
            return;
        }
        if self.candidates.contains(&Column::Float) && v.parse::<i64>().is_ok_and(|n| n.unsigned_abs() > 1 << 53) {
            self.past_float = true;
        }
        self.candidates.retain(|c| fits(*c, v));
        if self.candidates.first() == Some(&Column::AnyDecimal)
            && let Some((_, whole, frac)) = decimal_parts(v)
        {
            self.whole = self.whole.max(whole.trim_start_matches('0').len());
            self.scale = self.scale.max(frac.len());
        }
    }

    /// How it's written: the first type its values all read as.
    fn column(&self) -> Column {
        match self.candidates.first().copied().unwrap_or(Column::Text) {
            _ if !self.seen => Column::Text,
            Column::AnyDecimal if self.whole + self.scale <= MAX_DIGITS => {
                i8::try_from(self.scale).map_or(Column::Text, |s| Column::Decimal(38, s))
            }
            Column::AnyDecimal => Column::Text,
            Column::Float if self.past_float => Column::Text,
            column => column,
        }
    }
}

/// Each column's fit, from its type on `engine`.
fn fits_of(types: &[String], columns: usize, engine: Engine) -> Vec<Fit> {
    (0..columns)
        .map(|i| Fit::new(&types.get(i).map_or(TypeKind::Text, |ty| crate::db::type_kind(engine, ty))))
        .collect()
}

fn read_rows<'a>(fits: &mut [Fit], rows: impl IntoIterator<Item = &'a Vec<Option<String>>>) {
    for row in rows {
        for (fit, cell) in fits.iter_mut().zip(row) {
            if let Some(v) = cell {
                fit.read(v);
            }
        }
    }
}

/// One column's cells as `column`'s array (every value fits: `Fit` saw
/// to it).
fn array(column: Column, cells: &[Option<&str>]) -> std::io::Result<ArrayRef> {
    fn read<T>(cells: &[Option<&str>], f: impl Fn(&str) -> Option<T>) -> std::io::Result<Vec<Option<T>>> {
        cells
            .iter()
            .map(|c| {
                c.map(|v| f(v).ok_or_else(|| std::io::Error::other(format!("{v:?} didn't read as its type"))))
                    .transpose()
            })
            .collect()
    }
    Ok(match column {
        Column::Int => Arc::new(Int64Array::from(read(cells, |v| v.parse().ok())?)),
        Column::Float => Arc::new(Float64Array::from(read(cells, |v| v.parse().ok())?)),
        Column::Bool => Arc::new(BooleanArray::from(read(cells, bool_of)?)),
        Column::Decimal(p, s) => Arc::new(
            Decimal128Array::from(read(cells, |v| decimal_of(v, p, s))?)
                .with_precision_and_scale(p, s)
                .map_err(std::io::Error::other)?,
        ),
        Column::Date => Arc::new(Date32Array::from(read(cells, date_of)?)),
        Column::Timestamp => Arc::new(TimestampMicrosecondArray::from(read(cells, timestamp_of)?)),
        Column::TimestampUtc => Arc::new(TimestampMicrosecondArray::from(read(cells, moment_of)?).with_timezone("UTC")),
        Column::AnyDecimal | Column::Text => Arc::new(StringArray::from(cells.to_vec())),
    })
}

/// Names Parquet takes: each once (a second `a` is `a_2`).
fn unique(names: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(names.len());
    for name in names {
        let mut n = name.clone();
        let mut k = 2;
        while out.contains(&n) {
            n = format!("{name}_{k}");
            k += 1;
        }
        out.push(n);
    }
    out
}

fn properties() -> WriterProperties {
    WriterProperties::builder()
        .set_compression(Compression::SNAPPY)
        .set_max_row_group_row_count(Some(ROW_GROUP))
        .build()
}

/// Writes `rows` (any number of row groups' worth) to a Parquet file with
/// `names` and `columns`.
/// `stop`, once set, ends it between row groups (an error: "stopped").
fn write_rows<'a, W: Write + Send>(
    out: W,
    names: &[String],
    columns: &[Column],
    mut rows: impl Iterator<Item = std::io::Result<std::borrow::Cow<'a, Vec<Option<String>>>>>,
    stop: &std::sync::atomic::AtomicBool,
) -> std::io::Result<W> {
    if names.is_empty() {
        return Err(std::io::Error::other(t!("export.no_columns")));
    }
    let schema = Arc::new(Schema::new(
        unique(names).into_iter().zip(columns).map(|(n, c)| Field::new(n, c.data_type(), true)).collect::<Vec<_>>(),
    ));
    let mut writer =
        ArrowWriter::try_new(out, Arc::clone(&schema), Some(properties())).map_err(std::io::Error::other)?;
    loop {
        if stop.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(std::io::Error::other("stopped"));
        }
        let chunk: Vec<_> = rows.by_ref().take(ROW_GROUP).collect::<std::io::Result<_>>()?;
        let arrays = columns
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let cells: Vec<Option<&str>> = chunk.iter().map(|r| r.get(i).and_then(|v| v.as_deref())).collect();
                array(*c, &cells)
            })
            .collect::<std::io::Result<Vec<_>>>()?;
        writer
            .write(&RecordBatch::try_new(Arc::clone(&schema), arrays).map_err(std::io::Error::other)?)
            .map_err(std::io::Error::other)?;
        if chunk.len() < ROW_GROUP {
            break;
        }
    }
    writer.into_inner().map_err(std::io::Error::other)
}

/// The rows in `view` (indices into `outcome.rows`) as a Parquet file.
pub fn write<W: Write + Send>(outcome: &QueryOutcome, view: &[usize], engine: Engine, out: W) -> std::io::Result<W> {
    let rows: Vec<&Vec<Option<String>>> = view.iter().filter_map(|&i| outcome.rows.get(i)).collect();
    let mut fits = fits_of(&outcome.column_types, outcome.columns.len(), engine);
    read_rows(&mut fits, rows.iter().copied());
    let columns: Vec<Column> = fits.iter().map(Fit::column).collect();
    let never = std::sync::atomic::AtomicBool::new(false);
    write_rows(out, &outcome.columns, &columns, rows.into_iter().map(|r| Ok(std::borrow::Cow::Borrowed(r))), &never)
}

/// Every row of a result, a page at a time: each row to a scratch file
/// (one JSON array a line) while its values are read for their types,
/// the Parquet file written from it at the end. The scratch file goes
/// when it's done, or dropped.
pub struct Pages<W: Write + Send> {
    out: Option<W>,
    names: Vec<String>,
    fits: Vec<Fit>,
    scratch: std::io::BufWriter<std::fs::File>,
    scratch_path: std::path::PathBuf,
    /// The rows written so far.
    pub rows: u64,
}

impl<W: Write + Send> Pages<W> {
    /// Its scratch file at `scratch` (removed by the end).
    pub fn new(
        out: W,
        columns: &[String],
        column_types: &[String],
        engine: Engine,
        scratch: std::path::PathBuf,
    ) -> std::io::Result<Self> {
        let file = std::fs::File::create(&scratch)?;
        Ok(Pages {
            out: Some(out),
            names: columns.to_vec(),
            fits: fits_of(column_types, columns.len(), engine),
            scratch: std::io::BufWriter::new(file),
            scratch_path: scratch,
            rows: 0,
        })
    }

    pub fn page(&mut self, rows: &[Vec<Option<String>>]) -> std::io::Result<()> {
        self.rows += rows.len() as u64;
        read_rows(&mut self.fits, rows);
        for row in rows {
            serde_json::to_writer(&mut self.scratch, row)?;
            self.scratch.write_all(b"\n")?;
        }
        Ok(())
    }

    /// Writes the Parquet file from the scratch file, the types settled;
    /// `stop`, once set, ends it between row groups.
    pub fn finish(mut self, stop: &std::sync::atomic::AtomicBool) -> std::io::Result<W> {
        use std::io::BufRead as _;
        self.scratch.flush()?;
        let columns: Vec<Column> = self.fits.iter().map(Fit::column).collect();
        let back = std::io::BufReader::new(std::fs::File::open(&self.scratch_path)?);
        let rows = back.lines().map(|line| {
            let row: Vec<Option<String>> = serde_json::from_str(&line?)?;
            Ok(std::borrow::Cow::Owned(row))
        });
        let out = self.out.take().ok_or_else(|| std::io::Error::other("the file is gone"))?;
        let mut out = write_rows(out, &self.names, &columns, rows, stop)?;
        out.flush()?;
        Ok(out)
    }
}

/// The scratch file goes, however the export ended (done, failed, stopped).
impl<W: Write + Send> Drop for Pages<W> {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.scratch_path);
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use arrow_array::Array;
    use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;

    fn outcome(columns: &[(&str, &str)], rows: &[&[Option<&str>]]) -> QueryOutcome {
        QueryOutcome {
            columns: columns.iter().map(|(n, _)| (*n).to_string()).collect(),
            column_types: columns.iter().map(|(_, t)| (*t).to_string()).collect(),
            rows: rows.iter().map(|r| r.iter().map(|c| c.map(str::to_string)).collect()).collect(),
            ..QueryOutcome::default()
        }
    }

    /// `bytes` as a file a Parquet reader takes.
    fn file(bytes: &[u8]) -> std::fs::File {
        use std::io::{Seek as _, Write as _};
        let mut f = tempfile::tempfile().unwrap();
        f.write_all(bytes).unwrap();
        f.rewind().unwrap();
        f
    }

    /// The file read back: each column's name and type, and its batches.
    fn read(bytes: &[u8]) -> (Vec<(String, DataType)>, Vec<RecordBatch>) {
        let reader = ParquetRecordBatchReaderBuilder::try_new(file(bytes)).unwrap().build().unwrap();
        let batches: Vec<RecordBatch> = reader.map(Result::unwrap).collect();
        let schema = batches.first().map(RecordBatch::schema).unwrap();
        (schema.fields().iter().map(|f| (f.name().clone(), f.data_type().clone())).collect(), batches)
    }

    #[test]
    fn each_column_by_its_engine_s_type() {
        let o = outcome(
            &[
                ("id", "int4"),
                ("price", "numeric(10,2)"),
                ("ok", "bool"),
                ("day", "date"),
                ("at", "timestamptz"),
                ("zip", "text"),
            ],
            &[
                &[
                    Some("1"),
                    Some("12.30"),
                    Some("t"),
                    Some("2026-11-03"),
                    Some("2026-11-03 10:00:00+02"),
                    Some("00123"),
                ],
                &[Some("2"), None, Some("f"), None, Some("2026-11-03T08:00:00Z"), None],
            ],
        );
        let file = write(&o, &[0, 1], Engine::Postgres, Vec::new()).unwrap();
        let (fields, batches) = read(&file);
        let types: Vec<&DataType> = fields.iter().map(|(_, t)| t).collect();
        assert_eq!(
            types,
            [
                &DataType::Int64,
                &DataType::Decimal128(10, 2),
                &DataType::Boolean,
                &DataType::Date32,
                &DataType::Timestamp(TimeUnit::Microsecond, Some("UTC".into())),
                &DataType::Utf8,
            ]
        );
        let at = batches[0].column(4).as_any().downcast_ref::<TimestampMicrosecondArray>().unwrap();
        assert_eq!(at.value(0), at.value(1), "10:00+02 and 08:00Z are one moment");
        let zip = batches[0].column(5).as_any().downcast_ref::<StringArray>().unwrap();
        assert_eq!(zip.value(0), "00123", "text stays text");
        assert!(batches[0].column(1).is_null(1));
    }

    /// A column whose values don't all read as its type is text: nothing
    /// lost (MySQL's `0000-00-00`, a MongoDB field of two types).
    #[test]
    fn what_doesn_t_read_as_its_type_is_text() {
        let o = outcome(
            &[("d", "date"), ("n", "int")],
            &[&[Some("0000-00-00"), Some("1")], &[Some("2026-01-01"), Some("x")]],
        );
        let (fields, _) = read(&write(&o, &[0, 1], Engine::MySql, Vec::new()).unwrap());
        assert_eq!(fields.iter().map(|(_, t)| t.clone()).collect::<Vec<_>>(), [DataType::Utf8, DataType::Utf8]);
    }

    /// A scratch file for `Pages`, in a folder of the test's own.
    fn scratch() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("export.parquet.rows");
        (dir, path)
    }

    #[test]
    fn page_by_page_the_types_from_every_page() {
        let o = outcome(&[("n", "bigint"), ("n", "text")], &[]);
        let (_dir, path) = scratch();
        let mut pages = Pages::new(Vec::new(), &o.columns, &o.column_types, Engine::Postgres, path.clone()).unwrap();
        pages.page(&[vec![Some("1".into()), Some("a".into())]]).unwrap();
        pages.page(&[vec![Some("2".into()), Some("b".into())], vec![None, None]]).unwrap();
        let (fields, batches) = read(&pages.finish(&std::sync::atomic::AtomicBool::new(false)).unwrap());
        assert_eq!(fields, [("n".to_string(), DataType::Int64), ("n_2".to_string(), DataType::Utf8)]);
        assert_eq!(batches.iter().map(RecordBatch::num_rows).sum::<usize>(), 3);
        assert!(!path.exists(), "the scratch file goes");
    }

    /// A later page's value of another type changes the column's type,
    /// not the export: an integer column with a 1.5 is floats, one with
    /// `n/a` is text (a MongoDB field of two types).
    #[test]
    fn a_later_value_of_another_type_widens_its_column() {
        let o = outcome(&[("price", "int"), ("note", "int")], &[]);
        let (_dir, path) = scratch();
        let mut pages = Pages::new(Vec::new(), &o.columns, &o.column_types, Engine::MongoDb, path).unwrap();
        pages.page(&[vec![Some("1".into()), Some("2".into())]]).unwrap();
        pages.page(&[vec![Some("1.5".into()), Some("n/a".into())]]).unwrap();
        let (fields, batches) = read(&pages.finish(&std::sync::atomic::AtomicBool::new(false)).unwrap());
        assert_eq!(fields.iter().map(|(_, t)| t.clone()).collect::<Vec<_>>(), [DataType::Float64, DataType::Utf8]);
        let note = batches[0].column(1).as_any().downcast_ref::<StringArray>().unwrap();
        assert_eq!((note.value(0), note.value(1)), ("2", "n/a"), "nothing lost");
    }

    /// Integers past 2^53 with a fraction among them: text, not floats
    /// (an id would change); a boolean column with a 2: integers.
    #[test]
    fn widening_never_changes_a_value() {
        let o = outcome(
            &[("id", "bigint"), ("flag", "BOOLEAN")],
            &[&[Some("9007199254740993"), Some("1")], &[Some("1.5"), Some("2")]],
        );
        let (fields, batches) = read(&write(&o, &[0, 1], Engine::MySql, Vec::new()).unwrap());
        assert_eq!(fields.iter().map(|(_, t)| t.clone()).collect::<Vec<_>>(), [DataType::Utf8, DataType::Int64]);
        let id = batches[0].column(0).as_any().downcast_ref::<StringArray>().unwrap();
        assert_eq!(id.value(0), "9007199254740993");
    }

    /// A stop ends the writing between row groups.
    #[test]
    fn a_stop_ends_the_writing() {
        let o = outcome(&[("n", "int")], &[&[Some("1")]]);
        let (_dir, path) = scratch();
        let mut pages = Pages::new(Vec::new(), &o.columns, &o.column_types, Engine::Postgres, path.clone()).unwrap();
        pages.page(&o.rows).unwrap();
        let err = pages.finish(&std::sync::atomic::AtomicBool::new(true)).unwrap_err();
        assert_eq!(err.to_string(), "stopped");
        assert!(!path.exists(), "the scratch file goes");
    }

    /// A column with no value at all is text; MySQL's TIMESTAMP (a moment,
    /// but given in the session's zone without one) is a plain timestamp.
    #[test]
    fn moments_without_a_zone_and_columns_without_values() {
        let o = outcome(&[("at", "timestamp"), ("never", "int")], &[]);
        let (_dir, path) = scratch();
        let mut pages = Pages::new(Vec::new(), &o.columns, &o.column_types, Engine::MySql, path).unwrap();
        pages.page(&[vec![None, None]]).unwrap();
        pages.page(&[vec![Some("2026-11-03 10:00:00".into()), None]]).unwrap();
        let (fields, _) = read(&pages.finish(&std::sync::atomic::AtomicBool::new(false)).unwrap());
        assert_eq!(fields[0].1, DataType::Timestamp(TimeUnit::Microsecond, None));
        assert_eq!(fields[1].1, DataType::Utf8);
    }

    /// The drivers name a decimal without its scale (`NUMERIC`): the
    /// values give it, the same for the rows shown and every row.
    #[test]
    fn a_decimal_s_scale_from_its_values() {
        let o = outcome(&[("price", "NUMERIC")], &[&[Some("12.5")], &[Some("-3.25")], &[None]]);
        let (fields, batches) = read(&write(&o, &[0, 1, 2], Engine::Postgres, Vec::new()).unwrap());
        assert_eq!(fields[0].1, DataType::Decimal128(38, 2));
        let price = batches[0].column(0).as_any().downcast_ref::<Decimal128Array>().unwrap();
        assert_eq!((price.value(0), price.value(1)), (1250, -325));
        let (_dir, path) = scratch();
        let mut pages = Pages::new(Vec::new(), &o.columns, &o.column_types, Engine::Postgres, path).unwrap();
        pages.page(&o.rows[..1]).unwrap();
        pages.page(&o.rows[1..]).unwrap();
        let (every_row, _) = read(&pages.finish(&std::sync::atomic::AtomicBool::new(false)).unwrap());
        assert_eq!(every_row, fields, "the same types either way");
    }

    /// PostgreSQL's moments read in binary come as `… UTC`.
    #[test]
    fn moments_in_each_engine_s_form() {
        let at = moment_of("2026-11-03T08:00:00Z");
        assert_eq!(moment_of("2026-11-03 10:00:00+02"), at);
        assert_eq!(moment_of("2026-11-03 08:00:00 UTC"), at);
        assert_eq!(moment_of("2026-11-03 08:00:00"), None, "no zone: not a moment");
    }

    #[test]
    fn an_empty_result_is_a_file_with_its_columns() {
        let o = outcome(&[("id", "integer")], &[]);
        let (_dir, path) = scratch();
        let pages = Pages::new(Vec::new(), &o.columns, &o.column_types, Engine::Sqlite, path).unwrap();
        let bytes = pages.finish(&std::sync::atomic::AtomicBool::new(false)).unwrap();
        let reader = ParquetRecordBatchReaderBuilder::try_new(file(&bytes)).unwrap();
        assert_eq!(reader.schema().field(0).name(), "id");
    }

    /// A result with no columns (an UPDATE's) makes no file.
    #[test]
    fn no_columns_no_file() {
        assert!(write(&QueryOutcome::default(), &[], Engine::Postgres, Vec::new()).is_err());
    }

    #[test]
    fn decimals_by_their_scale() {
        assert_eq!(decimal_of("12.3", 10, 2), Some(1230));
        assert_eq!(decimal_of("-0.05", 10, 2), Some(-5));
        assert_eq!(decimal_of("1.234", 10, 2), None, "past its scale");
        assert_eq!(decimal_of("1234.5", 4, 1), None, "past its precision: never written to overflow");
        assert_eq!(decimal_of("1e5", 10, 2), None);
    }

    /// Exported from each engine's real results (its own type names): the
    /// integer and text columns typed, every row there.
    #[tokio::test]
    async fn from_each_engine_s_results() {
        for engine in Engine::BUILT {
            let Some((db, _)) = crate::db::tests::own_live_db(engine, "podelis_parquet").await else { continue };
            db.run("DROP TABLE IF EXISTS pq_t", 1).await.unwrap();
            db.run("CREATE TABLE pq_t (id INTEGER, name VARCHAR(20), price DECIMAL(10,2))", 1).await.unwrap();
            db.run("INSERT INTO pq_t VALUES (1, 'Ada', 12.50), (2, NULL, 3.25)", 1).await.unwrap();
            let o = db.run("SELECT id, name, price FROM pq_t ORDER BY id", 100).await.unwrap();
            let (fields, batches) = read(&write(&o, &[0, 1], engine, Vec::new()).unwrap());
            let types: Vec<&DataType> = fields.iter().map(|(_, t)| t).collect();
            assert_eq!(types[..2], [&DataType::Int64, &DataType::Utf8], "{engine:?}: {:?}", o.column_types);
            // A decimal, as its driver names it (often with no scale): Parquet's
            // own, where the engine keeps it one (not SQLite's affinity).
            if engine.dialect().is_some_and(|d| !d.keeps_only_affinity()) {
                assert!(
                    matches!(types[2], DataType::Decimal128(_, 2)),
                    "{engine:?}: {:?} as {:?}",
                    o.column_types,
                    types[2]
                );
            }
            assert_eq!(batches.iter().map(RecordBatch::num_rows).sum::<usize>(), 2, "{engine:?}");
            db.run("DROP TABLE pq_t", 1).await.unwrap();
        }
    }

    /// From MongoDB's documents: a number, a moment and text, by mongosh's
    /// type names.
    #[cfg(feature = "mongodb")]
    #[tokio::test]
    async fn from_mongodb_s_documents() {
        let Some(port) = std::env::var("PODELIS_TEST_MONGO_PORT").ok().and_then(|p| p.parse().ok()) else { return };
        let params = crate::db::ConnectParams {
            engine: Engine::MongoDb,
            host: std::env::var("PODELIS_TEST_MONGO_HOST").unwrap_or_else(|_| "127.0.0.1".into()),
            port,
            user: String::new(),
            password: String::new(),
            database: "podelis_parquet".into(),
            ssl_mode: crate::db::SslMode::Prefer,
            read_only: false,
            ssh: None,
            password_command: Vec::new(),
            for_agents: false,
        };
        let db = crate::db::Database::connect(params).await.unwrap();
        db.run("db.pq.drop()", 1).await.unwrap();
        db.run(
            "db.pq.insertMany([{n: 1, at: ISODate('2026-11-03T10:00:00Z'), name: 'Ada'}, {n: 2, name: 'Linas'}])",
            1,
        )
        .await
        .unwrap();
        let o = db.run("db.pq.find({}, {_id: 0}).sort({n: 1})", 100).await.unwrap();
        let (fields, batches) = read(&write(&o, &[0, 1], Engine::MongoDb, Vec::new()).unwrap());
        assert_eq!(
            fields,
            [
                ("n".to_string(), DataType::Int64),
                ("at".to_string(), DataType::Timestamp(TimeUnit::Microsecond, Some("UTC".into()))),
                ("name".to_string(), DataType::Utf8),
            ],
            "{:?}",
            o.column_types
        );
        assert!(batches[0].column(1).is_null(1), "a field one document lacks: null");
        db.run("db.pq.drop()", 1).await.unwrap();
    }
}
