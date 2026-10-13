//! Schema navigator details: indexes and foreign keys loaded when a table
//! is expanded, and table, view and function definitions.

use super::*;

impl App {
    /// Loads table `t`'s details unless they're loaded or on their way.
    pub(super) fn request_details(&mut self, t: usize) {
        let Some(db) = self.database.clone() else { return };
        let Some(table) = self.schema.as_mut().and_then(|p| p.tables.get_mut(t)) else { return };
        if table.details.is_some() || table.details_loading {
            return;
        }
        table.details_loading = true;
        let (schema, name) = (table.info.schema.clone(), table.info.name.clone());
        let send = self.sender();
        tokio::spawn(async move {
            let result = db.table_details(&schema, &name).await.map_err(|e| crate::db::error_message(&e));
            send(DbEventKind::TableDetails { schema, table: name, result });
        });
    }

    pub(super) fn apply_details(&mut self, schema: &str, name: &str, result: Result<TableDetails, String>) {
        let Some(panel) = &mut self.schema else { return };
        let Some(t) = panel.tables.iter().position(|t| t.info.schema == schema && t.info.name == name) else {
            return;
        };
        panel.tables[t].details_loading = false;
        match result {
            Ok(details) => {
                panel.tables[t].details = Some(details);
                let wanted = (schema.to_string(), name.to_string());
                if panel.show_definition_of.take_if(|w| *w == wanted).is_some() {
                    self.show_table_definition(t);
                }
                if std::mem::take(&mut self.pending_follow) {
                    self.follow_foreign_key();
                }
            }
            Err(e) => {
                panel.show_definition_of = None;
                self.set_status(format!("Couldn't load details of {name}: {e}"), true);
            }
        }
    }

    /// Opens table `t`'s definition, loading its details first if needed.
    pub(super) fn show_table_definition(&mut self, t: usize) {
        let engine = self.engine();
        let Some(panel) = &mut self.schema else { return };
        let Some(table) = panel.tables.get(t) else { return };
        let title = format!("{} {}", if table.info.is_view { "VIEW" } else { "TABLE" }, table.info.name);
        if table.details.is_none() {
            panel.show_definition_of = Some((table.info.schema.clone(), table.info.name.clone()));
            // Browsing: the popup says so rather than showing the last one.
            if self.cell_view.as_ref().is_some_and(|v| v.browse) {
                self.cell_view = Some(CellView::definition(title, "Loading…".to_string()));
            }
            return self.request_details(t);
        }
        self.cell_view = Some(CellView::definition(title, table_definition(engine, table)));
    }

    /// `f` in the navigator: the table's neighbours through foreign keys,
    /// two steps each way.
    pub(super) fn show_related(&mut self, t: usize) {
        let Some(db) = self.database.clone() else { return };
        let Some(table) = self.schema.as_ref().and_then(|p| p.tables.get(t)) else { return };
        let (schema, name) = (table.info.schema.clone(), table.info.name.clone());
        let send = self.sender();
        tokio::spawn(async move {
            let text = db
                .links()
                .await
                .map(|links| crate::related::describe(&links, &schema, &name))
                .map_err(|e| format!("Couldn't read the foreign keys: {}", crate::db::error_message(&e)));
            send(DbEventKind::Definition(format!("RELATED · {name}"), text));
        });
    }

    /// ←/→ in a navigator definition: the previous or next table's, view's
    /// or function's, selecting it in the navigator too.
    pub(super) fn step_definition(&mut self, back: bool) {
        let Some(panel) = &mut self.schema else { return };
        let nodes = panel.nodes();
        let browsable = |n: &SchemaNode| matches!(n, SchemaNode::Table(_) | SchemaNode::Routine(_));
        let next = if back {
            nodes[..panel.selected.min(nodes.len())].iter().rposition(browsable)
        } else {
            nodes.iter().skip(panel.selected + 1).position(browsable).map(|i| i + panel.selected + 1)
        };
        let Some(i) = next else { return };
        panel.selected = i;
        match nodes[i] {
            SchemaNode::Table(t) => self.show_table_definition(t),
            SchemaNode::Routine(r) => {
                if let Some(view) = &mut self.cell_view {
                    view.text = "Loading…".to_string();
                    view.scroll = 0;
                }
                self.show_routine(r);
            }
            _ => {}
        }
    }

    pub(super) fn show_routine(&mut self, r: usize) {
        let Some(db) = self.database.clone() else { return };
        let Some(routine) = self.schema.as_ref().and_then(|p| p.routines.get(r)).cloned() else { return };
        let send = self.sender();
        self.routine_definition_pending = true;
        tokio::spawn(async move {
            let title = format!("{} {}", routine.kind.to_uppercase(), routine.name);
            let result = db.routine_definition(&routine).await.map_err(|e| crate::db::error_message(&e));
            send(DbEventKind::Definition(title, result));
        });
    }
}

/// A view's query as a CREATE VIEW, SQLite's own CREATE TABLE, or else a
/// summary of the columns, indexes and foreign keys.
fn table_definition(engine: Engine, table: &SchemaTable) -> String {
    let info = &table.info;
    let qualified = format!("{}.{}", engine.quote_ident(&info.schema), engine.quote_ident(&info.name));
    let details = table.details.clone().unwrap_or_default();
    if let Some(def) = &details.definition {
        if info.is_view && !def.trim_start().to_ascii_uppercase().starts_with("CREATE") {
            return format!("CREATE VIEW {qualified} AS\n{def}");
        }
        if info.is_view || engine.capabilities().stores_create_sql {
            return def.clone();
        }
    }
    let mut out = format!("-- {qualified}\n");
    let name_w = table.columns.iter().map(|c| c.name.chars().count()).max().unwrap_or(0);
    for c in &table.columns {
        let _ = write!(out, "{:<name_w$}  {}", c.name, c.data_type);
        if !c.nullable {
            out.push_str(" NOT NULL");
        }
        if c.primary_key {
            out.push_str(" PRIMARY KEY");
        }
        if let Some(target) = table.foreign_key(&c.name) {
            let _ = write!(out, " → {target}");
        }
        out.push('\n');
    }
    if !details.indexes.is_empty() {
        out.push_str("\n-- indexes\n");
        for ix in &details.indexes {
            let line = ix.definition.clone().unwrap_or_else(|| {
                let unique = if ix.unique { "UNIQUE " } else { "" };
                format!("{unique}INDEX {} ({})", ix.name, ix.columns)
            });
            let _ = writeln!(out, "{line}");
        }
    }
    out
}
