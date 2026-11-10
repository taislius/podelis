//! The navigator's keys: the catalog, system tables, ordering.

use crate::app::*;

impl App {
    pub(in crate::app) fn toggle_schema(&mut self) {
        self.panes.schema_visible = !self.panes.schema_visible;
        if self.panes.schema_visible {
            self.focus = Focus::Schema;
            if self.connection.schema.is_none() {
                self.load_catalog();
            }
        } else if self.focus == Focus::Schema {
            self.focus = Focus::Editor;
        }
    }

    /// (Re)loads the catalog in the background, keeping the current one
    /// visible meanwhile.
    pub(in crate::app) fn load_catalog(&mut self) {
        let Some(db) = self.connection.database.clone() else { return };
        // The schema may have changed: the next ask has the gateway read it
        // again too.
        self.ask.gateway_current = false;
        self.connection.schema.get_or_insert_with(SchemaPanel::default).loading = true;
        let send = self.sender();
        let system = self.settings.system_tables;
        tokio::spawn(async move {
            // A `SET sql_mode` may have changed what lint rules should see.
            db.refresh_sql_mode().await;
            send(DbEventKind::Catalog(db.catalog(system).await.map_err(|e| crate::db::error_message(&e))));
            // Listing routines may need privileges the user lacks: then
            // the section just stays empty.
            send(DbEventKind::Routines(db.routines().await.unwrap_or_default()));
            // Tables a rollback can't undo (MySQL's MyISAM, MEMORY): on a
            // connection of its own, as the catalog is read.
            if let Some(sql) = db.engine().dialect().and_then(crate::db::SqlDialect::tables_without_rollback)
                && let Ok(Some(found)) = db.run_aside(sql, 100_000).await
            {
                let cell = |r: &[Option<String>], i: usize| r.get(i).cloned().flatten().unwrap_or_default();
                let tables = found
                    .rows
                    .iter()
                    .map(|r| crate::app::session::Unrollable {
                        schema: cell(r, 0).to_lowercase(),
                        table: cell(r, 1).to_lowercase(),
                    })
                    .collect();
                send(DbEventKind::Unrollable(tables));
            }
        });
    }

    /// Shows or hides the database's own catalog tables, for this session.
    pub(in crate::app) fn toggle_system_tables(&mut self) {
        self.settings.system_tables = !self.settings.system_tables;
        let said =
            if self.settings.system_tables { t!("navigator.showing_system") } else { t!("navigator.hiding_system") };
        self.set_status(said.to_string(), false);
        self.load_catalog();
    }

    pub(in crate::app) fn handle_schema_key(&mut self, key: KeyEvent) {
        let Some(panel) = &mut self.connection.schema else { return };
        if panel.filter_editing {
            // While typing: the arrows pick among what's left, Enter opens
            // the one picked (the filter stays), Ctrl+V pastes.
            let nodes = panel.nodes();
            match key.code {
                KeyCode::Down => return panel.selected = step_selectable(&nodes, panel.selected + 1, 1),
                KeyCode::Up => {
                    return panel.selected = step_selectable(&nodes, panel.selected.saturating_sub(1), -1);
                }
                KeyCode::Char('v') if key.modifiers.contains(KeyModifiers::CONTROL) => return self.paste_clipboard(),
                _ => {}
            }
            match panel.filter.handle(key) {
                InputResult::Submit => {
                    panel.filter_editing = false;
                    return self.run_command(crate::app::commands::Command::Expand);
                }
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
        if key.code == KeyCode::Esc && !panel.filter.is_empty() {
            panel.filter.clear();
            panel.selected = step_selectable(&panel.nodes(), 0, 1);
            return;
        }
        if let Some(command) = self.pane_command(key) {
            return self.run_command(command);
        }
        let Some(panel) = &mut self.connection.schema else { return };
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
            KeyCode::Right | KeyCode::Char('l') => match node {
                SchemaNode::Table(t) => {
                    panel.tables[t].expanded = true;
                    self.request_details(t);
                }
                SchemaNode::Routine(r) => self.show_routine(r),
                _ => {}
            },
            KeyCode::Left | KeyCode::Char('h') => {
                if let Some(t) = node.table() {
                    panel.tables[t].expanded = false;
                    panel.selected = panel.nodes().iter().position(|n| *n == SchemaNode::Table(t)).unwrap_or(0);
                }
            }
            _ => {}
        }
    }

    /// What's selected in the navigator.
    pub(in crate::app) fn selected_node(&self) -> Option<SchemaNode> {
        let panel = self.connection.schema.as_ref()?;
        panel.nodes().get(panel.selected).copied()
    }

    /// `Enter`: a table opens or closes, a function shows.
    pub(in crate::app) fn expand_selected(&mut self) {
        match self.selected_node() {
            Some(SchemaNode::Table(t)) => {
                let Some(panel) = &mut self.connection.schema else { return };
                let table = &mut panel.tables[t];
                table.expanded = !table.expanded;
                if table.expanded {
                    self.request_details(t);
                }
            }
            Some(SchemaNode::Routine(r)) => self.show_routine(r),
            _ => {}
        }
    }

    /// `d`: the definition of the table, view or function selected.
    pub(in crate::app) fn show_selected_definition(&mut self) {
        match self.selected_node() {
            Some(SchemaNode::Routine(r)) => self.show_routine(r),
            Some(node) => {
                if let Some(t) = node.table() {
                    self.show_table_definition(t);
                }
            }
            None => {}
        }
    }

    /// `f`: the tables related to the one selected.
    pub(in crate::app) fn show_selected_related(&mut self) {
        if let Some(t) = self.selected_node().and_then(SchemaNode::table) {
            self.show_related(t);
        }
    }

    /// `s`: a `SELECT` from the table selected, added to the editor and run.
    pub(in crate::app) fn select_from_selected(&mut self) {
        let engine = self.engine();
        let Some(t) = self.selected_node().and_then(SchemaNode::table) else { return };
        let Some(panel) = &self.connection.schema else { return };
        let info = &panel.tables[t].info;
        let table = format!("{}.{}", engine.quote_ident(&info.schema), engine.quote_ident(&info.name));
        // Without SQL, no rows statement to write (yet).
        let Some(dialect) = engine.dialect() else { return };
        let query = format!("{};", dialect.select_rows(&table, None, Some(100)));
        // After everything, a blank line apart: never inside another
        // statement. Then it runs, alone (as F6 would), and the keys stay
        // here, for the next table.
        let focus = self.focus;
        self.append_statement(&query);
        self.focus = focus;
        if self.connection.database.is_some() {
            self.execute_current();
        }
    }

    /// `a`: the selected table's statistics updated (`ANALYZE`), as `s`
    /// does its select: added after everything and run alone, through the
    /// usual checks (a production or read-only connection's).
    pub(in crate::app) fn analyze_selected(&mut self) {
        let engine = self.engine();
        let Some(t) = self.selected_node().and_then(SchemaNode::table) else { return };
        let Some(panel) = &self.connection.schema else { return };
        let info = &panel.tables[t].info;
        let table = format!("{}.{}", engine.quote_ident(&info.schema), engine.quote_ident(&info.name));
        let Some(statement) = engine.dialect().and_then(|d| d.analyze_table(&table)) else { return };
        let focus = self.focus;
        self.append_statement(&format!("{statement};"));
        self.focus = focus;
        if self.connection.database.is_some() {
            self.execute_current();
        }
    }

    /// `y`: the selected name, as it's written in SQL.
    pub(in crate::app) fn copy_schema_name(&mut self) {
        match self.schema_name() {
            Some((name, what)) => self.copy_text(name.clone(), &t!("copy.name", what = what, name = name)),
            None => self.set_status(t!("copy.nothing").to_string(), false),
        }
    }

    /// `o` in the navigator: orders the tables by name, rows or size,
    /// keeping the selected one selected.
    pub(in crate::app) fn cycle_schema_sort(&mut self) {
        let sizes = self.engine().capabilities().table_sizes;
        let Some(panel) = &mut self.connection.schema else { return };
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
        self.set_status(t!("navigator.ordered", order = order), false);
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
