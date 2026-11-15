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
        join(&self.group, &self.name)
    }

    /// In the group autocomplete offers as snippets (in any case).
    pub fn is_snippet(&self) -> bool {
        self.group.eq_ignore_ascii_case(SNIPPETS)
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

/// Why a change to the library wasn't made. The app says it in the user's
/// language (`app::windows::library::said`).
#[derive(Debug, PartialEq, Eq)]
pub enum LibraryError {
    /// The path given has no name after its group.
    NoName,
    /// A saved query of that "group/name" is there already (in any case):
    /// its path as saved.
    Taken(String),
    /// The saved query isn't there any more.
    Gone,
    /// Its file couldn't be read, and writing would lose what's in it.
    SaveBlocked,
    /// Writing the file failed.
    SaveFailed(String),
}

/// Why the library's file wasn't read as it is (`Library::load_from`).
#[derive(Debug, PartialEq, Eq)]
pub enum LoadWarning {
    /// It couldn't be read; saving is refused.
    Unreadable { path: PathBuf, error: String },
    /// It's JSON, but not saved queries as this version writes them (another
    /// version's, perhaps); left as it is, saving refused.
    OtherShape { path: PathBuf, error: String },
    /// It wasn't JSON: moved to `backup`, a new library starts.
    MovedAside { error: String, backup: PathBuf },
    /// It wasn't JSON and couldn't be moved aside; saving is refused.
    InvalidKept { error: String, move_error: String },
}

pub struct Library {
    path: Option<PathBuf>,
    /// In library order (`sort`): ungrouped first, then by group and name.
    queries: Vec<SavedQuery>,
    /// The file is there but couldn't be read or moved aside: writing
    /// would lose what's in it.
    save_blocked: bool,
    /// What was in the file isn't here (it couldn't be read, or was moved
    /// aside): a tab's link to a saved query that's missing is kept.
    incomplete: bool,
}

impl Library {
    /// The saved queries in `path` (none without a path), and a warning to
    /// show when its file couldn't be read. A file that can't be read, or
    /// is JSON of another shape (another version's, perhaps), is left as it
    /// is and saving is refused; one that isn't JSON at all is moved aside
    /// (`….json.bak-<time>`) rather than overwritten by the next save.
    pub fn load_from(path: Option<PathBuf>) -> (Self, Option<LoadWarning>) {
        let library = |path, queries, save_blocked, incomplete| Self { path, queries, save_blocked, incomplete };
        let Some(file) = path.clone() else { return (library(None, Vec::new(), false, false), None) };
        let text = match fs::read_to_string(&file) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return (library(path, Vec::new(), false, false), None);
            }
            Err(e) => {
                let warning = LoadWarning::Unreadable { path: file, error: e.to_string() };
                return (library(path, Vec::new(), true, true), Some(warning));
            }
        };
        match serde_json::from_str(&text) {
            Ok(queries) => (library(path, queries, false, false), None),
            Err(e) if serde_json::from_str::<serde_json::Value>(&text).is_ok() => {
                let warning = LoadWarning::OtherShape { path: file, error: e.to_string() };
                (library(path, Vec::new(), true, true), Some(warning))
            }
            Err(e) => {
                let backup = backup_path(&file);
                match fs::rename(&file, &backup) {
                    Ok(()) => {
                        let warning = LoadWarning::MovedAside { error: e.to_string(), backup };
                        (library(path, Vec::new(), false, true), Some(warning))
                    }
                    Err(rename) => {
                        let warning = LoadWarning::InvalidKept { error: e.to_string(), move_error: rename.to_string() };
                        (library(path, Vec::new(), true, true), Some(warning))
                    }
                }
            }
        }
    }

    /// Writes `queries` and, once they're written, keeps them: a save that
    /// fails leaves the library as it was.
    fn commit(&mut self, mut queries: Vec<SavedQuery>) -> Result<(), LibraryError> {
        sort(&mut queries);
        if let Some(path) = &self.path {
            if self.save_blocked {
                return Err(LibraryError::SaveBlocked);
            }
            let text = serde_json::to_string_pretty(&queries).map_err(|e| e.to_string());
            text.and_then(|text| write_atomic(path, &text).map_err(|e| format!("{e:#}")))
                .map_err(LibraryError::SaveFailed)?;
        }
        self.queries = queries;
        Ok(())
    }

    /// Whether what the file held may be missing here (see `load_from`'s
    /// warning): a saved query not found isn't one that was deleted.
    pub fn incomplete(&self) -> bool {
        self.incomplete
    }

    /// The saved queries, in library order.
    pub fn iter(&self) -> impl Iterator<Item = &SavedQuery> {
        self.queries.iter()
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.queries.len()
    }

    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.queries.is_empty()
    }

    /// The snippets group's queries, which autocomplete offers.
    pub fn snippets(&self) -> impl Iterator<Item = &SavedQuery> {
        self.queries.iter().filter(|q| q.is_snippet())
    }

    /// Whether there's a snippets group.
    pub fn has_snippets(&self) -> bool {
        self.snippets().next().is_some()
    }

    /// Sets when a saved query last changed, for tests of dates.
    #[cfg(test)]
    pub fn set_changed_for_tests(&mut self, id: &str, at: i64) {
        if let Some(q) = self.queries.iter_mut().find(|q| q.id == id) {
            q.changed = Some(at);
        }
    }

    pub fn get(&self, id: &str) -> Option<&SavedQuery> {
        self.queries.iter().find(|q| q.id == id)
    }

    /// Saves a new query under "group/name"; returns its id.
    pub fn add(&mut self, path: &str, sql: &str) -> Result<String, LibraryError> {
        let (group, name) = split_path(path);
        if name.is_empty() {
            return Err(LibraryError::NoName);
        }
        self.check_free(&group, &name, None)?;
        let id = uuid::Uuid::new_v4().to_string();
        let changed = Some(chrono::Local::now().timestamp());
        let mut queries = self.queries.clone();
        queries.push(SavedQuery { id: id.clone(), name, group, sql: sql.to_string(), changed });
        self.commit(queries)?;
        Ok(id)
    }

    pub fn update_sql(&mut self, id: &str, sql: &str) -> Result<(), LibraryError> {
        let mut queries = self.queries.clone();
        let q = queries.iter_mut().find(|q| q.id == id).ok_or(LibraryError::Gone)?;
        if q.sql == sql {
            return Ok(());
        }
        q.sql = sql.to_string();
        q.changed = Some(chrono::Local::now().timestamp());
        self.commit(queries)
    }

    /// Renames (and possibly regroups) a saved query from "group/name".
    pub fn rename(&mut self, id: &str, path: &str) -> Result<(), LibraryError> {
        let (group, name) = split_path(path);
        if name.is_empty() {
            return Err(LibraryError::NoName);
        }
        if self.get(id).is_none() {
            return Err(LibraryError::Gone);
        }
        self.check_free(&group, &name, Some(id))?;
        let mut queries = self.queries.clone();
        let q = queries.iter_mut().find(|q| q.id == id).ok_or(LibraryError::Gone)?;
        q.group = group;
        q.name = name;
        self.commit(queries)
    }

    pub fn delete(&mut self, id: &str) -> Result<(), LibraryError> {
        let queries = self.queries.iter().filter(|q| q.id != id).cloned().collect();
        self.commit(queries)
    }

    fn check_free(&self, group: &str, name: &str, except: Option<&str>) -> Result<(), LibraryError> {
        let taken = self.queries.iter().find(|q| {
            q.group.eq_ignore_ascii_case(group) && q.name.eq_ignore_ascii_case(name) && Some(q.id.as_str()) != except
        });
        // Named as it's saved, which may differ in case from what was typed.
        taken.map_or(Ok(()), |q| Err(LibraryError::Taken(q.path())))
    }

    /// Saved queries matching `query` (its words, and its `@` dates), in
    /// library order.
    #[cfg(test)]
    pub fn search(&self, query: &str) -> Vec<&SavedQuery> {
        let (words, when) = split_dates(query, chrono::Local::now());
        self.search_in(&words, when)
    }

    /// Saved queries with every one of `words`, changed within `when`.
    pub fn search_in(&self, words: &str, when: Option<Span>) -> Vec<&SavedQuery> {
        self.queries
            .iter()
            .filter(|q| when.is_none_or(|w| w.holds(q.changed)) && matches(words, &[&q.name, &q.group, &q.sql]))
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

/// Ungrouped first, then by group and name.
fn sort(queries: &mut [SavedQuery]) {
    queries.sort_by(|a, b| {
        (!a.group.is_empty(), a.group.to_lowercase(), a.name.to_lowercase()).cmp(&(
            !b.group.is_empty(),
            b.group.to_lowercase(),
            b.name.to_lowercase(),
        ))
    });
}

/// Where a file that couldn't be read is moved: `queries.json.bak-<time>`,
/// with `-2`, `-3`… after it when one of that second is there already.
fn backup_path(path: &std::path::Path) -> PathBuf {
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
    let first = path.with_extension(format!("json.bak-{stamp}"));
    if !first.exists() {
        return first;
    }
    (2..=u32::MAX).map(|n| path.with_extension(format!("json.bak-{stamp}-{n}"))).find(|p| !p.exists()).unwrap_or(first)
}

fn join(group: &str, name: &str) -> String {
    if group.is_empty() { name.to_string() } else { format!("{group}/{name}") }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    proptest::proptest! {
    // Cases that failed once are kept in tests/ (proptest's default would
    // be a folder at the crate's root) and tried again first.
    #![proptest_config(ProptestConfig {
        failure_persistence: Some(Box::new(proptest::test_runner::FileFailurePersistence::Direct(
            "tests/proptest-regressions/library.txt",
        ))),
        ..ProptestConfig::with_cases(400)
    })]

        #[test]
        fn library_search_never_panics(query in "\\PC{0,20}", text in "\\PC{0,40}") {
            let _ = matches(&query, &[&text]);
        }
    }

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
        let mut lib = Library::load_from(Some(path.clone())).0;
        let a = lib.add("reports/churn", "select 1").unwrap();
        let b = lib.add("adhoc", "select 2").unwrap();
        assert_eq!(lib.add("Reports/Churn", "x"), Err(LibraryError::Taken("reports/churn".into())));
        assert_eq!(lib.add("reports/", "x"), Err(LibraryError::NoName));
        // Ungrouped queries sort first.
        assert_eq!(lib.queries.iter().map(SavedQuery::path).collect::<Vec<_>>(), ["adhoc", "reports/churn"]);

        assert_eq!(lib.rename("no such id", "adhoc"), Err(LibraryError::Gone), "gone before taken");
        lib.rename(&a, "finance/churn by month").unwrap();
        lib.update_sql(&b, "select 3").unwrap();
        let lib2 = Library::load_from(Some(path)).0;
        assert_eq!(lib2.get(&a).unwrap().path(), "finance/churn by month");
        assert_eq!(lib2.get(&b).unwrap().sql, "select 3");

        lib.delete(&b).unwrap();
        assert_eq!(lib.queries.len(), 1);
    }

    #[test]
    fn an_invalid_file_is_moved_aside_not_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("q.json");
        fs::write(&path, "[{\"id\": \"a\", \"na").unwrap();
        let (mut lib, warning) = Library::load_from(Some(path.clone()));
        assert!(
            matches!(&warning, Some(LoadWarning::MovedAside { backup, .. }) if backup.to_string_lossy().contains(".bak-"))
        );
        assert!(lib.incomplete());
        assert!(lib.queries.is_empty());
        let backups: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().contains(".bak-"))
            .collect();
        assert_eq!(backups.len(), 1);
        assert_eq!(fs::read_to_string(backups[0].path()).unwrap(), "[{\"id\": \"a\", \"na");
        // Saving starts a new file; the old one stays where it was moved.
        lib.add("fresh", "select 1").unwrap();
        assert_eq!(Library::load_from(Some(path)).0.queries.len(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn an_unreadable_file_is_not_overwritten() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("q.json");
        fs::write(&path, "[]").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).unwrap();
        if fs::read_to_string(&path).is_ok() {
            return; // Running as root: permissions don't stop it.
        }
        let (mut lib, warning) = Library::load_from(Some(path.clone()));
        assert!(matches!(warning, Some(LoadWarning::Unreadable { .. })));
        assert_eq!(lib.add("x", "select 1"), Err(LibraryError::SaveBlocked));
        assert!(lib.queries.is_empty(), "a save refused changes nothing");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "[]");
    }

    #[test]
    fn json_of_another_shape_is_kept_and_not_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("q.json");
        let newer = r#"{"version": 2, "queries": []}"#;
        fs::write(&path, newer).unwrap();
        let (mut lib, warning) = Library::load_from(Some(path.clone()));
        assert!(matches!(warning, Some(LoadWarning::OtherShape { .. })) && lib.incomplete());
        assert_eq!(lib.add("x", "select 1"), Err(LibraryError::SaveBlocked));
        assert_eq!(fs::read_to_string(&path).unwrap(), newer);
    }

    #[test]
    fn backups_of_one_second_dont_overwrite_each_other() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("q.json");
        for text in ["first {", "second {"] {
            fs::write(&path, text).unwrap();
            let _ = Library::load_from(Some(path.clone()));
        }
        let mut kept: Vec<String> = fs::read_dir(dir.path())
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| fs::read_to_string(e.path()).unwrap())
            .collect();
        kept.sort();
        assert_eq!(kept, ["first {", "second {"]);
    }

    #[test]
    fn a_failed_save_changes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("q.json");
        let mut lib = Library::load_from(Some(path)).0;
        let id = lib.add("reports/churn", "select 1").unwrap();
        // Its folder is a file now: nothing can be written there.
        let blocker = dir.path().join("blocker");
        fs::write(&blocker, "").unwrap();
        lib.path = Some(blocker.join("q.json"));
        assert!(matches!(lib.add("other", "select 2"), Err(LibraryError::SaveFailed(_))));
        assert!(lib.rename(&id, "finance/churn").is_err());
        assert!(lib.update_sql(&id, "select 3").is_err());
        assert!(lib.delete(&id).is_err());
        assert_eq!(lib.queries.len(), 1);
        assert_eq!((lib.queries[0].path().as_str(), lib.queries[0].sql.as_str()), ("reports/churn", "select 1"));
    }

    #[test]
    fn snippets_are_found_in_any_case() {
        let mut lib = Library::load_from(None).0;
        lib.add("Snippets/top", "select 1").unwrap();
        assert!(lib.has_snippets());
        assert!(lib.queries[0].is_snippet());
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
        let mut lib = Library::load_from(None).0;
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
