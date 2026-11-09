//! Schema navigator details: indexes and foreign keys loaded when a table
//! is expanded, and table, view and function definitions.

use crate::app::*;

impl App {
    /// Loads table `t`'s details unless they're loaded or on their way.
    pub(in crate::app) fn request_details(&mut self, t: usize) {
        let Some(db) = self.connection.database.clone() else { return };
        let Some(table) = self.connection.schema.as_mut().and_then(|p| p.tables.get_mut(t)) else { return };
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

    pub(in crate::app) fn apply_details(&mut self, schema: &str, name: &str, result: Result<TableDetails, String>) {
        let Some(panel) = &mut self.connection.schema else { return };
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
                self.set_status(t!("schema.details_failed", name = name, e = e), true);
            }
        }
    }

    /// Opens table `t`'s definition, loading its details first if needed.
    pub(in crate::app) fn show_table_definition(&mut self, t: usize) {
        let engine = self.engine();
        let Some(panel) = &mut self.connection.schema else { return };
        let Some(table) = panel.tables.get(t) else { return };
        let title = format!("{} {}", if table.info.is_view { "VIEW" } else { "TABLE" }, table.info.name);
        if table.details.is_none() {
            panel.show_definition_of = Some((table.info.schema.clone(), table.info.name.clone()));
            // Browsing: the popup says so rather than showing the last one.
            if self.popups.cell_view.as_ref().is_some_and(|v| v.browse) {
                self.popups.cell_view = Some(CellView::definition(title, "Loading…".to_string()));
            }
            return self.request_details(t);
        }
        self.popups.cell_view = Some(CellView::definition(title, table_definition(engine, table)));
    }

    /// `f` in the navigator: the table's neighbours through foreign keys,
    /// two steps each way.
    pub(in crate::app) fn show_related(&mut self, t: usize) {
        let Some(db) = self.connection.database.clone() else { return };
        let Some(table) = self.connection.schema.as_ref().and_then(|p| p.tables.get(t)) else { return };
        let (schema, name) = (table.info.schema.clone(), table.info.name.clone());
        let send = self.sender();
        tokio::spawn(async move {
            let text = db
                .links()
                .await
                .map(|links| crate::related::describe(&links, &schema, &name))
                .map_err(|e| t!("schema.links_failed", e = crate::db::error_message(&e)));
            send(DbEventKind::Definition { title: format!("{} · {name}", t!("schema.related")), text });
        });
    }

    /// ←/→ in a navigator definition: the previous or next table's, view's
    /// or function's, selecting it in the navigator too.
    pub(in crate::app) fn step_definition(&mut self, back: bool) {
        let Some(panel) = &mut self.connection.schema else { return };
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
                if let Some(view) = &mut self.popups.cell_view {
                    view.text = "Loading…".to_string();
                    view.scroll = 0;
                }
                self.show_routine(r);
            }
            _ => {}
        }
    }

    pub(in crate::app) fn show_routine(&mut self, r: usize) {
        let Some(db) = self.connection.database.clone() else { return };
        let Some(routine) = self.connection.schema.as_ref().and_then(|p| p.routines.get(r)).cloned() else { return };
        let send = self.sender();
        self.routine_definition_pending = true;
        tokio::spawn(async move {
            let title = format!("{} {}", routine.kind.to_uppercase(), routine.name);
            let result = db.routine_definition(&routine).await.map_err(|e| crate::db::error_message(&e));
            send(DbEventKind::Definition { title, text: result });
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
        let _ = write!(out, "\n-- {}\n", t!("schema.indexes"));
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

pub struct SchemaTable {
    pub info: TableInfo,
    pub expanded: bool,
    pub columns: Vec<ColumnInfo>,
    /// Indexes, foreign keys and definition, loaded on first expand.
    pub details: Option<TableDetails>,
    pub details_loading: bool,
}

impl SchemaTable {
    pub(in crate::app) fn new(info: TableInfo, columns: Vec<ColumnInfo>) -> Self {
        SchemaTable { info, expanded: false, columns, details: None, details_loading: false }
    }

    /// Where column `name` points as a foreign key (`orders.id`).
    pub fn foreign_key(&self, column: &str) -> Option<&str> {
        let details = self.details.as_ref()?;
        details.foreign_keys.iter().find(|f| f.column == column).map(|f| f.target.as_str())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SchemaSection {
    Tables,
    Views,
    /// The database's own catalog tables and views, when shown.
    System,
    Routines,
}

impl SchemaSection {
    /// Whether `info` is listed in this section.
    pub fn holds(self, info: &TableInfo) -> bool {
        match self {
            SchemaSection::Tables => !info.system && !info.is_view,
            SchemaSection::Views => !info.system && info.is_view,
            SchemaSection::System => info.system,
            SchemaSection::Routines => false,
        }
    }
}

/// The order of the navigator's tables (`o` cycles it).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SchemaSort {
    #[default]
    Name,
    /// Most rows first (the server's estimate).
    Rows,
    /// Biggest on disk first, indexes included.
    Size,
}

impl SchemaSort {
    pub fn next(self) -> Self {
        match self {
            SchemaSort::Name => SchemaSort::Rows,
            SchemaSort::Rows => SchemaSort::Size,
            SchemaSort::Size => SchemaSort::Name,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            SchemaSort::Name => t!("navigator.by_name"),
            SchemaSort::Rows => t!("navigator.by_rows"),
            SchemaSort::Size => t!("navigator.by_size"),
        }
    }
}

/// One visible row of the schema tree.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SchemaNode {
    /// A section header; not selectable.
    Section(SchemaSection),
    Table(usize),
    Column(usize, usize),
    Index(usize, usize),
    Routine(usize),
}

impl SchemaNode {
    pub(in crate::app) fn table(self) -> Option<usize> {
        match self {
            SchemaNode::Table(t) | SchemaNode::Column(t, _) | SchemaNode::Index(t, _) => Some(t),
            SchemaNode::Section(_) | SchemaNode::Routine(_) => None,
        }
    }
}

/// The database catalog: shown in the schema browser and used for
/// autocomplete. Loaded on connect.
#[derive(Default)]
pub struct SchemaPanel {
    pub tables: Vec<SchemaTable>,
    pub loading: bool,
    pub error: Option<String>,
    pub selected: usize,
    /// Only tables whose name contains this (case-insensitive) are shown.
    pub filter: LineInput,
    pub filter_editing: bool,
    /// Functions and procedures.
    pub routines: Vec<RoutineInfo>,
    /// A table whose definition opens once its details arrive.
    pub show_definition_of: Option<(String, String)>,
    pub sort: SchemaSort,
    /// The first row on screen, and how many fit (as last drawn).
    pub scroll: usize,
    pub page: usize,
}

impl SchemaPanel {
    /// Scrolls so the selected row shows, with an expanded table's
    /// columns and indexes as far as they fit (the table stays on screen),
    /// for `height` rows. Returns the first row to show.
    pub fn scroll_into_view(&mut self, nodes: &[SchemaNode], height: usize) -> usize {
        self.page = height.max(1);
        let selected = self.selected.min(nodes.len().saturating_sub(1));
        if let Some(SchemaNode::Table(t)) = nodes.get(selected) {
            let children = nodes[selected + 1..].iter().take_while(|n| n.table() == Some(*t)).count();
            let wanted = (selected + children + 1).saturating_sub(self.page).min(selected);
            self.scroll = self.scroll.max(wanted);
        }
        if selected < self.scroll {
            // A section's heading comes along with its first table.
            let heading = selected.checked_sub(1).filter(|&i| matches!(nodes[i], SchemaNode::Section(_)));
            self.scroll = heading.unwrap_or(selected);
        }
        if selected >= self.scroll + self.page {
            self.scroll = selected + 1 - self.page;
        }
        // No empty space below the last row.
        self.scroll = self.scroll.min(nodes.len().saturating_sub(self.page));
        self.scroll
    }

    /// The visible rows: a TABLES section, then VIEWS, then SYSTEM (if
    /// loaded), each with its (filtered) tables and the columns of expanded
    /// ones.
    pub fn nodes(&self) -> Vec<SchemaNode> {
        let needle = self.filter.text().to_lowercase();
        let mut nodes = Vec::new();
        let mut buf = String::new();
        // Each name lowercased once, to filter and rank it.
        let mut rank = |name: &str| match_rank(name, &needle, &mut buf);
        for section in [SchemaSection::Tables, SchemaSection::Views, SchemaSection::System] {
            let mut ranked: Vec<(u8, usize)> = (0..self.tables.len())
                .filter(|&t| section.holds(&self.tables[t].info))
                .filter_map(|t| Some((rank(&self.tables[t].info.name)?, t)))
                .collect();
            // Biggest first; unknown sizes last. The sort is stable, so
            // ties stay by name. By name while filtering, the best matches
            // first: `products` before `alternative_prod` for "produc".
            match self.sort {
                SchemaSort::Name => ranked.sort_by_key(|&(rank, _)| rank),
                SchemaSort::Rows => ranked.sort_by_key(|&(_, t)| std::cmp::Reverse(self.tables[t].info.rows)),
                SchemaSort::Size => ranked.sort_by_key(|&(_, t)| std::cmp::Reverse(self.tables[t].info.bytes)),
            }
            let matching: Vec<usize> = ranked.into_iter().map(|(_, t)| t).collect();
            if matching.is_empty() {
                continue;
            }
            nodes.push(SchemaNode::Section(section));
            for t in matching {
                nodes.push(SchemaNode::Table(t));
                let table = &self.tables[t];
                if table.expanded {
                    nodes.extend((0..table.columns.len()).map(|c| SchemaNode::Column(t, c)));
                    let indexes = table.details.as_ref().map_or(0, |d| d.indexes.len());
                    nodes.extend((0..indexes).map(|i| SchemaNode::Index(t, i)));
                }
            }
        }
        let mut routines: Vec<(u8, usize)> =
            (0..self.routines.len()).filter_map(|r| Some((rank(&self.routines[r].name)?, r))).collect();
        routines.sort_by_key(|&(rank, _)| rank);
        if !routines.is_empty() {
            nodes.push(SchemaNode::Section(SchemaSection::Routines));
            nodes.extend(routines.into_iter().map(|(_, r)| SchemaNode::Routine(r)));
        }
        nodes
    }
}

/// How well `name` matches the filter `needle` (lowercase), best first: the
/// name itself, a name starting with it, a word in it starting with it
/// (after `_`, `.`, `-` or a space), anywhere else; `None` when it isn't in
/// it at all. Case is folded as the results' filter folds it, in `buf`. No
/// filter: all alike.
fn match_rank(name: &str, needle: &str, buf: &mut String) -> Option<u8> {
    if needle.is_empty() {
        return Some(0);
    }
    let name =
        crate::app::query_screen::results::view::lowered_for(name, needle, buf).filter(|n| n.contains(needle))?;
    Some(if name == needle {
        0
    } else if name.starts_with(needle) {
        1
    } else if name.match_indices(needle).any(|(i, _)| name[..i].ends_with(['_', '.', '-', ' '])) {
        // (A match right after a separator: `order_p` in `big_order_products` too.)
        2
    } else {
        3
    })
}

#[cfg(test)]
mod match_tests {
    #[test]
    fn the_best_matches_come_first() {
        let mut names = vec!["alternative_prod", "order_products", "products", "productsx", "prod", "reproduce"];
        let mut buf = String::new();
        names.sort_by_key(|n| super::match_rank(n, "prod", &mut buf));
        assert_eq!(names, ["prod", "products", "productsx", "alternative_prod", "order_products", "reproduce"]);
        assert_eq!(super::match_rank("cats", "prod", &mut buf), None, "not in it");
        // A filter with a separator in it still starts a word.
        assert_eq!(super::match_rank("big_order_products", "order_p", &mut buf), Some(2));
        assert_eq!(super::match_rank("xorder_px", "order_p", &mut buf), Some(3));
        // Folded as the filter folds it: a Lithuanian name ranks as itself.
        assert_eq!(super::match_rank("Žąsys", "žąsys", &mut buf), Some(0));
        assert_eq!(super::match_rank("ŽĄSYS_seni", "žąs", &mut buf), Some(1));
        // Greek's final sigma, as the filter's text lowercases it.
        assert_eq!(super::match_rank("ΟΔΟΣ", &"ΟΔΟΣ".to_lowercase(), &mut buf), Some(0));
    }
}
