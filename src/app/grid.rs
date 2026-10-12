//! A tab's result grid: the result set (or a script's sets) and how it's
//! viewed: filter, sort, marks, the selected cell and the scroll.

use ratatui::widgets::TableState;

use super::LineInput;
use super::results::{column_widths, compute_view};
use super::rows;
use crate::db::QueryOutcome;

#[derive(Default)]
pub struct ResultGrid {
    pub results: Option<QueryOutcome>,
    /// A script's result sets when more than one statement returned rows:
    /// `None` at `result_set`, the one in `results`.
    pub result_sets: Vec<Option<QueryOutcome>>,
    pub result_set: usize,
    /// Said after the result, e.g. that its changes were rolled back.
    pub result_note: Option<String>,
    /// Marked result rows (`Space`), as indices into `results.rows`.
    pub marked: rows::Marks,
    /// Rows shown, as indices into `results.rows` (filtered and sorted).
    pub view: Vec<usize>,
    /// Sort column and whether it's descending.
    pub sort: Option<(usize, bool)>,
    pub filter: LineInput,
    pub filter_editing: bool,
    pub col_widths: Vec<u16>,
    pub selected_col: usize,
    pub col_offset: usize,
    pub table_state: TableState,
    /// First result row on screen (the table only builds visible rows).
    pub row_offset: usize,
    /// Rows shown one column per line, like mysql's `\G` (`X`).
    pub vertical: bool,
    /// Only because the run ended with `\G`.
    pub vertical_by_g: bool,
}

impl ResultGrid {
    /// Shows a run's result; a script's earlier result sets are kept to
    /// switch to. Returns how many sets there are.
    pub fn show(&mut self, mut outcome: QueryOutcome) -> usize {
        self.result_note = None;
        let earlier = std::mem::take(&mut outcome.earlier);
        let sets = earlier.len() + 1;
        self.result_sets =
            if earlier.is_empty() { Vec::new() } else { earlier.into_iter().map(Some).chain([None]).collect() };
        self.result_set = self.result_sets.len().saturating_sub(1);
        self.reset_view(&outcome);
        self.results = Some(outcome);
        sets
    }

    /// Switches to result set `index`. False if it's already shown or
    /// doesn't exist.
    pub fn select_set(&mut self, index: usize) -> bool {
        if index == self.result_set || index >= self.result_sets.len() {
            return false;
        }
        let Some(next) = self.result_sets[index].take() else { return false };
        self.result_sets[self.result_set] = self.results.take();
        self.result_set = index;
        self.reset_view(&next);
        self.results = Some(next);
        true
    }

    /// The next (or previous) result set's index, wrapping around; `None`
    /// with fewer than two.
    pub fn step_set(&self, forward: bool) -> Option<usize> {
        let n = self.result_sets.len();
        (n >= 2).then(|| if forward { (self.result_set + 1) % n } else { (self.result_set + n - 1) % n })
    }

    /// Fresh selection, sort and filter for a newly shown result.
    fn reset_view(&mut self, outcome: &QueryOutcome) {
        self.marked.clear();
        self.table_state = TableState::default();
        if !outcome.rows.is_empty() {
            self.table_state.select(Some(0));
        }
        self.view = (0..outcome.rows.len()).collect();
        self.sort = None;
        self.filter.clear();
        self.filter_editing = false;
        self.selected_col = 0;
        self.col_offset = 0;
        self.row_offset = 0;
        self.col_widths = column_widths(outcome);
    }

    /// Recomputes the visible rows after the filter or sort changed, and
    /// selects the first.
    pub fn refresh_view(&mut self) {
        let Some(results) = &self.results else { return };
        self.view = compute_view(results, self.filter.text(), self.sort);
        self.table_state.select((!self.view.is_empty()).then_some(0));
    }
}
