//! A quick chart of a result column, as text: a bar per row (labelled by
//! another column), and the trend as a sparkline, with the count, sum and
//! range. For a look at the numbers without leaving the terminal.

use std::fmt::Write as _;

use crate::db::QueryOutcome;

/// How wide the longest bar is.
const BAR_WIDTH: usize = 40;
/// Bars past this many rows are left out (the sparkline has them all).
const MAX_BARS: usize = 200;
/// Points the sparkline is drawn with.
const SPARK_WIDTH: usize = 60;

/// The chart of column `col` for the rows in `view` (as shown: filtered and
/// sorted), or why there's none.
pub fn chart(outcome: &QueryOutcome, view: &[usize], col: usize) -> Result<String, String> {
    let name = outcome.columns.get(col).ok_or("no such column")?;
    let label_col = (0..outcome.columns.len()).find(|&c| c != col);
    let points: Vec<(String, f64)> = view
        .iter()
        .filter_map(|&r| {
            let row = outcome.rows.get(r)?;
            let value = row.get(col)?.as_deref()?.trim().parse::<f64>().ok().filter(|v| v.is_finite())?;
            let label = label_col.and_then(|c| row.get(c)?.clone()).unwrap_or_else(|| (r + 1).to_string());
            Some((label, value))
        })
        .collect();
    if points.is_empty() {
        return Err(format!("{name} has no numbers to chart"));
    }
    let values: Vec<f64> = points.iter().map(|p| p.1).collect();
    let sum: f64 = values.iter().sum();
    let min = values.iter().copied().fold(f64::INFINITY, f64::min);
    let max = values.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    #[allow(clippy::cast_precision_loss)] // a count of rows
    let avg = sum / values.len() as f64;

    let mut out = format!(
        "{name}: {} value(s) · sum {} · min {} · max {} · average {}\n",
        values.len(),
        number(sum),
        number(min),
        number(max),
        number(avg)
    );
    let _ = writeln!(out, "trend  {}\n", sparkline(&values));

    let scale = values.iter().map(|v| v.abs()).fold(0.0, f64::max);
    let label_w = points.iter().take(MAX_BARS).map(|p| p.0.chars().count().min(24)).max().unwrap_or(0);
    for (label, value) in points.iter().take(MAX_BARS) {
        let label: String = label.chars().take(24).collect();
        let _ = writeln!(out, "{label:>label_w$} │{} {}", bar(*value, scale), number(*value));
    }
    if points.len() > MAX_BARS {
        let _ = writeln!(out, "… {} more rows: the trend above has them all", points.len() - MAX_BARS);
    }
    Ok(out)
}

/// A bar `value / scale` of [`BAR_WIDTH`], in eighths; light for a negative
/// value.
fn bar(value: f64, scale: f64) -> String {
    const EIGHTHS: [char; 8] = [' ', '▏', '▎', '▍', '▌', '▋', '▊', '▉'];
    if scale <= 0.0 {
        return String::new();
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_precision_loss)]
    let eighths = ((value.abs() / scale) * (BAR_WIDTH * 8) as f64).round() as usize;
    let full = if value < 0.0 { '░' } else { '█' };
    let mut s: String = std::iter::repeat_n(full, eighths / 8).collect();
    if value >= 0.0 && !eighths.is_multiple_of(8) {
        s.push(EIGHTHS[eighths % 8]);
    }
    s
}

/// The values as one line of block heights, shrunk to [`SPARK_WIDTH`] by
/// averaging neighbours.
fn sparkline(values: &[f64]) -> String {
    const LEVELS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    let n = values.len().min(SPARK_WIDTH);
    let points: Vec<f64> = (0..n)
        .map(|i| {
            let (a, b) = (i * values.len() / n, ((i + 1) * values.len() / n).max(i * values.len() / n + 1));
            #[allow(clippy::cast_precision_loss)]
            let avg = values[a..b].iter().sum::<f64>() / (b - a) as f64;
            avg
        })
        .collect();
    let (lo, hi) = points.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), v| (lo.min(*v), hi.max(*v)));
    points
        .iter()
        .map(|v| {
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let level = if hi > lo { (((v - lo) / (hi - lo)) * 7.0).round() as usize } else { 3 };
            LEVELS[level.min(7)]
        })
        .collect()
}

/// A number without needless decimals: `12`, `12.5`, `0.333`.
fn number(v: f64) -> String {
    if v.fract() == 0.0 && v.abs() < 1e15 {
        format!("{v:.0}")
    } else {
        let s = format!("{v:.3}");
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn outcome(rows: &[(&str, Option<&str>)]) -> QueryOutcome {
        QueryOutcome {
            columns: vec!["day".into(), "sales".into()],
            rows: rows.iter().map(|(d, v)| vec![Some((*d).to_string()), v.map(str::to_string)]).collect(),
            ..QueryOutcome::default()
        }
    }

    #[test]
    fn bars_labels_and_totals() {
        let o = outcome(&[("mon", Some("10")), ("tue", Some("20")), ("wed", None), ("thu", Some("5.25"))]);
        let text = chart(&o, &[0, 1, 2, 3], 1).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], "sales: 3 value(s) · sum 35.25 · min 5.25 · max 20 · average 11.75");
        assert!(lines[1].starts_with("trend  "));
        assert_eq!(lines[3], format!("mon │{} 10", "█".repeat(20)));
        assert_eq!(lines[4], format!("tue │{} 20", "█".repeat(40)));
        assert_eq!(lines[5], format!("thu │{}▌ 5.25", "█".repeat(10)), "eighths for the rest");
        // As shown: the view's order.
        assert!(chart(&o, &[1, 0], 1).unwrap().lines().nth(3).unwrap().starts_with("tue"));
        assert_eq!(chart(&o, &[0], 0).unwrap_err(), "day has no numbers to chart");
    }

    #[test]
    fn sparklines_fit_and_follow_the_values() {
        let up: Vec<f64> = (0..500).map(f64::from).collect();
        let line = sparkline(&up);
        assert_eq!(line.chars().count(), SPARK_WIDTH);
        assert!(line.starts_with('▁') && line.ends_with('█'));
        assert_eq!(sparkline(&[3.0, 3.0]), "▄▄");
    }
}
