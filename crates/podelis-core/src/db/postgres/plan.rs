//! Postgres's JSON EXPLAIN output as an indented tree, with notes on what
//! stands out: the slowest (or costliest) step, big filtered scans, row
//! estimates far off the actual counts, sorts and hashes that spill to disk.

use std::fmt::Write as _;

use serde_json::Value;

use crate::db::dialect::{BIG_SCAN_ROWS, Plan, PlanNote, Scan, human_rows, misestimated};

/// The tables a JSON plan reads in full (sequential scans); it doesn't
/// say how many rows that is, only how many it keeps.
pub(super) fn full_scans(json: &str) -> Vec<Scan> {
    /// `limited`: a `Limit` above stops reading early, with nothing in
    /// between that needs every row first.
    fn walk(node: &Value, limited: bool, out: &mut Vec<Scan>) {
        let node_type = node.get("Node Type").and_then(Value::as_str).unwrap_or_default();
        // A filter may have to read the whole table to find a few rows.
        let stops_early = limited && node.get("Filter").is_none();
        if node_type.ends_with("Seq Scan")
            && !stops_early
            && let Some(table) = node.get("Relation Name").and_then(Value::as_str)
        {
            // Without a filter the planner's rows are the table's.
            let rows = if node.get("Filter").is_none() { node.get("Plan Rows").and_then(Value::as_f64) } else { None };
            let schema = node.get("Schema").and_then(Value::as_str).map(str::to_string);
            out.push(Scan { schema, ..Scan::new(table, rows) });
        }
        let limited = match node_type {
            "Limit" => true,
            // These read all their input before giving a row.
            "Sort" | "Incremental Sort" | "Aggregate" | "Hash" | "Materialize" | "WindowAgg" | "SetOp" | "Group"
            | "Memoize" => false,
            _ => limited,
        };
        for (i, child) in node.get("Plans").and_then(Value::as_array).into_iter().flatten().enumerate() {
            // A nested loop reads its inner side again for each outer row.
            walk(child, limited && !(node_type == "Nested Loop" && i > 0), out);
        }
    }
    let mut out = Vec::new();
    if let Ok(value) = serde_json::from_str::<Value>(json)
        && let Some(plan) = value.get(0).and_then(|v| v.get("Plan"))
    {
        walk(plan, false, &mut out);
    }
    out
}

pub(super) fn format(json: &str) -> Option<Plan> {
    let value: Value = serde_json::from_str(json).ok()?;
    let root = value.get(0)?;
    let mut tree = Tree::default();
    let top = tree.node(root.get("Plan")?, 0);
    let mut notes = std::mem::take(&mut tree.notes);
    // The step that took (or is expected to take) the largest share.
    if let Some((own, line, head)) = tree.heaviest.take()
        && tree.lines.len() > 1
    {
        let text = match top.time {
            Some(total) if total > 0.0 => {
                format!("Slowest step: {head}, {own:.1} ms of {total:.1} ms ({:.0}%)", own / total * 100.0)
            }
            _ if top.cost > 0.0 => {
                format!("Costliest step: {head}, {:.0}% of the estimated cost", own / top.cost * 100.0)
            }
            _ => String::new(),
        };
        if !text.is_empty() {
            notes.insert(0, PlanNote { text, line: Some(line) });
        }
    }
    let mut text = tree.lines.join("\n");
    for key in ["Planning Time", "Execution Time"] {
        if let Some(ms) = root.get(key).and_then(Value::as_f64) {
            let _ = write!(text, "\n{key}: {ms:.3} ms");
        }
    }
    Some(Plan { text, notes })
}

/// A node's totals, including its children: actual time (EXPLAIN ANALYZE)
/// and estimated cost.
struct Totals {
    time: Option<f64>,
    cost: f64,
}

#[derive(Default)]
struct Tree {
    lines: Vec<String>,
    notes: Vec<PlanNote>,
    /// The step with the most time (or cost) of its own: amount, line, name.
    heaviest: Option<(f64, usize, String)>,
}

impl Tree {
    fn note(&mut self, line: usize, text: String) {
        self.notes.push(PlanNote { text, line: Some(line) });
    }

    /// Notes what stands out about the step at line `at`.
    fn flag(&mut self, at: usize, node: &Value, head: &str) {
        let text = |key: &str| node.get(key).and_then(Value::as_str);
        let num = |key: &str| node.get(key).and_then(Value::as_f64);
        let relation = text("Relation Name");
        let loops = num("Actual Loops").unwrap_or(1.0);
        let actual = num("Actual Total Time").zip(num("Actual Rows"));
        let removed = num("Rows Removed by Filter");
        let sort_method = text("Sort Method");
        let batches = num("Hash Batches");
        let filtered = text("Filter").is_some();
        let on = relation.map(|r| format!(" on {r}")).unwrap_or_default();
        if head.starts_with("Seq Scan") && filtered {
            match (actual, removed) {
                (Some((_, rows)), Some(removed)) if (rows + removed) * loops >= BIG_SCAN_ROWS => self.note(
                    at,
                    format!(
                        "Seq Scan{on} read {} rows to keep {}: an index on the Filter's columns may help",
                        human_rows((rows + removed) * loops),
                        human_rows(rows * loops)
                    ),
                ),
                (None, _) if num("Plan Rows").is_some_and(|r| r >= BIG_SCAN_ROWS) => self.note(
                    at,
                    format!("Seq Scan{on} with a filter over a big table: an index on the Filter's columns may help"),
                ),
                _ => {}
            }
        }
        if let (Some(estimated), Some((_, rows))) = (num("Plan Rows"), actual)
            && misestimated(estimated, rows)
        {
            let fix =
                relation.map_or_else(|| "statistics may be stale".to_string(), |r| format!("ANALYZE {r} may help"));
            self.note(at, format!("{head}: estimated {} rows, got {}: {fix}", human_rows(estimated), human_rows(rows)));
        }
        if sort_method.is_some_and(|m| m.contains("external")) {
            self.note(at, "Sort spilled to disk: more work_mem would keep it in memory".to_string());
        }
        if batches.is_some_and(|n| n > 1.0) {
            self.note(at, "Hash spilled to disk in batches: more work_mem would keep it in memory".to_string());
        }
    }

    fn node(&mut self, node: &Value, depth: usize) -> Totals {
        let text = |key: &str| node.get(key).and_then(Value::as_str);
        let num = |key: &str| node.get(key).and_then(Value::as_f64);

        let mut head = text("Node Type").unwrap_or("?").to_string();
        if let Some(join) = text("Join Type").filter(|j| *j != "Inner") {
            head = format!("{join} {head}");
        }
        if let Some(idx) = text("Index Name") {
            let _ = write!(head, " using {idx}");
        }
        let relation = text("Relation Name");
        if let Some(rel) = relation {
            let _ = write!(head, " on {rel}");
            if let Some(alias) = text("Alias").filter(|a| *a != rel) {
                let _ = write!(head, " {alias}");
            }
        }
        let indent = "      ".repeat(depth.saturating_sub(1));
        let arrow = if depth == 0 { "" } else { "  ->  " };
        let mut line = format!("{indent}{arrow}{head}");
        if let (Some(a), Some(b), Some(rows)) = (num("Startup Cost"), num("Total Cost"), num("Plan Rows")) {
            let _ = write!(line, "  (cost={a:.2}..{b:.2} rows={rows})");
        }
        // EXPLAIN ANALYZE: what actually happened.
        let loops = num("Actual Loops").unwrap_or(1.0);
        let actual = num("Actual Total Time").zip(num("Actual Rows"));
        if let Some((time, rows)) = actual {
            let _ = write!(line, "  (actual time={time:.3} ms rows={rows} loops={loops})");
        }
        let at = self.lines.len();
        self.lines.push(line);

        let detail_indent = if depth == 0 { "  ".to_string() } else { format!("{indent}        ") };
        let mut detail = |s: String| self.lines.push(format!("{detail_indent}{s}"));
        for key in ["Index Cond", "Hash Cond", "Merge Cond", "Join Filter", "Filter", "Recheck Cond"] {
            if let Some(v) = text(key) {
                detail(format!("{key}: {v}"));
            }
        }
        let removed = num("Rows Removed by Filter");
        if let Some(n) = removed.filter(|n| *n > 0.0) {
            detail(format!("Rows Removed by Filter: {n}"));
        }
        if let (Some(hit), Some(read)) = (num("Shared Hit Blocks"), num("Shared Read Blocks"))
            && hit + read > 0.0
        {
            detail(format!("Buffers: shared hit={hit} read={read}"));
        }
        for key in ["Sort Key", "Group Key"] {
            if let Some(keys) = node.get(key).and_then(Value::as_array) {
                let list: Vec<&str> = keys.iter().filter_map(Value::as_str).collect();
                detail(format!("{key}: {}", list.join(", ")));
            }
        }
        let sort_method = text("Sort Method");
        if let Some(method) = sort_method {
            let space =
                num("Sort Space Used").map(|kb| format!("  {}: {kb}kB", text("Sort Space Type").unwrap_or("Space")));
            detail(format!("Sort Method: {method}{}", space.unwrap_or_default()));
        }
        let batches = num("Hash Batches");
        if let Some(n) = batches.filter(|n| *n > 1.0) {
            detail(format!("Hash Batches: {n}"));
        }

        self.flag(at, node, &head);

        let mut children = Totals { time: Some(0.0), cost: 0.0 };
        for child in node.get("Plans").and_then(Value::as_array).into_iter().flatten() {
            let t = self.node(child, depth + 1);
            children.time = children.time.zip(t.time).map(|(a, b)| a + b);
            children.cost += t.cost;
        }
        let totals = Totals { time: actual.map(|(time, _)| time * loops), cost: num("Total Cost").unwrap_or(0.0) };
        let own = match totals.time {
            Some(time) => (time - children.time.unwrap_or(0.0)).max(0.0),
            None => (totals.cost - children.cost).max(0.0),
        };
        if self.heaviest.as_ref().is_none_or(|(most, ..)| own > *most) {
            self.heaviest = Some((own, at, head));
        }
        totals
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn postgres_json_plan_as_tree() {
        let json = r#"[{"Plan": {"Node Type": "Hash Join", "Join Type": "Left", "Startup Cost": 1.5,
            "Total Cost": 20.25, "Plan Rows": 10, "Hash Cond": "(o.user_id = u.id)",
            "Plans": [
              {"Node Type": "Seq Scan", "Relation Name": "orders", "Alias": "o", "Startup Cost": 0,
               "Total Cost": 15, "Plan Rows": 500, "Filter": "(total > 10)"},
              {"Node Type": "Hash", "Startup Cost": 1, "Total Cost": 1, "Plan Rows": 3,
               "Plans": [{"Node Type": "Index Scan", "Index Name": "users_pkey",
                          "Relation Name": "users", "Alias": "u", "Startup Cost": 0.1, "Total Cost": 1,
                          "Plan Rows": 3, "Index Cond": "(id < 4)"}]}
            ]}, "Planning Time": 0.25}]"#;
        let plan = format(json).unwrap();
        assert_eq!(
            plan.text,
            "Left Hash Join  (cost=1.50..20.25 rows=10)\n\
             \x20 Hash Cond: (o.user_id = u.id)\n\
             \x20 ->  Seq Scan on orders o  (cost=0.00..15.00 rows=500)\n\
             \x20       Filter: (total > 10)\n\
             \x20 ->  Hash  (cost=1.00..1.00 rows=3)\n\
             \x20       ->  Index Scan using users_pkey on users u  (cost=0.10..1.00 rows=3)\n\
             \x20             Index Cond: (id < 4)\n\
             Planning Time: 0.250 ms"
        );
    }

    #[test]
    fn notes_what_stands_out_in_an_analyzed_plan() {
        let json = r#"[{"Plan": {"Node Type": "Sort", "Startup Cost": 900, "Total Cost": 950, "Plan Rows": 10,
            "Actual Total Time": 80.0, "Actual Rows": 20000, "Actual Loops": 1,
            "Sort Key": ["total"], "Sort Method": "external merge", "Sort Space Used": 4096, "Sort Space Type": "Disk",
            "Plans": [
              {"Node Type": "Seq Scan", "Relation Name": "orders", "Startup Cost": 0, "Total Cost": 800,
               "Plan Rows": 10, "Actual Total Time": 60.0, "Actual Rows": 20000, "Actual Loops": 1,
               "Filter": "(total > 10)", "Rows Removed by Filter": 180000}
            ]}, "Execution Time": 81.0}]"#;
        let plan = format(json).unwrap();
        let notes: Vec<(&str, Option<usize>)> = plan.notes.iter().map(|n| (n.text.as_str(), n.line)).collect();
        assert_eq!(
            notes,
            [
                ("Slowest step: Seq Scan on orders, 60.0 ms of 80.0 ms (75%)", Some(3)),
                ("Sort: estimated 10 rows, got 20.0k: statistics may be stale", Some(0)),
                ("Sort spilled to disk: more work_mem would keep it in memory", Some(0)),
                (
                    "Seq Scan on orders read 200.0k rows to keep 20.0k: an index on the Filter's columns may help",
                    Some(3)
                ),
                ("Seq Scan on orders: estimated 10 rows, got 20.0k: ANALYZE orders may help", Some(3)),
            ]
        );
        assert!(plan.text.contains("Sort Method: external merge  Disk: 4096kB"), "{}", plan.text);
        assert!(plan.text.contains("Rows Removed by Filter: 180000"), "{}", plan.text);
    }
}
