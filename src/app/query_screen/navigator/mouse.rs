//! The navigator's mouse: its filter line, a row of the tree picked (a
//! double click expands a table or shows a routine), the wheel through it.

use super::super::super::mouse::{WHEEL_LINES, inside};
use crate::app::*;

impl App {
    /// A click in the navigator: its filter line, or a row of the tree.
    pub(in crate::app) fn click_schema(&mut self, x: u16, y: u16, double: bool) {
        let hits = &self.hits;
        self.focus = Focus::Schema;
        if hits.schema_filter_clear.is_some_and(|r| inside(r, x, y)) {
            if let Some(panel) = &mut self.connection.schema {
                panel.filter_editing = false;
                panel.clear_filter();
            }
            return;
        }
        if inside(hits.schema_filter, x, y) {
            if let Some(panel) = &mut self.connection.schema {
                panel.filter_editing = true;
            }
            return;
        }
        let on_list = inside(hits.schema_list, x, y);
        let index = hits.schema_offset + y.saturating_sub(hits.schema_list.y) as usize;
        let Some(panel) = &mut self.connection.schema else { return };
        // Clicking past the filter line ends its typing (the filter stays):
        // keys then go to the tree, to what was clicked.
        panel.filter_editing = false;
        if !on_list {
            return;
        }
        let nodes = panel.nodes();
        let Some(&node) = nodes.get(index) else { return };
        if matches!(node, SchemaNode::Section(_)) {
            return;
        }
        panel.selected = index;
        if double {
            match node {
                SchemaNode::Table(t) => {
                    panel.tables[t].expanded = !panel.tables[t].expanded;
                    if panel.tables[t].expanded {
                        self.request_details(t);
                    }
                }
                SchemaNode::Routine(r) => self.show_routine(r),
                _ => {}
            }
        }
    }

    /// The wheel over the navigator: the selection moves through what can
    /// be selected.
    pub(in crate::app) fn wheel_navigator(&mut self, down: bool) {
        let Some(panel) = &mut self.connection.schema else { return };
        let nodes = panel.nodes();
        let mut at = panel.selected;
        for _ in 0..WHEEL_LINES {
            at = if down {
                super::step_selectable(&nodes, at + 1, 1)
            } else {
                super::step_selectable(&nodes, at.saturating_sub(1), -1)
            };
        }
        panel.selected = at;
    }
}
