//! Saved queries: named, optionally grouped, per project; searchable by
//! name, group and SQL. Stored as JSON next to the history files.

use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::config::store::write_atomic;

/// The group whose saved queries autocomplete offers as snippets.
pub const SNIPPETS: &str = "snippets";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedQuery {
    pub id: String,
    pub name: String,
    /// "" when not in a group.
    #[serde(default)]
    pub group: String,
    pub sql: String,
    /// When it was saved or its SQL last changed (unix seconds); none for
    /// queries saved before that was kept.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub changed: Option<i64>,
}

impl SavedQuery {
    /// "group/name", or just "name".
    pub fn path(&self) -> String {
        if self.group.is_empty() { self.name.clone() } else { format!("{}/{}", self.group, self.name) }
    }
}

/// Splits "group/name" (the last `/` separates them); trims both parts.
pub fn split_path(path: &str) -> (String, String) {
    match path.rsplit_once('/') {
        Some((group, name)) => (group.trim().to_string(), name.trim().to_string()),
        None => (String::new(), path.trim().to_string()),
    }
}

/// Whether every whitespace-separated word of `query` appears (case-
/// insensitively) in one of `fields`.
pub fn matches(query: &str, fields: &[&str]) -> bool {
    let haystack = fields.join("\n").to_lowercase();
    query.split_whitespace().all(|word| haystack.contains(&word.to_lowercase()))
}

pub struct Library {
    path: Option<PathBuf>,
    pub queries: Vec<SavedQuery>,
}

impl Library {
    pub fn load_from(path: Option<PathBuf>) -> Self {
        let queries = path
            .as_ref()
            .and_then(|p| fs::read_to_string(p).ok())
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default();
        Self { path, queries }
    }

    fn save(&self) -> anyhow::Result<()> {
        let Some(path) = &self.path else { return Ok(()) };
        write_atomic(path, &serde_json::to_string_pretty(&self.queries)?)
    }

    pub fn get(&self, id: &str) -> Option<&SavedQuery> {
        self.queries.iter().find(|q| q.id == id)
    }

    /// Saves a new query under "group/name"; returns its id.
    pub fn add(&mut self, path: &str, sql: &str) -> Result<String, String> {
        let (group, name) = split_path(path);
        if name.is_empty() {
            return Err("Give the query a name".into());
        }
        self.check_free(&group, &name, None)?;
        let id = uuid::Uuid::new_v4().to_string();
        let changed = Some(chrono::Local::now().timestamp());
        self.queries.push(SavedQuery { id: id.clone(), name, group, sql: sql.to_string(), changed });
        self.sort();
        self.save().map_err(|e| format!("Couldn't save: {e:#}"))?;
        Ok(id)
    }

    pub fn update_sql(&mut self, id: &str, sql: &str) -> Result<(), String> {
        let q = self.queries.iter_mut().find(|q| q.id == id).ok_or("The saved query no longer exists")?;
        if q.sql != sql {
            q.sql = sql.to_string();
            q.changed = Some(chrono::Local::now().timestamp());
        }
        self.save().map_err(|e| format!("Couldn't save: {e:#}"))
    }

    /// Renames (and possibly regroups) a saved query from "group/name".
    pub fn rename(&mut self, id: &str, path: &str) -> Result<(), String> {
        let (group, name) = split_path(path);
        if name.is_empty() {
            return Err("Give the query a name".into());
        }
        self.check_free(&group, &name, Some(id))?;
        let q = self.queries.iter_mut().find(|q| q.id == id).ok_or("The saved query no longer exists")?;
        q.group = group;
        q.name = name;
        self.sort();
        self.save().map_err(|e| format!("Couldn't save: {e:#}"))
    }

    pub fn delete(&mut self, id: &str) -> Result<(), String> {
        self.queries.retain(|q| q.id != id);
        self.save().map_err(|e| format!("Couldn't save: {e:#}"))
    }

    fn check_free(&self, group: &str, name: &str, except: Option<&str>) -> Result<(), String> {
        let taken = self.queries.iter().any(|q| {
            q.group.eq_ignore_ascii_case(group) && q.name.eq_ignore_ascii_case(name) && Some(q.id.as_str()) != except
        });
        if taken { Err(format!("A saved query named '{}' already exists", join(group, name))) } else { Ok(()) }
    }

    /// Ungrouped first, then by group and name.
    fn sort(&mut self) {
        self.queries.sort_by(|a, b| {
            (!a.group.is_empty(), a.group.to_lowercase(), a.name.to_lowercase()).cmp(&(
                !b.group.is_empty(),
                b.group.to_lowercase(),
                b.name.to_lowercase(),
            ))
        });
    }

    /// Saved queries matching `query` (its words, and its `@` dates), in
    /// library order.
    pub fn search(&self, query: &str) -> Vec<&SavedQuery> {
        let (words, when) = split_dates(query, chrono::Local::now());
        self.queries
            .iter()
            .filter(|q| when.is_none_or(|w| w.holds(q.changed)) && matches(&words, &[&q.name, &q.group, &q.sql]))
            .collect()
    }
}

/// A time span a search is narrowed to: from (inclusive) to (exclusive),
/// in unix seconds; either end open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Span {
    pub from: Option<i64>,
    pub to: Option<i64>,
}

impl Span {
    /// Whether a time is in it: one that isn't known never is.
    pub fn holds(self, at: Option<i64>) -> bool {
        at.is_some_and(|at| self.from.is_none_or(|f| at >= f) && self.to.is_none_or(|t| at < t))
    }
}

/// A search's words, and the span its `@` words narrow it to: `@today`,
/// `@yesterday`, `@7d` / `@2w` / `@3m` (the last days, weeks, months),
/// `@2026`, `@2026-10`, `@2026-10-21`, and `@from..to` of those dates
/// (either side may be left out: `@2026-10-01..`). Several narrow it
/// together. An `@` word that is none of these is searched for as it is.
pub fn split_dates(query: &str, now: chrono::DateTime<chrono::Local>) -> (String, Option<Span>) {
    let mut words = Vec::new();
    let mut span: Option<Span> = None;
    for word in query.split_whitespace() {
        match word.strip_prefix('@').and_then(|w| date_span(&w.to_lowercase(), now)) {
            Some(s) => {
                let both = span.map_or(s, |a| Span {
                    // (None is less than any time: the later start wins.)
                    from: a.from.max(s.from),
                    to: match (a.to, s.to) {
                        (Some(x), Some(y)) => Some(x.min(y)),
                        (x, y) => x.or(y),
                    },
                });
                span = Some(both);
            }
            None => words.push(word),
        }
    }
    (words.join(" "), span)
}

fn date_span(word: &str, now: chrono::DateTime<chrono::Local>) -> Option<Span> {
    use chrono::{Days, Months};
    let today = now.date_naive();
    let day = |d: chrono::NaiveDate| start_of(d);
    match word {
        "today" => return Some(Span { from: Some(day(today)?), to: None }),
        "yesterday" => return Some(Span { from: Some(day(today.pred_opt()?)?), to: Some(day(today)?) }),
        _ => {}
    }
    if let Some((a, b)) = word.split_once("..") {
        let from = if a.is_empty() { None } else { Some(day(period(a)?.0)?) };
        let to = if b.is_empty() { None } else { Some(day(period(b)?.1)?) };
        return Some(Span { from, to });
    }
    // The last N days, weeks or months, up to now.
    if let Some(unit) = word.chars().last().filter(|c| matches!(c, 'd' | 'w' | 'm')) {
        let n: u32 = word[..word.len() - 1].parse().ok().filter(|n| *n > 0)?;
        // (Checked: a count past the calendar's range is no date.)
        let from = match unit {
            'd' => today.checked_sub_days(Days::new(u64::from(n) - 1))?,
            'w' => today.checked_sub_days(Days::new(u64::from(n) * 7 - 1))?,
            _ => today.checked_sub_months(Months::new(n))?.succ_opt()?,
        };
        return Some(Span { from: Some(day(from)?), to: None });
    }
    let (first, after) = period(word)?;
    Some(Span { from: Some(day(first)?), to: Some(day(after)?) })
}

/// `2026`, `2026-10` or `2026-10-21`: its first day, and the day after it.
fn period(text: &str) -> Option<(chrono::NaiveDate, chrono::NaiveDate)> {
    use chrono::{Months, NaiveDate};
    let parts: Vec<&str> = text.split('-').collect();
    let num = |s: &str| s.parse::<u32>().ok();
    match parts.as_slice() {
        [y] if y.len() == 4 => {
            let first = NaiveDate::from_ymd_opt(i32::try_from(num(y)?).ok()?, 1, 1)?;
            Some((first, first.checked_add_months(Months::new(12))?))
        }
        [y, m] if y.len() == 4 => {
            let first = NaiveDate::from_ymd_opt(i32::try_from(num(y)?).ok()?, num(m)?, 1)?;
            Some((first, first.checked_add_months(Months::new(1))?))
        }
        [y, m, d] if y.len() == 4 => {
            let first = NaiveDate::from_ymd_opt(i32::try_from(num(y)?).ok()?, num(m)?, num(d)?)?;
            Some((first, first.succ_opt()?))
        }
        _ => None,
    }
}

/// When `day` starts here, in unix seconds: midnight, or where a clock
/// change skips midnight, the first hour that's there.
fn start_of(day: chrono::NaiveDate) -> Option<i64> {
    (0..3).find_map(|h| day.and_hms_opt(h, 0, 0)?.and_local_timezone(chrono::Local).earliest()).map(|t| t.timestamp())
}

fn join(group: &str, name: &str) -> String {
    if group.is_empty() { name.to_string() } else { format!("{group}/{name}") }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_split_on_the_last_slash() {
        assert_eq!(split_path(" reports / churn "), ("reports".into(), "churn".into()));
        assert_eq!(split_path("a/b/c"), ("a/b".into(), "c".into()));
        assert_eq!(split_path("plain"), (String::new(), "plain".into()));
    }

    #[test]
    fn add_rename_regroup_delete_and_persist() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("q.json");
        let mut lib = Library::load_from(Some(path.clone()));
        let a = lib.add("reports/churn", "select 1").unwrap();
        let b = lib.add("adhoc", "select 2").unwrap();
        assert!(lib.add("Reports/Churn", "x").unwrap_err().contains("already exists"));
        assert!(lib.add("reports/", "x").is_err());
        // Ungrouped queries sort first.
        assert_eq!(lib.queries.iter().map(SavedQuery::path).collect::<Vec<_>>(), ["adhoc", "reports/churn"]);

        lib.rename(&a, "finance/churn by month").unwrap();
        lib.update_sql(&b, "select 3").unwrap();
        let lib2 = Library::load_from(Some(path));
        assert_eq!(lib2.get(&a).unwrap().path(), "finance/churn by month");
        assert_eq!(lib2.get(&b).unwrap().sql, "select 3");

        lib.delete(&b).unwrap();
        assert_eq!(lib.queries.len(), 1);
    }

    #[test]
    fn dates_narrow_a_search() {
        use chrono::TimeZone;
        let now = chrono::Local.with_ymd_and_hms(2026, 10, 23, 12, 0, 0).unwrap();
        let at = |y, m, d, h| chrono::Local.with_ymd_and_hms(y, m, d, h, 0, 0).unwrap().timestamp();
        let span = |q: &str| split_dates(q, now).1.unwrap();
        let (words, s) = split_dates("orders @today", now);
        assert_eq!(words, "orders");
        assert!(s.unwrap().holds(Some(at(2026, 10, 23, 1))) && !s.unwrap().holds(Some(at(2026, 10, 22, 23))));
        assert!(span("@yesterday").holds(Some(at(2026, 10, 22, 9))));
        assert!(!span("@yesterday").holds(Some(at(2026, 10, 23, 9))));
        // The last 7 days: today and the six before it.
        assert!(span("@7d").holds(Some(at(2026, 10, 17, 0))) && !span("@7d").holds(Some(at(2026, 10, 16, 23))));
        assert!(span("@1m").holds(Some(at(2026, 9, 24, 0))) && !span("@1m").holds(Some(at(2026, 9, 23, 12))));
        assert!(span("@2026-10").holds(Some(at(2026, 10, 1, 0))) && !span("@2026-10").holds(Some(at(2026, 11, 1, 0))));
        assert!(span("@2025").holds(Some(at(2025, 12, 31, 23))));
        let range = span("@2026-10-01..2026-10-15");
        assert!(range.holds(Some(at(2026, 10, 15, 23))) && !range.holds(Some(at(2026, 10, 16, 0))));
        assert!(span("@2026-10-20..").holds(Some(at(2026, 10, 23, 0))));
        // Two narrow it together; no time is never in a span.
        let both = span("@2026-10 @..2026-10-05");
        assert!(both.holds(Some(at(2026, 10, 3, 0))) && !both.holds(Some(at(2026, 10, 6, 0))));
        assert!(!span("@today").holds(None));
        // Anything else after @ is searched for.
        assert_eq!(split_dates("@user @2026-13", now), ("@user @2026-13".to_string(), None));
        // A count past the calendar is no date, not a crash.
        assert_eq!(split_dates("@99999999d @20000000w @4000000000m", now).1, None);
    }

    #[test]
    fn search_needs_every_word_in_name_group_or_sql() {
        let mut lib = Library::load_from(None);
        lib.add("reports/churn", "select * from customers where status = 'paused'").unwrap();
        lib.add("adhoc/top", "select * from orders").unwrap();
        let names = |q: &str| lib.search(q).iter().map(|s| s.name.clone()).collect::<Vec<_>>();
        assert_eq!(names("CHURN"), ["churn"]);
        assert_eq!(names("reports paused"), ["churn"]);
        assert_eq!(names("orders"), ["top"]);
        assert_eq!(names("select"), ["top", "churn"]);
        assert!(names("orders paused").is_empty());
        assert_eq!(names("").len(), 2);
    }
}
