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
    if needle.is_ascii() {
        buf.clear();
        buf.push_str(haystack);
        buf.make_ascii_lowercase();
        buf.contains(needle)
    } else {
        // A non-ASCII letter only comes from lowercasing a non-ASCII one.
        !haystack.is_ascii() && haystack.to_lowercase().contains(needle)
    }
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
