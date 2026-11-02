//! How good the context builder is, as a number: for each question of a
//! gold set (`eval/questions.txt`), the tables the right SQL needs, and
//! whether the context chose them.
//!
//! - **Recall:** the gold tables in the context, out of all gold tables.
//!   A table left out can't be written into the query.
//! - **Precision:** the context's tables the SQL needs, out of all it
//!   chose. Extra tables cost tokens and can mislead.
//! - **Characters:** what the context costs.
//!
//! The schemas are snapshots (`eval/<database>.json`: tables, columns,
//! keys and comments, never rows), so the test runs without a server.
//! `dump_schemas` makes them again from live databases.
//!
//! `cargo test eval -- --nocapture` prints a line per database and the
//! questions that missed (`PODELIS_EVAL_ALL=1`: every question's extra
//! tables, and why they came).

use std::collections::BTreeMap;
use std::path::PathBuf;

use super::Schema;

fn folder() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/context/eval")
}

/// A snapshot (written by the app's `eval_snapshots::dump_schemas`).
fn schema(name: &str) -> Schema {
    let file = folder().join(format!("{name}.json"));
    let text = std::fs::read_to_string(&file).unwrap_or_else(|e| panic!("{}: {e}", file.display()));
    serde_json::from_str(&text).unwrap()
}

/// One line of `questions.txt`:
/// `database | question | gold tables | facts` (facts optional,
/// `subject / relation / value` separated by `;`).
struct Case {
    database: String,
    question: String,
    gold: Vec<String>,
    facts: Vec<(String, String, String)>,
}

fn cases() -> Vec<Case> {
    let text = std::fs::read_to_string(folder().join("questions.txt")).unwrap();
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| {
            let parts: Vec<&str> = l.split('|').map(str::trim).collect();
            assert!(parts.len() >= 3, "questions.txt: {l}");
            let facts = parts
                .get(3)
                .map(|f| {
                    f.split(';')
                        .map(|one| {
                            let p: Vec<&str> = one.split(" / ").map(str::trim).collect();
                            assert_eq!(p.len(), 3, "a fact is subject / relation / value: {one}");
                            (p[0].to_string(), p[1].to_string(), p[2].to_string())
                        })
                        .collect()
                })
                .unwrap_or_default();
            Case {
                database: parts[0].to_string(),
                question: parts[1].to_string(),
                gold: parts[2].split(',').map(|t| t.trim().to_lowercase()).filter(|t| !t.is_empty()).collect(),
                facts,
            }
        })
        .collect()
}

/// The context's tables without their schema, lowercased.
fn bare(name: &str) -> String {
    name.rsplit('.').next().unwrap_or(name).to_lowercase()
}

#[derive(Default)]
struct Tally {
    questions: usize,
    gold: usize,
    found: usize,
    chosen: usize,
    used: usize,
    chars: usize,
    misses: Vec<String>,
}

async fn measure() -> BTreeMap<String, Tally> {
    let mut schemas: BTreeMap<String, Schema> = BTreeMap::new();
    let mut out: BTreeMap<String, Tally> = BTreeMap::new();
    for case in cases() {
        let schema = schemas.entry(case.database.clone()).or_insert_with(|| self::schema(&case.database));
        let memory = {
            let dir = tempfile::tempdir().unwrap();
            let mut conn = crate::memory::open(dir.path(), "eval").await.unwrap();
            for (s, r, v) in &case.facts {
                crate::memory::add(&mut conn, s, r, v, "user", true).await.unwrap();
            }
            crate::memory::for_question(&mut conn, &case.question).await.unwrap()
        };
        let built = super::for_question(schema, &case.question, &memory);
        let chosen: Vec<String> = built.tables.iter().map(|t| bare(&t.name)).collect();
        let t = out.entry(case.database.clone()).or_default();
        t.questions += 1;
        t.gold += case.gold.len();
        let found = case.gold.iter().filter(|g| chosen.contains(g)).count();
        t.found += found;
        t.chosen += chosen.len();
        t.used += chosen.iter().filter(|c| case.gold.contains(c)).count();
        t.chars += built.text.len();
        if std::env::var_os("PODELIS_EVAL_ALL").is_some() {
            let extra: Vec<String> = built
                .tables
                .iter()
                .filter(|t| !case.gold.contains(&bare(&t.name)))
                .map(|t| format!("{} ({})", bare(&t.name), t.why))
                .collect();
            t.misses.push(format!("{}: extra {}", case.question, extra.join("; ")));
        }
        if found < case.gold.len() {
            let missing: Vec<&str> = case.gold.iter().filter(|g| !chosen.contains(g)).map(String::as_str).collect();
            t.misses.push(format!("{}: missing {} (chose {})", case.question, missing.join(", "), chosen.join(", ")));
        }
    }
    out
}

/// Recall and precision below these (per cent, per database) fail the
/// test: raise them as the builder gets better. Measured 2026-11-01:
/// recall 100% everywhere; precision chinook 35%, pagila 38%, sakila 33%.
const RECALL_FLOOR: &[(&str, u32)] = &[("chinook", 100), ("pagila", 100), ("sakila", 100)];
const PRECISION_FLOOR: &[(&str, u32)] = &[("chinook", 32), ("pagila", 35), ("sakila", 30)];

#[tokio::test]
async fn retrieval_recall_holds() {
    let results = measure().await;
    let pct = |a: usize, b: usize| (a * 100).checked_div(b).map_or(100, |p| p as u32);
    let mut failed = Vec::new();
    #[expect(clippy::print_stdout, reason = "the eval's numbers, read with --nocapture")]
    for (db, t) in &results {
        let recall = pct(t.found, t.gold);
        println!(
            "{db:<8} {:>2} questions  recall {recall:>3}% ({}/{})  precision {:>3}% ({}/{})  {:>6} chars a context",
            t.questions,
            t.found,
            t.gold,
            pct(t.used, t.chosen),
            t.used,
            t.chosen,
            t.chars / t.questions.max(1),
        );
        for m in &t.misses {
            println!("    {m}");
        }
        let floor = |of: &[(&str, u32)]| of.iter().find(|(d, _)| d == db).map_or(0, |(_, f)| *f);
        if recall < floor(RECALL_FLOOR) {
            failed.push(format!("{db}: recall {recall}% under {}%", floor(RECALL_FLOOR)));
        }
        let precision = pct(t.used, t.chosen);
        if precision < floor(PRECISION_FLOOR) {
            failed.push(format!("{db}: precision {precision}% under {}%", floor(PRECISION_FLOOR)));
        }
    }
    assert!(failed.is_empty(), "{failed:?}");
}
