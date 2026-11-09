//! Windows over the query screen: the library, the server's sessions, the
//! AI's memory.

use super::*;

/// The F3 window: search box, then saved queries (★) and history (↺).
pub(super) fn draw_library(f: &mut Frame, app: &mut App, area: Rect) {
    let t = theme();
    let Some(view) = &app.popups.library_view else { return };
    let rect = centered_box(area, fit(area.width * 4 / 5, 30, area.width), fit(area.height * 4 / 5, 8, area.height));
    f.render_widget(Clear, rect);
    let block = popup(t!("library.title"), t.accent);
    let inner = block.inner(rect);
    f.render_widget(block, rect);
    let items = app.library_items();
    let selected = view.selected.min(items.len().saturating_sub(1));
    // The selected query's first lines, under the list: what it is,
    // without opening it (only with room left for the list).
    let preview: Vec<String> = items
        .get(selected)
        .and_then(|item| app.library_item_sql(item))
        .map(|sql| sql.trim().lines().take(PREVIEW_LINES).map(str::to_string).collect())
        .unwrap_or_default();
    let preview_h = if preview.is_empty() || inner.height < 12 { 0 } else { preview.len() as u16 + 1 };
    let [search, list, preview_area, hints] = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(0), Constraint::Length(preview_h), Constraint::Length(1)])
        .areas(inner);

    let saved = items.iter().filter(|i| matches!(i, LibraryItem::Saved(_))).count();
    f.render_widget(
        Paragraph::new(Line::from(
            [
                vec![Span::styled(" / ", bold(t.accent))],
                input_spans(&view.query, fg(t.text_bright)),
                vec![Span::styled(
                    format!("   {}", t!("library.counts", saved = saved, history = items.len() - saved)),
                    dim(),
                )],
            ]
            .concat(),
        ))
        .style(Style::default().bg(t.inset)),
        search,
    );

    let width = list.width as usize;
    let now = chrono::Local::now();
    // Each saved query by id, once: not a search of the library per row.
    let saved: std::collections::HashMap<&str, &crate::config::library::SavedQuery> =
        app.library.queries.iter().map(|q| (q.id.as_str(), q)).collect();
    let rows: Vec<ListItem> = items
        .iter()
        .map(|item| {
            let at = match item {
                LibraryItem::Saved(id) => saved.get(id.as_str()).and_then(|q| q.changed),
                LibraryItem::History(_, at) => *at,
            };
            let when = at.map(|at| when_label(at, now)).unwrap_or_default();
            let (icon, icon_style, group, name, sql) = match item {
                LibraryItem::Saved(id) => {
                    let Some(q) = saved.get(id.as_str()) else { return ListItem::new("") };
                    let group = if q.group.is_empty() { String::new() } else { format!("{}/", q.group) };
                    ("★ ", fg(t.warn), group, q.name.clone(), q.sql.clone())
                }
                LibraryItem::History(sql, _) => ("↺ ", dim(), String::new(), String::new(), sql.clone()),
            };
            let preview = one_line(&sql);
            // The icon, group and name, and two spaces after a name.
            let gap = if name.is_empty() { 0 } else { 2 };
            let head_len = 2 + group.chars().count() + name.chars().count() + gap;
            let when_w = if when.is_empty() { 0 } else { when.chars().count() + 2 };
            let preview = truncate(&preview, width.saturating_sub(head_len + when_w + 1));
            // The time at the right edge.
            let pad = width.saturating_sub(head_len + preview.chars().count() + when_w + 1);
            ListItem::new(Line::from(vec![
                Span::styled(icon, icon_style),
                Span::styled(group, muted()),
                Span::styled(name.clone(), bold(t.text_bright)),
                Span::raw(if name.is_empty() { "" } else { "  " }),
                Span::styled(preview, if name.is_empty() { fg(t.text) } else { dim() }),
                Span::raw(" ".repeat(pad + 2)),
                Span::styled(when, dim()),
            ]))
        })
        .collect();
    if rows.is_empty() {
        f.render_widget(Paragraph::new(format!(" {}", t!("agents.no_matches"))).style(dim()), list);
    } else {
        let offset = view.offset.min(selected);
        let mut state = ListState::default().with_selected(Some(selected)).with_offset(offset);
        f.render_stateful_widget(List::new(rows).highlight_style(Style::default().bg(t.active)), list, &mut state);
        app.hits.library_list = (list, state.offset());
        if let Some(view) = &mut app.popups.library_view {
            view.offset = state.offset();
        }
    }
    if preview_h > 0 {
        let w = usize::from(preview_area.width);
        let mut lines = vec![Line::from(Span::styled("─".repeat(w), fg(t.line)))];
        lines.extend(preview.iter().map(|l| Line::from(Span::styled(truncate(&l.replace('\t', "    "), w), muted()))));
        f.render_widget(Paragraph::new(lines), preview_area);
    }
    let Some(view) = &app.popups.library_view else { return };

    let hint = if view.confirm_delete {
        Line::from(Span::styled(format!(" {}", t!("library.delete")), bold(t.warn)))
    } else {
        Line::from(Span::styled(format!(" {}", t!("library.keys")), dim()))
    };
    f.render_widget(Paragraph::new(hint).style(Style::default().bg(t.inset)), hints);
}

/// How many of the selected query's lines the library shows under the list.
const PREVIEW_LINES: usize = 5;

/// When a library row was saved or ran, short: the time today, the day
/// this year, else the date.
fn when_label(at: i64, now: chrono::DateTime<chrono::Local>) -> String {
    use chrono::{Datelike, TimeZone};
    let Some(then) = chrono::Local.timestamp_opt(at, 0).single() else { return String::new() };
    if then.date_naive() == now.date_naive() {
        then.format("%H:%M").to_string()
    } else if then.year() == now.year() {
        then.format("%m-%d %H:%M").to_string()
    } else {
        then.format("%Y-%m-%d").to_string()
    }
}

/// The activity monitor: one line per session on the server.
pub(super) fn draw_activity(f: &mut Frame, app: &App, area: Rect) {
    let t = theme();
    let Some(view) = &app.popups.activity else { return };
    let rect = centered_box(area, area.width.saturating_sub(6).max(40), area.height.saturating_sub(4).max(8));
    f.render_widget(Clear, rect);
    let title = format!("{} · {}", t!("activity.title"), tn!("activity.sessions", view.rows.len()));
    let keys = format!(" {} ", t!("activity.keys"));
    let block = popup(&title, t.accent).title_bottom(Line::from(Span::styled(keys, dim())).right_aligned());
    let inner = block.inner(rect);
    f.render_widget(block, rect);
    let w = inner.width as usize;
    let mut lines = vec![Line::from(Span::styled(
        truncate(
            &format!(
                "{:<8} {:<12} {:<12} {:<20} {:>7} {:<18} {}",
                "ID",
                t!("activity.user"),
                t!("activity.database"),
                t!("activity.state"),
                t!("activity.time"),
                t!("activity.waiting"),
                t!("activity.query")
            ),
            w,
        ),
        bold(t.muted),
    ))];
    if let Some(e) = &view.error {
        lines.push(Line::from(Span::styled(e.clone(), fg(t.error))));
    }
    if let Some((id, kill)) = &view.confirm {
        let what = if *kill { t!("activity.end_session") } else { t!("activity.cancel_query") };
        lines.push(Line::from(vec![
            Span::styled(format!("{what} {id}? "), bold(t.error)),
            Span::styled(" y ", bold(t.on_accent).bg(t.error)),
            Span::styled(format!(" {}", t!("confirm.yes_other_no")), muted()),
        ]));
    }
    let rows = (inner.height as usize).saturating_sub(lines.len());
    let first = view.selected.saturating_sub(rows.saturating_sub(1));
    for (i, a) in view.rows.iter().enumerate().skip(first).take(rows) {
        let time = a.seconds.map_or_else(String::new, |s| match s {
            s if s >= 3600 => format!("{}h{:02}m", s / 3600, s % 3600 / 60),
            s if s >= 60 => format!("{}m{:02}s", s / 60, s % 60),
            s => format!("{s}s"),
        });
        let waiting = if a.blocked_by.is_empty() {
            a.wait.clone()
        } else {
            t!("activity.blocked_by", ids = a.blocked_by.join(","))
        };
        let id = if a.own { format!("{}*", a.id) } else { a.id.clone() };
        let text = format!(
            "{:<8} {:<12} {:<12} {:<20} {:>7} {:<18} {}",
            truncate(&id, 8),
            truncate(&a.user, 12),
            truncate(&a.database, 12),
            truncate(&a.state, 20),
            time,
            truncate(&waiting, 18),
            one_line(&a.query)
        );
        let style = if i == view.selected {
            bold(t.text_bright).bg(t.active)
        } else if !a.blocked_by.is_empty() {
            fg(t.error)
        } else if a.state == "active" || a.state == "query" {
            fg(t.text)
        } else {
            muted()
        };
        lines.push(Line::from(Span::styled(truncate(&text, w), style)));
    }
    if view.rows.iter().any(|a| a.own) {
        lines.push(Line::from(Span::styled(t!("activity.this_tab"), dim())));
    }
    f.render_widget(Paragraph::new(lines), inner);
}

/// `Ctrl+K y k`: the project's memory, a fact a line; a model's
/// suggestions, waiting to be confirmed, in orange.
pub(super) fn draw_memory(f: &mut Frame, app: &App, area: Rect) {
    let t = theme();
    let Some(view) = &app.popups.memory_view else { return };
    let width = fit(100, 40, area.width.saturating_sub(4));
    let height = fit(view.facts.len() as u16 + 7, 10, area.height.saturating_sub(2));
    let rect = centered_box(area, width, height);
    f.render_widget(Clear, rect);
    let room = usize::from(width).saturating_sub(4);
    let mut lines = Vec::new();
    if view.loading {
        lines.push(Line::from(Span::styled(format!(" {}", t!("navigator.loading_tables")), dim())));
    } else if view.facts.is_empty() {
        lines.push(Line::from(Span::styled(format!(" {}", t!("memory.none")), dim())));
    }
    let shown = usize::from(height.saturating_sub(6));
    let first = view.selected.saturating_sub(shown.saturating_sub(1));
    for (i, fact) in view.facts.iter().enumerate().skip(first).take(shown) {
        let confirmed = fact["confirmed"].as_bool() == Some(true);
        let text = format!(
            " {} {} {}",
            fact["subject"].as_str().unwrap_or("?"),
            fact["relation"].as_str().unwrap_or("?"),
            fact["value"].as_str().unwrap_or("?"),
        );
        // The fact comes first: a long source (a migrations folder's
        // path) gets what's left of the row (16 at least), its end kept.
        let tail = if confirmed { String::new() } else { format!(", {}", t!("memory.not_confirmed")) };
        let left = room.saturating_sub(text.chars().count() + tail.len() + 4).max(16);
        let source = truncate_start(fact["source"].as_str().unwrap_or("?"), left);
        let who = format!("  · {source}{tail}");
        let text = truncate(&text, room.saturating_sub(who.chars().count()));
        let style = if confirmed { fg(t.text) } else { fg(t.warn) };
        let mut line = Line::from(vec![Span::styled(text, style), Span::styled(who, dim())]);
        if i == view.selected {
            line = line.style(Style::default().bg(t.active));
        }
        lines.push(line);
    }
    lines.push(Line::default());
    if let Some(input) = &view.adding {
        let (mark, help) =
            if view.editing.is_some() { (" ✎ ", t!("memory.edit_help")) } else { (" + ", t!("memory.add_help")) };
        lines.push(Line::from(
            [vec![Span::styled(mark, bold(t.accent))], input_spans(input, fg(t.text_bright))].concat(),
        ));
        lines.push(Line::from(Span::styled(format!(" {help}"), dim())));
    } else {
        lines.push(Line::from(Span::styled(format!(" {}", t!("memory.shown")), dim())));
        lines.push(Line::from(Span::styled(format!(" {}", t!("memory.keys")), dim())));
    }
    let project = app.project().map(|p| p.name.to_uppercase()).unwrap_or_default();
    f.render_widget(Paragraph::new(lines).block(popup(&format!("{} · {project}", t!("memory.title")), t.accent)), rect);
}
