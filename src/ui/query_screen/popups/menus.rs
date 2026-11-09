//! Ctrl+K (its letters, which-key, the list of every command) and the
//! panes' menus.

use super::*;

/// A pop-up menu: what can be done here, each with its key; what doesn't
/// apply dimmed.
pub(super) fn draw_menu(f: &mut Frame, app: &mut App, area: Rect) {
    let t = theme();
    let Some(menu) = &mut app.popups.menu else { return };
    let label_w = menu.items.iter().map(|i| i.label.chars().count()).max().unwrap_or(0);
    let keys_w = menu.items.iter().map(|i| i.keys.chars().count()).max().unwrap_or(0);
    let width = ((label_w + keys_w + 6) as u16).min(area.width);
    let rows = menu.rows(usize::from(area.height.saturating_sub(2)));
    let height = (rows.len() as u16 + 2).min(area.height);
    // Open at the pointer, kept on screen.
    let left = menu.at.0.min(area.right().saturating_sub(width));
    let top = menu.at.1.min(area.bottom().saturating_sub(height));
    let rect = Rect { x: left, y: top, width, height };
    // Too long for the screen: the selection stays in sight.
    let room = usize::from(height.saturating_sub(2)).max(1);
    let at = rows.iter().position(|r| *r == Some(menu.selected)).unwrap_or(0);
    if at < menu.scroll {
        menu.scroll = at;
    } else if at >= menu.scroll + room {
        menu.scroll = at + 1 - room;
    }
    menu.scroll = menu.scroll.min(rows.len().saturating_sub(room));
    let mut drawn = Vec::new();
    let lines: Vec<Line> = rows
        .iter()
        .enumerate()
        .skip(menu.scroll)
        .take(room)
        .map(|(row, entry)| {
            let Some(i) = *entry else {
                return Line::from(Span::styled("─".repeat(usize::from(width.saturating_sub(2))), dim()));
            };
            drawn.push((rect.y + 1 + (row - menu.scroll) as u16, i));
            let item = &menu.items[i];
            let selected = i == menu.selected;
            let style = match (selected, item.dimmed) {
                (true, true) => muted().bg(t.active),
                (true, false) => bold(t.text_bright).bg(t.active),
                (false, true) => dim(),
                (false, false) => fg(t.text),
            };
            let pad = label_w.saturating_sub(item.label.chars().count()) + 2;
            Line::from(vec![
                Span::styled(format!(" {}{}", item.label, " ".repeat(pad)), style),
                Span::styled(format!("{:>keys_w$} ", item.keys), if selected { muted().bg(t.active) } else { dim() }),
            ])
        })
        .collect();
    let title = if rows.len() > room {
        format!("{} {}/{}", menu.title, menu.selected + 1, menu.items.len())
    } else {
        menu.title.to_string()
    };
    let block = popup(&title, t.accent)
        .title_bottom(Line::from(Span::styled(format!(" {} ", t!("popup.choose_close")), dim())).right_aligned());
    f.render_widget(Clear, rect);
    f.render_widget(Paragraph::new(lines).block(block), rect);
    app.hits.menu = rect;
    app.hits.menu_rows = drawn;
}

/// The command palette: a search line, then matching commands and keys.
/// `(keys, what)` pairs in two columns of `width` cells each, keys in `key_style`.
fn two_columns(items: &[(String, String)], width: usize, key_style: Style) -> Vec<Line<'static>> {
    let key_w = items.iter().map(|(k, _)| k.chars().count()).max().unwrap_or(0).min(width / 2);
    let cell = |(key, what): &(String, String)| {
        let what_w = width.saturating_sub(key_w + 3);
        vec![
            Span::styled(format!(" {:<key_w$} ", truncate(key, key_w)), key_style),
            Span::styled(format!("{:<what_w$} ", truncate(what, what_w)), fg(theme().text)),
        ]
    };
    items.chunks(2).map(|pair| Line::from(pair.iter().flat_map(cell).collect::<Vec<_>>())).collect()
}

/// What a letter that opens a group says in Ctrl+K's menu.
fn leader_group_name(letter: char) -> &'static str {
    match letter {
        's' => t!("menu.group_server"),
        'y' => t!("menu.group_ai"),
        _ => t!("menu.group_other"),
    }
}

/// Ctrl+K's menu: its letters by group (those not at hand here dimmed; they
/// still work), the open tabs by number.
pub(super) fn draw_which_key(f: &mut Frame, app: &App, area: Rect) {
    use crate::app::commands::{ACTION_GROUPS, Group};
    use crate::config::keys::Action;
    let t = theme();
    let width = fit(84, 30, area.width);
    let inner = usize::from(width.saturating_sub(2));
    let col = inner / 2;
    let dimmed = |a: Action| {
        (app.focus != Focus::Editor && a.is_editor_action())
            || (app.focus == Focus::Schema && a == Action::TxMode)
            || matches!(app.can(crate::app::commands::Command::Action(a)), crate::app::commands::Can::No { .. })
    };
    let group_open = app.popups.palette.as_ref().and_then(crate::app::query_screen::palette::Palette::group);
    let letters = app.keymap.leader_keys();
    // Its key, its few words, and whether it's dimmed.
    type Item = (String, String, bool);
    let mut groups_shown: Vec<char> = Vec::new();
    let groups: Vec<(Group, Vec<Item>)> = ACTION_GROUPS
        .iter()
        .map(|(group, actions)| {
            let mut items: Vec<Item> = Vec::new();
            for (keys, a) in actions.iter().flat_map(|&a| letters.iter().filter(move |(_, x)| *x == a)) {
                match (group_open, keys.as_slice()) {
                    (None, [' ']) => items.push(("Space".to_string(), a.brief().to_string(), dimmed(*a))),
                    (None, [c]) => items.push((c.to_string(), a.brief().to_string(), dimmed(*a))),
                    // A group's letter once, where the first of it would be.
                    // (Not one a letter of its own has taken over.)
                    (None, [c, _])
                        if !groups_shown.contains(c)
                            && app.keymap.leader_step(&[*c]) == crate::config::keys::LeaderStep::Group =>
                    {
                        groups_shown.push(*c);
                        items.push((c.to_string(), leader_group_name(*c).to_string(), false));
                    }
                    (Some(open), [c, second]) if open == *c => {
                        items.push((second.to_string(), a.brief().to_string(), dimmed(*a)));
                    }
                    _ => {}
                }
            }
            if *group == Group::App && group_open.is_none() {
                items.push(("?".to_string(), t!("menu.all_keys").to_string(), false));
            }
            (*group, items)
        })
        .filter(|(_, items)| !items.is_empty())
        .collect();
    let title_w = groups.iter().map(|(g, _)| g.title().chars().count()).max().unwrap_or(0);
    let mut lines = vec![Line::from(Span::styled(format!(" {}", t!("menu.then_press")), bold(t.accent)))];
    for (group, items) in &groups {
        let mut spans = vec![Span::styled(format!(" {:<title_w$}  ", group.title().to_uppercase()), bold(t.text))];
        let mut used = title_w + 3;
        for (key, brief, dim_it) in items {
            let wide = key.chars().count() + brief.chars().count() + 3;
            if used + wide > inner && used > title_w + 3 {
                lines.push(Line::from(std::mem::take(&mut spans)));
                spans.push(Span::raw(" ".repeat(title_w + 3)));
                used = title_w + 3;
            }
            let (key_style, what_style) = if *dim_it { (dim(), dim()) } else { (bold(t.accent), fg(t.text)) };
            spans.push(Span::styled(format!("{key} "), key_style));
            spans.push(Span::styled(format!("{brief}  "), what_style));
            used += wide;
        }
        lines.push(Line::from(spans));
    }
    lines.push(Line::default());
    if app.tabs.len() > 1 && group_open.is_none() {
        // The open tabs by number: Ctrl+K 2 goes to the second.
        let tabs: Vec<(String, String)> = app
            .tabs
            .iter()
            .enumerate()
            .take(9)
            .map(|(i, tab)| {
                let dirty = if app.tab_dirty(i) { " ●" } else { "" };
                let here = if i == app.active_tab { format!(" ({})", t!("menu.this_one")) } else { String::new() };
                ((i + 1).to_string(), format!("{}{dirty}{here}", tab.name))
            })
            .collect();
        let more = app.tabs.len().saturating_sub(9);
        let title = if more > 0 {
            format!(" {} ({})", t!("menu.tabs"), t!("menu.more_tabs", more = more))
        } else {
            format!(" {}", t!("menu.tabs"))
        };
        // Its first key spelled out, as the menu's others are (none: no hint).
        let close = app.keymap.first_label(Action::CloseTab);
        let hint =
            if close.is_empty() { String::new() } else { format!("  {}", t!("menu.close_this_tab", key = close)) };
        lines.push(Line::from(vec![Span::styled(title, bold(t.accent)), Span::styled(hint, dim())]));
        lines.extend(two_columns(&tabs, col, bold(t.accent)));
        lines.push(Line::default());
    }
    let help = if group_open.is_some() { t!("menu.group_help") } else { t!("menu.search") };
    lines.push(Line::from(Span::styled(format!(" {help}"), dim())));

    let height = (lines.len() as u16 + 2).min(area.height);
    let rect = Rect { y: area.y + area.height.saturating_sub(height).min(2), ..centered_box(area, width, height) };
    f.render_widget(Clear, rect);
    let leader = app.keymap.first_label(Action::Palette).to_uppercase();
    let title = group_open.map_or(leader.clone(), |c| format!("{leader} {}", c.to_uppercase()));
    f.render_widget(Paragraph::new(lines).block(popup(&title, t.accent)), rect);
}

pub(super) fn draw_palette(f: &mut Frame, app: &App, area: Rect) {
    let t = theme();
    let Some(palette) = &app.popups.palette else { return };
    if !palette.searching() {
        // Until the menu is due, only the status bar says Ctrl+K is waiting.
        if palette.menu_shown() {
            draw_which_key(f, app, area);
        }
        return;
    }
    let items = app.palette_items();
    let width = fit(76, 30, area.width);
    // The connections list says its keys under it (Ctrl+D closes).
    let help = u16::from(palette.conns_only()) * 2;
    let rows = (area.height.saturating_sub(6 + help) as usize).clamp(1, 14).min(items.len().max(1));
    // Two rows down from the top: no taller than what's left below that.
    let rect = Rect {
        y: area.y + 2,
        ..centered_box(area, width, (rows as u16 + 4 + help).min(area.height.saturating_sub(2)))
    };
    f.render_widget(Clear, rect);
    let title = if palette.conns_only() {
        t!("conn_list.title")
    } else if palette.tabs_only() {
        t!("tab_list.title")
    } else {
        t!("palette.title")
    };
    let block = popup(title, t.accent);
    let inner = block.inner(rect);
    f.render_widget(block, rect);
    let [search, list, keys] = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(2), Constraint::Min(0), Constraint::Length(help)])
        .areas(inner);
    if help > 0 {
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(format!(" {}", t!("conn_list.help")), dim()))),
            Rect { y: keys.y + 1, height: 1, ..keys },
        );
    }
    f.render_widget(
        Paragraph::new(Line::from(
            [vec![Span::styled(" › ", bold(t.accent))], input_spans(&palette.query, fg(t.text_bright))].concat(),
        )),
        search,
    );
    let first = palette.selected.saturating_sub(rows.saturating_sub(1));
    let w = list.width as usize;
    let lines: Vec<Line> = items
        .iter()
        .enumerate()
        .skip(first)
        .take(rows)
        .map(|(i, cmd)| {
            let keys = cmd.keys(app);
            let name = truncate(&cmd.text(app), w.saturating_sub(keys.chars().count() + 4));
            let pad = w.saturating_sub(name.chars().count() + keys.chars().count() + 2);
            let style = if i == palette.selected { bold(t.text_bright).bg(t.active) } else { fg(t.text) };
            Line::from(vec![
                Span::styled(format!(" {name}{}", " ".repeat(pad)), style),
                Span::styled(format!("{keys} "), if i == palette.selected { muted().bg(t.active) } else { dim() }),
            ])
        })
        .collect();
    let lines = if lines.is_empty() {
        vec![Line::from(Span::styled(format!(" {}", t!("palette.none")), dim()))]
    } else {
        lines
    };
    f.render_widget(Paragraph::new(lines), list);
}
