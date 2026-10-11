//! MySQL's `EXPLAIN FORMAT=TREE` / `EXPLAIN ANALYZE` text, with notes on
//! what stands out: the slowest (or costliest) step, full scans of big
//! tables and row estimates far off the actual counts.

use crate::db::dialect::{BIG_SCAN_ROWS, Plan, PlanNote, human_rows, misestimated};

/// One `-> …` line of the tree.
struct Step {
    line: usize,
    indent: usize,
    head: String,
    /// The estimate: cost (including the steps below) and rows.
    cost: Option<f64>,
    rows: Option<f64>,
    /// EXPLAIN ANALYZE: time per loop (ms, including the steps below),
    /// rows per loop, and loops.
    time: Option<f64>,
    actual_rows: Option<f64>,
    loops: f64,
}

/// The number after `key=` in `text` (`rows=1.5e+6`, `time=0.04..0.05`
/// gives the last).
fn field(text: &str, key: &str) -> Option<f64> {
    let rest = &text[text.find(key)? + key.len()..];
    let value = rest.split([' ', ')']).next()?;
    value.rsplit("..").next()?.parse().ok()
}

fn parse(line: usize, text: &str) -> Option<Step> {
    let trimmed = text.trim_start();
    let body = trimmed.strip_prefix("-> ")?;
    let head = body.split("  (").next().unwrap_or(body).split(" (cost=").next().unwrap_or(body).trim();
    let estimate = body.find("(cost=").map(|i| &body[i..]);
    let actual = body.find("(actual ").map(|i| &body[i..]);
    Some(Step {
        line,
        indent: text.len() - trimmed.len(),
        head: head.to_string(),
        cost: estimate.and_then(|e| field(e, "cost=")),
        rows: estimate.and_then(|e| field(e, "rows=")),
        time: actual.and_then(|a| field(a, "time=")),
        actual_rows: actual.and_then(|a| field(a, "rows=")),
        loops: actual.and_then(|a| field(a, "loops=")).unwrap_or(1.0),
    })
}

pub(super) fn analyze(text: String) -> Plan {
    let steps: Vec<Step> = text.lines().enumerate().filter_map(|(i, l)| parse(i, l)).collect();
    let mut notes = Vec::new();
    let mut heaviest: Option<(f64, &Step)> = None;
    let analyzed = steps.iter().any(|s| s.time.is_some());
    for (i, step) in steps.iter().enumerate() {
        let note = |text: String| PlanNote { text, line: Some(step.line) };
        let read = step.actual_rows.map(|r| r * step.loops).or(step.rows);
        if let Some(table) = step.head.strip_prefix("Table scan on ")
            && read.is_some_and(|r| r >= BIG_SCAN_ROWS)
        {
            let table = table.split_whitespace().next().unwrap_or(table);
            notes.push(note(format!(
                "Full table scan on {table} ({} rows): an index on the filtered or joined columns may help",
                human_rows(read.unwrap_or(0.0))
            )));
        }
        if let (Some(estimated), Some(actual)) = (step.rows, step.actual_rows)
            && misestimated(estimated, actual)
        {
            notes.push(note(format!(
                "{}: estimated {} rows, got {}: ANALYZE TABLE may help",
                step.head,
                human_rows(estimated),
                human_rows(actual)
            )));
        }
        // Its own share: its total less its direct children's.
        let children = steps[i + 1..].iter().take_while(|c| c.indent > step.indent);
        let child_indent = steps.get(i + 1).filter(|c| c.indent > step.indent).map(|c| c.indent);
        let direct: Vec<&Step> = children.filter(|c| Some(c.indent) == child_indent).collect();
        let total = |s: &Step| if analyzed { s.time.map(|t| t * s.loops) } else { s.cost };
        let own = total(step).map(|t| (t - direct.iter().filter_map(|c| total(c)).sum::<f64>()).max(0.0));
        if let Some(own) = own
            && heaviest.is_none_or(|(most, _)| own > most)
        {
            heaviest = Some((own, step));
        }
    }
    let whole = steps.first().and_then(|s| if analyzed { s.time.map(|t| t * s.loops) } else { s.cost });
    if let (Some((own, step)), Some(whole)) = (heaviest, whole)
        && steps.len() > 1
        && whole > 0.0
    {
        let share = own / whole * 100.0;
        let text = if analyzed {
            format!("Slowest step: {}, {own:.1} ms of {whole:.1} ms ({share:.0}%)", step.head)
        } else {
            format!("Costliest step: {}, {share:.0}% of the estimated cost", step.head)
        };
        notes.insert(0, PlanNote { text, line: Some(step.line) });
    }
    Plan { text, notes }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notes_big_scans_misestimates_and_the_slowest_step() {
        let text = "-> Filter: (t.a > 5)  (cost=1000 rows=3) (actual time=0.1..60 rows=5000 loops=1)\n    \
                    -> Table scan on t  (cost=990 rows=50000) (actual time=0.05..50 rows=50000 loops=1)";
        let plan = analyze(text.to_string());
        let notes: Vec<(&str, Option<usize>)> = plan.notes.iter().map(|n| (n.text.as_str(), n.line)).collect();
        assert_eq!(
            notes,
            [
                ("Slowest step: Table scan on t, 50.0 ms of 60.0 ms (83%)", Some(1)),
                ("Filter: (t.a > 5): estimated 3 rows, got 5.0k: ANALYZE TABLE may help", Some(0)),
                ("Full table scan on t (50.0k rows): an index on the filtered or joined columns may help", Some(1)),
            ]
        );
        assert_eq!(plan.text, text);
    }

    #[test]
    fn small_plans_have_nothing_to_say() {
        assert!(analyze("-> Table scan on t  (cost=0.35 rows=1)".into()).notes.is_empty());
    }
}
