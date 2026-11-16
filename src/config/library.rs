//! Saved queries: named, optionally grouped, per project; searchable by
//! name, group and SQL. Stored as JSON next to the history files.

use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::config::store::write_atomic;

/// The group whose saved queries autocomplete offers as snippets.
pub const SNIPPETS: &str = "snippets";

/// A saved query's id: what a tab tied to it keeps. Written as the plain
/// string it always was.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SavedQueryId(String);

impl SavedQueryId {
    fn new() -> Self {
        Self(uuid::Uuid::new_v4().to_string())
    }
}

/// An id made up by a test, for a link to a query that isn't saved.
#[cfg(test)]
impl From<&str> for SavedQueryId {
    fn from(id: &str) -> Self {
        Self(id.to_string())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedQuery {
    pub id: SavedQueryId,
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
    /// Another Podėlis on the project changed this query's SQL since it was
    /// read here: saving again writes this one over it.
    ChangedElsewhere(String),
    /// Another program made the file something other than saved queries
    /// (why, as the reader said it); saving is refused so it isn't lost.
    Spoilt(String),
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
    /// The file as this library last read or wrote it: another Podėlis on
    /// the project changed it when it's different (`refresh`).
    on_disk: Option<Seen>,
    /// Why the file stopped being saved queries after it was read here.
    spoilt: Option<String>,
}

/// A file as it was seen: its size and time (to tell cheaply that it's the
/// same), and its text's hash (to tell that a file touched again holds the
/// same text).
#[derive(Clone, Copy, PartialEq, Eq)]
struct Seen {
    len: u64,
    modified: Option<std::time::SystemTime>,
    hash: u64,
}

impl Seen {
    fn of(meta: &fs::Metadata, text: &str) -> Self {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        text.hash(&mut hasher);
        Self { len: meta.len(), modified: meta.modified().ok(), hash: hasher.finish() }
    }

    fn same_file(&self, meta: &fs::Metadata) -> bool {
        self.len == meta.len() && self.modified.is_some() && self.modified == meta.modified().ok()
    }
}

impl Library {
    /// The saved queries in `path` (none without a path), and a warning to
    /// show when its file couldn't be read. A file that can't be read, or
    /// is JSON of another shape (another version's, perhaps), is left as it
    /// is and saving is refused; one that isn't JSON at all is moved aside
    /// (`….json.bak-<time>`) rather than overwritten by the next save.
    pub fn load_from(path: Option<PathBuf>) -> (Self, Option<LoadWarning>) {
        let library = |path, queries, save_blocked, incomplete| Self {
            path,
            queries,
            save_blocked,
            incomplete,
            on_disk: None,
            spoilt: None,
        };
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
            Ok(queries) => {
                let on_disk = fs::metadata(&file).ok().map(|meta| Seen::of(&meta, &text));
                (Self { on_disk, ..library(path, queries, false, false) }, None)
            }
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

    /// Takes in what another Podėlis on the project wrote since this one
    /// last read or wrote the file, so a change starts from it rather than
    /// writing over it; whether anything changed. A file that couldn't be
    /// read before and can now is taken in too (saving allowed again). One
    /// gone, or that can't be read now, is left: the next save writes
    /// what's here. One that isn't saved queries now stops saving, so it
    /// isn't overwritten, until it's what it was again.
    /// Its size and time unchanged, the file is taken as unchanged: cheap
    /// enough for opening the library or completion.
    pub fn refresh(&mut self) -> bool {
        self.take_in(false)
    }

    /// As `refresh`; `always_read` reads it whatever its size and time say
    /// (before a change: a filesystem's times may be too coarse to tell).
    fn take_in(&mut self, always_read: bool) -> bool {
        let Some(path) = &self.path else { return false };
        let Ok(meta) = fs::metadata(path) else { return false };
        if !always_read && self.on_disk.is_some_and(|seen| seen.same_file(&meta)) {
            return false;
        }
        let Ok(text) = fs::read_to_string(path) else { return false };
        let seen = Seen::of(&meta, &text);
        if self.on_disk.is_some_and(|before| before.hash == seen.hash) {
            // Touched, or put back as it was after being spoilt.
            self.on_disk = Some(seen);
            if self.spoilt.take().is_some() {
                self.save_blocked = false;
            }
            return false;
        }
        match serde_json::from_str::<Vec<SavedQuery>>(&text) {
            Ok(mut queries) => {
                sort(&mut queries);
                self.queries = queries;
                self.on_disk = Some(seen);
                // What couldn't be read here is in hand now; queries moved
                // aside with a file that wasn't JSON are still only there.
                if self.save_blocked {
                    self.incomplete = false;
                }
                self.save_blocked = false;
                self.spoilt = None;
                true
            }
            Err(e) => {
                if self.on_disk.is_some() {
                    self.spoilt = Some(e.to_string());
                }
                self.save_blocked = true;
                false
            }
        }
    }

    /// Holds other Podėlis on the project off this library's file while a
    /// change reads it, changes it and writes it (released when dropped).
    /// None where there's no file (or no lock to be had): nothing to wait for.
    fn lock(&self) -> Option<fs::File> {
        use std::os::unix::fs::OpenOptionsExt as _;
        let path = self.path.as_ref()?;
        let lock = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .mode(0o600)
            .open(path.with_extension("lock"))
            .ok()?;
        lock.lock().ok()?;
        Some(lock)
    }

    /// Writes `queries` and, once they're written, keeps them: a save that
    /// fails leaves the library as it was.
    fn commit(&mut self, mut queries: Vec<SavedQuery>) -> Result<(), LibraryError> {
        sort(&mut queries);
        if let Some(path) = &self.path {
            if self.save_blocked {
                return Err(self.spoilt.clone().map_or(LibraryError::SaveBlocked, LibraryError::Spoilt));
            }
            let text = serde_json::to_string_pretty(&queries).map_err(|e| LibraryError::SaveFailed(e.to_string()))?;
            write_atomic(path, &text).map_err(|e| LibraryError::SaveFailed(format!("{e:#}")))?;
            self.on_disk = fs::metadata(path).ok().map(|meta| Seen::of(&meta, &text));
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
    pub fn set_changed_for_tests(&mut self, id: &SavedQueryId, at: i64) {
        if let Some(q) = self.queries.iter_mut().find(|q| q.id == *id) {
            q.changed = Some(at);
        }
    }

    pub fn get(&self, id: &SavedQueryId) -> Option<&SavedQuery> {
        self.queries.iter().find(|q| q.id == *id)
    }

    /// Saves a new query under "group/name"; returns its id.
    pub fn add(&mut self, path: &str, sql: &str) -> Result<SavedQueryId, LibraryError> {
        let _lock = self.lock();
        self.take_in(true);
        let (group, name) = split_path(path);
        if name.is_empty() {
            return Err(LibraryError::NoName);
        }
        self.check_free(&group, &name, None)?;
        let id = SavedQueryId::new();
        let changed = Some(chrono::Local::now().timestamp());
        let mut queries = self.queries.clone();
        queries.push(SavedQuery { id: id.clone(), name, group, sql: sql.to_string(), changed });
        self.commit(queries)?;
        Ok(id)
    }

    /// Saves a query's new SQL. When another Podėlis on the project changed
    /// it meanwhile, says so the first time rather than writing over it.
    pub fn update_sql(&mut self, id: &SavedQueryId, sql: &str) -> Result<(), LibraryError> {
        let _lock = self.lock();
        let before = self.get(id).map(|q| q.sql.clone());
        self.take_in(true);
        if let (Some(before), Some(now)) = (before, self.get(id))
            && now.sql != before
            && now.sql != sql
        {
            return Err(LibraryError::ChangedElsewhere(now.path()));
        }
        let mut queries = self.queries.clone();
        let q = queries.iter_mut().find(|q| q.id == *id).ok_or(LibraryError::Gone)?;
        if q.sql == sql {
            return Ok(());
        }
        q.sql = sql.to_string();
        q.changed = Some(chrono::Local::now().timestamp());
        self.commit(queries)
    }

    /// Renames (and possibly regroups) a saved query from "group/name".
    pub fn rename(&mut self, id: &SavedQueryId, path: &str) -> Result<(), LibraryError> {
        let _lock = self.lock();
        self.take_in(true);
        let (group, name) = split_path(path);
        if name.is_empty() {
            return Err(LibraryError::NoName);
        }
        if self.get(id).is_none() {
            return Err(LibraryError::Gone);
        }
        self.check_free(&group, &name, Some(id))?;
        let mut queries = self.queries.clone();
        let q = queries.iter_mut().find(|q| q.id == *id).ok_or(LibraryError::Gone)?;
        q.group = group;
        q.name = name;
        self.commit(queries)
    }

    pub fn delete(&mut self, id: &SavedQueryId) -> Result<(), LibraryError> {
        let _lock = self.lock();
        self.take_in(true);
        if self.get(id).is_none() {
            return Err(LibraryError::Gone);
        }
        let queries = self.queries.iter().filter(|q| q.id != *id).cloned().collect();
        self.commit(queries)
    }

    fn check_free(&self, group: &str, name: &str, except: Option<&SavedQueryId>) -> Result<(), LibraryError> {
        let taken = self.queries.iter().find(|q| {
            q.group.eq_ignore_ascii_case(group) && q.name.eq_ignore_ascii_case(name) && Some(&q.id) != except
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

        assert_eq!(lib.rename(&SavedQueryId::new(), "adhoc"), Err(LibraryError::Gone), "gone before taken");
        lib.rename(&a, "finance/churn by month").unwrap();
        lib.update_sql(&b, "select 3").unwrap();
        let lib2 = Library::load_from(Some(path)).0;
        assert_eq!(lib2.get(&a).unwrap().path(), "finance/churn by month");
        assert_eq!(lib2.get(&b).unwrap().sql, "select 3");

        lib.delete(&b).unwrap();
        assert_eq!(lib.queries.len(), 1);
        assert_eq!(lib.delete(&b), Err(LibraryError::Gone));
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
    fn ids_are_written_as_the_strings_they_were() {
        let old = r#"[{"id": "1b2c", "name": "churn", "group": "reports", "sql": "select 1"}]"#;
        let queries: Vec<SavedQuery> = serde_json::from_str(old).unwrap();
        assert_eq!(queries[0].id, SavedQueryId::from("1b2c"));
        assert!(serde_json::to_string(&queries).unwrap().contains(r#""id":"1b2c""#));
    }

    #[test]
    fn two_windows_on_one_project_keep_each_others_changes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("q.json");
        let mut a = Library::load_from(Some(path.clone())).0;
        let mut b = Library::load_from(Some(path.clone())).0;
        let one = a.add("one", "select 1").unwrap();
        let two = b.add("two", "select 2").unwrap();
        assert_eq!(Library::load_from(Some(path)).0.queries.len(), 2, "b's save kept a's query");
        // A name a took is taken for b too, and what a deleted is gone for b.
        assert!(matches!(b.add("ONE", "x"), Err(LibraryError::Taken(_))));
        a.delete(&two).unwrap();
        assert_eq!(b.update_sql(&two, "select 3"), Err(LibraryError::Gone));
        b.update_sql(&one, "select 11").unwrap();
        a.refresh();
        assert_eq!(a.get(&one).unwrap().sql, "select 11");
    }

    #[cfg(unix)]
    #[test]
    fn a_file_readable_again_is_taken_in_and_saving_allowed() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("q.json");
        let mut first = Library::load_from(Some(path.clone())).0;
        let kept = first.add("kept", "select 1").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).unwrap();
        if fs::read_to_string(&path).is_ok() {
            return; // Running as root: permissions don't stop it.
        }
        let mut lib = Library::load_from(Some(path.clone())).0;
        assert_eq!(lib.add("x", "select 2"), Err(LibraryError::SaveBlocked));
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        lib.add("x", "select 2").unwrap();
        assert!(lib.get(&kept).is_some() && !lib.incomplete());
        assert_eq!(Library::load_from(Some(path)).0.queries.len(), 2);
    }

    #[test]
    fn a_file_spoilt_by_another_window_stops_saving_until_its_back() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("q.json");
        let mut lib = Library::load_from(Some(path.clone())).0;
        lib.add("one", "select 1").unwrap();
        let good = fs::read_to_string(&path).unwrap();
        fs::write(&path, "not json").unwrap();
        assert!(matches!(lib.add("two", "select 2"), Err(LibraryError::Spoilt(_))));
        assert_eq!(fs::read_to_string(&path).unwrap(), "not json");
        // Put back as it was (an undo): saving goes on.
        fs::write(&path, good).unwrap();
        lib.add("two", "select 2").unwrap();
        assert_eq!(Library::load_from(Some(path)).0.queries.len(), 2);
    }

    #[test]
    fn two_windows_saving_at_once_lose_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("q.json");
        let windows: Vec<_> = ["a", "b", "c"]
            .into_iter()
            .map(|window| {
                let path = path.clone();
                std::thread::spawn(move || {
                    let mut lib = Library::load_from(Some(path)).0;
                    for i in 0..25 {
                        lib.add(&format!("{window}/{i}"), "select 1").unwrap();
                    }
                })
            })
            .collect();
        for window in windows {
            window.join().unwrap();
        }
        assert_eq!(Library::load_from(Some(path)).0.queries.len(), 75);
    }

    #[test]
    fn sql_changed_in_another_window_is_said_before_its_written_over() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("q.json");
        let mut a = Library::load_from(Some(path.clone())).0;
        let id = a.add("one", "select 1").unwrap();
        let mut b = Library::load_from(Some(path)).0;
        b.update_sql(&id, "select 2").unwrap();
        assert_eq!(a.update_sql(&id, "select 1 -- mine"), Err(LibraryError::ChangedElsewhere("one".into())));
        // Said once: again, it's written.
        a.update_sql(&id, "select 1 -- mine").unwrap();
        b.refresh();
        assert_eq!(b.get(&id).unwrap().sql, "select 1 -- mine");
    }

    #[test]
    fn queries_moved_aside_stay_missing_when_another_window_saves() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("q.json");
        fs::write(&path, "not json").unwrap();
        let mut lib = Library::load_from(Some(path.clone())).0;
        assert!(lib.incomplete());
        Library::load_from(Some(path)).0.add("fresh", "select 1").unwrap();
        assert!(lib.refresh());
        assert!(lib.incomplete(), "what was moved aside is still only in the backup");
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
