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
/// line so multi-line queries round-trip.
pub struct History {
    path: Option<PathBuf>,
    entries: Vec<String>,
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
        let entries = path
            .as_ref()
            .and_then(|p| fs::read_to_string(p).ok())
            .map(|text| text.lines().filter_map(|line| serde_json::from_str::<String>(line).ok()).collect())
            .unwrap_or_default();
        Self { path, entries, pos: None, draft: String::new() }
    }

    /// All entries, oldest first.
    pub fn entries(&self) -> &[String] {
        &self.entries
    }

    pub fn push(&mut self, query: &str) {
        self.pos = None;
        let query = query.trim();
        if query.is_empty() || query.len() > MAX_ENTRY_BYTES || self.entries.last().is_some_and(|last| last == query) {
            return;
        }
        self.entries.push(query.to_string());
        if self.entries.len() > MAX_ENTRIES {
            self.entries.drain(..self.entries.len() - MAX_ENTRIES);
        }
        let mut total: usize = self.entries.iter().map(String::len).sum();
        let mut drop = 0;
        while total > MAX_BYTES && drop + 1 < self.entries.len() {
            total -= self.entries[drop].len();
            drop += 1;
        }
        self.entries.drain(..drop);
        self.save();
    }

    fn save(&self) {
        let Some(path) = &self.path else { return };
        let text: String =
            self.entries.iter().filter_map(|e| serde_json::to_string(e).ok()).map(|line| line + "\n").collect();
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
