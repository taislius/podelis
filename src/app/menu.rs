//! Pop-up menus: right-click a pane, or press `m` (results, schema) or
//! Shift+F10 / F10 (anywhere), to see what you can do there, each with its
//! key. Choosing an item does exactly what its key does.

use super::*;
use crate::keys::Action;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuAction {
    Action(Action),
    ResultsKey(KeyCode),
    SchemaKey(KeyCode),
}

#[derive(Clone, Debug)]
pub struct MenuItem {
    pub label: &'static str,
    /// The key that does the same, shown as a hint.
    pub keys: String,
    action: MenuAction,
}

pub struct Menu {
    pub title: &'static str,
    pub items: Vec<MenuItem>,
    pub selected: usize,
    /// Screen cell the menu opens at (top-left).
    pub at: (u16, u16),
}

fn results_item(label: &'static str, key: KeyCode, hint: &str) -> MenuItem {
    MenuItem { label, keys: hint.to_string(), action: MenuAction::ResultsKey(key) }
}

fn schema_item(label: &'static str, key: KeyCode, hint: &str) -> MenuItem {
    MenuItem { label, keys: hint.to_string(), action: MenuAction::SchemaKey(key) }
}

impl App {
    fn action_item(&self, label: &'static str, action: Action) -> MenuItem {
        MenuItem { label, keys: self.keymap.label(action).to_string(), action: MenuAction::Action(action) }
    }

    /// The focused pane's menu, at `at` (a click) or next to the pane.
    pub(super) fn open_menu(&mut self, at: Option<(u16, u16)>) {
        let (title, items) = match self.focus {
            Focus::Results if self.tab.grid.results.as_ref().is_some_and(|r| !r.columns.is_empty()) => {
                use KeyCode::Char;
                // A plan's table: what changes data or needs a query's table is left out.
                let plan = self.tab.plan_swapped;
                let for_plan = |item: &MenuItem| {
                    !plan
                        || !matches!(item.action, MenuAction::ResultsKey(Char('c' | 'a' | 'd' | 'f' | 'M' | 'i' | '>')))
                };
                (
                    if plan { "QUERY PLAN" } else { "RESULTS" },
                    vec![
                        results_item("Show the full value", KeyCode::Enter, "↵"),
                        results_item("Show the row vertically", Char('x'), "x"),
                        results_item("All rows one column per line (\\G) / table", Char('X'), "X"),
                        results_item("Edit this cell", Char('c'), "c"),
                        results_item("Add a row", Char('a'), "a"),
                        results_item("Mark / unmark this row", Char(' '), "Space"),
                        results_item("Delete the marked rows (or this one)", Char('d'), "d"),
                        results_item("Copy the cell", Char('y'), "y"),
                        results_item("Copy the row", Char('Y'), "Y"),
                        results_item("Copy rows as a Markdown table", Char('C'), "C"),
                        results_item("Follow the foreign key", Char('f'), "f"),
                        results_item("Pipe rows through a command", Char('|'), "|"),
                        results_item("Open rows in the viewer", Char('o'), "o"),
                        results_item("Sort by this column", Char('s'), "s"),
                        results_item("Filter rows", Char('/'), "/"),
                        results_item("Export as CSV", Char('e'), "e"),
                        results_item("Export as JSON", Char('J'), "J"),
                        results_item("Export as INSERT statements", Char('i'), "i"),
                        results_item("Load more rows", Char('M'), "M"),
                        results_item("Zoom this pane (and back)", Char('z'), "z"),
                    ]
                    .into_iter()
                    // A script's other result sets.
                    .chain(
                        (self.tab.grid.result_sets.len() > 1).then(|| results_item("Next result set", Char('>'), ">")),
                    )
                    .filter(for_plan)
                    .collect(),
                )
            }
            Focus::Schema if self.schema.is_some() => {
                use KeyCode::Char;
                (
                    "NAVIGATOR",
                    vec![
                        schema_item("Expand / collapse, or show a function", KeyCode::Enter, "↵"),
                        schema_item("SELECT * from it: add to the editor and run", Char('s'), "s"),
                        schema_item("Show the definition", Char('d'), "d"),
                        schema_item("Filter by name", Char('/'), "/"),
                        schema_item("Order by name, rows or size", Char('o'), "o"),
                        schema_item("Refresh", Char('r'), "r"),
                        schema_item("Show / hide system tables", Char('.'), "."),
                        schema_item("Zoom this pane (and back)", Char('z'), "z"),
                    ],
                )
            }
            _ => (
                "EDITOR",
                vec![
                    self.action_item("Run (the selection, or everything)", Action::Run),
                    self.action_item("Run the statement at the cursor", Action::RunStatement),
                    self.action_item("Explain (plan only)", Action::Explain),
                    self.action_item("Explain analyze (runs it)", Action::ExplainAnalyze),
                    self.action_item("Format SQL", Action::Format),
                    self.action_item("Comment / uncomment lines", Action::Comment),
                    self.action_item("Find and replace", Action::Find),
                    self.action_item("Insert a snippet or saved query", Action::InsertSnippet),
                    self.action_item("Save to the library", Action::SaveTab),
                    self.action_item("Search saved queries and history", Action::Library),
                    self.action_item("Zoom this pane (and back)", Action::Zoom),
                    self.action_item("All commands…", Action::Palette),
                ],
            ),
        };
        let pane = match self.focus {
            Focus::Results => self.hits.result_rows,
            Focus::Schema => self.hits.schema_list,
            Focus::Editor => self.hits.editor_text,
        };
        let at = at.unwrap_or((pane.x + 2, pane.y + 1));
        self.completion = None;
        self.menu = Some(Menu { title, items, selected: 0, at });
    }

    pub(super) fn handle_menu_key(&mut self, key: KeyEvent) {
        let Some(menu) = &mut self.menu else { return };
        if let Some(i) = list::step(menu.selected, menu.items.len(), key, Nav::MENU) {
            menu.selected = i;
            return;
        }
        let chosen = match key.code {
            KeyCode::Esc | KeyCode::Char('m') => {
                self.menu = None;
                return;
            }
            KeyCode::Enter => menu.items.get(menu.selected).cloned(),
            // An item's own key works too.
            KeyCode::Char(c) => menu.items.iter().find(|i| i.keys == c.to_string()).cloned(),
            _ => None,
        };
        let Some(item) = chosen else { return };
        self.menu = None;
        match item.action {
            MenuAction::Action(a) => self.run_action(a),
            MenuAction::ResultsKey(code) => self.handle_results_key(KeyEvent::new(code, KeyModifiers::NONE)),
            MenuAction::SchemaKey(code) => self.handle_schema_key(KeyEvent::new(code, KeyModifiers::NONE)),
        }
    }

    /// Clicks on the menu: choose an item, or close it when clicking
    /// outside. `rect` is where the UI drew it.
    pub(super) fn click_menu(&mut self, x: u16, y: u16) {
        let Some(menu) = &self.menu else { return };
        let r = self.hits.menu;
        let inside = x >= r.x && x < r.x + r.width && y > r.y && y < r.y + r.height.saturating_sub(1);
        if !inside {
            self.menu = None;
            return;
        }
        let index = (y - r.y - 1) as usize;
        if index < menu.items.len() {
            if let Some(m) = &mut self.menu {
                m.selected = index;
            }
            self.handle_menu_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        }
    }
}
