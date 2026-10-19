//! Pop-up menus: right-click a pane, or press `m` (results, schema) or
//! Shift+F10 / F10 (anywhere), to see what you can do there, each with its
//! key. Choosing an item does exactly what its key does.

use super::*;
use crate::config::keys::Action;

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
        [(Clip::Copy, t!("menu.copy"), "Ctrl+C"), (Clip::Cut, t!("menu.cut"), "Ctrl+X")]
            .into_iter()
            .filter(|_| self.tab.editor.selection().is_some())
            .chain([(Clip::Paste, t!("menu.paste"), "Ctrl+V")])
            .map(|(clip, label, keys)| MenuItem { label, keys: keys.to_string(), action: MenuAction::Clip(clip) })
            .chain([
                self.action_item(t!("menu.run"), Action::Run),
                self.action_item(t!("menu.run_statement"), Action::RunStatement),
                self.action_item(t!("menu.select_statement"), Action::SelectStatement),
                self.action_item(
                    if self.panes.statement_zoom { t!("menu.unzoom_statement") } else { t!("menu.zoom_statement") },
                    Action::ZoomStatement,
                ),
                self.action_item(t!("menu.explain"), Action::Explain),
                self.action_item(t!("menu.explain_analyze"), Action::ExplainAnalyze),
                self.action_item(t!("menu.format"), Action::Format),
                self.action_item(t!("menu.comment"), Action::Comment),
                self.action_item(t!("menu.find"), Action::Find),
                self.action_item(t!("menu.snippet"), Action::InsertSnippet),
                self.action_item(t!("menu.save"), Action::SaveTab),
                self.action_item(t!("menu.library"), Action::Library),
                self.action_item(t!("menu.tab_list"), Action::TabList),
                self.action_item(t!("menu.zoom"), Action::Zoom),
                self.action_item(t!("menu.all_commands"), Action::Palette),
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
            results_item(t!("menu.export_csv"), Char('e'), "e"),
            results_item(t!("menu.export_json"), Char('J'), "J"),
            results_item(t!("menu.export_sql"), Char('i'), "i"),
            command(t!("menu.export_ascii"), Command::ExportAscii),
            command(t!("menu.export_all_csv_full"), Command::ExportAll(PagedFormat::Csv)),
            command(t!("menu.export_all_json"), Command::ExportAll(PagedFormat::Json)),
            command(t!("menu.export_all_sql"), Command::ExportAll(PagedFormat::Sql)),
            results_item(t!("menu.copy_markdown"), Char('C'), "C"),
            command(t!("menu.copy_csv"), Command::CopyRows(ExportFormat::Csv)),
            command(t!("menu.copy_json"), Command::CopyRows(ExportFormat::Json)),
            command(t!("menu.copy_sql"), Command::CopyRows(ExportFormat::Sql)),
            command(t!("menu.copy_ascii"), Command::CopyRows(ExportFormat::Ascii)),
            results_item(t!("menu.pipe"), Char('|'), "|"),
            results_item(t!("menu.open_rows"), Char('o'), "o"),
        ];
        // A plan's table: only what its rows can go to.
        if self.tab.plan_swapped {
            items.retain(|i| !matches!(i.action, MenuAction::ResultsKey(Char('i')) | MenuAction::Command(_)));
        }
        let pane = self.hits.result_rows;
        let at = at.unwrap_or((pane.x + 2, pane.y + 1));
        self.completion = None;
        self.menu = Some(Menu { title: t!("menu.export_title"), items, selected: 0, at });
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
                    ("open" | "xdg-open", false) => t!("menu.its_app").to_string(),
                    (program, true) => t!("menu.here", program = program),
                    (program, false) => program.to_string(),
                };
                let label = match format {
                    OpenFormat::Csv => t!("menu.as_csv"),
                    OpenFormat::Json => t!("menu.as_json"),
                    OpenFormat::Sql => t!("menu.as_sql"),
                    OpenFormat::Tsv => t!("menu.as_tsv"),
                    OpenFormat::Text => t!("menu.as_text"),
                    OpenFormat::Ascii => t!("menu.as_ascii"),
                };
                MenuItem { label, keys, action: MenuAction::Command(super::palette::Command::OpenRowsAs(format)) }
            })
            .collect();
        let pane = self.hits.result_rows;
        self.completion = None;
        self.menu = Some(Menu { title: t!("menu.open_title"), items, selected: 0, at: (pane.x + 2, pane.y + 1) });
    }

    /// The results kept from earlier runs: to switch between and pin.
    fn run_items(&self) -> Vec<MenuItem> {
        use KeyCode::Char;
        let n = self.tab.runs.len();
        let at = self.tab.run_at;
        let pinned = self.tab.shown_run().is_some_and(|r| r.pinned);
        [
            (at > 0).then(|| results_item(t!("menu.earlier_result"), Char(','), ",")),
            (at + 1 < n).then(|| results_item(t!("menu.later_result"), Char('.'), ".")),
            (n > 0).then(|| results_item(if pinned { t!("menu.unpin") } else { t!("menu.pin") }, Char('p'), "p")),
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
            if plan { t!("results.plan") } else { t!("menu.results_title") },
            vec![
                results_item(t!("menu.full_value"), KeyCode::Enter, "↵"),
                results_item(t!("menu.row_vertical"), Char('x'), "x"),
                results_item(t!("menu.vertical"), Char('X'), "X"),
                results_item(t!("menu.edit_cell"), Char('c'), "c"),
                results_item(t!("menu.add_row"), Char('a'), "a"),
                results_item(t!("menu.mark_row"), Char(' '), "Space"),
                results_item(t!("menu.delete_rows"), Char('d'), "d"),
                results_item(t!("menu.copy_cell"), Char('y'), "y"),
                results_item(t!("menu.copy_row"), Char('Y'), "Y"),
                results_item(t!("menu.copy_rows_markdown"), Char('C'), "C"),
                MenuItem {
                    label: t!("menu.copy_rows_csv"),
                    keys: String::new(),
                    action: MenuAction::Command(super::palette::Command::CopyRows(ExportFormat::Csv)),
                },
                results_item(t!("menu.follow"), Char('f'), "f"),
                results_item(t!("menu.pipe_rows"), Char('|'), "|"),
                results_item(t!("menu.open_rows_app"), Char('o'), "o"),
                results_item(t!("menu.sort"), Char('s'), "s"),
                results_item(t!("menu.chart"), Char('b'), "b"),
                results_item(t!("menu.filter"), Char('/'), "/"),
                results_item(t!("menu.export_as_csv"), Char('e'), "e"),
                results_item(t!("menu.export_as_json"), Char('J'), "J"),
                results_item(t!("menu.export_as_sql"), Char('i'), "i"),
                MenuItem {
                    label: t!("menu.export_all_csv"),
                    keys: String::new(),
                    action: MenuAction::Command(super::palette::Command::ExportAll(PagedFormat::Csv)),
                },
                results_item(t!("menu.load_more"), Char('M'), "M"),
                results_item(t!("menu.zoom"), Char('z'), "z"),
            ]
            .into_iter()
            // A script's other result sets.
            .chain((self.tab.grid.result_sets.len() > 1).then(|| results_item(t!("menu.next_set"), Char('>'), ">")))
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
            Focus::Schema if self.connection.schema.is_some() => {
                use KeyCode::Char;
                (
                    t!("menu.navigator_title"),
                    vec![
                        schema_item(t!("menu.expand"), KeyCode::Enter, "↵"),
                        schema_item(t!("menu.select_from"), Char('s'), "s"),
                        schema_item(t!("menu.definition"), Char('d'), "d"),
                        schema_item(t!("menu.related"), Char('f'), "f"),
                        schema_item(t!("menu.copy_name"), Char('y'), "y"),
                        schema_item(t!("menu.filter_name"), Char('/'), "/"),
                        schema_item(t!("menu.order"), Char('o'), "o"),
                        schema_item(t!("menu.refresh"), Char('r'), "r"),
                        schema_item(t!("menu.system"), Char('.'), "."),
                        schema_item(t!("menu.zoom"), Char('z'), "z"),
                    ],
                )
            }
            // Messages, a plan's text, or nothing run yet: no rows to act on.
            Focus::Results => {
                use KeyCode::Char;
                let messages = self.tab.result_tab == ResultTab::Messages && !self.notes.log.is_empty();
                (
                    t!("menu.results_title"),
                    [
                        results_item(t!("menu.show_rows"), Char('1'), "1"),
                        results_item(t!("menu.show_messages"), Char('2'), "2"),
                        results_item(t!("menu.show_plan"), Char('3'), "3"),
                    ]
                    .into_iter()
                    .chain(messages.then(|| results_item(t!("menu.whole_message"), KeyCode::Enter, "↵")))
                    .chain(messages.then(|| results_item(t!("menu.copy_message"), Char('y'), "y")))
                    .chain(messages.then(|| results_item(t!("menu.copy_messages"), Char('Y'), "Y")))
                    .chain(self.run_items())
                    .chain([self.action_item(t!("menu.zoom"), Action::Zoom)])
                    .collect(),
                )
            }
            _ => (t!("menu.editor_title"), self.editor_items()),
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
