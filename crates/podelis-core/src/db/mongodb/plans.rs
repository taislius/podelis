//! MongoDB's plans: the `explain` command that plans a statement without
//! running it, and what its answer says (the winning plan's stages, and the
//! collections it reads whole: a `COLLSCAN` no `LIMIT` stops early).

use std::fmt::Write as _;

use serde_json::Value;

use super::language::{Call, call, tokenize};
use crate::db::Plan;
use crate::db::dialect::PlanNote;
use crate::language::TokenKind;

/// A call's arguments, split at the commas outside brackets.
fn split_args(args: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let (mut depth, mut start) = (0i32, 0);
    for t in tokenize(args) {
        let s = &args[t.range.clone()];
        match (t.kind, s) {
            (TokenKind::Punct, "(" | "[" | "{") => depth += 1,
            (TokenKind::Punct, ")" | "]" | "}") => depth -= 1,
            (TokenKind::Punct, ",") if depth == 0 => {
                parts.push(args[start..t.range.start].trim());
                start = t.range.end;
            }
            _ => {}
        }
    }
    parts.push(args[start..].trim());
    parts.retain(|p| !p.is_empty());
    parts
}

/// A name as a JSON string.
fn quoted(name: &str) -> String {
    Value::String(name.to_string()).to_string()
}

/// What `.<name>(…)` chained after the call says, as written.
fn chained<'a>(statement: &'a str, call: &Call<'_>, name: &str) -> Option<&'a str> {
    call.chain.iter().rev().find(|(m, _)| *m == name).map(|(_, a)| statement[a.clone()].trim())
}

/// The command that plans `statement` (a `find`, `findOne`, count,
/// `distinct` or `aggregate` on a collection) without running it, as a
/// statement Podėlis runs; `None` for anything else.
pub fn explain_command(statement: &str) -> Option<String> {
    let call = call(statement)?;
    let collection = quoted(call.collection.as_deref()?);
    let args = split_args(&statement[call.args.clone()]);
    let arg = |i: usize, empty: &'static str| args.get(i).copied().unwrap_or(empty);
    let inner = match call.method {
        "find" | "findOne" => {
            let mut find = format!("find: {collection}, filter: {}", arg(0, "{}"));
            if let Some(projection) = args.get(1).copied().or_else(|| chained(statement, &call, "projection")) {
                let _ = write!(find, ", projection: {projection}");
            }
            for (name, key) in [("sort", "sort"), ("skip", "skip"), ("limit", "limit")] {
                if let Some(value) = chained(statement, &call, name).filter(|v| !v.is_empty()) {
                    let _ = write!(find, ", {key}: {value}");
                }
            }
            if call.method == "findOne" {
                find.push_str(", limit: 1");
            }
            find
        }
        "countDocuments" | "count" => format!("count: {collection}, query: {}", arg(0, "{}")),
        "distinct" => format!("distinct: {collection}, key: {}, query: {}", arg(0, "\"_id\""), arg(1, "{}")),
        "aggregate" => format!("aggregate: {collection}, pipeline: {}, cursor: {{}}", arg(0, "[]")),
        _ => return None,
    };
    Some(format!("db.runCommand({{explain: {{{inner}}}, verbosity: \"queryPlanner\"}})"))
}

/// Every winning plan in an `explain` answer (an aggregation's are in its
/// stages; a newer server's under `queryPlan`), each with the collection
/// it reads: the `namespace` beside it, without the database (a
/// `$lookup`'s or `$unionWith`'s plan has its own).
fn winning_plans(answer: &Value) -> Vec<(Option<String>, &Value)> {
    let mut found = Vec::new();
    let mut stack = vec![(None::<String>, answer)];
    while let Some((ns, v)) = stack.pop() {
        match v {
            Value::Object(map) => {
                let ns = match map.get("namespace") {
                    Some(Value::String(n)) => Some(n.split_once('.').map_or(n.as_str(), |(_, c)| c).to_string()),
                    _ => ns,
                };
                for (key, value) in map {
                    match key.as_str() {
                        "winningPlan" => found.push((ns.clone(), value.get("queryPlan").unwrap_or(value))),
                        // Plans not chosen: what they'd read doesn't count.
                        "rejectedPlans" => {}
                        _ => stack.push((ns.clone(), value)),
                    }
                }
            }
            Value::Array(items) => stack.extend(items.iter().map(|item| (ns.clone(), item))),
            _ => {}
        }
    }
    found
}

/// A plan's stages, the outermost first, each with its depth.
fn stages(plan: &Value) -> Vec<(usize, &Value)> {
    let mut out = Vec::new();
    let mut stack = vec![(0, plan)];
    while let Some((depth, stage)) = stack.pop() {
        out.push((depth, stage));
        let mut inputs: Vec<&Value> = stage.get("inputStage").into_iter().collect();
        if let Some(Value::Array(many)) = stage.get("inputStages") {
            inputs.extend(many);
        }
        stack.extend(inputs.into_iter().rev().map(|s| (depth + 1, s)));
    }
    out
}

fn stage_name(stage: &Value) -> &str {
    stage.get("stage").and_then(Value::as_str).unwrap_or("?")
}

/// The collections an `explain` answer reads whole: a `COLLSCAN` in a
/// winning plan with no `LIMIT` (nor a limited sort) over it, each once.
pub fn collections_read_whole(answer: &Value) -> Vec<String> {
    let mut whole = Vec::new();
    for (ns, plan) in winning_plans(answer) {
        let stages = stages(plan);
        let limited = stages.iter().any(|(_, s)| {
            matches!(stage_name(s), "LIMIT" | "EOF") || (stage_name(s) == "SORT" && s.get("limitAmount").is_some())
        });
        if !limited
            && stages.iter().any(|(_, s)| stage_name(s) == "COLLSCAN")
            && let Some(ns) = ns
            && !whole.contains(&ns)
        {
            whole.push(ns);
        }
    }
    whole
}

/// An `explain` answer as Podėlis shows a plan: the winning plan's stages
/// as a tree (with what each reads by), and a note for a collection read
/// whole. `None` if it isn't one.
pub fn format_plan(columns: &[String], rows: &[Vec<Option<String>>]) -> Option<Plan> {
    let row = rows.first()?;
    let mut answer = serde_json::Map::new();
    for (name, cell) in columns.iter().zip(row) {
        let value = cell.as_deref().map(|c| serde_json::from_str(c).unwrap_or_else(|_| Value::String(c.to_string())));
        answer.insert(name.clone(), value.unwrap_or(Value::Null));
    }
    let answer = Value::Object(answer);
    let plans = winning_plans(&answer);
    if plans.is_empty() {
        return None;
    }
    let mut text = String::new();
    for (_, plan) in plans {
        for (depth, stage) in stages(plan) {
            let by = stage
                .get("indexName")
                .and_then(Value::as_str)
                .map(|i| format!(" (index {i})"))
                .or_else(|| stage.get("filter").filter(|f| f.as_object().is_some_and(|f| !f.is_empty())).map(|f| format!(" filter {f}")))
                .unwrap_or_default();
            let _ = writeln!(text, "{}{}{by}", "  ".repeat(depth), stage_name(stage));
        }
    }
    let notes = collections_read_whole(&answer)
        .into_iter()
        .map(|c| PlanNote {
            text: format!("reads all of {c} (COLLSCAN): an index on what it filters by would spare that"),
            line: None,
        })
        .collect();
    Some(Plan { text: text.trim_end().to_string(), notes })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reads_plan_is_asked_for_by_its_own_command() {
        let cmd = |s: &str| explain_command(s).unwrap_or_default();
        assert_eq!(
            cmd("db.users.find({age: {$gt: 30}}, {name: 1}).sort({name: 1}).limit(5)"),
            "db.runCommand({explain: {find: \"users\", filter: {age: {$gt: 30}}, projection: {name: 1}, sort: {name: 1}, limit: 5}, verbosity: \"queryPlanner\"})"
        );
        assert!(cmd("db.users.findOne({_id: 1})").contains("limit: 1"));
        assert!(cmd("db.getCollection('my orders').countDocuments({})").contains("count: \"my orders\", query: {}"));
        assert!(cmd("db.o.aggregate([{$match: {x: 1}}, {$group: {_id: '$y'}}])")
            .contains("aggregate: \"o\", pipeline: [{$match: {x: 1}}, {$group: {_id: '$y'}}], cursor: {}"));
        assert!(cmd("db.o.distinct('y', {x: 1})").contains("key: 'y', query: {x: 1}"));
        assert_eq!(explain_command("db.o.insertOne({x: 1})"), None, "plans of reads only");
        assert_eq!(explain_command("db.runCommand({ping: 1})"), None);
    }

    fn answer(winning: &str) -> Value {
        serde_json::from_str(&format!(
            r#"{{"queryPlanner": {{"namespace": "shop.orders", "winningPlan": {winning},
                "rejectedPlans": [{{"stage": "COLLSCAN"}}]}}, "ok": 1}}"#
        ))
        .unwrap()
    }

    #[test]
    fn a_collection_read_whole_is_one_no_limit_stops() {
        let whole = answer(r#"{"stage": "COLLSCAN", "filter": {"x": {"$eq": 1}}}"#);
        assert_eq!(collections_read_whole(&whole), ["orders"]);
        let indexed = answer(r#"{"stage": "FETCH", "inputStage": {"stage": "IXSCAN", "indexName": "x_1"}}"#);
        assert!(collections_read_whole(&indexed).is_empty(), "a rejected plan's scan doesn't count");
        let limited = answer(r#"{"stage": "LIMIT", "inputStage": {"stage": "COLLSCAN"}}"#);
        assert!(collections_read_whole(&limited).is_empty());
        // A newer server's: the plan under `queryPlan`.
        let newer = answer(r#"{"queryPlan": {"stage": "COLLSCAN"}, "slotBasedPlan": {}}"#);
        assert_eq!(collections_read_whole(&newer), ["orders"]);
    }

    /// A plan inside the pipeline (a `$unionWith`'s) is of its own
    /// collection, not the one the aggregation starts from.
    #[test]
    fn a_scan_is_of_the_collection_its_plan_is_about() {
        let answer: Value = serde_json::from_str(
            r#"{"stages": [
                {"$cursor": {"queryPlanner": {"namespace": "shop.small",
                    "winningPlan": {"stage": "IXSCAN", "indexName": "_id_"}}}},
                {"$unionWith": {"coll": "huge", "pipeline": [{"$cursor": {"queryPlanner": {
                    "namespace": "shop.huge", "winningPlan": {"stage": "COLLSCAN"}}}}]}}
            ]}"#,
        )
        .unwrap();
        assert_eq!(collections_read_whole(&answer), ["huge"]);
    }

    #[test]
    fn its_plan_as_shown_with_what_stands_out() {
        let answer = answer(r#"{"stage": "SORT", "inputStage": {"stage": "COLLSCAN", "filter": {"x": 1}}}"#);
        let columns: Vec<String> = ["queryPlanner", "ok"].map(String::from).to_vec();
        let rows = vec![vec![Some(answer["queryPlanner"].to_string()), Some("1".into())]];
        let plan = format_plan(&columns, &rows).unwrap();
        assert_eq!(plan.text, "SORT\n  COLLSCAN filter {\"x\":1}");
        assert!(plan.notes[0].text.contains("reads all of orders"), "{:?}", plan.notes);
        assert!(format_plan(&["n".into()], &[vec![Some("1".into())]]).is_none(), "not a plan");
    }
}
