use super::*;
pub use crate::transfer::export::display_prefix;

pub const NULL_TEXT: &str = "NULL";
pub(super) const MAX_COL_WIDTH: usize = 40;

pub(super) fn column_widths(outcome: &QueryOutcome) -> Vec<u16> {
    let headers: Vec<u16> = (0..outcome.columns.len())
        // Room for the " ▲"/" ▼" sort marker next to the name.
        .map(|i| (outcome.columns[i].width() + 2).clamp(1, MAX_COL_WIDTH) as u16)
        .collect();
    widen_columns(headers, &outcome.rows)
}

/// Grows `widths` to fit `rows` (e.g. a newly streamed page), capped at
/// `MAX_COL_WIDTH`, without rescanning rows that were already measured.
pub(super) fn widen_columns(mut widths: Vec<u16>, rows: &[Vec<Option<String>>]) -> Vec<u16> {
    for row in rows {
        for (w, cell) in widths.iter_mut().zip(row) {
            // Past the cap the rest doesn't matter: measure only the start.
            let len = cell
                .as_deref()
                .map_or(NULL_TEXT.len(), |v| display_prefix(v, MAX_COL_WIDTH).width())
                .min(MAX_COL_WIDTH);
            *w = (*w).max(len as u16);
        }
    }
    widths
}

/// How many columns starting at `start` fit in `avail` cells (at least one).
pub(super) fn columns_fitting(widths: &[u16], start: usize, avail: u16) -> usize {
    let mut used = 0u32;
    let mut count = 0;
    for &w in &widths[start..] {
        let need = u32::from(w) + u32::from(count != 0);
        if used + need > u32::from(avail) && count > 0 {
            break;
        }
        used += need;
        count += 1;
    }
    count
}

/// The first visible column and how many are visible, scrolling `offset`
/// just enough to keep `selected` on screen.
pub fn visible_columns(widths: &[u16], offset: usize, selected: usize, avail: u16) -> (usize, usize) {
    if widths.is_empty() {
        return (0, 0);
    }
    let selected = selected.min(widths.len() - 1);
    let mut offset = offset.min(selected);
    while offset + columns_fitting(widths, offset, avail) <= selected {
        offset += 1;
    }
    (offset, columns_fitting(widths, offset, avail))
}

/// Rows to show, as indices into `outcome.rows`: those containing `filter`
/// (case-insensitive, any column), ordered by `sort` (column, descending).
/// Numbers sort numerically; NULLs go last.
pub(super) fn compute_view(outcome: &QueryOutcome, filter: &str, sort: Option<(usize, bool)>) -> Vec<usize> {
    let needle = filter.to_lowercase();
    // One buffer for every cell's lowercased copy, not an allocation each.
    let mut buf = String::new();
    let mut view: Vec<usize> = (0..outcome.rows.len())
        .filter(|&i| {
            needle.is_empty()
                || outcome.rows[i]
                    .iter()
                    .any(|c| contains_ignoring_case(c.as_deref().unwrap_or(NULL_TEXT), &needle, &mut buf))
        })
        .collect();
    if let Some((col, desc)) = sort {
        // Each cell's key once, not on every comparison.
        let mut keyed: Vec<(SortKey, usize)> =
            view.into_iter().map(|i| (SortKey::of(outcome.rows[i][col].as_deref()), i)).collect();
        // Stable, so equal cells keep their order.
        keyed.sort_by(|(a, _), (b, _)| a.cmp(b, desc));
        view = keyed.into_iter().map(|(_, i)| i).collect();
    }
    view
}

/// Where a cell goes when its column is sorted: numbers by value, then
/// text, then NULLs. Comparing numbers as numbers only when both cells are
/// numbers wasn't a consistent order in mixed columns (`2 < 10 < 1a < 2`),
/// and Rust's sort may panic on one.
enum SortKey<'a> {
    Number(f64),
    Text(&'a str),
    Null,
}

impl<'a> SortKey<'a> {
    fn of(cell: Option<&'a str>) -> Self {
        match cell {
            None => SortKey::Null,
            Some(text) => text.trim().parse::<f64>().map_or(SortKey::Text(text), SortKey::Number),
        }
    }

    /// Ascending or descending, with NULLs last either way.
    fn cmp(&self, other: &Self, desc: bool) -> std::cmp::Ordering {
        use std::cmp::Ordering;
        let ord = match (self, other) {
            (SortKey::Null, SortKey::Null) => return Ordering::Equal,
            (SortKey::Null, _) => return Ordering::Greater,
            (_, SortKey::Null) => return Ordering::Less,
            (SortKey::Number(x), SortKey::Number(y)) => x.total_cmp(y),
            (SortKey::Number(_), SortKey::Text(_)) => Ordering::Less,
            (SortKey::Text(_), SortKey::Number(_)) => Ordering::Greater,
            (SortKey::Text(x), SortKey::Text(y)) => x.cmp(y),
        };
        if desc { ord.reverse() } else { ord }
    }
}

/// Whether `haystack` contains `needle` (already lowercase), ignoring case.
/// An ASCII needle, the usual case, only needs ASCII lowercased, in `buf`,
/// even in UTF-8 text: its bytes can't match inside another character.
/// Then the standard library's fast search does the rest.
pub(super) fn contains_ignoring_case(haystack: &str, needle: &str, buf: &mut String) -> bool {
    lowered_for(haystack, needle, buf).is_some_and(|h| h.contains(needle))
}

/// `haystack` lowercased (in `buf`) as far as finding `needle` in it needs;
/// `None` when it can't hold it. What [`contains_ignoring_case`] searches.
pub(super) fn lowered_for<'b>(haystack: &str, needle: &str, buf: &'b mut String) -> Option<&'b str> {
    buf.clear();
    if needle.is_ascii() {
        buf.push_str(haystack);
        buf.make_ascii_lowercase();
    } else if haystack.is_ascii() {
        // A non-ASCII letter only comes from lowercasing a non-ASCII one.
        return None;
    } else {
        // As the needle was lowercased (a final sigma is `ς`, not `σ`).
        buf.push_str(&haystack.to_lowercase());
    }
    Some(buf)
}

/// The full value of a cell for the detail view: JSON is pretty-printed.
pub(super) fn pretty_value(value: &str) -> String {
    let trimmed = value.trim_start();
    if (trimmed.starts_with('{') || trimmed.starts_with('['))
        && let Ok(json) = serde_json::from_str::<serde_json::Value>(value)
    {
        return serde_json::to_string_pretty(&json).unwrap_or_else(|_| value.to_string());
    }
    value.to_string()
}

pub(super) fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub fn format_elapsed(d: Duration) -> String {
    if d.as_millis() < 1000 { format!("{} ms", d.as_millis()) } else { format!("{:.2} s", d.as_secs_f64()) }
}

/// The inline "SET column = …" prompt for editing a result cell.
pub struct CellEdit {
    /// Index into `results.rows` and the column.
    pub row: usize,
    pub col: usize,
    pub column: String,
    /// Quoted, possibly schema-qualified table name.
    pub table: String,
    /// Primary key column names and this row's values, as literals.
    pub key: Vec<(String, String)>,
    /// The column's type, as the result names it.
    pub column_type: String,
    pub nullable: bool,
    pub text: LineInput,
}

pub struct CellView {
    pub title: String,
    pub text: String,
    pub scroll: u16,
    /// Showing the selected row as `column  value` lines (`x`): ←/→ move
    /// to the previous/next row.
    pub record: bool,
    /// SQL that comes with the text (a schema comparison's), and the
    /// connection it's for: `y` copies it, `e` opens it in a tab there.
    pub sql: Option<(String, u64)>,
    /// How far it scrolls, as last drawn (the text wraps to the popup).
    pub max_scroll: u16,
    /// A definition opened from the navigator (`d`): ←/→ show the previous
    /// or next table's, view's or function's.
    pub browse: bool,
    /// `y` copies the text itself (a message), saying this.
    pub copies_text: Option<&'static str>,
    /// A row a foreign key points to (`f`): `e` opens its lookup (the
    /// tab's name, the SQL) in a tab of its own.
    pub follow: Option<(String, String)>,
}

impl CellView {
    pub fn new(title: String, text: String) -> Self {
        CellView {
            title,
            text,
            scroll: 0,
            record: false,
            sql: None,
            max_scroll: u16::MAX,
            browse: false,
            copies_text: None,
            follow: None,
        }
    }

    /// A navigator definition (`d`), browsable with ←/→.
    pub fn definition(title: String, text: String) -> Self {
        CellView { browse: true, ..CellView::new(title, text) }
    }
}

impl App {
    #[cfg(test)]
    pub fn set_status_for_tests(&mut self, status: &str) {
        self.set_status(status.to_string(), false);
    }

    #[cfg(test)]
    pub fn show_results_for_tests(&mut self, outcome: QueryOutcome) {
        self.show_results(outcome);
    }

    /// A run of the editor's `range` starting, as `run_now` would: its
    /// own result, kept.
    #[cfg(test)]
    pub fn open_run_for_tests(&mut self, range: std::ops::Range<usize>) {
        let text = self.tab.editor.text();
        self.open_run(Some((&text[range.clone()], range.start)));
    }

    #[cfg(test)]
    pub fn show_plan_for_tests(&mut self, outcome: QueryOutcome) {
        self.show_plan(outcome);
    }

    pub(super) fn show_results(&mut self, outcome: QueryOutcome) {
        self.close_cell_popups();
        let sets = self.tab.grid.show(outcome);
        let Some(outcome) = &self.tab.grid.results else { return };

        let elapsed = format_elapsed(outcome.elapsed);
        let mut status = if outcome.statements == 0 {
            t!("run.nothing").to_string()
        } else if !outcome.columns.is_empty() {
            if outcome.truncated() {
                t!("run.showing_first", shown = outcome.rows.len(), total = outcome.total_rows, time = elapsed)
            } else {
                tn!("run.rows_in", outcome.rows.len(), time = elapsed)
            }
        } else {
            tn!("run.affected_in", outcome.rows_affected.unwrap_or(0), time = elapsed)
        };
        if outcome.statements > 1 {
            let _ = write!(status, " · {}", tn!("run.statements", outcome.statements));
        }
        if sets > 1 {
            let _ = write!(status, " · {}", tn!("run.result_sets", sets));
        }
        self.set_status(status, false);
    }

    /// The popups about one cell of the old result.
    pub(super) fn close_cell_popups(&mut self) {
        self.popups.cell_view = None;
        self.cell_edit = None;
    }

    /// Shows result set `index` of a script's results.
    pub(super) fn show_result_set(&mut self, index: usize) {
        if !self.tab.grid.select_set(index) {
            return;
        }
        self.close_cell_popups();
        let Some(next) = &self.tab.grid.results else { return };
        let from = if next.statement > 0 {
            format!(", {}", t!("run.from_statement", n = next.statement))
        } else {
            String::new()
        };
        let status = t!(
            "run.result_set",
            n = index + 1,
            of = self.tab.grid.result_sets.len(),
            from = from,
            rows = tn!("rows.rows", next.total_rows)
        );
        self.set_status(status, false);
        self.set_result_tab(ResultTab::Rows);
    }

    /// The next (or previous) result set, wrapping around.
    pub(super) fn step_result_set(&mut self, forward: bool) {
        match self.tab.grid.step_set(forward) {
            Some(index) => self.show_result_set(index),
            None => {
                self.set_status(t!("run.one_result_set").into(), false);
            }
        }
    }
}
