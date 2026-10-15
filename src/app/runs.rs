//! A tab's earlier results, kept for reference: each query run opens a
//! new one (`#1`, `#2`…) above the grid, with the statement it ran and
//! its lines in the editor. The last [`KEEP`] are kept, and pinned ones
//! as well. `,` / `.` (or a click) show an earlier / later one, `p` pins.
//!
//! The one shown lives in the tab (`grid`, `last_sql`…, so every key
//! works on it as on the newest); its slot in `runs` holds it while
//! another is shown.

use super::*;

/// How many unpinned results a tab keeps.
pub const KEEP: usize = 10;

/// How many rows the unpinned results a tab keeps (besides the newest)
/// may hold together: past it, the oldest go first.
pub const KEEP_ROWS: usize = 200_000;

/// A run's result, and what ran.
#[derive(Default)]
pub struct KeptRun {
    /// Counted per tab from 1; dropping old ones doesn't renumber.
    pub number: usize,
    /// The statement, where it was in the editor, and when it ran.
    pub source: Option<PlanSource>,
    pub pinned: bool,
    /// Its result while another is shown.
    parked: Parked,
}

/// What a run's result needs of the tab, swapped in when it's shown.
#[derive(Default)]
struct Parked {
    grid: ResultGrid,
    last_sql: Option<(String, usize)>,
    last_query_infos: Vec<StatementInfo>,
    filled: Option<Filled>,
    error: Option<String>,
}

impl KeptRun {
    /// The rows it holds while parked (every result set of a script).
    fn rows(&self) -> usize {
        let grid = &self.parked.grid;
        let sets = grid.result_sets.iter().flatten().chain(&grid.results);
        sets.map(|r| r.rows.len()).sum()
    }
}

impl TabState {
    /// Swaps the shown run's result between the tab and its slot.
    fn swap_shown(&mut self) {
        let Some(run) = self.runs.get_mut(self.run_at) else { return };
        let p = &mut run.parked;
        std::mem::swap(&mut self.grid, &mut p.grid);
        std::mem::swap(&mut self.last_sql, &mut p.last_sql);
        std::mem::swap(&mut self.last_query_infos, &mut p.last_query_infos);
        std::mem::swap(&mut self.last_filled, &mut p.filled);
        std::mem::swap(&mut self.run_error, &mut p.error);
    }

    /// The oldest unpinned result, if one more would be past [`KEEP`]
    /// or [`KEEP_ROWS`]. The newest kept stays, however big.
    fn past_keeping(&self) -> Option<usize> {
        let (newest, older) = self.runs.split_last()?;
        let unpinned = older.iter().filter(|r| !r.pinned);
        let (count, rows) = unpinned.fold((0, 0), |(n, rows), r| (n + 1, rows + r.rows()));
        let count = count + usize::from(!newest.pinned);
        older.iter().position(|r| !r.pinned).filter(|_| count >= KEEP || rows > KEEP_ROWS)
    }

    /// The run shown, if any has been.
    pub fn shown_run(&self) -> Option<&KeptRun> {
        self.runs.get(self.run_at)
    }

    /// A new run's place, shown (empty until its result comes): the one
    /// shown is kept, a newest one without a result gives way, and the
    /// oldest unpinned go past [`KEEP`] (or [`KEEP_ROWS`]).
    fn open_run(&mut self, source: Option<PlanSource>) {
        // `X` stays on from one run to the next (`\G` doesn't).
        let vertical = self.grid.vertical && !self.grid.vertical_by_g;
        // The shown one to its slot; what the tab held before any run
        // (or the slot's empty leftovers) goes.
        self.swap_shown();
        self.grid = ResultGrid { vertical, ..ResultGrid::default() };
        self.last_sql = None;
        self.last_query_infos.clear();
        self.last_filled = None;
        self.run_error = None;
        if self.runs.last().is_some_and(|r| !r.pinned && r.parked.grid.results.is_none()) {
            self.runs.pop();
        }
        // Room for the new one.
        while let Some(oldest) = self.past_keeping() {
            self.runs.remove(oldest);
        }
        self.runs_made += 1;
        self.runs.push(KeptRun { number: self.runs_made, source, ..KeptRun::default() });
        self.run_at = self.runs.len() - 1;
    }
}

impl App {
    /// Before a query's run (not a re-run of the one shown): its own
    /// result, after the ones kept. `sql` is as the editor has it, from
    /// byte `offset`; `None` for rows that aren't a query's (a comparison).
    pub(super) fn open_run(&mut self, ran: Option<(&str, usize)>) {
        self.unswap_plan_grid();
        // The rows a stream hadn't sent stay loadable with `M`.
        self.close_stream();
        let text = self.tab.editor.text();
        // Where it is in the editor (a run from elsewhere isn't).
        let source = ran.filter(|(sql, _)| !sql.trim().is_empty()).and_then(|(sql, offset)| {
            let before = text.get(..offset).filter(|_| text[offset..].starts_with(sql))?;
            Some(PlanSource::new(sql.to_string(), false, before.matches('\n').count()))
        });
        self.tab.open_run(source);
        self.close_cell_popups();
    }

    /// Shows kept result `i` (an index into `runs`).
    pub(super) fn show_run(&mut self, i: usize) {
        if i == self.tab.run_at || i >= self.tab.runs.len() {
            return;
        }
        // Its outcome goes to the result shown.
        if self.running_here() {
            return self
                .set_status("Wait for the query to finish (or press Esc) to see an earlier result".into(), true);
        }
        self.stop_watch(": another result");
        self.close_stream();
        self.close_cell_popups();
        self.set_result_tab(ResultTab::Rows);
        self.tab.swap_shown();
        self.tab.run_at = i;
        self.tab.swap_shown();
        let run = &self.tab.runs[i];
        let mut status = format!("Result #{} of {} kept", run.number, self.tab.runs.len());
        if let Some(source) = &run.source {
            let _ = write!(status, ": {}", one_line(&source.sql));
        }
        self.set_status(status, false);
    }

    /// `,` / `.`: the result before or after the one shown.
    pub(super) fn step_run(&mut self, forward: bool) {
        let n = self.tab.runs.len();
        if n < 2 {
            return self.set_status("No other results kept yet: each query you run keeps its own".into(), false);
        }
        let at = self.tab.run_at;
        let next = if forward { (at + 1).min(n - 1) } else { at.saturating_sub(1) };
        if next == at {
            let edge = if forward { "the newest" } else { "the oldest kept" };
            return self.set_status(format!("This is {edge} result"), false);
        }
        self.show_run(next);
    }

    /// `p`: keeps the result shown past the last [`KEEP`], or lets it go.
    pub(super) fn toggle_pin_run(&mut self) {
        let at = self.tab.run_at;
        let Some(run) = self.tab.runs.get_mut(at) else {
            return self.set_status("Nothing to pin: run a query first".into(), false);
        };
        run.pinned = !run.pinned;
        let status = if run.pinned {
            format!("Pinned result #{}: kept however many queries run after it", run.number)
        } else {
            format!("Unpinned result #{}: it goes once {KEEP} newer ones are kept", run.number)
        };
        self.set_status(status, false);
    }

    /// Runs the shown result's query again (`M`, after a row change): it
    /// replaces that result instead of keeping a new one.
    pub(super) fn rerun_shown(&mut self, sql: String, offset: usize, infos: Vec<StatementInfo>) {
        let filled = self.tab.last_filled.clone();
        self.same_result = true;
        self.run_now(sql, offset, infos, RunPurpose::Query, filled);
    }

    /// Selects the shown result's statement in the editor, if it's still
    /// there as it ran.
    pub(super) fn go_to_run_statement(&mut self) {
        let Some(source) = self.tab.shown_run().and_then(|r| r.source.clone()) else { return };
        self.go_to_source(&source, "The result's statement", "F6 runs it again");
    }
}
