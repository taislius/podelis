use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use crate::config::store::write_atomic;

const MAX_ENTRIES: usize = 500;
/// A query this long (a pasted dump, a generated script) isn't kept: the
/// file is rewritten on every run, and recalling it is rarely useful.
const MAX_ENTRY_BYTES: usize = 1 << 20;
/// The oldest entries go once all of them add up to this.
const MAX_BYTES: usize = 16 << 20;

/// Previously run queries, oldest first, persisted as one JSON string per
/// line so multi-line queries round-trip. When each ran comes on the line
/// before it, as `{"at": <unix seconds>}`: a version that reads only the
/// strings skips those lines and still reads every query.
pub struct History {
    path: Option<PathBuf>,
    entries: Vec<String>,
    /// When each entry ran (unix seconds), if known: entries from before
    /// times were kept have none.
    times: Vec<Option<i64>>,
    /// Index of the entry being shown while browsing; `None` when editing.
    pos: Option<usize>,
    /// What was in the editor before browsing started.
    draft: String,
}

pub fn project_history_path(dir: &Path, project_id: &str) -> PathBuf {
    dir.join(format!("{project_id}.jsonl"))
}

/// Moves the pre-projects global history file to the project that the old
/// connections were migrated into.
pub fn migrate_legacy(legacy: &Path, dir: &Path, project_id: &str) {
    let target = project_history_path(dir, project_id);
    if legacy.exists() && !target.exists() && fs::create_dir_all(dir).is_ok() {
        let _ = fs::rename(legacy, target);
    }
}

impl History {
    pub fn load_from(path: Option<PathBuf>) -> Self {
        let text = path.as_ref().and_then(|p| fs::read_to_string(p).ok()).unwrap_or_default();
        let (mut entries, mut times) = (Vec::new(), Vec::new());
        let mut at = None;
        for line in text.lines() {
            match serde_json::from_str::<serde_json::Value>(line) {
                Ok(serde_json::Value::String(sql)) => {
                    entries.push(sql);
                    times.push(at.take());
                }
                Ok(value) => at = value["at"].as_i64(),
                Err(_) => {}
            }
        }
        Self { path, entries, times, pos: None, draft: String::new() }
    }

    /// When entry `i` ran (unix seconds), if that's known.
    pub fn time(&self, i: usize) -> Option<i64> {
        self.times.get(i).copied().flatten()
    }

    /// All entries, oldest first.
    pub fn entries(&self) -> &[String] {
        &self.entries
    }

    pub fn push(&mut self, query: &str) {
        self.pos = None;
        let query = query.trim();
        if query.is_empty() || query.len() > MAX_ENTRY_BYTES {
            return;
        }
        // Run again: when it last ran. Written when that's another day (what
        // `@today` and the library's dates go by), not on every run.
        if self.entries.last().is_some_and(|last| last == query) {
            let now = chrono::Local::now();
            let day =
                |at: i64| chrono::DateTime::from_timestamp(at, 0).map(|t| t.with_timezone(&chrono::Local).date_naive());
            let Some(at) = self.times.last_mut() else { return };
            let new_day = at.is_none_or(|at| day(at) != Some(now.date_naive()));
            *at = Some(now.timestamp());
            if new_day {
                self.save();
            }
            return;
        }
        self.entries.push(query.to_string());
        self.times.push(Some(chrono::Local::now().timestamp()));
        if self.entries.len() > MAX_ENTRIES {
            let over = self.entries.len() - MAX_ENTRIES;
            self.entries.drain(..over);
            self.times.drain(..over);
        }
        let mut total: usize = self.entries.iter().map(String::len).sum();
        let mut drop = 0;
        while total > MAX_BYTES && drop + 1 < self.entries.len() {
            total -= self.entries[drop].len();
            drop += 1;
        }
        self.entries.drain(..drop);
        self.times.drain(..drop);
        self.save();
    }

    fn save(&self) {
        let Some(path) = &self.path else { return };
        let mut text = String::new();
        for (entry, at) in self.entries.iter().zip(&self.times) {
            let Ok(line) = serde_json::to_string(entry) else { continue };
            if let Some(at) = at {
                let _ = writeln!(text, "{{\"at\":{at}}}");
            }
            text.push_str(&line);
            text.push('\n');
        }
        // History is a convenience; failing to persist it isn't worth an error.
        let _ = write_atomic(path, &text);
    }

    /// Stops browsing, e.g. because the user edited the recalled text.
    pub fn reset(&mut self) {
        self.pos = None;
    }

    /// Steps back in history. Returns the text to put in the editor.
    pub fn older(&mut self, current: &str) -> Option<&str> {
        let pos = match self.pos {
            None if self.entries.is_empty() => return None,
            None => {
                self.draft = current.to_string();
                self.entries.len() - 1
            }
            Some(p) => p.saturating_sub(1),
        };
        self.pos = Some(pos);
        Some(&self.entries[pos])
    }

    /// Steps forward; past the newest entry returns the saved draft.
    pub fn newer(&mut self) -> Option<&str> {
        let pos = self.pos?;
        if pos + 1 < self.entries.len() {
            self.pos = Some(pos + 1);
            Some(&self.entries[pos + 1])
        } else {
            self.pos = None;
            Some(&self.draft)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn browse_and_restore_draft() {
        let mut h = History::load_from(None);
        h.push("select 1");
        h.push("select 2");
        assert_eq!(h.older("draft"), Some("select 2"));
        assert_eq!(h.older("ignored"), Some("select 1"));
        assert_eq!(h.older("ignored"), Some("select 1"));
        assert_eq!(h.newer(), Some("select 2"));
        assert_eq!(h.newer(), Some("draft"));
        assert_eq!(h.newer(), None);
    }

    #[test]
    fn times_are_kept_and_old_files_still_read() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("h.jsonl");
        // A file from before times: its entries have none.
        fs::write(&path, "\"select 1\"\n").unwrap();
        let mut h = History::load_from(Some(path.clone()));
        assert_eq!((h.entries.len(), h.time(0)), (1, None));
        h.push("select 2");
        let h = History::load_from(Some(path.clone()));
        assert_eq!(h.entries, ["select 1", "select 2"]);
        assert!(h.time(0).is_none() && h.time(1).is_some());
        // Run again: when it last ran.
        let mut h = History::load_from(Some(path.clone()));
        h.times[1] = Some(1);
        h.push("select 2");
        assert!(h.time(1).is_some_and(|at| at > 1) && h.entries.len() == 2);
        h.push("select 3");
        // Read the way an older version does: only the strings.
        let text = fs::read_to_string(&path).unwrap();
        let strings: Vec<String> = text.lines().filter_map(|l| serde_json::from_str::<String>(l).ok()).collect();
        assert_eq!(strings, ["select 1", "select 2", "select 3"]);
    }

    #[test]
    fn dedupes_consecutive_and_skips_blank() {
        let mut h = History::load_from(None);
        h.push("select 1");
        h.push(" select 1 \n");
        h.push("   ");
        assert_eq!(h.entries, ["select 1"]);
    }

    #[test]
    fn persists_multiline_entries() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("h.jsonl");
        let mut h = History::load_from(Some(path.clone()));
        h.push("select\n  1");
        let h2 = History::load_from(Some(path));
        assert_eq!(h2.entries, ["select\n  1"]);
    }

    #[test]
    fn legacy_history_moves_to_project() {
        let dir = tempfile::tempdir().unwrap();
        let legacy = dir.path().join("history.jsonl");
        fs::write(&legacy, "\"select 1\"\n").unwrap();
        let hist_dir = dir.path().join("history");
        migrate_legacy(&legacy, &hist_dir, "p1");
        assert!(!legacy.exists());
        let h = History::load_from(Some(project_history_path(&hist_dir, "p1")));
        assert_eq!(h.entries, ["select 1"]);
    }

    #[test]
    fn caps_entries() {
        let mut h = History::load_from(None);
        for i in 0..MAX_ENTRIES + 10 {
            h.push(&format!("select {i}"));
        }
        assert_eq!(h.entries.len(), MAX_ENTRIES);
        assert_eq!(h.entries[0], "select 10");
    }

    #[test]
    fn caps_sizes() {
        let mut h = History::load_from(None);
        h.push(&"x".repeat(MAX_ENTRY_BYTES + 1));
        assert!(h.entries.is_empty(), "too long to keep");
        let big = "y".repeat(MAX_ENTRY_BYTES - 2);
        for i in 0..20 {
            h.push(&format!("{big}{i}"));
        }
        let total: usize = h.entries.iter().map(String::len).sum();
        assert!(total <= MAX_BYTES && h.entries.len() < 20);
        assert!(h.entries.last().unwrap().ends_with("19"), "the newest stays");
    }
}
