//! Watch mode: the last read run again every few seconds, like `watch`,
//! with the rows that changed since the run before marked. For a queue
//! draining, replication lag, a migration's progress, locks.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use super::*;

pub struct Watch {
    pub every: Duration,
    /// When it runs next (once the last run is done).
    pub next: Instant,
    pub runs: u32,
    /// The rows of the run before, to mark what changed.
    previous: Option<Vec<Vec<Option<String>>>>,
    /// What's watched: the query, in which tab, on which connection (by
    /// the app's generation). Anything else stops it.
    sql: String,
    tab: usize,
    generation: u64,
}

/// The intervals offered in the command palette, in seconds.
pub const EVERY: [u64; 5] = [2, 5, 10, 30, 60];

/// Which of `next`'s rows are new or changed since `previous`: matched by
/// the first column when it tells rows apart on both sides, else as whole
/// rows (a changed row then shows as new).
pub fn changed_rows(previous: &[Vec<Option<String>>], next: &[Vec<Option<String>>]) -> HashSet<usize> {
    let first = |rows: &[Vec<Option<String>>]| -> Option<HashMap<Option<String>, usize>> {
        let mut keys = HashMap::new();
        for (i, row) in rows.iter().enumerate() {
            if keys.insert(row.first().cloned().flatten(), i).is_some() {
                return None;
            }
        }
        Some(keys)
    };
    if let (Some(before), Some(_)) = (first(previous), first(next)) {
        return (0..next.len())
            .filter(|&i| {
                let key = next[i].first().cloned().flatten();
                before.get(&key).is_none_or(|&j| previous[j] != next[i])
            })
            .collect();
    }
    let mut left: HashMap<&Vec<Option<String>>, usize> = HashMap::new();
    for row in previous {
        *left.entry(row).or_default() += 1;
    }
    (0..next.len())
        .filter(|&i| match left.get_mut(&next[i]) {
            Some(n) if *n > 0 => {
                *n -= 1;
                false
            }
            _ => true,
        })
        .collect()
}

impl App {
    /// Starts running the last query again every `seconds`: reads only.
    pub(super) fn start_watch(&mut self, seconds: u64) {
        if self.tab.last_sql.is_none() {
            return self.set_status("Run a query first, then watch it".to_string(), true);
        }
        if !self.tab.last_query_infos.iter().all(|i| i.kind == Kind::Read) {
            return self.set_status("Only a query that reads can be watched".to_string(), true);
        }
        let every = Duration::from_secs(seconds);
        let sql = self.tab.last_sql.as_ref().map(|(sql, _)| sql.clone()).unwrap_or_default();
        self.watch = Some(Watch {
            every,
            next: Instant::now(),
            runs: 0,
            previous: None,
            sql,
            tab: self.active_tab,
            generation: self.generation,
        });
        self.watch_tick();
    }

    pub(super) fn stop_watch(&mut self, why: &str) {
        if self.watch.take().is_some() {
            self.tab.grid.changed_rows.clear();
            self.set_status(format!("Stopped watching{why}"), false);
        }
    }

    /// Runs the query again when it's time and the last run is done.
    pub(super) fn watch_tick(&mut self) {
        let due = self.watch.as_ref().is_some_and(|w| Instant::now() >= w.next);
        if !due || self.executing {
            return;
        }
        let Some((sql, offset)) = self.tab.last_sql.clone() else { return self.stop_watch("") };
        let infos = self.tab.last_query_infos.clone();
        // Only ever the read it started with, where it started.
        let same = self
            .watch
            .as_ref()
            .is_some_and(|w| w.sql == sql && w.tab == self.active_tab && w.generation == self.generation);
        if !same || !infos.iter().all(|i| i.kind == Kind::Read) {
            return self.stop_watch(": the query or connection changed");
        }
        let filled = self.last_run_filled.clone();
        self.watch_rerun = true;
        self.run_now(sql, offset, infos, RunPurpose::Query, filled);
        self.watch_rerun = false;
    }

    /// After a watched run: marks what changed, keeps the place in the grid,
    /// and plans the next run.
    pub(super) fn after_watched_run(&mut self, place: (Option<usize>, usize, usize)) {
        let Some(watch) = &mut self.watch else { return };
        watch.runs += 1;
        watch.next = Instant::now() + watch.every;
        let rows = self.tab.grid.results.as_ref().map(|r| r.rows.clone()).unwrap_or_default();
        let changed = watch.previous.as_ref().map(|before| changed_rows(before, &rows)).unwrap_or_default();
        watch.previous = Some(rows);
        let (every, runs) = (watch.every.as_secs(), watch.runs);
        let grid = &mut self.tab.grid;
        let (selected, col, offset) = place;
        grid.table_state.select(selected.map(|s| s.min(grid.view.len().saturating_sub(1))));
        grid.selected_col = col;
        grid.row_offset = offset;
        let note = if runs == 1 { String::new() } else { format!(" · {} changed", changed.len()) };
        grid.changed_rows = changed;
        self.set_status(format!("Watching every {every} s · run {runs}{note} · Esc stops"), false);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(list: &[(&str, &str)]) -> Vec<Vec<Option<String>>> {
        list.iter().map(|(a, b)| vec![Some((*a).to_string()), Some((*b).to_string())]).collect()
    }

    #[test]
    fn what_changed_between_runs() {
        // By the first column: a changed value and a new row.
        let before = rows(&[("1", "queued"), ("2", "queued")]);
        let after = rows(&[("1", "queued"), ("2", "done"), ("3", "queued")]);
        assert_eq!(changed_rows(&before, &after), HashSet::from([1, 2]));
        // Without a key that tells rows apart: as whole rows.
        let before = rows(&[("x", "1"), ("x", "1")]);
        let after = rows(&[("x", "1"), ("x", "2"), ("x", "1")]);
        assert_eq!(changed_rows(&before, &after), HashSet::from([1]));
        assert!(changed_rows(&after, &after).is_empty());
    }
}
