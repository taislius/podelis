//! Pop-up menus: right-click a pane, or press `m` (results, schema) or
//! Shift+F10 / F10 (anywhere), to see what you can do there, each with its
//! key. Each menu is data: groups of commands. What doesn't apply stays in
//! its place, dimmed, and says why when chosen; destructive items are last,
//! apart.

use crate::app::commands::{self, Can, Command as C, Here};
use crate::app::*;
use crate::config::keys::Action as A;

#[derive(Clone, Debug)]
pub struct MenuItem {
    pub label: String,
    /// The key that does the same, shown as a hint.
    pub keys: String,
    /// That key, when it's one a press in the menu takes.
    key: Option<KeyCode>,
    /// Doesn't apply now: drawn dimmed.
    pub dimmed: bool,
    pub command: C,
}

pub struct Menu {
    pub title: &'static str,
    pub items: Vec<MenuItem>,
    /// The items a separator comes before.
    gaps: Vec<usize>,
    pub selected: usize,
    /// The first row drawn, when they don't all fit.
    pub scroll: usize,
    /// Screen cell the menu opens at (top-left).
    pub at: (u16, u16),
}

impl Menu {
    /// Its rows, an item's index or a separator (`None`). With no room for
    /// the separators, they go first.
    pub fn rows(&self, room: usize) -> Vec<Option<usize>> {
        let with_gaps = self.items.len() + self.gaps.len() <= room;
        let mut rows = Vec::with_capacity(self.items.len() + self.gaps.len());
        for i in 0..self.items.len() {
            if with_gaps && self.gaps.contains(&i) {
                rows.push(None);
            }
            rows.push(Some(i));
        }
        rows
    }
}

/// Over a grid of rows.
const ROWS_MENU: &[&[C]] = &[
    &[C::FullValue, C::RecordView, C::Vertical, C::Action(A::Zoom)],
    &[C::Sort, C::FilterRows, C::Chart, C::Follow],
    &[C::CopyCell, C::CopyRow, C::CopyMenu],
    &[C::ExportMenu, C::OpenRows, C::PipeRows],
    &[C::LoadMore, C::OtherResults],
    &[C::EditCell, C::AddRow, C::MarkRow],
    &[C::DeleteRows],
];

/// Over a plan's table: another menu, not the same one dimmed; it's only
/// ever looked at.
const PLAN_TABLE_MENU: &[&[C]] = &[
    &[C::FullValue, C::RecordView, C::Vertical, C::Action(A::Zoom)],
    &[C::Sort, C::FilterRows, C::Chart],
    &[C::CopyCell, C::CopyRow, C::CopyMenu],
    &[C::ExportMenu, C::OpenRows, C::PipeRows],
];

/// Messages, a plan's text, or nothing run yet: no rows to act on.
const NO_ROWS_MENU: &[&[C]] = &[
    &[C::ShowRows, C::ShowMessages, C::ShowPlan],
    &[C::WholeMessage, C::CopyMessage, C::CopyMessages],
    &[C::CopyPlanLines, C::CopyWholePlan],
    &[C::OtherResults, C::Action(A::Zoom)],
];

const OTHER_RESULTS_MENU: &[&[C]] = &[&[C::NextSet, C::PrevSet], &[C::EarlierResult, C::LaterResult, C::Pin]];

const NAVIGATOR_MENU: &[&[C]] = &[
    &[C::Expand, C::Definition, C::Related],
    &[C::SelectFrom, C::AnalyzeTable, C::CopyName],
    &[C::FilterNames, C::OrderNames, C::SystemTables, C::RefreshSchema, C::Action(A::Zoom)],
    &[C::CompareSchema(None), C::SnapshotSchema],
];

const EDITOR_MENU: &[&[C]] = &[
    &[C::ClipCopy, C::ClipCut, C::ClipPaste],
    &[
        C::Action(A::Run),
        C::Action(A::RunStatement),
        C::Action(A::Explain),
        C::Action(A::ExplainAnalyze),
        C::Action(A::AiExplain),
    ],
    &[C::Action(A::Commit), C::Action(A::Rollback), C::Action(A::TxMode)],
    &[
        C::Action(A::SelectStatement),
        C::Action(A::SelectWord),
        C::Action(A::ZoomStatement),
        C::Action(A::Format),
        C::Action(A::Comment),
        C::Action(A::Find),
        C::Action(A::NextWarning),
    ],
    &[C::Action(A::InsertSnippet), C::Action(A::SaveStatement), C::Action(A::SaveTab), C::Action(A::Library)],
    &[C::Action(A::Zoom), C::AllCommands],
    &[C::Action(A::CloseTab)],
];

impl App {
    /// Opens a menu of these groups, at `at` or next to the focused pane.
    fn show_menu<G, I>(&mut self, title: &'static str, groups: G, at: Option<(u16, u16)>)
    where
        G: IntoIterator<Item = I>,
        I: IntoIterator<Item = C>,
    {
        let here = self.here();
        let mut items = Vec::new();
        let mut gaps = Vec::new();
        let editable = std::cell::OnceCell::new();
        for group in groups {
            if !items.is_empty() {
                gaps.push(items.len());
            }
            items.extend(group.into_iter().map(|command| MenuItem {
                label: command.label(self),
                keys: command.key_here(self, here),
                key: command.key_code_here(here).filter(|code| *code != KeyCode::Enter),
                dimmed: matches!(self.can_with(command, &editable), Can::No { .. }),
                command,
            }));
        }
        let pane = match self.focus {
            Focus::Results => self.hits.result_rows,
            Focus::Schema => self.hits.schema_list,
            Focus::Editor => self.hits.editor_text,
        };
        let at = at.unwrap_or((pane.x + 2, pane.y + 1));
        self.popups.completion = None;
        self.popups.menu = Some(Menu { title, items, gaps, selected: 0, scroll: 0, at });
    }

    /// The focused pane's menu, at `at` (a click) or next to the pane.
    pub(super) fn open_menu(&mut self, at: Option<(u16, u16)>) {
        let (title, groups): (&'static str, &[&[C]]) = match self.here() {
            Here::Rows => (t!("menu.results_title"), ROWS_MENU),
            Here::PlanTable => (t!("results.plan"), PLAN_TABLE_MENU),
            Here::Messages | Here::PlanText | Here::Empty => (t!("menu.results_title"), NO_ROWS_MENU),
            Here::Navigator => (t!("menu.navigator_title"), NAVIGATOR_MENU),
            Here::Editor => (t!("menu.editor_title"), EDITOR_MENU),
        };
        self.show_menu(title, groups.iter().map(|group| group.iter().copied()), at);
    }

    /// The formats the marked rows (or those shown) can go `to`: the
    /// clipboard (`C`), or an app (`o`).
    pub(in crate::app) fn open_rows_menu(&mut self, to: Dest) {
        let title = if to == Dest::Copy { t!("menu.copy_title") } else { t!("menu.open_title") };
        self.show_menu(title, [Format::for_dest(to).map(move |format| C::Rows { to, format })], None);
        // `C` opened this one; `C` again, or `Enter`, is Markdown: what `C`
        // has always copied.
        let markdown = C::Rows { to: Dest::Copy, format: Format::Markdown };
        if let Some(menu) = &mut self.popups.menu
            && let Some(at) = menu.items.iter().position(|i| i.command == markdown)
        {
            menu.selected = at;
            menu.items[at].key = Some(KeyCode::Char('C'));
            menu.items[at].keys = "C".to_string();
        }
    }

    /// A script's other result sets, and the results kept from earlier runs.
    pub(in crate::app) fn open_other_results_menu(&mut self) {
        let groups = OTHER_RESULTS_MENU.iter().map(|group| group.iter().copied());
        self.show_menu(t!("menu.other_results_title"), groups, None);
    }

    /// `o`: the formats to open the rows in, each with what opens it.
    pub(in crate::app) fn open_viewer_menu(&mut self) {
        self.open_rows_menu(Dest::Open);
        let shown: Vec<String> = Format::for_dest(Dest::Open)
            .map(|format| {
                let viewer = self.viewer_for(format);
                let program = viewer.command().split_whitespace().next().unwrap_or_default();
                match (program, viewer.in_terminal()) {
                    ("open" | "xdg-open", false) => t!("menu.its_app").to_string(),
                    (program, true) => t!("menu.here", program = program),
                    (program, false) => program.to_string(),
                }
            })
            .collect();
        if let Some(menu) = &mut self.popups.menu {
            for (item, keys) in menu.items.iter_mut().zip(shown) {
                item.keys = keys;
            }
        }
    }

    pub(super) fn handle_menu_key(&mut self, key: KeyEvent) {
        let Some(menu) = &mut self.popups.menu else { return };
        // An item's own key first: it does what the item says. (`m` closes;
        // j and k move, unless an item has the key.)
        let own = menu.items.iter().find(|i| i.key == Some(key.code)).filter(|_| commands::is_plain(key)).cloned();
        if own.is_none()
            && let Some(i) = list::step(menu.selected, menu.items.len(), key, Nav::MENU)
        {
            menu.selected = i;
            return;
        }
        let chosen = match key.code {
            KeyCode::Esc | KeyCode::Char('m') => {
                self.popups.menu = None;
                return;
            }
            KeyCode::Enter => menu.items.get(menu.selected).cloned(),
            _ => own,
        };
        let Some(item) = chosen else { return };
        self.popups.menu = None;
        self.run_command(item.command);
    }

    /// Clicks on the menu: choose an item, or close it when clicking
    /// outside. The UI noted which item it drew on each row.
    pub(super) fn click_menu(&mut self, x: u16, y: u16) {
        let r = self.hits.menu;
        if !(x >= r.x && x < r.x + r.width && y >= r.y && y < r.y + r.height) {
            self.popups.menu = None;
            return;
        }
        let Some(&(_, index)) = self.hits.menu_rows.iter().find(|(row, _)| *row == y) else { return };
        if let Some(menu) = &mut self.popups.menu {
            menu.selected = index;
        }
        self.handle_menu_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    }
}
