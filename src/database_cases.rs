//! The database interface's cases (`testdata/database_cases.toml`) against
//! the workspace's tests: every test a case names exists, a case `tested`
//! on an engine names one, every live test (`fn live_…` with a test
//! attribute) is claimed by a case, and ids are unique. What's left to do
//! is printed per area (`cargo test database_cases -- --nocapture`).
//! Here, in the app's crate, because it reads every crate's code.

use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;

const ENGINES: [&str; 5] = ["pg", "mysql", "sqlite", "duckdb", "mongo"];
const STATUSES: [&str; 6] = ["tested", "partial", "code", "driver", "no", "na"];

#[derive(Deserialize)]
struct Cases {
    case: Vec<Case>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Case {
    id: String,
    what: String,
    pg: Option<String>,
    mysql: Option<String>,
    sqlite: Option<String>,
    duckdb: Option<String>,
    mongo: Option<String>,
    #[serde(default)]
    tests: Vec<String>,
    note: Option<String>,
}

impl Case {
    /// Each engine's status, where it's given.
    fn statuses(&self) -> Vec<(&'static str, &str)> {
        let given = [&self.pg, &self.mysql, &self.sqlite, &self.duckdb, &self.mongo];
        ENGINES.iter().zip(given).filter_map(|(engine, s)| s.as_deref().map(|s| (*engine, s))).collect()
    }

    /// Its area: what its id says before the first `.`.
    fn area(&self) -> &str {
        self.id.split('.').next().unwrap_or_default()
    }
}

fn cases() -> Vec<Case> {
    let text = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/testdata/database_cases.toml"));
    toml::from_str::<Cases>(text).expect("testdata/database_cases.toml reads").case
}

/// Every test function in the workspace (`#[test]`, `#[tokio::test]`), by
/// name; the live ones are those named `live_…`.
fn tests() -> BTreeSet<String> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut stack = vec![root.join("src")];
    for entry in std::fs::read_dir(root.join("crates")).unwrap().flatten() {
        stack.push(entry.path().join("src"));
    }
    let mut found = BTreeSet::new();
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if path.extension().is_none_or(|e| e != "rs") {
                continue;
            }
            let code = std::fs::read_to_string(&path).unwrap();
            let lines: Vec<&str> = code.lines().collect();
            for (i, line) in lines.iter().enumerate() {
                let Some(name) = line.split("fn ").nth(1).and_then(|rest| rest.split(['(', '<']).next()) else {
                    continue;
                };
                // A test attribute in the few lines above (a doc comment, a
                // cfg, between them).
                let above = &lines[i.saturating_sub(4)..i];
                if above.iter().any(|l| l.trim_start().starts_with("#[test]") || l.contains("#[tokio::test")) {
                    found.insert(name.trim().to_string());
                }
            }
        }
    }
    found
}

#[test]
fn every_case_is_proven_by_tests_that_exist() {
    let cases = cases();
    let tests = tests();
    let mut problems = Vec::new();
    let mut ids = BTreeSet::new();
    let mut claimed = BTreeSet::new();
    for case in &cases {
        if !ids.insert(case.id.as_str()) {
            problems.push(format!("{}: the id is used twice", case.id));
        }
        if !case.id.contains('.') || case.what.trim().is_empty() {
            problems.push(format!("{}: an id is `area.name`, with what it is", case.id));
        }
        for (engine, status) in case.statuses() {
            if !STATUSES.contains(&status) {
                problems.push(format!("{} {engine}: no status `{status}` (one of {STATUSES:?})", case.id));
            }
            if status == "tested" && case.tests.is_empty() {
                problems.push(format!("{} {engine}: tested, by no test named", case.id));
            }
            if status == "partial" && case.note.is_none() {
                problems.push(format!("{} {engine}: partial, without a note of what's missing", case.id));
            }
        }
        if case.statuses().is_empty() {
            problems.push(format!("{}: no engine's status", case.id));
        }
        for test in &case.tests {
            if !tests.contains(test) {
                problems.push(format!("{}: no test `{test}`", case.id));
            }
            claimed.insert(test.as_str());
        }
    }
    for live in tests.iter().filter(|t| t.starts_with("live_")) {
        if !claimed.contains(live.as_str()) {
            problems.push(format!("`{live}` proves no case: name it in a case's tests"));
        }
    }
    assert!(problems.is_empty(), "testdata/database_cases.toml:\n  {}", problems.join("\n  "));
}

/// The cases as a matrix, per area, with what's left to do: written to
/// `target/database-cases.md` (read, not failed: it's the list to work
/// through, and the support matrix made from this file).
#[test]
fn the_database_cases_as_a_matrix() {
    use std::fmt::Write as _;
    let cases = cases();
    let mut areas: BTreeMap<&str, Vec<&Case>> = BTreeMap::new();
    for case in &cases {
        areas.entry(case.area()).or_default().push(case);
    }
    let counted = |cases: &[&Case]| {
        let statuses = cases.iter().flat_map(|c| c.statuses()).filter(|(_, s)| *s != "na");
        statuses.fold((0, 0), |(tested, all), (_, s)| (tested + usize::from(s == "tested"), all + 1))
    };
    let all: Vec<&Case> = cases.iter().collect();
    let (tested, total) = counted(&all);
    let mut out = String::from("# Database cases\n\nMade from `testdata/database_cases.toml` by ");
    let _ = writeln!(
        out,
        "`database_cases::the_database_cases_as_a_matrix`: {tested} of {total} tested, {} cases.",
        cases.len()
    );
    for (area, cases) in &areas {
        let (tested, total) = counted(cases);
        let _ = write!(out, "\n## {area}: {tested} of {total} tested\n\n");
        out.push_str("| Case | What | PostgreSQL | MySQL | SQLite | DuckDB | MongoDB | Tests, note |\n");
        out.push_str("|---|---|---|---|---|---|---|---|\n");
        for case in cases {
            let status = |s: &Option<String>| s.clone().unwrap_or_default();
            let tests: Vec<String> = case.tests.iter().map(|t| format!("`{t}`")).collect();
            let note = case.note.as_deref().map(|n| format!(" ({n})")).unwrap_or_default();
            let _ = writeln!(
                out,
                "| {} | {} | {} | {} | {} | {} | {} | {}{note} |",
                case.id,
                case.what,
                status(&case.pg),
                status(&case.mysql),
                status(&case.sqlite),
                status(&case.duckdb),
                status(&case.mongo),
                tests.join(", ")
            );
        }
    }
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/database-cases.md");
    std::fs::write(&path, out).expect("target/database-cases.md written");
}
