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
    /// A palette command without a key of its own.
    Command(super::palette::Command),
    Clip(Clip),
}

/// The editor's clipboard items.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Clip {
    Copy,
    Cut,
    Paste,
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

    /// The editor's menu: the clipboard, then actions with their keys.
    fn editor_items(&self) -> Vec<MenuItem> {
        // Copy and cut only with something selected.
        [(Clip::Copy, "Copy", "Ctrl+C"), (Clip::Cut, "Cut", "Ctrl+X")]
            .into_iter()
            .filter(|_| self.tab.editor.selection().is_some())
            .chain([(Clip::Paste, "Paste", "Ctrl+V")])
            .map(|(clip, label, keys)| MenuItem { label, keys: keys.to_string(), action: MenuAction::Clip(clip) })
            .chain([
                self.action_item("Run (the selection, or everything)", Action::Run),
                self.action_item("Run the statement at the cursor", Action::RunStatement),
                self.action_item("Select the statement", Action::SelectStatement),
                self.action_item(
                    if self.statement_zoom { "Show the whole text again" } else { "Zoom the statement (alone)" },
                    Action::ZoomStatement,
                ),
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
            ])
            .collect()
    }

    /// `export ▾` above the results: the rows shown to a file or the
    /// clipboard, or every row of the result to a file.
    pub(super) fn open_export_menu(&mut self, at: Option<(u16, u16)>) {
        use super::palette::Command;
        use KeyCode::Char;
        let command = |label, command| MenuItem { label, keys: String::new(), action: MenuAction::Command(command) };
        let mut items = vec![
            results_item("Export the rows shown as CSV", Char('e'), "e"),
            results_item("Export the rows shown as JSON", Char('J'), "J"),
            results_item("Export the rows shown as INSERT statements", Char('i'), "i"),
            command("Export all rows as CSV (not just those loaded)", Command::ExportAll(ExportFormat::Csv)),
            command("Export all rows as JSON", Command::ExportAll(ExportFormat::Json)),
            command("Export all rows as INSERT statements", Command::ExportAll(ExportFormat::Sql)),
            results_item("Copy the rows as a Markdown table", Char('C'), "C"),
            command("Copy the rows as CSV", Command::CopyRows(ExportFormat::Csv)),
            command("Copy the rows as JSON", Command::CopyRows(ExportFormat::Json)),
            command("Copy the rows as INSERT statements", Command::CopyRows(ExportFormat::Sql)),
            results_item("Pipe the rows through a command", Char('|'), "|"),
            results_item("Open the rows in an app (CSV, JSON, SQL…)", Char('o'), "o"),
        ];
        // A plan's table: only what its rows can go to.
        if self.tab.plan_swapped {
            items.retain(|i| !matches!(i.action, MenuAction::ResultsKey(Char('i')) | MenuAction::Command(_)));
        }
        let pane = self.hits.result_rows;
        let at = at.unwrap_or((pane.x + 2, pane.y + 1));
        self.completion = None;
        self.menu = Some(Menu { title: "EXPORT", items, selected: 0, at });
    }

    /// `o`: the formats to open the rows in, each with what opens it.
    pub(super) fn open_viewer_menu(&mut self) {
        use super::pipes::OpenFormat;
        let items = OpenFormat::ALL
            .into_iter()
            .map(|format| {
                let viewer = self.viewer_for(format);
                let program = viewer.command().split_whitespace().next().unwrap_or_default();
                let keys = match (program, viewer.in_terminal()) {
                    ("open" | "xdg-open", false) => "its app".to_string(),
                    (program, true) => format!("here: {program}"),
                    (program, false) => program.to_string(),
                };
                let label = match format {
                    OpenFormat::Csv => "As CSV (a spreadsheet)",
                    OpenFormat::Json => "As JSON",
                    OpenFormat::Sql => "As INSERT statements (SQL)",
                    OpenFormat::Tsv => "As TSV (tab-separated)",
                    OpenFormat::Text => "As a text table",
                };
                MenuItem { label, keys, action: MenuAction::Command(super::palette::Command::OpenRowsAs(format)) }
            })
            .collect();
        let pane = self.hits.result_rows;
        self.completion = None;
        self.menu = Some(Menu { title: "OPEN THE ROWS", items, selected: 0, at: (pane.x + 2, pane.y + 1) });
    }

    /// The results kept from earlier runs: to switch between and pin.
    fn run_items(&self) -> Vec<MenuItem> {
        use KeyCode::Char;
        let n = self.tab.runs.len();
        let at = self.tab.run_at;
        let pinned = self.tab.shown_run().is_some_and(|r| r.pinned);
        [
            (at > 0).then(|| results_item("Earlier result (kept from a run before)", Char(','), ",")),
            (at + 1 < n).then(|| results_item("Later result", Char('.'), ".")),
            (n > 0).then(|| {
                results_item(if pinned { "Unpin this result" } else { "Pin this result (keep it)" }, Char('p'), "p")
            }),
        ]
        .into_iter()
        .flatten()
        .collect()
    }

    /// The menu over a grid of rows: a result's, or a plan's table.
    fn grid_menu(&self) -> (&'static str, Vec<MenuItem>) {
        use KeyCode::Char;
        // A plan's table: what changes data or needs a query's table is left out.
        let plan = self.tab.plan_swapped;
        let for_plan = |item: &MenuItem| {
            !plan
                || !matches!(
                    item.action,
                    MenuAction::ResultsKey(Char('c' | 'a' | 'd' | 'f' | 'M' | 'i' | '>' | ',' | '.' | 'p'))
                        | MenuAction::Command(_)
                )
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
                results_item("Copy the row (or the marked rows)", Char('Y'), "Y"),
                results_item("Copy rows as a Markdown table", Char('C'), "C"),
                MenuItem {
                    label: "Copy rows as CSV",
                    keys: String::new(),
                    action: MenuAction::Command(super::palette::Command::CopyRows(ExportFormat::Csv)),
                },
                results_item("Follow the foreign key", Char('f'), "f"),
                results_item("Pipe rows through a command", Char('|'), "|"),
                results_item("Open rows in an app (CSV, JSON, SQL…)", Char('o'), "o"),
                results_item("Sort by this column", Char('s'), "s"),
                results_item("Chart this column", Char('b'), "b"),
                results_item("Filter rows", Char('/'), "/"),
                results_item("Export as CSV", Char('e'), "e"),
                results_item("Export as JSON", Char('J'), "J"),
                results_item("Export as INSERT statements", Char('i'), "i"),
                MenuItem {
                    label: "Export all rows as CSV",
                    keys: String::new(),
                    action: MenuAction::Command(super::palette::Command::ExportAll(ExportFormat::Csv)),
                },
                results_item("Load more rows", Char('M'), "M"),
                results_item("Zoom this pane (and back)", Char('z'), "z"),
            ]
            .into_iter()
            // A script's other result sets.
            .chain((self.tab.grid.result_sets.len() > 1).then(|| results_item("Next result set", Char('>'), ">")))
            .chain(self.run_items())
            .filter(for_plan)
            .collect(),
        )
    }

    /// The focused pane's menu, at `at` (a click) or next to the pane.
    pub(super) fn open_menu(&mut self, at: Option<(u16, u16)>) {
        let (title, items) = match self.focus {
            Focus::Results
                if self.grid_shown() && self.tab.grid.results.as_ref().is_some_and(|r| !r.columns.is_empty()) =>
            {
                self.grid_menu()
            }
            Focus::Schema if self.schema.is_some() => {
                use KeyCode::Char;
                (
                    "NAVIGATOR",
                    vec![
                        schema_item("Expand / collapse, or show a function", KeyCode::Enter, "↵"),
                        schema_item("SELECT * from it: add to the editor and run", Char('s'), "s"),
                        schema_item("Show the definition", Char('d'), "d"),
                        schema_item("Related tables (foreign keys, both ways)", Char('f'), "f"),
                        schema_item("Copy the name", Char('y'), "y"),
                        schema_item("Filter by name", Char('/'), "/"),
                        schema_item("Order by name, rows or size", Char('o'), "o"),
                        schema_item("Refresh", Char('r'), "r"),
                        schema_item("Show / hide system tables", Char('.'), "."),
                        schema_item("Zoom this pane (and back)", Char('z'), "z"),
                    ],
                )
            }
            // Messages, a plan's text, or nothing run yet: no rows to act on.
            Focus::Results => {
                use KeyCode::Char;
                let messages = self.tab.result_tab == ResultTab::Messages && !self.messages.is_empty();
                (
                    "RESULTS",
                    [
                        results_item("Show the rows", Char('1'), "1"),
                        results_item("Show the messages", Char('2'), "2"),
                        results_item("Show the plan", Char('3'), "3"),
                    ]
                    .into_iter()
                    .chain(messages.then(|| results_item("Show the whole message", KeyCode::Enter, "↵")))
                    .chain(messages.then(|| results_item("Copy the message", Char('y'), "y")))
                    .chain(messages.then(|| results_item("Copy every message", Char('Y'), "Y")))
                    .chain(self.run_items())
                    .chain([self.action_item("Zoom this pane (and back)", Action::Zoom)])
                    .collect(),
                )
            }
            _ => ("EDITOR", self.editor_items()),
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
            MenuAction::Command(command) => self.run_command(command),
            MenuAction::ResultsKey(code) => self.handle_results_key(KeyEvent::new(code, KeyModifiers::NONE)),
            MenuAction::SchemaKey(code) => self.handle_schema_key(KeyEvent::new(code, KeyModifiers::NONE)),
            MenuAction::Clip(Clip::Copy) => self.copy_focused(),
            MenuAction::Clip(Clip::Cut) => self.cut_selection(),
            MenuAction::Clip(Clip::Paste) => self.paste_clipboard(),
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
