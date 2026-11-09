//! Every key's window (F1, `?`): the query screen's keys from the commands
//! and the panes' tables, so a command with a key can't be left out; the
//! other screens' by hand.

use super::*;
use crate::config::keys::Action;

/// The query screen's keys: from the commands and the panes' tables, so a
/// command with a key can't be left out. By hand only what isn't a command.
fn query_help(app: &App) -> Vec<(&'static str, Vec<(String, String)>)> {
    use crate::app::commands::{ACTION_GROUPS, Command, Group, Here, pane_key_list};
    let owned = |rows: &[(&str, &str)]| -> Vec<(String, String)> {
        rows.iter().map(|(k, w)| ((*k).to_string(), (*w).to_string())).collect()
    };
    let mut out: Vec<(&'static str, Vec<(String, String)>)> = ACTION_GROUPS
        .iter()
        .map(|(group, actions)| {
            let mut rows: Vec<(String, String)> = actions
                .iter()
                .map(|&a| (a, app.keymap.label(a)))
                .filter(|(_, keys)| !keys.is_empty())
                .map(|(a, keys)| {
                    let what = if a == Action::Palette { t!("help.keys_menu") } else { a.describe() };
                    (keys.to_string(), what.to_string())
                })
                .collect();
            let by_hand: Vec<(&str, &str)> = match group {
                Group::Run => vec![("Esc", t!("help.cancel"))],
                Group::Editor => vec![
                    ("Tab", t!("help.complete")),
                    (t!("help.key_select"), t!("help.select")),
                    ("Ctrl+C / Ctrl+X / Ctrl+V", t!("help.copy")),
                    (t!("help.key_vim"), t!("help.vim")),
                    (t!("help.key_rebind"), t!("help.rebind")),
                ],
                Group::Tabs => vec![("Ctrl+K 1…9 · 0 · Alt+1…9 · 0", t!("help.switch_tab"))],
                Group::View => vec![("Tab / Shift+Tab", t!("help.next_pane"))],
                Group::App => vec![("? · F1", t!("palette.help")), ("Ctrl+Q", t!("palette.quit"))],
                _ => Vec::new(),
            };
            rows.extend(owned(&by_hand));
            (group.title(), rows)
        })
        .collect();
    if let Some(leader) = app.settings.local_leader.chars().next().filter(|_| app.vim_enabled()) {
        let rows = crate::app::commands::EDITOR_LEADER_KEYS
            .iter()
            .map(|&(c, a)| (format!("{leader} {c}"), Command::Action(a).label(app)))
            .collect();
        out.push((t!("help.local_leader"), rows));
    }
    let of = |here: Here, by_hand: &[(&str, &str)]| {
        let mut rows: Vec<(String, String)> =
            pane_key_list(here).into_iter().map(|(keys, c): (String, Command)| (keys, c.label(app))).collect();
        rows.extend(owned(by_hand));
        rows
    };
    out.push((t!("pane.results"), of(Here::Rows, &[("[ · ]", t!("help.result_tabs"))])));
    out.push((t!("messages.title"), of(Here::Messages, &[])));
    out.push((t!("results.plan"), of(Here::PlanText, &[])));
    out.push((
        t!("navigator.title"),
        of(Here::Navigator, &[("→ / ←", t!("help.expand")), ("PgUp · PgDn", t!("help.page"))]),
    ));
    out
}

/// The keys of the screen by section, as (keys, what they do).
pub(super) fn help_sections(app: &App) -> Vec<(&'static str, Vec<(String, String)>)> {
    let sections: Vec<(&'static str, Vec<(&str, &str)>)> = match app.screen {
        Screen::Query => return query_help(app),
        Screen::Projects | Screen::Connections | Screen::Connect => vec![
            (
                t!("help.projects"),
                vec![
                    ("↵", t!("help.open_project")),
                    ("n · r · d", t!("help.new_rename_delete")),
                    ("Tab / Shift+Tab", t!("help.next_pane_round")),
                    ("→", t!("help.its_connections")),
                    ("Esc", t!("help.back_to_tabs")),
                    ("Ctrl+Q", t!("help.quit")),
                ],
            ),
            (
                t!("help.connections"),
                vec![
                    ("↵", t!("help.connect")),
                    ("e · →", t!("help.edit_settings")),
                    ("n · d · m", t!("help.new_delete_move")),
                    ("c", t!("help.containers")),
                    ("←", t!("hint.projects")),
                    ("Esc", t!("help.back_to_tabs")),
                ],
            ),
            (
                t!("help.settings"),
                vec![
                    ("↑↓", t!("help.fields")),
                    ("← →", t!("help.change_option")),
                    ("↵ · Ctrl+S", t!("help.connect_save")),
                    ("Ctrl+T", t!("help.test")),
                    (t!("help.key_paste_url"), t!("help.fills_form")),
                    ("Esc", t!("help.back_to_connections")),
                ],
            ),
        ],
    };
    sections
        .into_iter()
        .map(|(title, keys)| (title, keys.into_iter().map(|(k, w)| (k.to_string(), w.to_string())).collect()))
        .collect()
}

pub(super) fn draw_help(f: &mut Frame, app: &mut App, area: Rect) {
    let t = theme();
    let sections = help_sections(app);
    // Configured keys can be long: size the key column to the longest.
    let key_w = sections.iter().flat_map(|(_, keys)| keys).map(|(k, _)| k.chars().count()).max().unwrap_or(0).max(22);
    // A long description ends in `…`, not at the border.
    let what_w = usize::from(area.width).saturating_sub(key_w + 7);
    let mut lines = Vec::new();
    // With no model, AI's keys are there but dimmed, and say why.
    let ai_off = app.ai_state() == crate::app::AiState::Off;
    for (title, keys) in sections {
        let off = ai_off && title == crate::app::commands::Group::Ai.title();
        let heading =
            if off { format!("{} · {}", title.to_uppercase(), t!("bar.ai_off")) } else { title.to_uppercase() };
        lines.push(Line::from(Span::styled(heading, if off { dim() } else { bold(t.accent) })));
        for (key, what) in keys {
            lines.push(Line::from(vec![
                Span::styled(format!("  {key:<key_w$} "), if off { dim() } else { bold(t.text) }),
                Span::styled(truncate(&what, what_w), if off { dim() } else { muted() }),
            ]));
        }
        lines.push(Line::default());
    }
    lines.push(Line::from(Span::styled(t!("help.closes"), dim())));
    let height = fit(lines.len() as u16 + 2, 5, area.height);
    let width = lines.iter().map(Line::width).max().unwrap_or(0) as u16 + 4;
    let rect = centered_box(area, fit(width.max(64), 20, area.width), height);
    // Longer than the screen: the arrows scroll it, as far as there's more.
    let most = (lines.len() as u16 + 2).saturating_sub(height);
    app.help.scroll = app.help.scroll.min(most);
    f.render_widget(Clear, rect);
    // With more than fits, the title says which lines these are.
    let room = height.saturating_sub(2);
    let title = if most > 0 {
        let last = (app.help.scroll + room).min(lines.len() as u16);
        format!("{} {}–{last}/{}", t!("help.title"), app.help.scroll + 1, lines.len())
    } else {
        t!("help.title").to_string()
    };
    f.render_widget(Paragraph::new(lines).scroll((app.help.scroll, 0)).block(popup(&title, t.accent)), rect);
}
