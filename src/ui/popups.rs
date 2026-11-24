//! Popups on every screen: a yes-or-no question and the text view (a
//! value, a message, a connection test's steps).

use super::*;

/// The text view's keys (a cell, a row with `x`, a definition…), most
/// important first; closing it last. The keys are the window's table's.
fn text_view_keys(view: &crate::app::CellView) -> Vec<String> {
    use crate::app::key_table::{LEFT_RIGHT, UP_DOWN, hint};
    use crate::app::windows::cell::{CELL_KEYS, CellKey};
    let key = |command, what| hint(CELL_KEYS, command, what);
    let moving = |keys: &str, what: &str| format!("{keys} {what}");
    let close = key(CellKey::Close, t!("cell_view.close"));
    if view.record {
        vec![
            moving(UP_DOWN, t!("cell_view.scroll_next_row")),
            moving(LEFT_RIGHT, t!("cell_view.row")),
            key(CellKey::Copy, t!("cell_view.copy_row")),
            close,
        ]
    } else if view.browse {
        // A table's: its CREATE TABLE, or back from it.
        let create = view.create_table.as_ref().map(|_| {
            let what = if view.overview.is_some() { t!("cell_view.overview") } else { t!("cell_view.create_table") };
            key(CellKey::CreateTable, what)
        });
        create
            .into_iter()
            .chain([
                moving(LEFT_RIGHT, t!("cell_view.prev_next_table")),
                moving(UP_DOWN, t!("cell_view.scroll")),
                key(CellKey::Copy, t!("cell_view.copy")),
                close,
            ])
            .collect()
    } else if view.follow.is_some() {
        vec![
            key(CellKey::Open, t!("cell_view.open_tab")),
            key(CellKey::Copy, t!("cell_view.copy")),
            moving(UP_DOWN, t!("cell_view.scroll")),
            close,
        ]
    } else if view.sql.is_some() {
        vec![
            moving(UP_DOWN, t!("cell_view.scroll")),
            key(CellKey::Copy, t!("cell_view.copy_sql")),
            key(CellKey::Open, t!("cell_view.open_tab_there")),
            close,
        ]
    } else {
        // A message: copied whole with `y`, or a part of it with the
        // terminal's own selection (the app has the mouse otherwise).
        vec![
            moving(UP_DOWN, t!("cell_view.scroll")),
            key(CellKey::Copy, t!("cell_view.copy")),
            t!("cell_view.select").to_string(),
            close,
        ]
    }
}

/// As many of `keys` as fit in `width`, joined by ` · `: the first ones,
/// and always the last (how to close).
fn fitting_hints<S: AsRef<str>>(keys: &[S], width: usize) -> String {
    let Some((last, rest)) = keys.split_last() else { return String::new() };
    let mut shown: Vec<&str> = Vec::new();
    let mut used = 1 + last.as_ref().chars().count();
    for key in rest {
        let key = key.as_ref();
        let w = key.chars().count() + 3;
        if used + w > width {
            break;
        }
        used += w;
        shown.push(key);
    }
    shown.push(last.as_ref());
    format!(" {}", shown.join(" · "))
}

pub(super) fn draw_cell_view(f: &mut Frame, app: &mut App, area: Rect) {
    let t = theme();
    // The navigator on screen, for definitions to sit beside.
    let navigator =
        app.hits.schema.filter(|_| app.panes.schema_shown && !app.panes.zoomed && app.screen == Screen::Query);
    let Some(view) = &mut app.popups.cell_view else { return };
    // As big as the text needs, up to nearly the whole screen. Stepping
    // through tables or rows (←/→), one size for all, so it doesn't jump.
    let steps = view.browse || view.record;
    let text_w = view.text.lines().map(|l| l.chars().count()).max().unwrap_or(0) as u16;
    let wanted_w =
        if steps { 100 } else { text_w.max(view.title.chars().count() as u16 + 4).saturating_add(4).max(50) };
    let width = fit(wanted_w, 30, area.width.saturating_sub(8));
    // The text, its keys on a line of their own, and the frame.
    let wanted_h = if steps { u16::MAX } else { (view.text.lines().count() as u16).saturating_add(3) };
    let height = fit(wanted_h, 5, area.height.saturating_sub(4));
    // A definition from the navigator: right of it, keeping it in sight
    // (centred if there's no room there).
    let beside = navigator.filter(|_| view.browse).and_then(|nav| {
        let room = area.right().saturating_sub(nav.right() + 1);
        (room >= 40).then(|| Rect { x: nav.right(), y: nav.y, width: width.min(room), height: height.min(nav.height) })
    });
    let rect = beside.unwrap_or_else(|| centered_box(area, width, height));
    f.render_widget(Clear, rect);
    let block = popup(&view.title, t.accent);
    let inner = block.inner(rect);
    f.render_widget(block, rect);
    let [body, keys] = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(1)])
        .areas(inner);
    let text = Paragraph::new(view.text.as_str()).wrap(Wrap { trim: false });
    let lines = text.line_count(body.width);
    view.max_scroll = u16::try_from(lines.saturating_sub(usize::from(body.height))).unwrap_or(u16::MAX);
    view.scroll = view.scroll.min(view.max_scroll);
    f.render_widget(text.scroll((view.scroll, 0)), body);
    let hint = fitting_hints(&text_view_keys(view), usize::from(keys.width));
    f.render_widget(Paragraph::new(Span::styled(hint, dim())).style(Style::default().bg(t.inset)), keys);
}

pub(super) fn draw_confirm(f: &mut Frame, app: &App, area: Rect) {
    let t = theme();
    let Some(confirm) = &app.popups.confirm else { return };
    let color = if confirm.danger { t.error } else { t.accent };
    let mut lines = vec![Line::from(Span::styled(confirm.message.clone(), bold(color))), Line::default()];
    // The details are wrapped below, once the width is known.
    let details_at = lines.len();
    if !confirm.details.is_empty() {
        lines.push(Line::default());
    }
    let mut keys = Vec::new();
    if confirm.offer_transaction {
        keys.push(Span::styled(" t ", bold(t.on_accent).bg(t.ok)));
        keys.push(Span::styled(format!(" {}   ", t!("confirm.review_first")), fg(t.text)));
    }
    // Sending a schema to a model: this once, or always for the project.
    if matches!(confirm.action, crate::app::ConfirmAction::ShareSchema { .. }) {
        keys.push(Span::styled(" a ", bold(t.on_accent).bg(t.accent)));
        keys.push(Span::styled(format!(" {}", t!("confirm.always")), fg(t.text)));
    }
    // An agent's write: only `n` or Esc say no, so typing can't.
    let agent = matches!(confirm.action, crate::app::ConfirmAction::AgentWrite(_));
    let share = matches!(confirm.action, crate::app::ConfirmAction::ShareSchema { .. });
    lines.push(Line::from(vec![
        Span::styled(" y ", bold(t.on_accent).bg(color)),
        Span::styled(
            if share { format!(" {}   ", t!("confirm.this_once")) } else { format!(" {}   ", t!("confirm.yes")) },
            fg(t.text),
        ),
        // An agent's write and the sharing question: only `n` or Esc say
        // no, so typing can't.
        Span::styled(
            if agent || share {
                format!(" {} ", t!("confirm.n_esc"))
            } else {
                format!(" {} ", t!("confirm.any_other_key"))
            },
            bold(t.text).bg(t.active),
        ),
        Span::styled(
            if agent || share { format!(" {}", t!("confirm.no")) } else { format!(" {}", t!("cell.cancel")) },
            muted(),
        ),
    ]));
    if !keys.is_empty() {
        lines.push(Line::from(keys));
    }
    let widest_detail = confirm.details.iter().map(|d| d.chars().count() + 4).max().unwrap_or(0);
    let widest = lines.iter().map(Line::width).max().unwrap_or(0).max(widest_detail);
    let width = fit(widest as u16 + 4, 40, area.width.saturating_sub(4));
    // Each detail a bullet, its wrapped lines under its text, not the bullet.
    let room = usize::from(width.saturating_sub(2)).saturating_sub(4).max(10);
    let details = confirm.details.iter().flat_map(|d| {
        query_screen::results::wrap_words(d, room).into_iter().enumerate().map(|(i, part)| {
            Line::from(Span::styled(format!("{}{part}", if i == 0 { "  • " } else { "    " }), fg(t.text)))
        })
    });
    lines.splice(details_at..details_at, details);
    let text = Paragraph::new(lines).wrap(Wrap { trim: false });
    // Counted as wrapped: a line too long for the box takes more rows, and
    // the last row (its keys) must still show.
    let rows = text.line_count(width.saturating_sub(2));
    let height = (u16::try_from(rows).unwrap_or(u16::MAX).saturating_add(2)).min(area.height);
    let rect = centered_box(area, width, height);
    f.render_widget(Clear, rect);
    let title = if confirm.danger { t!("confirm.title_danger") } else { t!("confirm.title") };
    f.render_widget(text.block(popup(title, color)), rect);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hints_drop_the_less_important_keys_but_not_closing() {
        let keys = ["↑↓ scroll, next row", "←→ row", "y copy row", "Esc close"];
        assert_eq!(fitting_hints(&keys, 80), " ↑↓ scroll, next row · ←→ row · y copy row · Esc close");
        assert_eq!(fitting_hints(&keys, 40), " ↑↓ scroll, next row · Esc close");
        assert_eq!(fitting_hints(&keys, 5), " Esc close");
    }
}
