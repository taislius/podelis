//! Result sets as text: CSV, TSV, JSON, `INSERT` statements and an
//! aligned table. Used by exports, pipes and the command line alike.

use std::borrow::Cow;
use std::fmt::Write as _;

use crate::db::{Engine, QueryOutcome};

/// The text formats a result can be written in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    /// Aligned columns with a header rule, like psql.
    Table,
    Csv,
    Tsv,
    Json,
}

/// Makes a cell value fit on one table line.
pub fn display_cell(value: &str) -> Cow<'_, str> {
    if value.contains(['\n', '\r', '\t']) {
        Cow::Owned(value.replace("\r\n", "↵").replace(['\n', '\r'], "↵").replace('\t', " "))
    } else {
        Cow::Borrowed(value)
    }
}

/// [`display_cell`] of at most the first `max` characters: enough to fill
/// a column that wide, without copying or measuring a huge value (a JSON
/// document, a log) on every frame.
pub fn display_prefix(value: &str, max: usize) -> Cow<'_, str> {
    let end = value.char_indices().nth(max).map_or(value.len(), |(i, _)| i);
    display_cell(&value[..end])
}

fn csv_field(value: &str) -> Cow<'_, str> {
    if value.contains([',', '"', '\n', '\r']) {
        Cow::Owned(format!("\"{}\"", value.replace('"', "\"\"")))
    } else {
        Cow::Borrowed(value)
    }
}

/// CSV (RFC 4180) of the rows in `view` order. `NULL` becomes an empty field.
pub fn csv(outcome: &QueryOutcome, view: &[usize]) -> String {
    let mut out = String::new();
    let header: Vec<_> = outcome.columns.iter().map(|c| csv_field(c)).collect();
    out.push_str(&header.join(","));
    out.push_str("\r\n");
    for row in view.iter().map(|&i| &outcome.rows[i]) {
        let fields: Vec<_> = row.iter().map(|c| csv_field(c.as_deref().unwrap_or(""))).collect();
        out.push_str(&fields.join(","));
        out.push_str("\r\n");
    }
    out
}

/// TSV with a header line. Tabs and line breaks inside values become a
/// space and `\n`, so every row is one line; NULL is empty.
pub fn tsv(outcome: &QueryOutcome, view: &[usize]) -> String {
    let clean = |v: &str| v.replace('\t', " ").replace("\r\n", "\\n").replace(['\n', '\r'], "\\n");
    let mut out = outcome.columns.iter().map(|c| clean(c)).collect::<Vec<_>>().join("\t");
    out.push('\n');
    for &i in view {
        let cells: Vec<String> = outcome.rows[i].iter().map(|c| c.as_deref().map(clean).unwrap_or_default()).collect();
        out.push_str(&cells.join("\t"));
        out.push('\n');
    }
    out
}

/// JSON array of objects (one per row, keys in column order) for the rows
/// in `view`. Values are strings as displayed; `NULL` becomes `null`.
/// Duplicate column names get a `_2`, `_3`... suffix.
pub fn json(outcome: &QueryOutcome, view: &[usize]) -> String {
    let mut seen: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    let keys: Vec<String> = outcome
        .columns
        .iter()
        .map(|c| {
            let n = seen.entry(c.as_str()).or_insert(0);
            *n += 1;
            let key = if *n == 1 { c.clone() } else { format!("{c}_{n}") };
            serde_json::to_string(&key).unwrap_or_default()
        })
        .collect();
    let rows: Vec<String> = view
        .iter()
        .map(|&i| {
            let fields: Vec<String> = keys
                .iter()
                .zip(&outcome.rows[i])
                .map(|(k, v)| format!("{k}: {}", serde_json::to_string(v).unwrap_or_default()))
                .collect();
            format!("  {{{}}}", fields.join(", "))
        })
        .collect();
    if rows.is_empty() { "[]\n".to_string() } else { format!("[\n{}\n]\n", rows.join(",\n")) }
}

/// One `INSERT INTO table (...) VALUES (...);` per row in `view` order.
/// Numbers in numeric columns stay unquoted, binary values are written in
/// hex, and NULL stays NULL.
pub fn sql_inserts(outcome: &QueryOutcome, view: &[usize], table: &str, engine: Engine) -> String {
    let numeric = |i: usize| {
        let ty = outcome.column_types.get(i).map(|t| t.to_ascii_uppercase()).unwrap_or_default();
        ["INT", "NUMERIC", "DECIMAL", "FLOAT", "DOUBLE", "REAL"].iter().any(|n| ty.contains(n))
    };
    let columns: Vec<String> = outcome.columns.iter().map(|c| engine.quote_ident(c)).collect();
    let columns = columns.join(", ");
    let mut out = String::new();
    for &r in view {
        let values: Vec<String> = outcome.rows[r]
            .iter()
            .enumerate()
            .map(|(i, v)| match v {
                None => "NULL".to_string(),
                Some(v) if numeric(i) && v.parse::<f64>().is_ok() => v.clone(),
                Some(v) => crate::db::value_literal(v, outcome.column_types.get(i).map_or("", |t| t), engine),
            })
            .collect();
        let _ = writeln!(out, "INSERT INTO {table} ({columns}) VALUES ({});", values.join(", "));
    }
    out
}

/// A file format written a page of rows at a time, for results too big to
/// hold: the same text as [`csv`], [`json`] and [`sql_inserts`] give.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Paged {
    Csv,
    Json,
    Inserts { table: String, engine: Engine },
}

/// Writes a result's rows to `out` page by page.
pub struct PagedWriter<W: std::io::Write> {
    out: W,
    format: Paged,
    /// The columns and their types; `rows` holds the page being written.
    shape: QueryOutcome,
    pub rows: u64,
}

impl<W: std::io::Write> PagedWriter<W> {
    pub fn new(out: W, format: Paged, columns: &[String], column_types: &[String]) -> Self {
        let shape =
            QueryOutcome { columns: columns.to_vec(), column_types: column_types.to_vec(), ..QueryOutcome::default() };
        Self { out, format, shape, rows: 0 }
    }

    pub fn page(&mut self, rows: Vec<Vec<Option<String>>>) -> std::io::Result<()> {
        let first = self.rows == 0;
        self.rows += rows.len() as u64;
        self.shape.rows = rows;
        let all: Vec<usize> = (0..self.shape.rows.len()).collect();
        let text = match &self.format {
            Paged::Csv => {
                let text = csv(&self.shape, &all);
                if first { text } else { text[csv(&self.shape, &[]).len()..].to_string() }
            }
            Paged::Json if all.is_empty() => String::new(),
            Paged::Json => {
                let text = json(&self.shape, &all);
                let rows = text.strip_prefix("[\n").and_then(|t| t.strip_suffix("\n]\n")).unwrap_or_default();
                format!("{}{rows}", if first { "[\n" } else { ",\n" })
            }
            Paged::Inserts { table, engine } => sql_inserts(&self.shape, &all, table, *engine),
        };
        self.out.write_all(text.as_bytes())
    }

    /// Ends the file (JSON's closing bracket) and flushes it.
    pub fn finish(mut self) -> std::io::Result<W> {
        let end = match (&self.format, self.rows) {
            (Paged::Json, 0) => "[]\n".to_string(),
            (Paged::Json, _) => "\n]\n".to_string(),
            (Paged::Csv, 0) => csv(&self.shape, &[]),
            _ => String::new(),
        };
        self.out.write_all(end.as_bytes())?;
        self.out.flush()?;
        Ok(self.out)
    }
}

/// Every row in `format`.
pub fn render_all_rows(outcome: &QueryOutcome, format: Format) -> String {
    render(outcome, &(0..outcome.rows.len()).collect::<Vec<_>>(), format)
}

/// The rows in `view` (indices into `outcome.rows`) in `format`.
pub fn render(outcome: &QueryOutcome, view: &[usize], format: Format) -> String {
    match format {
        Format::Csv => csv(outcome, view),
        Format::Tsv => tsv(outcome, view),
        Format::Json => json(outcome, view),
        Format::Table => table(outcome, view),
    }
}

/// Aligned columns with a header rule, like psql.
fn table(outcome: &QueryOutcome, view: &[usize]) -> String {
    const MAX_WIDTH: usize = 60;
    let cells: Vec<Vec<String>> = view
        .iter()
        .map(|&i| &outcome.rows[i])
        .map(|r| {
            r.iter()
                .map(|c| c.as_deref().map_or_else(|| "NULL".to_string(), |v| display_cell(v).into_owned()))
                .collect()
        })
        .collect();
    let width = |s: &str| unicode_width::UnicodeWidthStr::width(s);
    let widths: Vec<usize> = (0..outcome.columns.len())
        .map(|i| {
            let data = cells.iter().map(|r| width(&r[i])).max().unwrap_or(0);
            width(&outcome.columns[i]).max(data).min(MAX_WIDTH)
        })
        .collect();
    let fit = |s: &str, w: usize| {
        let mut out = String::new();
        let mut used = 0;
        for c in s.chars() {
            let cw = unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
            if used + cw > w {
                out.pop();
                out.push('…');
                break;
            }
            out.push(c);
            used += cw;
        }
        let pad = w.saturating_sub(width(&out));
        out + &" ".repeat(pad)
    };
    let line = |row: &[String]| {
        let parts: Vec<String> = row.iter().zip(&widths).map(|(c, w)| fit(c, *w)).collect();
        parts.join(" | ").trim_end().to_string() + "\n"
    };
    let mut out = line(&outcome.columns);
    out += &(widths.iter().map(|w| "-".repeat(*w)).collect::<Vec<_>>().join("-+-") + "\n");
    for row in &cells {
        out += &line(row);
    }
    let _ = writeln!(out, "({} row{})", view.len(), if view.len() == 1 { "" } else { "s" });
    out
}

/// The widest an [`ascii_table`] column gets: a longer value (a JSON
/// document, a log) is cut there with `…`, not padded into every row.
const ASCII_COLUMN_MAX: usize = 120;

/// An ASCII table, drawn with `+`, `-` and `|`: each column as wide as
/// its widest value (up to [`ASCII_COLUMN_MAX`]; its name whole), `NULL` for a null, line
/// breaks (`\r\n` too), tabs and other control characters shown as a space
/// so a row stays one line.
pub fn ascii_table(outcome: &QueryOutcome, view: &[usize]) -> String {
    use unicode_width::UnicodeWidthChar;
    let width = |s: &str| unicode_width::UnicodeWidthStr::width(s);
    // Line breaks, tabs and other control characters (a log's colours) as
    // a space: they'd break the lines or the widths.
    let flat = |v: &str| v.replace("\r\n", " ").replace(char::is_control, " ");
    let clean = |v: &str| {
        let flat = flat(v);
        if width(&flat) <= ASCII_COLUMN_MAX {
            return flat;
        }
        let mut used = 0;
        let mut cut: String = flat
            .chars()
            .take_while(|c| {
                used += c.width().unwrap_or(0);
                used < ASCII_COLUMN_MAX
            })
            .collect();
        cut.push('…');
        cut
    };
    let cells: Vec<Vec<String>> = view
        .iter()
        .map(|&i| outcome.rows[i].iter().map(|c| c.as_deref().map_or_else(|| "NULL".to_string(), clean)).collect())
        .collect();
    // Names whole, however long: the header says which column it is.
    let header: Vec<String> = outcome.columns.iter().map(|c| flat(c)).collect();
    let widths: Vec<usize> = (0..header.len())
        .map(|i| cells.iter().map(|r| width(&r[i])).chain([width(&header[i])]).max().unwrap_or(0))
        .collect();
    let rule = format!("+{}+\n", widths.iter().map(|w| "-".repeat(*w)).collect::<Vec<_>>().join("+"));
    let line = |row: &[String]| {
        let parts: Vec<String> =
            row.iter().zip(&widths).map(|(c, w)| format!("{c}{}", " ".repeat(w - width(c)))).collect();
        format!("|{}|\n", parts.join("|"))
    };
    let mut out = rule.clone();
    out += &line(&header);
    out += &rule;
    for row in &cells {
        out += &line(row);
    }
    out + &rule
}

/// Lines up tab-separated output into columns (the text view doesn't
/// show tabs).
pub fn align_tabs(text: &str) -> String {
    use unicode_width::UnicodeWidthStr;
    const MAX: usize = 40;
    if !text.contains('\t') {
        return text.to_string();
    }
    let rows: Vec<Vec<&str>> = text.lines().map(|l| l.split('\t').collect()).collect();
    let mut widths: Vec<usize> = Vec::new();
    for row in &rows {
        for (i, cell) in row.iter().enumerate() {
            let w = cell.width().min(MAX);
            match widths.get_mut(i) {
                Some(cur) => *cur = (*cur).max(w),
                None => widths.push(w),
            }
        }
    }
    let mut out = String::new();
    for row in rows {
        let last = row.len().saturating_sub(1);
        for (i, cell) in row.into_iter().enumerate() {
            out.push_str(cell);
            if i < last {
                out.push_str(&" ".repeat(widths[i].saturating_sub(cell.width()) + 2));
            }
        }
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn outcome(columns: &[&str], rows: &[&[Option<&str>]]) -> QueryOutcome {
        QueryOutcome {
            columns: columns.iter().map(ToString::to_string).collect(),
            column_types: columns.iter().map(|_| String::new()).collect(),
            rows: rows.iter().map(|r| r.iter().map(|c| c.map(str::to_string)).collect()).collect(),
            total_rows: rows.len() as u64,
            ..QueryOutcome::default()
        }
    }

    #[test]
    fn csv_escaping() {
        let o = outcome(&["a", "b,c"], &[&[Some("x\"y"), None], &[Some("1\n2"), Some("plain")]]);
        assert_eq!(csv(&o, &[0, 1]), "a,\"b,c\"\r\n\"x\"\"y\",\r\n\"1\n2\",plain\r\n");
    }

    #[test]
    fn ascii_table_draws_borders() {
        let o = outcome(&["id", "name"], &[&[Some("1"), Some("Žąsis\nthe goose")], &[Some("22"), None]]);
        let want = "+--+---------------+\n\
                    |id|name           |\n\
                    +--+---------------+\n\
                    |1 |Žąsis the goose|\n\
                    |22|NULL           |\n\
                    +--+---------------+\n";
        assert_eq!(ascii_table(&o, &[0, 1]), want);
        // A header with a line break stays one line too.
        let o = outcome(&["a\nb"], &[&[Some("1")]]);
        assert_eq!(ascii_table(&o, &[0]).lines().nth(1), Some("|a b|"));
        let o = outcome(&["id", "name"], &[&[Some("1"), Some("x")], &[Some("22"), None]]);
        // CRLF is one space; a huge value is cut, not padded into every row.
        let long = "x".repeat(500);
        let wide = outcome(&["v"], &[&[Some("a\r\nb")], &[Some(&long)]]);
        let table = ascii_table(&wide, &[0, 1]);
        assert!(table.contains("|a b"), "{table}");
        let coloured = outcome(&["v"], &[&[Some("\x1b[31mred")]]);
        assert_eq!(ascii_table(&coloured, &[0]).lines().nth(3), Some("| [31mred|"));
        let widest = table.lines().map(|l| l.chars().count()).max();
        assert_eq!(widest, Some(ASCII_COLUMN_MAX + 2), "{table}");
        assert!(table.contains("x…|"), "{table}");
        let named = outcome(&[&long], &[&[Some("1")]]);
        assert!(ascii_table(&named, &[0]).contains(&format!("|{long}|")), "a long name stays whole");
        // Only the rows shown, in their order.
        assert!(ascii_table(&o, &[1]).contains("|22|NULL|\n"), "{}", ascii_table(&o, &[1]));
    }

    #[test]
    fn display_cell_flattens_newlines() {
        assert_eq!(display_cell("a\r\nb\tc"), "a↵b c");
        assert_eq!(display_prefix("a\r\nb\tc", 3), "a↵");
        assert_eq!(display_prefix("žąsis", 2), "žą");
        assert_eq!(display_prefix("ab", 10), "ab");
        assert!(matches!(display_cell("plain"), Cow::Borrowed(_)));
    }

    #[test]
    fn json_export_keeps_column_order_and_dedupes_keys() {
        let o = outcome(&["id", "id", "note"], &[&[Some("1"), Some("2"), None], &[Some("3"), Some("4"), Some("q\"")]]);
        assert_eq!(
            json(&o, &[1, 0]),
            "[\n  {\"id\": \"3\", \"id_2\": \"4\", \"note\": \"q\\\"\"},\n  {\"id\": \"1\", \"id_2\": \"2\", \"note\": null}\n]\n"
        );
        assert_eq!(json(&o, &[]), "[]\n");
    }

    #[test]
    fn sql_insert_export() {
        let mut o = outcome(
            &["id", "Name", "note"],
            &[&[Some("1"), Some("O'Neil"), None], &[Some("2"), Some("Bo"), Some("x")]],
        );
        o.column_types = vec!["INT4".into(), "TEXT".into(), "TEXT".into()];
        assert_eq!(
            sql_inserts(&o, &[1, 0], "public.people", Engine::Postgres),
            "INSERT INTO public.people (id, \"Name\", note) VALUES (2, 'Bo', 'x');\n\
             INSERT INTO public.people (id, \"Name\", note) VALUES (1, 'O''Neil', NULL);\n"
        );
        // Binary values go in as hex literals, not as text.
        let mut o = outcome(&["b", "t"], &[&[Some("0xDEADBEEF"), Some("0xDEADBEEF")]]);
        o.column_types = vec!["BLOB".into(), "TEXT".into()];
        assert_eq!(
            sql_inserts(&o, &[0], "t", Engine::MySql),
            "INSERT INTO t (b, t) VALUES (0xDEADBEEF, '0xDEADBEEF');\n"
        );
    }

    /// Written a page at a time, a file reads the same as written at once.
    #[test]
    fn pages_add_up_to_the_whole_file() {
        let mut o =
            outcome(&["id", "note"], &[&[Some("1"), Some("a,b")], &[Some("2"), None], &[Some("3"), Some("c\"d")]]);
        o.column_types = vec!["INT4".into(), "TEXT".into()];
        let all: Vec<usize> = (0..3).collect();
        let inserts = Paged::Inserts { table: "t".into(), engine: Engine::Postgres };
        for (format, whole) in [
            (Paged::Csv, csv(&o, &all)),
            (Paged::Json, json(&o, &all)),
            (inserts, sql_inserts(&o, &all, "t", Engine::Postgres)),
        ] {
            let mut w = PagedWriter::new(Vec::new(), format.clone(), &o.columns, &o.column_types);
            w.page(o.rows[..1].to_vec()).unwrap();
            w.page(Vec::new()).unwrap();
            w.page(o.rows[1..].to_vec()).unwrap();
            assert_eq!(w.rows, 3);
            assert_eq!(String::from_utf8(w.finish().unwrap()).unwrap(), whole, "{format:?}");
            // No rows at all.
            let w = PagedWriter::new(Vec::new(), format.clone(), &o.columns, &o.column_types);
            let empty = String::from_utf8(w.finish().unwrap()).unwrap();
            assert_eq!(
                empty,
                match format {
                    Paged::Csv => csv(&o, &[]),
                    Paged::Json => json(&o, &[]),
                    Paged::Inserts { .. } => String::new(),
                }
            );
        }
    }

    #[test]
    fn tsv_rows_are_one_line_each() {
        let outcome = QueryOutcome {
            columns: vec!["id".into(), "note".into()],
            rows: vec![vec![Some("1".into()), Some("a\tb\nc".into())], vec![Some("2".into()), None]],
            ..QueryOutcome::default()
        };
        assert_eq!(tsv(&outcome, &[0, 1]), "id\tnote\n1\ta b\\nc\n2\t\n");
        assert_eq!(tsv(&outcome, &[1]), "id\tnote\n2\t\n");
        let old_mac = super::tests::outcome(&["v"], &[&[Some("a\rb\r\nc")]]);
        assert_eq!(tsv(&old_mac, &[0]), "v\na\\nb\\nc\n");
    }

    #[test]
    fn tab_output_is_aligned() {
        assert_eq!(align_tabs("a\tbb\nccc\td\n"), "a    bb\nccc  d\n");
        assert_eq!(align_tabs("no tabs"), "no tabs");
    }
}
