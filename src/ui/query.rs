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
    let show_schema = app.schema_visible && body.width >= SCHEMA_MIN_TOTAL_WIDTH;
    app.schema_shown = show_schema;
    if !show_schema && app.focus == Focus::Schema {
        app.focus = Focus::Editor;
    }
    let (schema_area, main) = if show_schema {
        let [s, m] = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(schema_width(app.schema_width, body.width)), Constraint::Min(20)])
            .areas(body);
        (Some(s), m)
    } else {
        (None, body)
    };
    let [editor_area, results_area] = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(app.editor_percent), Constraint::Min(5)])
        .areas(main);

    // Zoomed: only the focused pane, using the whole body.
    let cursor = if app.zoomed {
        // The top bar's project, drawn already, stays clickable.
        app.hits = crate::app::Hits { project: app.hits.project, ..crate::app::Hits::default() };
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
        let cursor = draw_editor(f, app, editor_area);
        draw_results(f, app, results_area);
        cursor
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
            Overlay::TabPrompt => draw_tab_prompt(f, app, area),
            Overlay::ParamForm => draw_param_form(f, app, area),
            Overlay::Review => draw_review(f, app, area),
            Overlay::TxPicker => draw_tx_picker(f, app, area),
            Overlay::Activity => draw_activity(f, app, area),
            Overlay::Agents => super::agents::draw_agents(f, app, area),
            Overlay::Palette => draw_palette(f, app, area),
            Overlay::RowForm => draw_row_form(f, app, area),
            Overlay::PipePrompt => {
                let command = app.pipe_prompt.clone().unwrap_or_default();
                draw_pipe_prompt(f, &command, area);
            }
            Overlay::Menu => draw_menu(f, app, area),
            Overlay::Confirm => draw_confirm(f, app, area),
        }
    }
}

/// A pop-up menu: what can be done here, each with its key.
fn draw_menu(f: &mut Frame, app: &mut App, area: Rect) {
    let t = theme();
    let Some(menu) = &app.menu else { return };
    let label_w = menu.items.iter().map(|i| i.label.chars().count()).max().unwrap_or(0);
    let keys_w = menu.items.iter().map(|i| i.keys.chars().count()).max().unwrap_or(0);
    let width = ((label_w + keys_w + 6) as u16).min(area.width);
    let height = (menu.items.len() as u16 + 3).min(area.height);
    // Open at the pointer, kept on screen.
    let x = menu.at.0.min(area.right().saturating_sub(width));
    let y = menu.at.1.min(area.bottom().saturating_sub(height));
    let rect = Rect { x, y, width, height };
    let lines: Vec<Line> = menu
        .items
        .iter()
        .enumerate()
        .map(|(i, item)| {
            let style = if i == menu.selected { bold(t.text_bright).bg(t.active) } else { fg(t.text) };
            let pad = label_w.saturating_sub(item.label.chars().count()) + 2;
            Line::from(vec![
                Span::styled(format!(" {}{}", item.label, " ".repeat(pad)), style),
                Span::styled(
                    format!("{:>keys_w$} ", item.keys),
                    if i == menu.selected { muted().bg(t.active) } else { dim() },
                ),
            ])
        })
        .collect();
    let block = popup(menu.title, t.accent)
        .title_bottom(Line::from(Span::styled(" ↵ choose · Esc close ", dim())).right_aligned());
    f.render_widget(Clear, rect);
    f.render_widget(Paragraph::new(lines).block(block), rect);
    app.hits.menu = rect;
}

/// The F3 window: search box, then saved queries (★) and history (↺).
fn draw_library(f: &mut Frame, app: &App, area: Rect) {
    let t = theme();
    let Some(view) = &app.library_view else { return };
    let rect = centered_box(area, fit(area.width * 4 / 5, 30, area.width), fit(area.height * 4 / 5, 8, area.height));
    f.render_widget(Clear, rect);
    let block = popup("QUERY LIBRARY", t.accent);
    let inner = block.inner(rect);
    f.render_widget(block, rect);
    let [search, list, hints] = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(0), Constraint::Length(1)])
        .areas(inner);

    let items = app.library_items();
    let saved = items.iter().filter(|i| matches!(i, LibraryItem::Saved(_))).count();
    f.render_widget(
        Paragraph::new(Line::from(
            [
                vec![Span::styled(" / ", bold(t.accent))],
                input_spans(&view.query, fg(t.text_bright)),
                vec![Span::styled(format!("   {saved} saved · {} from history", items.len() - saved), dim())],
            ]
            .concat(),
        ))
        .style(Style::default().bg(t.inset)),
        search,
    );

    let width = list.width as usize;
    let rows: Vec<ListItem> = items
        .iter()
        .map(|item| {
            let (icon, icon_style, group, name, sql) = match item {
                LibraryItem::Saved(id) => {
                    let Some(q) = app.library.get(id) else { return ListItem::new("") };
                    let group = if q.group.is_empty() { String::new() } else { format!("{}/", q.group) };
                    ("★ ", fg(t.warn), group, q.name.clone(), q.sql.clone())
                }
                LibraryItem::History(sql) => ("↺ ", dim(), String::new(), String::new(), sql.clone()),
            };
            let preview = one_line(&sql);
            let head_len = 2 + group.chars().count() + name.chars().count() + 2;
            let preview = truncate(&preview, width.saturating_sub(head_len + 1));
            ListItem::new(Line::from(vec![
                Span::styled(icon, icon_style),
                Span::styled(group, muted()),
                Span::styled(name.clone(), bold(t.text_bright)),
                Span::raw(if name.is_empty() { "" } else { "  " }),
                Span::styled(preview, if name.is_empty() { fg(t.text) } else { dim() }),
            ]))
        })
        .collect();
    if rows.is_empty() {
        f.render_widget(Paragraph::new(" Nothing matches.").style(dim()), list);
    } else {
        let mut state = ListState::default().with_selected(Some(view.selected.min(rows.len() - 1)));
        f.render_stateful_widget(List::new(rows).highlight_style(Style::default().bg(t.active)), list, &mut state);
    }

    let hint = if view.confirm_delete {
        Line::from(Span::styled(" Delete this saved query? y/n", bold(t.warn)))
    } else {
        Line::from(Span::styled(
            " type to search name, group and SQL · ↵ open · F4 rename/move · ^D delete · Esc close",
            dim(),
        ))
    };
    f.render_widget(Paragraph::new(hint).style(Style::default().bg(t.inset)), hints);
}

/// The activity monitor: one line per session on the server.
fn draw_activity(f: &mut Frame, app: &App, area: Rect) {
    let t = theme();
    let Some(view) = &app.activity else { return };
    let rect = centered_box(area, area.width.saturating_sub(6).max(40), area.height.saturating_sub(4).max(8));
    f.render_widget(Clear, rect);
    let title = format!("ACTIVITY · {} session(s)", view.rows.len());
    let keys = " ↑↓ select · ↵ full query · c cancel query · x end session · r refresh · Esc close ";
    let block = popup(&title, t.accent).title_bottom(Line::from(Span::styled(keys, dim())).right_aligned());
    let inner = block.inner(rect);
    f.render_widget(block, rect);
    let w = inner.width as usize;
    let mut lines = vec![Line::from(Span::styled(
        truncate(
            &format!(
                "{:<8} {:<12} {:<12} {:<20} {:>7} {:<18} QUERY",
                "ID", "USER", "DATABASE", "STATE", "TIME", "WAITING ON"
            ),
            w,
        ),
        bold(t.muted),
    ))];
    if let Some(e) = &view.error {
        lines.push(Line::from(Span::styled(e.clone(), fg(t.error))));
    }
    if let Some((id, kill)) = &view.confirm {
        let what = if *kill { "End session" } else { "Cancel the running query of session" };
        lines.push(Line::from(vec![
            Span::styled(format!("{what} {id}? "), bold(t.error)),
            Span::styled(" y ", bold(t.on_accent).bg(t.error)),
            Span::styled(" yes   any other key: no", muted()),
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
        let waiting =
            if a.blocked_by.is_empty() { a.wait.clone() } else { format!("blocked by {}", a.blocked_by.join(",")) };
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
        lines.push(Line::from(Span::styled("* this tab's session", dim())));
    }
    f.render_widget(Paragraph::new(lines), inner);
}

/// The add-row form: one line per column, with its type.
fn draw_row_form(f: &mut Frame, app: &App, area: Rect) {
    let t = theme();
    let Some(form) = &app.row_form else { return };
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
            Span::styled("default", dim())
        } else {
            Span::styled(truncate(v, value_w), fg(t.text))
        };
        lines.push(Line::from(vec![marker, name, ty, value]));
    }
    lines.push(Line::default());
    lines.push(Line::from(Span::styled(
        " empty = default · NULL · numbers as-is · =expr raw SQL · ! = required",
        dim(),
    )));
    lines.push(Line::from(Span::styled(" ↑↓ field · ↵ add the row · Esc cancel", dim())));
    let height = fit(lines.len() as u16 + 2, 6, area.height);
    let rect = centered_box(area, width, height);
    f.render_widget(Clear, rect);
    f.render_widget(Paragraph::new(lines).block(popup(&format!("ADD ROW · {}", form.table), t.accent)), rect);
}

/// `| command`: what to pipe the result rows through.
fn draw_pipe_prompt(f: &mut Frame, command: &LineInput, area: Rect) {
    let t = theme();
    let rect = centered_box(area, fit(72, 30, area.width), 5);
    f.render_widget(Clear, rect);
    let lines = vec![
        Line::from([vec![Span::styled(" | ", bold(t.accent))], input_spans(command, fg(t.text_bright))].concat()),
        Line::default(),
        Line::from(Span::styled(" rows go in as TSV with a header (JSON for jq) · ↵ run · Esc cancel", dim())),
    ];
    f.render_widget(Paragraph::new(lines).block(popup("PIPE ROWS THROUGH", t.accent)), rect);
}

/// The save-as / rename prompt.
fn draw_tab_prompt(f: &mut Frame, app: &App, area: Rect) {
    let t = theme();
    let Some(prompt) = &app.tab_prompt else { return };
    let title = match prompt.kind {
        TabPromptKind::SaveAs => "SAVE QUERY",
        TabPromptKind::Rename | TabPromptKind::RenameSaved(_) => "RENAME",
    };
    let rect = centered_box(area, fit(64, 24, area.width), 5);
    f.render_widget(Clear, rect);
    let lines = vec![
        Line::from([vec![Span::styled(" › ", bold(t.accent))], input_spans(&prompt.text, fg(t.text_bright))].concat()),
        Line::default(),
        Line::from(Span::styled(" group/name, e.g. reports/churn by month · ↵ ok · Esc cancel", dim())),
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

/// Whether `items` fit in `two_columns` of `width` without cutting any.
fn fits_in_columns(items: &[(String, String)], width: usize) -> bool {
    let key_w = items.iter().map(|(k, _)| k.chars().count()).max().unwrap_or(0);
    key_w <= width / 2 && items.iter().all(|(_, what)| what.chars().count() <= width.saturating_sub(key_w + 3))
}

/// One key and what it does per line, across `width`.
fn one_column(items: &[(String, String)], width: usize, key_style: Style) -> Vec<Line<'static>> {
    let key_w = items.iter().map(|(k, _)| k.chars().count()).max().unwrap_or(0).min(width / 2);
    let what_w = width.saturating_sub(key_w + 3);
    items
        .iter()
        .map(|(key, what)| {
            Line::from(vec![
                Span::styled(format!(" {:<key_w$} ", truncate(key, key_w)), key_style),
                Span::styled(truncate(what, what_w), fg(theme().text)),
            ])
        })
        .collect()
}

/// Ctrl+K's menu: one-letter actions, then the focused pane's own keys.
fn draw_which_key(f: &mut Frame, app: &App, area: Rect) {
    let t = theme();
    let width = fit(84, 30, area.width);
    let col = (width.saturating_sub(2) / 2) as usize;
    // Writes and transactions first, then the query, then the rest.
    const ORDER: &str = "xcreaf-inzmltodpush";
    let mut keys = app.keymap.leader_keys();
    // The editor's own (explain, format…) only from the editor; they still
    // work elsewhere, they're just not what's at hand there.
    keys.retain(|(_, a)| app.focus == Focus::Editor || !a.is_editor_action());
    // The navigator doesn't write: no write modes there.
    keys.retain(|(_, a)| app.focus != Focus::Schema || *a != crate::keys::Action::TxMode);
    // Commit and roll back only with a transaction to end.
    keys.retain(|(_, a)| {
        app.txn != TxnState::Idle || !matches!(a, crate::keys::Action::Commit | crate::keys::Action::Rollback)
    });
    keys.sort_by_key(|(c, _)| ORDER.find(*c).unwrap_or(ORDER.len()));
    let mut leader: Vec<(String, String)> =
        keys.into_iter().map(|(c, a)| (c.to_string(), a.brief().to_string())).collect();
    leader.push(("?".into(), "all keys".into()));
    let (pane, pane_keys) = chrome::pane_keys(app);
    // The open tabs by number: Ctrl+K 2 goes to the second.
    let tabs: Vec<(String, String)> = app
        .tabs
        .iter()
        .enumerate()
        .take(9)
        .map(|(i, tab)| {
            let dirty = if app.tab_dirty(i) { " ●" } else { "" };
            let here = if i == app.active_tab { " (this one)" } else { "" };
            ((i + 1).to_string(), format!("{}{dirty}{here}", tab.name))
        })
        .collect();

    let mut lines = vec![Line::from(Span::styled(" THEN PRESS", bold(t.accent)))];
    lines.extend(two_columns(&leader, col, bold(t.accent)));
    lines.push(Line::default());
    if app.tabs.len() > 1 {
        let more = app.tabs.len().saturating_sub(9);
        let title = if more > 0 { format!(" TABS (+{more} more: type to search)") } else { " TABS".to_string() };
        lines.push(Line::from(Span::styled(title, bold(t.accent))));
        lines.extend(two_columns(&tabs, col, bold(t.accent)));
        lines.push(Line::default());
    }
    // The letters above take the keys while this menu is open: these are
    // for the pane itself.
    lines.push(Line::from(vec![
        Span::styled(format!(" IN THE {}", pane.to_uppercase()), bold(t.accent)),
        Span::styled("  (close this menu, then press)", dim()),
    ]));
    // Two columns if nothing gets cut, else one, if it fits on the screen.
    let rows_left = usize::from(area.height).saturating_sub(lines.len() + 6);
    if fits_in_columns(&pane_keys, col) || pane_keys.len() > rows_left {
        lines.extend(two_columns(&pane_keys, col, bold(t.text)));
    } else {
        lines.extend(one_column(&pane_keys, usize::from(width.saturating_sub(2)), bold(t.text)));
    }
    lines.push(Line::default());
    lines.push(Line::from(Span::styled(" Type to search every command · F1 all keys · Esc closes", dim())));

    let height = (lines.len() as u16 + 2).min(area.height);
    let rect = Rect { y: area.y + area.height.saturating_sub(height).min(2), ..centered_box(area, width, height) };
    f.render_widget(Clear, rect);
    let title = app.keymap.label(crate::keys::Action::Palette).split(" · ").next().unwrap_or_default().to_uppercase();
    f.render_widget(Paragraph::new(lines).block(popup(&title, t.accent)), rect);
}

fn draw_palette(f: &mut Frame, app: &App, area: Rect) {
    let t = theme();
    let Some(palette) = &app.palette else { return };
    if !palette.searching {
        // Until the menu is due, only the status bar says Ctrl+K is waiting.
        if palette.menu {
            draw_which_key(f, app, area);
        }
        return;
    }
    let items = app.palette_items();
    let width = fit(76, 30, area.width);
    let rows = (area.height.saturating_sub(6) as usize).clamp(1, 14).min(items.len().max(1));
    let rect = Rect { y: area.y + 2, ..centered_box(area, width, (rows as u16 + 4).min(area.height)) };
    f.render_widget(Clear, rect);
    let block = popup("COMMANDS", t.accent);
    let inner = block.inner(rect);
    f.render_widget(block, rect);
    let [search, list] = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(2), Constraint::Min(0)])
        .areas(inner);
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
    let lines = if lines.is_empty() { vec![Line::from(Span::styled(" No matching command", dim()))] } else { lines };
    f.render_widget(Paragraph::new(lines), list);
}

/// Ctrl+K x: auto-commit, manual commit or rollback-only.
fn draw_tx_picker(f: &mut Frame, app: &App, area: Rect) {
    let t = theme();
    let Some(highlighted) = app.tx_picker else { return };
    let mut lines: Vec<Line> = TxMode::ALL
        .iter()
        .map(|&mode| {
            let row = if mode == highlighted { Style::default().bg(t.active) } else { Style::default() };
            let current = if mode == app.tx_mode { "●" } else { " " };
            Line::from(vec![
                Span::styled(format!(" {} ", mode.letter()), bold(t.on_accent).bg(t.accent)),
                Span::styled(format!(" {current} {:<14} ", mode.label()), bold(t.text_bright).patch(row)),
                Span::styled(format!("{} ", mode.explain()), muted().patch(row)),
            ])
        })
        .collect();
    lines.push(Line::default());
    lines.push(Line::from(Span::styled(" ● current · Esc keeps it", dim())));
    let width = fit(lines.iter().map(Line::width).max().unwrap_or(0) as u16 + 2, 30, area.width);
    let rect = centered_box(area, width, lines.len() as u16 + 2);
    f.render_widget(Clear, rect);
    f.render_widget(Paragraph::new(lines).block(popup("TRANSACTION MODE", t.accent)), rect);
}

/// After writes ran in a transaction: commit or roll back?
fn draw_review(f: &mut Frame, app: &App, area: Rect) {
    let t = theme();
    let Some(review) = &app.review else { return };
    let rows = review.rows_affected;
    let lines = vec![
        Line::from(Span::styled(
            format!("{rows} row{} changed, not committed yet", if rows == 1 { "" } else { "s" }),
            bold(t.warn),
        )),
        Line::default(),
        Line::from(vec![
            Span::styled(" c ", bold(t.on_accent).bg(t.ok)),
            Span::styled(" commit   ", fg(t.text)),
            Span::styled(" r ", bold(t.on_accent).bg(t.error)),
            Span::styled(" roll back   ", fg(t.text)),
            Span::styled(" Esc ", bold(t.text).bg(t.active)),
            Span::styled(" keep it open to check", muted()),
        ]),
    ];
    let width = fit(lines.iter().map(Line::width).max().unwrap_or(0) as u16 + 4, 40, area.width.saturating_sub(4));
    let rect = centered_box(area, width, 5);
    f.render_widget(Clear, rect);
    f.render_widget(Paragraph::new(lines).block(popup("REVIEW", t.warn)), rect);
}

/// The query parameters form: one line per placeholder.
fn draw_param_form(f: &mut Frame, app: &App, area: Rect) {
    let t = theme();
    let Some(form) = &app.param_form else { return };
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
    lines.push(Line::from(Span::styled(" numbers and NULL go in as-is, text is quoted, =expr is raw SQL", dim())));
    lines.push(Line::from(Span::styled(" ↑↓ field · ↵ run · Esc cancel", dim())));
    let height = fit(lines.len() as u16 + 2, 5, area.height);
    let rect = centered_box(area, width, height);
    f.render_widget(Clear, rect);
    f.render_widget(Paragraph::new(lines).block(popup("QUERY PARAMETERS", t.accent)), rect);
}

/// The text view's keys (a cell, a row with `x`, a definition…), most
/// important first; closing it last.
fn text_view_keys(view: &crate::app::CellView) -> &'static [&'static str] {
    if view.record {
        &["↑↓ scroll, next row", "←→ row", "y copy row", "Esc close"]
    } else if view.browse {
        &["←→ previous / next table", "↑↓ scroll", "y copy", "Esc close"]
    } else if view.sql.is_some() {
        &["↑↓ scroll", "y copy SQL", "e open it in a tab there", "Esc close"]
    } else {
        &["↑↓ scroll", "y copy", "Esc close"]
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
    let navigator = app.hits.schema.filter(|_| app.schema_shown && !app.zoomed && app.screen == Screen::Query);
    let Some(view) = &mut app.cell_view else { return };
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
    let hint = fitting_hints(text_view_keys(view), usize::from(keys.width));
    f.render_widget(Paragraph::new(Span::styled(hint, dim())).style(Style::default().bg(t.inset)), keys);
}

pub(super) fn draw_confirm(f: &mut Frame, app: &App, area: Rect) {
    let t = theme();
    let Some(confirm) = &app.confirm else { return };
    let color = if confirm.danger { t.error } else { t.accent };
    let mut lines = vec![Line::from(Span::styled(confirm.message.clone(), bold(color))), Line::default()];
    lines.extend(confirm.details.iter().map(|d| Line::from(Span::styled(format!("  • {d}"), fg(t.text)))));
    if !confirm.details.is_empty() {
        lines.push(Line::default());
    }
    let mut keys = Vec::new();
    if confirm.offer_transaction {
        keys.push(Span::styled(" t ", bold(t.on_accent).bg(t.ok)));
        keys.push(Span::styled(" in a transaction, review first   ", fg(t.text)));
    }
    lines.push(Line::from(vec![
        Span::styled(" y ", bold(t.on_accent).bg(color)),
        Span::styled(" yes   ", fg(t.text)),
        Span::styled(" any other key ", bold(t.text).bg(t.active)),
        Span::styled(" cancel", muted()),
    ]));
    if !keys.is_empty() {
        lines.push(Line::from(keys));
    }
    let width = fit(lines.iter().map(Line::width).max().unwrap_or(0) as u16 + 4, 40, area.width.saturating_sub(4));
    let height = (lines.len() as u16 + 2).min(area.height);
    let rect = centered_box(area, width, height);
    f.render_widget(Clear, rect);
    let title = if confirm.danger { "CONFIRM" } else { "QUESTION" };
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }).block(popup(title, color)), rect);
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
