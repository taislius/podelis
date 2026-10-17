//! The navigator's keys: the catalog, system tables, ordering.

use super::super::*;

impl App {
    pub(in crate::app) fn toggle_schema(&mut self) {
        self.schema_visible = !self.schema_visible;
        if self.schema_visible {
            self.focus = Focus::Schema;
            if self.schema.is_none() {
                self.load_catalog();
            }
        } else if self.focus == Focus::Schema {
            self.focus = Focus::Editor;
        }
    }

    /// (Re)loads the catalog in the background, keeping the current one
    /// visible meanwhile.
    pub(in crate::app) fn load_catalog(&mut self) {
        let Some(db) = self.database.clone() else { return };
        self.schema.get_or_insert_with(SchemaPanel::default).loading = true;
        let send = self.sender();
        let system = self.settings.system_tables;
        tokio::spawn(async move {
            // A `SET sql_mode` may have changed what lint rules should see.
            db.refresh_sql_mode().await;
            send(DbEventKind::Catalog(db.catalog(system).await.map_err(|e| crate::db::error_message(&e))));
            // Listing routines may need privileges the user lacks: then
            // the section just stays empty.
            send(DbEventKind::Routines(db.routines().await.unwrap_or_default()));
        });
    }

    /// Shows or hides the database's own catalog tables, for this session.
    pub(in crate::app) fn toggle_system_tables(&mut self) {
        self.settings.system_tables = !self.settings.system_tables;
        let shown = if self.settings.system_tables { "Showing" } else { "Hiding" };
        self.set_status(format!("{shown} system tables"), false);
        self.load_catalog();
    }

    #[allow(clippy::too_many_lines)] // one arm per key
    pub(in crate::app) fn handle_schema_key(&mut self, key: KeyEvent) {
        let engine = self.engine();
        let Some(panel) = &mut self.schema else { return };
        if panel.filter_editing {
            match panel.filter.handle(key) {
                InputResult::Submit => panel.filter_editing = false,
                InputResult::Cancel => {
                    panel.filter_editing = false;
                    panel.filter.clear();
                }
                InputResult::Changed => {}
                InputResult::Moved | InputResult::Ignored => return,
            }
            panel.selected = step_selectable(&panel.nodes(), 0, 1);
            return;
        }
        match key.code {
            KeyCode::Char('m') => return self.open_menu(None),
            KeyCode::Char(' ') => return self.open_palette(false),
            KeyCode::Char('z') => return self.toggle_zoom(),
            KeyCode::Char('r') => return self.load_catalog(),
            KeyCode::Char('.') => return self.toggle_system_tables(),
            KeyCode::Char('/') => return panel.filter_editing = true,
            KeyCode::Char('o') => return self.cycle_schema_sort(),
            KeyCode::Char('y') => return self.copy_focused(),
            KeyCode::Esc if !panel.filter.is_empty() => {
                panel.filter.clear();
                panel.selected = step_selectable(&panel.nodes(), 0, 1);
                return;
            }
            _ => {}
        }
        let nodes = panel.nodes();
        let Some(&node) = nodes.get(panel.selected) else { return };
        match key.code {
            KeyCode::Down | KeyCode::Char('j') => panel.selected = step_selectable(&nodes, panel.selected + 1, 1),
            KeyCode::PageDown => panel.selected = step_selectable(&nodes, panel.selected + panel.page.max(1), -1),
            KeyCode::PageUp => {
                panel.selected = step_selectable(&nodes, panel.selected.saturating_sub(panel.page.max(1)), 1);
            }
            KeyCode::Home | KeyCode::Char('g') => panel.selected = step_selectable(&nodes, 0, 1),
            KeyCode::End | KeyCode::Char('G') => panel.selected = step_selectable(&nodes, nodes.len(), -1),
            KeyCode::Up | KeyCode::Char('k') => {
                panel.selected = step_selectable(&nodes, panel.selected.saturating_sub(1), -1);
            }
            KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => match node {
                SchemaNode::Table(t) => {
                    let table = &mut panel.tables[t];
                    table.expanded = !table.expanded || key.code != KeyCode::Enter;
                    if table.expanded {
                        self.request_details(t);
                    }
                }
                SchemaNode::Routine(r) => self.show_routine(r),
                _ => {}
            },
            KeyCode::Char('f') => {
                if let Some(t) = node.table() {
                    self.show_related(t);
                }
            }
            KeyCode::Char('d') => match node {
                SchemaNode::Routine(r) => self.show_routine(r),
                _ => {
                    if let Some(t) = node.table() {
                        self.show_table_definition(t);
                    }
                }
            },
            KeyCode::Left | KeyCode::Char('h') => {
                if let Some(t) = node.table() {
                    panel.tables[t].expanded = false;
                    panel.selected = panel.nodes().iter().position(|n| *n == SchemaNode::Table(t)).unwrap_or(0);
                }
            }
            KeyCode::Char('s') => {
                let Some(t) = node.table() else { return };
                let info = &panel.tables[t].info;
                let query = format!(
                    "SELECT * FROM {}.{} LIMIT 100;",
                    engine.quote_ident(&info.schema),
                    engine.quote_ident(&info.name)
                );
                // After everything, a blank line apart: never inside another
                // statement. Then it runs, alone (as F6 would).
                self.append_statement(&query);
                if self.database.is_some() {
                    self.execute_current();
                }
            }
            _ => {}
        }
    }

    /// `o` in the navigator: orders the tables by name, rows or size,
    /// keeping the selected one selected.
    pub(in crate::app) fn cycle_schema_sort(&mut self) {
        let sizes = self.engine().capabilities().table_sizes;
        let Some(panel) = &mut self.schema else { return };
        let before = panel.nodes().get(panel.selected).copied();
        panel.sort = panel.sort.next();
        // SQLite keeps no sizes: there's nothing to order by.
        if panel.sort == SchemaSort::Size && !sizes {
            panel.sort = panel.sort.next();
        }
        let nodes = panel.nodes();
        panel.selected = before.and_then(|n| nodes.iter().position(|m| *m == n)).unwrap_or(0);
        panel.selected = step_selectable(&nodes, panel.selected, 1);
        panel.scroll = 0;
        let order = panel.sort.label();
        self.set_status(format!("Navigator ordered by {order}"), false);
    }
}

/// The nearest selectable (non-section) row from `from` in direction
/// `dir`, falling back to the other direction.
pub(in crate::app) fn step_selectable(nodes: &[SchemaNode], from: usize, dir: isize) -> usize {
    let ok = |i: usize| nodes.get(i).is_some_and(|n| !matches!(n, SchemaNode::Section(_)));
    let mut i = from.min(nodes.len().saturating_sub(1));
    loop {
        if ok(i) {
            return i;
        }
        match i.checked_add_signed(dir) {
            Some(next) if next < nodes.len() => i = next,
            _ => break,
        }
    }
    (0..nodes.len())
        .rev()
        .find(|&i| ok(i))
        .filter(|_| dir < 0)
        .or_else(|| (0..nodes.len()).find(|&i| ok(i)))
        .unwrap_or(0)
}
