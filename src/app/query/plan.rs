//! Plans: EXPLAIN and EXPLAIN ANALYZE, a plan made aside while the
//! session is busy, the plan grid, going to a plan's statement.

use super::super::*;

impl App {
    /// Shows the plan for the selection or the statement at the cursor,
    /// without running it.
    /// While a query runs, it's made as soon as that finishes.
    pub(in crate::app) fn explain_current(&mut self) {
        if self.connection.database.is_none() {
            return;
        }
        let Some((stmt, line)) = self.statement_to_explain() else {
            return self.set_status(t!("editor.no_statement").to_string(), true);
        };
        if stmt.to_ascii_uppercase().starts_with("EXPLAIN") {
            return self.set_status(t!("plan.already_explain").to_string(), true);
        }
        if self.run.executing {
            return self.explain_aside(stmt, line);
        }
        self.explain_statement(stmt, line);
    }

    /// F7 while a query runs: made at once on a connection of its own where
    /// that sees the same as the session, else after the query.
    pub(in crate::app) fn explain_aside(&mut self, stmt: String, line: usize) {
        let version = self.connection.database.as_ref().map_or("", |db| db.version());
        let explain = self.engine().dialect().explain(&stmt, version);
        // (A file never: see `run_aside`.)
        let aside = !self.engine().is_file_based()
            && self.connection.txn == TxnState::Idle
            && crate::sql::params::find(&explain, self.engine()).is_empty();
        match self.connection.database.clone().filter(|_| aside) {
            Some(db) => {
                let send = self.sender();
                let limit = self.settings.row_limit;
                let tab = self.active_tab;
                tokio::spawn(async move {
                    let plan = db.run_aside(&explain, limit).await.ok().flatten();
                    send(DbEventKind::PlanAside { tab, stmt, line, plan });
                });
            }
            None => self.queue_plan(stmt, line),
        }
    }

    /// Only in the tab whose query runs: it's made there after it.
    pub(in crate::app) fn queue_plan(&mut self, stmt: String, line: usize) {
        if !self.running_here() {
            return self.set_status(self.running_elsewhere(t!("plan.f7_after")), true);
        }
        self.queued_plan = Some((stmt, line));
        self.set_status(t!("plan.queued").into(), false);
    }

    /// The plan made beside the running query, or (where it couldn't be)
    /// made after it.
    pub(in crate::app) fn plan_aside(&mut self, stmt: String, line: usize, plan: Option<QueryOutcome>) {
        match plan {
            Some(outcome) => {
                self.plan_request = Some((stmt, false, line));
                self.show_plan(outcome);
            }
            None if self.run.executing => self.queue_plan(stmt, line),
            None => self.explain_statement(stmt, line),
        }
    }

    /// The plan queued while a query ran, now that it's done.
    /// Not while something still runs (the plan waits) or asks the user
    /// something (a review, a confirmation: the plan is dropped).
    pub(in crate::app) fn run_queued_plan(&mut self) {
        if self.run.executing {
            return;
        }
        let Some((stmt, line)) = self.queued_plan.take() else { return };
        if !self.popup_open() {
            self.explain_statement(stmt, line);
        }
    }

    /// After a failed run, the queued plan isn't made: the error stays in view.
    pub(in crate::app) fn drop_queued_plan(&mut self) {
        if self.queued_plan.take().is_some() {
            let _ = write!(self.notes.status, " · {}", t!("plan.not_made"));
        }
    }

    pub(in crate::app) fn explain_statement(&mut self, stmt: String, line: usize) {
        let version = self.connection.database.as_ref().map_or("", |db| db.version());
        let explain = self.engine().dialect().explain(&stmt, version);
        self.plan_request = Some((stmt, false, line));
        if self.ask_for_params(&explain, 0, true) {
            return;
        }
        let infos = analyze::analyze(&parse(&explain, self.engine()));
        self.run_now(explain, 0, infos, RunPurpose::Explain, None);
    }

    /// Shows `tab` in the results pane: the QUERY PLAN's table, if it has
    /// one, takes the grid's place while it's shown.
    pub(in crate::app) fn set_result_tab(&mut self, tab: ResultTab) {
        // A filter being typed stays with its grid.
        self.tab.grid.filter_editing = false;
        self.tab.result_tab = tab;
        self.tab.tab_scroll = 0;
        self.tab.tab_hscroll = 0;
        self.sync_plan_grid();
    }

    /// Swaps the plan's grid in or out to match the tab shown.
    pub(in crate::app) fn sync_plan_grid(&mut self) {
        let want = self.tab.result_tab == ResultTab::Plan && self.tab.plan_grid.is_some();
        if want != self.tab.plan_swapped
            && let Some(other) = &mut self.tab.plan_grid
        {
            std::mem::swap(&mut self.tab.grid, other);
            self.tab.plan_swapped = want;
            self.cell_edit = None;
        }
    }

    /// The result's own grid back in place (before a run's outcome lands).
    pub(in crate::app) fn unswap_plan_grid(&mut self) {
        if self.tab.plan_swapped
            && let Some(other) = &mut self.tab.plan_grid
        {
            std::mem::swap(&mut self.tab.grid, other);
            self.tab.plan_swapped = false;
        }
    }

    /// Whether the pane shows a grid of rows: the result, or a plan's table.
    pub(in crate::app) fn grid_shown(&self) -> bool {
        self.tab.result_tab == ResultTab::Rows || self.tab.plan_swapped
    }

    /// Shows the statement the plan is for in the editor, selected and
    /// scrolled to from its first line, if it's still there.
    pub(in crate::app) fn go_to_plan_statement(&mut self) {
        let Some(source) = self.tab.plan_source.clone() else { return };
        self.go_to_source(&source, t!("plan.its_statement"), t!("plan.f7_again"));
    }

    /// Selects `source`'s statement in the editor and scrolls to it, if
    /// it's still there; `what` names it, `again` says how to redo it.
    pub(in crate::app) fn go_to_source(&mut self, source: &PlanSource, what: &str, again: &str) {
        let text = self.tab.editor.text();
        let Some((offset, line)) = source.find_in(&text) else {
            return self.set_status(t!("plan.gone", what = what), true);
        };
        let ed = &mut self.tab.editor;
        let start = ed.pos_of_offset(offset);
        let end = ed.pos_of_offset(offset + source.sql.len());
        // The cursor at its start (so the view starts there), the rest selected.
        ed.set_cursor(start);
        ed.set_anchor(Some(end));
        ed.scroll_row = start.0.saturating_sub(1);
        self.completion = None;
        // Zoomed, the editor is what's shown now.
        self.focus = Focus::Editor;
        let last = line + source.sql.matches('\n').count();
        let lines = if last > line {
            t!("results.lines", first = line + 1, last = last + 1)
        } else {
            t!("results.line", line = line + 1)
        };
        self.set_status(format!("{what}, {lines} ({again})"), false);
    }

    /// The selection, or the statement at the cursor, to EXPLAIN, and the
    /// editor line it starts on (from 0).
    pub(in crate::app) fn statement_to_explain(&self) -> Option<(String, usize)> {
        let text = self.tab.editor.text();
        let (start, stmt) = if let Some((a, b)) = self.tab.editor.selection_offsets() {
            let selected = &text[a..b];
            let lead = selected.len() - selected.trim_start().len();
            (a + lead, selected.trim().trim_end_matches(';').trim())
        } else {
            let s = sql::statement_at(&text, self.tab.editor.cursor_offset(), self.engine())?;
            (s.as_ptr() as usize - text.as_ptr() as usize, s)
        };
        Some((stmt.to_string(), text[..start].matches('\n').count()))
    }

    pub(in crate::app) fn show_plan(&mut self, outcome: QueryOutcome) {
        let plan = self
            .engine()
            .dialect()
            .format_plan(&outcome.columns, &outcome.rows)
            .unwrap_or_else(|| crate::db::Plan { text: t!("plan.none").to_string(), notes: Vec::new() });
        self.tab.plan = Some(plan);
        self.tab.plan_selected = None;
        self.tab.plan_source =
            self.plan_request.take().map(|(sql, analyzed, line)| PlanSource::new(sql, analyzed, line));
        // A plan with columns (MySQL's classic EXPLAIN) is a grid too.
        self.unswap_plan_grid();
        self.tab.plan_grid = (outcome.columns.len() > 1 && self.engine().capabilities().plan_as_grid).then(|| {
            let mut grid = Box::<ResultGrid>::default();
            grid.show(outcome);
            grid.table_state.select(Some(0));
            grid
        });
        self.set_result_tab(ResultTab::Plan);
        // Ready to scroll the plan; `1` goes back to the result.
        self.focus = Focus::Results;
        self.set_status(t!("plan.shown").to_string(), false);
    }
}
