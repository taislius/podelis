//! The query screen: schema navigator on the left, editor above results on
//! the right, plus its popups.

use super::editor::{draw_completion, draw_editor};
use super::results::draw_results;
use super::schema::draw_schema;
use super::*;

/// The navigator's width: as resized (up to half the screen), else a
/// quarter of it within limits.
pub(crate) fn schema_width(resized: Option<u16>, total: u16) -> u16 {
    match resized {
        Some(w) => fit(w, SCHEMA_WIDTH / 2, total / 2),
        None => fit(total / 4, SCHEMA_WIDTH, 42),
    }
}

pub(super) fn draw_query(f: &mut Frame, app: &mut App, body: Rect) {
    app.hits.navigator_too_narrow = body.width < SCHEMA_MIN_TOTAL_WIDTH;
    let show_schema = app.panes.schema_visible && body.width >= SCHEMA_MIN_TOTAL_WIDTH;
    app.panes.schema_shown = show_schema;
    if !show_schema && app.focus == Focus::Schema {
        app.focus = Focus::Editor;
    }
    let (schema_area, main) = if show_schema {
        let [s, m] = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(schema_width(app.panes.schema_width, body.width)), Constraint::Min(20)])
            .areas(body);
        (Some(s), m)
    } else {
        (None, body)
    };
    let [editor_area, results_area] = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(app.panes.editor_percent), Constraint::Min(5)])
        .areas(main);

    // Zoomed: only the focused pane, using the whole body.
    let cursor = if app.panes.zoomed {
        // The top bar's project, drawn already, stays clickable.
        app.hits = crate::app::Hits {
            project: app.hits.project,
            navigator_too_narrow: app.hits.navigator_too_narrow,
            ..crate::app::Hits::default()
        };
        match app.focus {
            Focus::Schema => {
                app.hits.schema = Some(body);
                draw_schema(f, app, body);
                None
            }
            Focus::Results => {
                draw_results(f, app, body);
                None
            }
            Focus::Editor => draw_editor(f, app, body),
        }
    } else {
        app.hits.schema = schema_area;
        if let Some(schema_area) = schema_area {
            draw_schema(f, app, schema_area);
        }
        // The statement zoom: the editor taller, the results under it. It
        // stays while another pane has the keys (an Explain from it shows
        // its plan there); Shift+F12, or Esc in the editor, ends it.
        if app.panes.statement_zoom {
            // In place of the editor's pane, taller (two thirds at least);
            // the results under it, smaller, still whole.
            let height = editor_area.height.max(main.height.saturating_mul(2) / 3).min(main.height.saturating_sub(5));
            let popover = Rect { height, ..editor_area };
            let below = Rect { y: popover.bottom(), height: main.bottom().saturating_sub(popover.bottom()), ..main };
            draw_results(f, app, below);
            draw_editor(f, app, popover)
        } else {
            let cursor = draw_editor(f, app, editor_area);
            draw_results(f, app, results_area);
            cursor
        }
    };

    // Popups, back to front.
    let area = f.area();
    for overlay in app.overlays().into_iter().rev() {
        match overlay {
            Overlay::Completion => {
                if let Some(cursor) = cursor {
                    draw_completion(f, app, cursor, area);
                }
            }
            Overlay::CellView => draw_cell_view(f, app, area),
            Overlay::Library => draw_library(f, app, area),
            Overlay::Settings => super::settings::draw_settings(f, app, area),
            Overlay::TabPrompt => draw_tab_prompt(f, app, area),
            Overlay::ParamForm => draw_param_form(f, app, area),
            Overlay::Review => draw_review(f, app, area),
            Overlay::TxPicker => draw_tx_picker(f, app, area),
            Overlay::Sandbox => draw_sandbox(f, app, area),
            Overlay::Activity => draw_activity(f, app, area),
            Overlay::Agents => super::agents::draw_agents(f, app, area),
            Overlay::Palette => draw_palette(f, app, area),
            Overlay::RowForm => draw_row_form(f, app, area),
            Overlay::PipePrompt => {
                let command = app.popups.pipe_prompt.clone().unwrap_or_default();
                draw_pipe_prompt(f, &command, area);
            }
            Overlay::AskPrompt => draw_ask_prompt(f, app, area),
            Overlay::Memory => draw_memory(f, app, area),
            Overlay::Menu => draw_menu(f, app, area),
            Overlay::Confirm => draw_confirm(f, app, area),
        }
    }
}

/// A pop-up menu: what can be done here, each with its key; what doesn't
/// apply dimmed.
fn draw_menu(f: &mut Frame, app: &mut App, area: Rect) {
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

/// The F3 window: search box, then saved queries (★) and history (↺).
fn draw_library(f: &mut Frame, app: &mut App, area: Rect) {
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
fn draw_activity(f: &mut Frame, app: &App, area: Rect) {
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

/// The add-row form: one line per column, with its type.
fn draw_row_form(f: &mut Frame, app: &App, area: Rect) {
    let t = theme();
    let Some(form) = &app.popups.row_form else { return };
    let label_w = form.columns.iter().map(|c| c.name.chars().count()).max().unwrap_or(0).min(24);
    let width = fit(80, 30, area.width);
    let type_w = 14;
    let value_w = (width as usize).saturating_sub(label_w + type_w + 10);
    let rows = (area.height.saturating_sub(8) as usize).max(1);
    let first = form.field.saturating_sub(rows - 1);
    let mut lines = Vec::new();
    for (i, (c, v)) in form.columns.iter().zip(&form.values).enumerate().skip(first).take(rows) {
        let focused = i == form.field;
        let marker = if focused { Span::styled(" › ", bold(t.accent)) } else { Span::raw("   ") };
        let mut ty = c.data_type.to_lowercase();
        if c.primary_key {
            ty.push_str(" PK");
        } else if !c.nullable {
            ty.push_str(" !");
        }
        let name = Span::styled(format!("{:<label_w$} ", truncate(&c.name, label_w)), bold(t.text));
        let ty = Span::styled(format!("{:<type_w$} ", truncate(&ty, type_w)), dim());
        let value = if focused {
            let shown: String =
                v.chars().rev().take(value_w.saturating_sub(1)).collect::<Vec<_>>().into_iter().rev().collect();
            Span::styled(format!("{shown}█"), fg(t.text_bright).bg(t.active))
        } else if v.is_empty() {
            Span::styled(t!("row_form.default"), dim())
        } else {
            Span::styled(truncate(v, value_w), fg(t.text))
        };
        lines.push(Line::from(vec![marker, name, ty, value]));
    }
    lines.push(Line::default());
    lines.push(Line::from(Span::styled(format!(" {}", t!("row_form.help")), dim())));
    lines.push(Line::from(Span::styled(format!(" {}", t!("row_form.keys")), dim())));
    let height = fit(lines.len() as u16 + 2, 6, area.height);
    let rect = centered_box(area, width, height);
    f.render_widget(Clear, rect);
    f.render_widget(
        Paragraph::new(lines).block(popup(&format!("{} · {}", t!("row_form.title"), form.table), t.accent)),
        rect,
    );
}

/// `| command`: what to pipe the result rows through.
fn draw_pipe_prompt(f: &mut Frame, command: &LineInput, area: Rect) {
    let t = theme();
    let rect = centered_box(area, fit(72, 30, area.width), 5);
    f.render_widget(Clear, rect);
    let lines = vec![
        Line::from([vec![Span::styled(" | ", bold(t.accent))], input_spans(command, fg(t.text_bright))].concat()),
        Line::default(),
        Line::from(Span::styled(format!(" {}", t!("pipe.help")), dim())),
    ];
    f.render_widget(Paragraph::new(lines).block(popup(t!("pipe.title"), t.accent)), rect);
}

/// `Ctrl+K q`: the question for a model, about the tab's connection.
fn draw_ask_prompt(f: &mut Frame, app: &App, area: Rect) {
    let t = theme();
    let Some(question) = &app.popups.ask_prompt else { return };
    let rect = centered_box(area, fit(80, 30, area.width), 6);
    f.render_widget(Clear, rect);
    let lines = vec![
        Line::from([vec![Span::styled(" ? ", bold(t.accent))], input_spans(question, fg(t.text_bright))].concat()),
        Line::default(),
        Line::from(Span::styled(format!(" {}", t!("ask.help_1")), dim())),
        Line::from(Span::styled(format!(" {}", t!("ask.help_2")), dim())),
    ];
    let title = t!("ask.title", name = app.connection.form.name.to_uppercase());
    f.render_widget(Paragraph::new(lines).block(popup(&title, t.accent)), rect);
}

/// `Ctrl+K y k`: the project's memory, a fact a line; a model's
/// suggestions, waiting to be confirmed, in orange.
fn draw_memory(f: &mut Frame, app: &App, area: Rect) {
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

/// The save-as / rename prompt.
fn draw_tab_prompt(f: &mut Frame, app: &App, area: Rect) {
    let t = theme();
    let Some(prompt) = &app.popups.tab_prompt else { return };
    let title = match prompt.kind {
        TabPromptKind::SaveAs => t!("tab_prompt.save"),
        TabPromptKind::SaveStatement(_) => t!("tab_prompt.save_statement"),
        TabPromptKind::Rename | TabPromptKind::RenameSaved(_) => t!("tab_prompt.rename"),
    };
    let rect = centered_box(area, fit(64, 24, area.width), 5);
    f.render_widget(Clear, rect);
    let lines = vec![
        Line::from([vec![Span::styled(" › ", bold(t.accent))], input_spans(&prompt.text, fg(t.text_bright))].concat()),
        Line::default(),
        Line::from(Span::styled(format!(" {}", t!("tab_prompt.help")), dim())),
    ];
    f.render_widget(Paragraph::new(lines).block(popup(title, t.accent)), rect);
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
fn draw_which_key(f: &mut Frame, app: &App, area: Rect) {
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
    let group_open = app.popups.palette.as_ref().and_then(crate::app::palette::Palette::group);
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

fn draw_palette(f: &mut Frame, app: &App, area: Rect) {
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

/// Ctrl+K x: auto-commit, manual commit or rollback-only.
fn draw_tx_picker(f: &mut Frame, app: &App, area: Rect) {
    let t = theme();
    let Some(highlighted) = app.popups.tx_picker else { return };
    let mut lines: Vec<Line> = TxMode::ALL
        .iter()
        .map(|&mode| {
            let row = if mode == highlighted { Style::default().bg(t.active) } else { Style::default() };
            let current = if mode == app.connection.tx_mode { "●" } else { " " };
            Line::from(vec![
                Span::styled(format!(" {} ", mode.letter()), bold(t.on_accent).bg(t.accent)),
                Span::styled(format!(" {current} {:<14} ", mode.label()), bold(t.text_bright).patch(row)),
                Span::styled(format!("{} ", mode.explain()), muted().patch(row)),
            ])
        })
        .collect();
    lines.push(Line::default());
    lines.push(Line::from(Span::styled(format!(" {}", t!("tx_picker.help")), dim())));
    let width = fit(lines.iter().map(Line::width).max().unwrap_or(0) as u16 + 2, 30, area.width);
    let rect = centered_box(area, width, lines.len() as u16 + 2);
    f.render_widget(Clear, rect);
    f.render_widget(Paragraph::new(lines).block(popup(t!("tx_picker.title"), t.accent)), rect);
}

/// The sandbox: its steps, what they changed per table, and how to end it.
fn draw_sandbox(f: &mut Frame, app: &App, area: Rect) {
    let t = theme();
    let (Some(view), Some(sandbox)) = (&app.popups.sandbox_view, &app.connection.sandbox) else { return };
    let width = fit(area.width.saturating_sub(8).min(100), 40, area.width);
    let w = usize::from(width.saturating_sub(4));
    let mut lines = vec![Line::from(Span::styled(format!(" {}", t!("sandbox.steps")), bold(t.accent)))];
    for (i, step) in sandbox.steps.iter().enumerate() {
        let rows = tn!("sandbox.rows", step.rows);
        let head = format!(" {:>2}. ", i + 1);
        let sql = truncate(&one_line(&step.sql), w.saturating_sub(head.len() + rows.chars().count() + 3));
        lines.push(Line::from(vec![
            Span::styled(head, bold(t.text)),
            Span::styled(sql, fg(t.text)),
            Span::styled(format!("  {rows}"), dim()),
        ]));
    }
    lines.push(Line::default());
    lines.push(Line::from(Span::styled(format!(" {}", t!("sandbox.summary")), bold(t.accent))));
    for (table, rows, ddl) in sandbox.summary() {
        let what = if ddl { t!("sandbox.schema_changed").to_string() } else { tn!("sandbox.rows", rows) };
        lines.push(Line::from(vec![Span::styled(format!(" {table}  "), bold(t.text)), Span::styled(what, muted())]));
    }
    let age = sandbox.started.map_or(0, |s| s.elapsed().as_secs());
    lines.push(Line::default());
    lines.push(Line::from(Span::styled(
        format!(" {}", t!("sandbox.open_for", time = format!("{}:{:02}", age / 60, age % 60))),
        dim(),
    )));
    if let Some(locks) = &view.locks {
        lines.push(Line::from(Span::styled(format!(" {locks}"), dim())));
    }
    let key = |k: &str, what: &str, bg| {
        [Span::styled(format!(" {k} "), bold(t.on_accent).bg(bg)), Span::styled(format!(" {what}  "), fg(t.text))]
    };
    lines.push(Line::from(
        [
            key("u", t!("sandbox.undo"), t.accent),
            key("d", t!("sandbox.discard"), t.error),
            key("s", t!("sandbox.keep_script"), t.accent),
            key("r", t!("sandbox.run_for_real"), t.ok),
        ]
        .concat(),
    ));
    let height = (lines.len() as u16 + 2).min(area.height);
    let rect = centered_box(area, width, height);
    f.render_widget(Clear, rect);
    let block = popup(t!("sandbox.title"), t.warn)
        .title_bottom(Line::from(Span::styled(format!(" {} ", t!("sandbox.esc")), dim())).right_aligned());
    f.render_widget(Paragraph::new(lines).scroll((view.scroll, 0)).block(block), rect);
}

/// After writes ran in a transaction: commit or roll back?
fn draw_review(f: &mut Frame, app: &App, area: Rect) {
    let t = theme();
    let Some(review) = &app.popups.review else { return };
    let rows = review.rows_affected;
    let lines = vec![
        Line::from(Span::styled(tn!("review.changed", rows), bold(t.warn))),
        Line::default(),
        Line::from(vec![
            Span::styled(" c ", bold(t.on_accent).bg(t.ok)),
            Span::styled(format!(" {}   ", t!("review.commit")), fg(t.text)),
            Span::styled(" r ", bold(t.on_accent).bg(t.error)),
            Span::styled(format!(" {}   ", t!("review.rollback")), fg(t.text)),
            Span::styled(" Esc ", bold(t.text).bg(t.active)),
            Span::styled(format!(" {}", t!("review.keep_open")), muted()),
        ]),
    ];
    let width = fit(lines.iter().map(Line::width).max().unwrap_or(0) as u16 + 4, 40, area.width.saturating_sub(4));
    let rect = centered_box(area, width, 5);
    f.render_widget(Clear, rect);
    f.render_widget(Paragraph::new(lines).block(popup(t!("review.title"), t.warn)), rect);
}

/// The query parameters form: one line per placeholder.
fn draw_param_form(f: &mut Frame, app: &App, area: Rect) {
    let t = theme();
    let Some(form) = &app.popups.param_form else { return };
    let label_w = form.params.iter().map(|p| p.name.chars().count()).max().unwrap_or(0).min(24);
    let width = fit(72, 30, area.width);
    let value_w = (width as usize).saturating_sub(label_w + 8);
    // Keep the focused field visible when there are many.
    let rows = (area.height.saturating_sub(7) as usize).max(1);
    let first = form.field.saturating_sub(rows - 1);
    let mut lines = Vec::new();
    for (i, (p, v)) in form.params.iter().zip(&form.values).enumerate().skip(first).take(rows) {
        let focused = i == form.field;
        let marker = if focused { Span::styled(" › ", bold(t.accent)) } else { Span::raw("   ") };
        let name = Span::styled(format!("{:<label_w$}  ", truncate(&p.name, label_w)), bold(t.text));
        let value = if focused {
            // Show the end of a long value, where the typing happens.
            let shown: String =
                v.chars().rev().take(value_w.saturating_sub(1)).collect::<Vec<_>>().into_iter().rev().collect();
            Span::styled(format!("{shown}█"), fg(t.text_bright).bg(t.active))
        } else {
            Span::styled(truncate(v, value_w), fg(t.text))
        };
        lines.push(Line::from(vec![marker, name, value]));
    }
    lines.push(Line::default());
    lines.push(Line::from(Span::styled(format!(" {}", t!("params.help")), dim())));
    lines.push(Line::from(Span::styled(format!(" {}", t!("params.keys")), dim())));
    let height = fit(lines.len() as u16 + 2, 5, area.height);
    let rect = centered_box(area, width, height);
    f.render_widget(Clear, rect);
    f.render_widget(Paragraph::new(lines).block(popup(t!("params.title"), t.accent)), rect);
}

/// The text view's keys (a cell, a row with `x`, a definition…), most
/// important first; closing it last.
fn text_view_keys(view: &crate::app::CellView) -> Vec<&'static str> {
    if view.record {
        vec![t!("cell_view.scroll_next_row"), t!("cell_view.row"), t!("cell_view.copy_row"), t!("cell_view.close")]
    } else if view.browse {
        vec![t!("cell_view.prev_next_table"), t!("cell_view.scroll"), t!("cell_view.copy"), t!("cell_view.close")]
    } else if view.follow.is_some() {
        vec![t!("cell_view.open_tab"), t!("cell_view.copy"), t!("cell_view.scroll"), t!("cell_view.close")]
    } else if view.sql.is_some() {
        vec![t!("cell_view.scroll"), t!("cell_view.copy_sql"), t!("cell_view.open_tab_there"), t!("cell_view.close")]
    } else {
        vec![t!("cell_view.scroll"), t!("cell_view.copy"), t!("cell_view.close")]
    }
}

/// As many of `keys` as fit in `width`, joined by ` · `: the first ones,
/// and always the last (how to close).
fn fitting_hints(keys: &[&str], width: usize) -> String {
    let Some((last, rest)) = keys.split_last() else { return String::new() };
    let mut shown: Vec<&str> = Vec::new();
    let mut used = 1 + last.chars().count();
    for key in rest {
        let w = key.chars().count() + 3;
        if used + w > width {
            break;
        }
        used += w;
        shown.push(key);
    }
    shown.push(last);
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
        super::results::wrap_words(d, room).into_iter().enumerate().map(|(i, part)| {
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
