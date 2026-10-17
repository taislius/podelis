//! `QUERY RESULT`: tabs (rows / messages / plan), the table with a
//! row-number column and underlined headers, and a footer.

use super::*;

/// How a column's values are coloured and aligned, from its type name.
#[derive(Clone, Copy, PartialEq, Eq)]
enum TypeClass {
    Number,
    Id,
    Time,
    Bool,
    Json,
    Text,
}

fn type_class(ty: &str) -> TypeClass {
    let ty = ty.to_ascii_uppercase();
    let has = |s: &str| ty.contains(s);
    if has("UUID") || ty == "OID" {
        TypeClass::Id
    } else if has("INT")
        || has("NUMERIC")
        || has("DECIMAL")
        || has("FLOAT")
        || has("DOUBLE")
        || has("REAL")
        || has("MONEY")
        || ty == "SERIAL"
        || ty == "BIGSERIAL"
    {
        TypeClass::Number
    } else if has("TIME") || has("DATE") || has("INTERVAL") || ty == "YEAR" {
        TypeClass::Time
    } else if has("BOOL") || ty == "BIT" {
        TypeClass::Bool
    } else if has("JSON") {
        TypeClass::Json
    } else {
        TypeClass::Text
    }
}

fn class_style(class: TypeClass) -> Style {
    let t = theme();
    match class {
        TypeClass::Number => bold(t.ok),
        TypeClass::Id => fg(t.ident),
        TypeClass::Time => muted(),
        TypeClass::Bool => fg(t.number),
        TypeClass::Json => fg(t.keyword),
        TypeClass::Text => fg(t.text),
    }
}

pub(super) fn draw_results(f: &mut Frame, app: &mut App, area: Rect) {
    let t = theme();
    let focused = app.focus == Focus::Results;
    // How long it ran is the result's, in its footer.
    let info = app.running_label().unwrap_or_default();
    let info = if app.panes.zoomed { format!("ZOOMED · {info}") } else { info };
    let block = pane("Query result", &info, focused);
    app.hits.results = area;
    app.hits.result_rows = Rect::default();
    app.hits.plan_source = Rect::default();
    app.hits.run_source = Rect::default();
    app.hits.run_tabs.clear();
    app.hits.messages = Rect::default();
    let inner = block.inner(area);
    f.render_widget(block, area);
    let [tabs, body, footer] = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(0), Constraint::Length(1)])
        .areas(inner);

    draw_tabs(f, app, tabs);
    f.render_widget(Block::default().style(Style::default().bg(t.inset)), footer);
    match app.tab.result_tab {
        ResultTab::Messages => {
            let widest = draw_messages(f, app, body);
            draw_across_footer(f, app, footer, widest, body.width);
        }
        // A plan that's a table: what it's for, its notes, then the grid
        // with the results' keys.
        ResultTab::Plan if app.tab.plan_swapped => {
            let body = draw_plan_source(f, app, body);
            let notes = plan_note_lines(app, body.width);
            let height = (notes.len() as u16).min(body.height / 2);
            let [top, rest] = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Length(height), Constraint::Min(0)])
                .areas(body);
            f.render_widget(Paragraph::new(notes), top);
            draw_rows(f, app, rest, footer);
        }
        ResultTab::Plan => {
            let body = draw_plan_source(f, app, body);
            let widest = draw_plan(f, app, body);
            draw_across_footer(f, app, footer, widest, body.width);
        }
        ResultTab::Rows => {
            let body = draw_run_source(f, app, body);
            draw_rows(f, app, body, footer);
        }
    }
    // What a key here did, else the last run's outcome (or progress), until
    // the next key; the cell info comes back after it.
    let run = (app.notes.status_in_results && !app.notes.status.is_empty())
        .then_some((&app.notes.status, app.notes.status_is_error));
    if let Some((message, is_error)) = app.notes.results_note.as_ref().map(|(m, e)| (m, *e)).or(run)
        && app.cell_edit.is_none()
    {
        let style = if is_error { bold(t.error) } else { fg(t.text) };
        // In place of the cell info: none of it may show through.
        f.render_widget(Clear, footer);
        f.render_widget(Block::default().style(Style::default().bg(t.inset)), footer);
        f.render_widget(
            Paragraph::new(Span::styled(format!(" {}", truncate(&one_line(message), footer.width as usize)), style)),
            footer,
        );
    }
}

fn draw_tabs(f: &mut Frame, app: &mut App, area: Rect) {
    let t = theme();
    f.render_widget(Block::default().style(Style::default().bg(t.inset)), area);
    // The result's grid, even while a plan's table is shown in its place.
    let result_grid = if app.tab.plan_swapped { app.tab.plan_grid.as_deref() } else { Some(&app.tab.grid) };
    let rows = result_grid.and_then(|g| g.results.as_ref()).map(|r| {
        if r.columns.is_empty() {
            format!("{} affected", r.rows_affected.unwrap_or(0))
        } else if app.run.stream.is_some() || result_grid.is_some_and(|g| g.cut_short) {
            format!("{}+ rows", r.total_rows)
        } else {
            format!("{} rows", r.total_rows)
        }
    });
    let focused = app.focus == Focus::Results;
    let tab = |label: String, extra: Option<String>, active: bool| -> Vec<Span<'static>> {
        // Quieter than the pane's title: the background marks the active
        // tab, and its label lights up only while the pane has focus.
        let label_style = if active && focused { fg(t.text) } else { muted() };
        let bg = if active { t.tab } else { t.inset };
        let mut spans = vec![Span::styled(format!(" {label}"), label_style.bg(bg))];
        if let Some(extra) = extra {
            spans.push(Span::styled(format!(" {extra}"), fg(if active { t.accent } else { t.dim }).bg(bg)));
        }
        spans.push(Span::styled(" ", Style::default().bg(bg)));
        spans.push(Span::styled("│", fg(t.line)));
        spans
    };
    let mut left = Vec::new();
    let mut tab_hits = Vec::new();
    let width = |spans: &[Span]| spans.iter().map(Span::width).sum::<usize>() as u16;
    let start = area.x;
    // A script's result sets: which one is shown.
    let label = if let Some(g) = result_grid.filter(|g| g.result_sets.len() > 1) {
        format!("RESULT {}/{}", g.result_set + 1, g.result_sets.len())
    } else {
        "RESULT".into()
    };
    left.extend(tab(label, rows, app.tab.result_tab == ResultTab::Rows));
    tab_hits.push((start, area.x + width(&left), ResultTab::Rows));
    let errors = app.notes.log.iter().filter(|m| m.is_error).count();
    let msg_extra = (!app.notes.log.is_empty()).then(|| {
        if errors > 0 { format!("{} · {errors} err", app.notes.log.len()) } else { app.notes.log.len().to_string() }
    });
    let tab_x = area.x + width(&left);
    left.extend(tab("MESSAGES".into(), msg_extra, app.tab.result_tab == ResultTab::Messages));
    tab_hits.push((tab_x, area.x + width(&left), ResultTab::Messages));
    let tab_x = area.x + width(&left);
    left.extend(tab("QUERY PLAN".into(), None, app.tab.result_tab == ResultTab::Plan));
    tab_hits.push((tab_x, area.x + width(&left), ResultTab::Plan));
    app.hits.result_tabs = tab_hits;
    app.hits.result_tabs_y = area.y;

    // The filter is the grid's: shown with it, not over MESSAGES or a text plan.
    let grid_shown = app.tab.result_tab == ResultTab::Rows || app.tab.plan_swapped;
    let right = if !grid_shown {
        Line::default()
    } else if app.tab.grid.filter_editing {
        let mut spans = vec![Span::styled("filter: ", bold(t.accent))];
        spans.extend(input_spans(&app.tab.grid.filter, fg(t.text_bright)));
        spans.push(Span::raw(" "));
        Line::from(spans)
    } else if !app.tab.grid.filter.is_empty() {
        Line::from(vec![Span::styled(format!("filter: {} ", app.tab.grid.filter.text()), bold(t.accent))])
    } else {
        // Key hints are in the status bar, not here.
        Line::default()
    };
    // Menus to click, quiet: what right-click and `m` open, and the
    // exports (with rows to export).
    let rows_shown = grid_shown && app.tab.grid.results.as_ref().is_some_and(|r| !r.columns.is_empty());
    let mut buttons = Vec::new();
    if rows_shown {
        buttons.push((" export ▾ ", true));
    }
    buttons.push((" menu ▾ ", false));
    let buttons_w: u16 = buttons.iter().map(|(label, _)| label.chars().count() as u16).sum();
    let left_w = width(&left);
    let right_w = right.width() as u16;
    // Only with room for them after the tabs (and a filter).
    let buttons_w = if left_w + right_w + buttons_w < area.width { buttons_w } else { 0 };
    let [tabs_area, filter_area, buttons_area] = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(0), Constraint::Length(right_w.min(area.width)), Constraint::Length(buttons_w)])
        .areas(area);
    f.render_widget(Paragraph::new(Line::from(left)), tabs_area);
    f.render_widget(Paragraph::new(right).alignment(Alignment::Right), filter_area);
    app.hits.result_buttons.clear();
    if buttons_w > 0 {
        let mut at = buttons_area.x;
        let spans: Vec<Span> = buttons
            .into_iter()
            .map(|(label, export)| {
                let label_w = label.chars().count() as u16;
                app.hits.result_buttons.push((at, at + label_w, export));
                at += label_w;
                Span::styled(label, muted())
            })
            .collect();
        f.render_widget(Paragraph::new(Line::from(spans)), buttons_area);
    }
}

/// Scrolls `app`'s tab across no further than the widest line allows.
fn clamp_across(app: &mut App, widest: usize, width: u16) -> u16 {
    let most = u16::try_from(widest.saturating_sub(usize::from(width))).unwrap_or(u16::MAX);
    app.tab.tab_hscroll = app.tab.tab_hscroll.min(most);
    app.tab.tab_hscroll
}

/// Under MESSAGES and QUERY PLAN when their lines are wider than the
/// pane: which columns show, and that ←/→ scroll.
fn draw_across_footer(f: &mut Frame, app: &App, footer: Rect, widest: usize, width: u16) {
    let width = usize::from(width);
    if widest <= width {
        return;
    }
    let from = usize::from(app.tab.tab_hscroll);
    let to = (from + width).min(widest);
    let left = if from > 0 { "◀ " } else { "  " };
    let right = if to < widest { " ▶" } else { "  " };
    let text = format!("{left}cols {}–{to} of {widest}{right}  ←/→ scroll ", from + 1);
    f.render_widget(Paragraph::new(Span::styled(text, dim())).alignment(Alignment::Right), footer);
}

/// Where a result would be: that it's running, why it failed, or how
/// to get one.
fn draw_no_result(f: &mut Frame, app: &App, area: Rect) {
    let t = theme();
    let run = app.keymap.short_label(crate::config::keys::Action::Run);
    let text = if let Some(running) = app.running_label() {
        // "running… 12 s", as the pane's title has it.
        let running = running.trim_start_matches('r');
        if app.run.cancel_failed {
            // Orange: it needs the user (see `stop_waiting`).
            Paragraph::new(format!(" R{running} · the server didn't take the cancel · Esc again stops waiting"))
                .style(fg(t.warn))
        } else {
            Paragraph::new(format!(" R{running} · Esc cancels it")).style(fg(t.text))
        }
    } else if let Some(err) = &app.tab.run_error {
        // The link, not the SQL: nothing to fix in the editor.
        let link = err.contains("the server doesn't answer") || err.contains("couldn't reconnect");
        let (said, then) = if link {
            (" The server doesn't answer:", " Check the network or VPN, then run it again: Podėlis reconnects.")
        } else {
            (" The query failed:", " Fix it in the editor and run it again; MESSAGES keeps the error.")
        };
        Paragraph::new(vec![
            Line::from(Span::styled(said, bold(t.error))),
            Line::from(Span::styled(format!(" {err}"), fg(t.error))),
            Line::from(Span::styled(then, dim())),
        ])
        .wrap(Wrap { trim: false })
    } else if !app.connected() {
        Paragraph::new(" Not connected: Ctrl+O picks a connection for this tab.").style(dim())
    } else if app.tab.runs.is_empty() {
        Paragraph::new(format!(" Nothing run in this tab yet: write a query above and press {run}.")).style(dim())
    } else {
        Paragraph::new(" No result.").style(dim())
    };
    f.render_widget(text, area);
}

/// Returns the widest line's width.
fn draw_messages(f: &mut Frame, app: &mut App, area: Rect) -> usize {
    let t = theme();
    if app.notes.log.is_empty() {
        f.render_widget(Paragraph::new(" No messages yet.").style(dim()), area);
        return 0;
    }
    let selected = app.notes.selected;
    let lines: Vec<Line> = app
        .notes
        .log
        .iter()
        .enumerate()
        .map(|(i, m)| {
            let line = Line::from(vec![
                Span::styled(format!(" {}  ", m.time), dim()),
                Span::styled(one_line(&m.text), if m.is_error { fg(t.error) } else { fg(t.text) }),
            ]);
            if selected == Some(i) { line.style(Style::default().bg(t.active)) } else { line }
        })
        .collect();
    // Newest at the bottom; scrolling moves up from there.
    let bottom = (lines.len() as u16).saturating_sub(area.height);
    let mut scroll = bottom.saturating_sub(app.tab.tab_scroll);
    // A message selected with the keys is scrolled into view.
    if let Some(sel) = selected.filter(|_| std::mem::take(&mut app.notes.reveal)) {
        let sel = u16::try_from(sel).unwrap_or(u16::MAX);
        if sel < scroll {
            scroll = sel;
        } else if sel >= scroll + area.height {
            scroll = sel + 1 - area.height;
        }
        app.tab.tab_scroll = bottom - scroll.min(bottom);
    }
    app.hits.messages = area;
    app.hits.messages_offset = usize::from(scroll);
    let widest = lines.iter().map(Line::width).max().unwrap_or(0);
    let across = clamp_across(app, widest, area.width);
    f.render_widget(Paragraph::new(lines).scroll((scroll, across)), area);
    widest
}

/// Above a plan: the statement it's for, EXPLAIN or EXPLAIN ANALYZE, when
/// it was made, and whether that statement has been edited since. Returns
/// the area left below it.
fn draw_plan_source(f: &mut Frame, app: &mut App, area: Rect) -> Rect {
    app.hits.plan_source = Rect::default();
    let t = theme();
    let Some(source) = app.tab.plan_source.as_ref().filter(|_| app.tab.plan.is_some() && area.height > 2) else {
        return area;
    };
    let [line, rest] = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(0)])
        .areas(area);
    let kind = if source.analyzed { " EXPLAIN ANALYZE " } else { " EXPLAIN " };
    let when = format!("  {} · {}", source.at.format("%H:%M:%S"), age(chrono::Local::now() - source.at));
    let (place, place_style, found) = source_place(app, source);
    let place = format!("  {place}");
    let room =
        usize::from(area.width).saturating_sub(kind.chars().count() + place.chars().count() + when.chars().count() + 1);
    let spans = vec![
        Span::styled(kind, bold(t.on_accent).bg(if source.analyzed { t.warn } else { t.accent })),
        Span::styled(format!(" {}", truncate(&one_line(&source.sql), room)), fg(t.text_bright)),
        Span::styled("  ", Style::default()),
        Span::styled(place.trim_start().to_string(), place_style),
        Span::styled(when, dim()),
    ];
    f.render_widget(Paragraph::new(Line::from(spans)), line);
    if found {
        app.hits.plan_source = line;
    }
    rest
}

/// Where `source`'s statement is in the editor now (lines above it may
/// have changed), styled as a link; once it's been edited away, where it
/// was, and that what's shown may not be for what's there now. And
/// whether it's there.
fn source_place(app: &App, source: &crate::app::PlanSource) -> (String, Style, bool) {
    let t = theme();
    let lines = |first: usize| {
        let last = first + source.sql.matches('\n').count();
        if last > first { format!("lines {}–{}", first + 1, last + 1) } else { format!("line {}", first + 1) }
    };
    match source.find_in_editor(&app.tab.editor) {
        Some((_, n)) => (lines(n), fg(t.accent).add_modifier(Modifier::UNDERLINED), true),
        None => (format!("was {} · edited since", lines(source.line)), fg(t.warn), false),
    }
}

/// Above a result: the kept results' numbers (with more than one), then
/// the statement it's for, its lines in the editor and when it ran.
/// Returns the area left below it.
fn draw_run_source(f: &mut Frame, app: &mut App, area: Rect) -> Rect {
    let t = theme();
    if app.tab.runs.is_empty() || area.height < 3 {
        return area;
    }
    let [line, rest] = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(0)])
        .areas(area);
    let mut spans = vec![Span::raw(" ")];
    let mut x = line.x + 1;
    let several = app.tab.runs.len() > 1;
    let focused = app.focus == Focus::Results;
    for (i, run) in app.tab.runs.iter().enumerate().filter(|_| several) {
        let shown = i == app.tab.run_at;
        // Like the pane's tabs: the shown one on the tab colour; pinned
        // ones in the accent.
        let style = match (shown, run.pinned) {
            (true, _) if focused => fg(t.text_bright),
            (true, _) => fg(t.text),
            (false, true) => fg(t.accent),
            (false, false) => muted(),
        };
        let style = if run.pinned { style.add_modifier(Modifier::BOLD) } else { style };
        let style = if shown { style.bg(t.tab) } else { style };
        let label = format!(" #{} ", run.number);
        let w = label.chars().count() as u16;
        app.hits.run_tabs.push((x, x + w, i));
        spans.push(Span::styled(label, style));
        x += w;
    }
    if several {
        spans.push(Span::styled(" ", Style::default()));
    }
    let run = &app.tab.runs[app.tab.run_at];
    match &run.source {
        Some(source) => {
            let (place, place_style, found) = source_place(app, source);
            let when = format!("  {} · {}", source.at.format("%H:%M:%S"), age(chrono::Local::now() - source.at));
            let pin = if run.pinned { "  pinned" } else { "" };
            let used: usize = spans.iter().map(Span::width).sum();
            let room = usize::from(line.width)
                .saturating_sub(used + place.chars().count() + when.chars().count() + pin.len() + 3);
            spans.push(Span::styled(truncate(&one_line(&source.sql), room), fg(t.text)));
            spans.push(Span::raw("  "));
            spans.push(Span::styled(place, place_style));
            spans.push(Span::styled(when, dim()));
            spans.push(Span::styled(pin, fg(t.accent)));
            if found {
                app.hits.run_source = Rect { x, ..line };
            }
        }
        None => spans.push(Span::styled(if run.pinned { "pinned" } else { "" }, fg(t.accent))),
    }
    // The run tabs' row, for clicks (the statement only where it's linked).
    app.hits.run_source.y = line.y;
    f.render_widget(Paragraph::new(Line::from(spans)), line);
    rest
}

/// How long ago, roughly: `12 s ago`, `5 min ago`, `2 h ago`, `3 days ago`.
fn age(d: chrono::TimeDelta) -> String {
    let s = d.num_seconds().max(0);
    match s {
        0..5 => "just now".to_string(),
        5..60 => format!("{s} s ago"),
        60..3600 => format!("{} min ago", s / 60),
        3600..86400 => format!("{} h ago", s / 3600),
        _ => format!("{} days ago", s / 86400),
    }
}

/// The plan's notes, numbered (the same number marks its step), then a
/// blank line; nothing without notes.
fn plan_note_lines(app: &App, width: u16) -> Vec<Line<'static>> {
    let t = theme();
    let Some(plan) = &app.tab.plan else { return Vec::new() };
    let mut lines = Vec::new();
    let room = usize::from(width).saturating_sub(5).max(20);
    for (i, note) in plan.notes.iter().enumerate() {
        for (j, part) in wrap_words(&note.text, room).into_iter().enumerate() {
            let label = if j == 0 {
                Span::styled(format!(" {} ", i + 1), bold(t.on_accent).bg(t.warn))
            } else {
                Span::raw("   ")
            };
            lines.push(Line::from(vec![label, Span::styled(format!(" {part}"), fg(t.warn))]));
        }
    }
    if !plan.notes.is_empty() {
        lines.push(Line::default());
    }
    lines
}

/// Returns the widest line's width.
fn draw_plan(f: &mut Frame, app: &mut App, area: Rect) -> usize {
    let t = theme();
    match &app.tab.plan {
        None => {
            f.render_widget(
                Paragraph::new(" Press F7 in the editor to see the plan for the statement at the cursor.").style(dim()),
                area,
            );
            0
        }
        Some(plan) => {
            // What stands out first, numbered; the same number marks its step.
            let mut lines = plan_note_lines(app, area.width);
            let notes = lines.len();
            let selected = app.tab.plan_selected.map(|(a, b)| a.min(b)..=a.max(b));
            for (n, l) in plan.text.lines().enumerate() {
                let trimmed = l.trim_start();
                let marks: Vec<String> = (1..=plan.notes.len())
                    .filter(|i| plan.notes[i - 1].line == Some(n))
                    .map(|i| i.to_string())
                    .collect();
                let style = if !marks.is_empty() {
                    bold(t.warn)
                } else if trimmed.starts_with("->") || !l.starts_with(' ') {
                    fg(t.text_bright)
                } else {
                    muted()
                };
                let mut line = Line::from(Span::styled(format!(" {l}"), style));
                if !marks.is_empty() {
                    line.push_span(Span::styled(format!("  ◀ {}", marks.join(",")), fg(t.warn)));
                }
                if selected.as_ref().is_some_and(|r| r.contains(&n)) {
                    line = line.style(Style::default().bg(t.selection));
                }
                lines.push(line);
            }
            app.hits.plan = area;
            app.hits.plan_notes = notes;
            let widest = lines.iter().map(Line::width).max().unwrap_or(0);
            let across = clamp_across(app, widest, area.width);
            f.render_widget(Paragraph::new(lines).scroll((app.tab.tab_scroll, across)), area);
            widest
        }
    }
}

fn draw_rows(f: &mut Frame, app: &mut App, area: Rect, footer: Rect) {
    let t = theme();
    let Some(results) = &app.tab.grid.results else {
        return draw_no_result(f, app, area);
    };
    if results.columns.is_empty() {
        let mut msg = results.rows_affected.map_or_else(|| " OK".to_string(), |n| format!(" {n} row(s) affected"));
        msg = format!("{msg} in {}", crate::app::format_elapsed(results.elapsed));
        if let Some(note) = &app.tab.grid.result_note {
            msg = format!("{msg} — {note}");
        }
        f.render_widget(Paragraph::new(msg).style(bold(t.ok)), area);
        return;
    }
    if app.tab.grid.vertical {
        return draw_vertical(f, app, area, footer);
    }

    let num_width = (app.tab.grid.view.len().max(1).to_string().len().max(2) + 2) as u16;
    let avail = area.width.saturating_sub(num_width + 1);
    let (offset, count) =
        visible_columns(&app.tab.grid.col_widths, app.tab.grid.col_offset, app.tab.grid.selected_col, avail);
    app.tab.grid.col_offset = offset;
    let visible = offset..offset + count;
    let classes: Vec<TypeClass> = (0..results.columns.len())
        .map(|i| type_class(results.column_types.get(i).map_or("", String::as_str)))
        .collect();

    // Headers like the QUERY PLAN's table: the name, then a rule under the
    // column (the selected cell's type is in the footer). The sorted column
    // is picked out.
    let rule = |width: u16, style: Style| Line::from(Span::styled("─".repeat(usize::from(width)), style));
    let mut header_cells = vec![Cell::from(Text::from(vec![
        Line::from(Span::styled(" #", dim())),
        rule(num_width.saturating_sub(1), dim()).right_aligned(),
    ]))];
    for (i, name) in results.columns[visible.clone()].iter().enumerate() {
        let col = offset + i;
        let sorted = match app.tab.grid.sort {
            Some((c, desc)) if c == col => Some(if desc { " ▼" } else { " ▲" }),
            _ => None,
        };
        let (name_style, rule_style) =
            if sorted.is_some() { (bold(t.accent), fg(t.accent)) } else { (fg(t.text_bright), fg(t.text)) };
        let mut first = Line::from(vec![Span::styled(name.clone(), name_style)]);
        if let Some(arrow) = sorted {
            first.push_span(Span::styled(arrow, fg(t.accent)));
        }
        let first = if classes[col] == TypeClass::Number { first.right_aligned() } else { first };
        header_cells.push(Cell::from(Text::from(vec![first, rule(app.tab.grid.col_widths[col], rule_style)])));
    }
    let header = Row::new(header_cells).height(2);

    // Only the rows on screen are built: a streamed result can hold far
    // more rows than fit, and building them all every frame is slow.
    let visible_rows = area.height.saturating_sub(2) as usize;
    let selected_row = app.tab.grid.table_state.selected();
    if let Some(sel) = selected_row {
        if sel < app.tab.grid.row_offset {
            app.tab.grid.row_offset = sel;
        } else if visible_rows > 0 && sel >= app.tab.grid.row_offset + visible_rows {
            app.tab.grid.row_offset = sel + 1 - visible_rows;
        }
    }
    app.tab.grid.row_offset = app.tab.grid.row_offset.min(app.tab.grid.view.len().saturating_sub(1));
    let window = app.tab.grid.row_offset..(app.tab.grid.row_offset + visible_rows).min(app.tab.grid.view.len());
    let body_rows = app.tab.grid.view[window.clone()].iter().zip(window.clone()).map(|(&i, n)| {
        let changed = app.tab.grid.changed_rows.contains(&i);
        let mut cells = vec![row_number(app.tab.grid.marked.contains(&i), Some(n) == selected_row, n, changed)];
        for (j, v) in results.rows[i][visible.clone()].iter().enumerate() {
            let class = classes[offset + j];
            let cell = match v {
                Some(v) => {
                    // Only what fits: a cell can hold megabytes.
                    let shown = display_prefix(v, usize::from(app.tab.grid.col_widths[offset + j]) + 1);
                    let line = Line::from(Span::styled(shown.into_owned(), class_style(class)));
                    if class == TypeClass::Number { Cell::from(line.right_aligned()) } else { Cell::from(line) }
                }
                None => Cell::from(Span::styled(NULL_SYMBOL, dim())),
            };
            cells.push(cell);
        }
        // New or changed since watch mode's run before: bold.
        let row = Row::new(cells);
        if changed { row.style(Style::default().add_modifier(Modifier::BOLD)) } else { row }
    });

    let mut widths = vec![Constraint::Length(num_width)];
    widths.extend(app.tab.grid.col_widths[visible.clone()].iter().map(|w| Constraint::Length(*w)));
    // The table sees only the window, so give it a window-relative state.
    let mut state = TableState::default()
        .with_selected(selected_row.map(|s| s - window.start))
        .with_selected_column(Some(app.tab.grid.selected_col.saturating_sub(offset) + 1));
    let table = Table::new(body_rows, widths)
        .header(header)
        .row_highlight_style(Style::default().bg(t.active))
        .cell_highlight_style(bold(t.text_bright).bg(t.selected));
    f.render_stateful_widget(table, area, &mut state);

    // Where rows and columns are, for clicks (columns are 1 cell apart).
    app.hits.result_rows = Rect { y: area.y + 2, height: area.height.saturating_sub(2), ..area };
    let mut x = area.x + num_width + 1;
    app.hits.result_cols = visible
        .map(|col| {
            let w = app.tab.grid.col_widths[col];
            let range = (x, x + w, col);
            x += w + 1;
            range
        })
        .collect();

    draw_rows_footer(f, app, footer, offset, count);
}

/// The rows one column per line, like mysql's `\G`: a `*** 3. row ***`
/// heading, then `name: value` lines. Scrolled so the selected field shows,
/// with its row's heading when it fits.
fn draw_vertical(f: &mut Frame, app: &mut App, area: Rect, footer: Rect) {
    let t = theme();
    let grid = &mut app.tab.grid;
    let Some(results) = &grid.results else { return };
    let cols = results.columns.len();
    let block = cols + 1;
    let height = usize::from(area.height).max(1);
    let selected = grid.table_state.selected().unwrap_or(0).min(grid.view.len().saturating_sub(1));
    // Whole rows that fit; the selected one among them.
    let fit = (height / block).max(1);
    if selected < grid.row_offset {
        grid.row_offset = selected;
    } else if selected >= grid.row_offset + fit {
        grid.row_offset = selected + 1 - fit;
    }
    let first = grid.row_offset;
    // Within a row taller than the pane: down to the selected field.
    let field_line = (selected - first) * block + 1 + grid.selected_col.min(cols - 1);
    let scroll = (field_line + 1).saturating_sub(height);

    let names = results
        .columns
        .iter()
        .map(|c| unicode_width::UnicodeWidthStr::width(c.as_str()))
        .max()
        .unwrap_or(0)
        .min(usize::from(area.width) / 3);
    let room = usize::from(area.width).saturating_sub(names + 3);
    let rule = usize::from(area.width).min(64);
    let shown = grid.view.len().min(first + fit + 1);
    let mut lines = Vec::new();
    for (n, &i) in grid.view[first..shown].iter().enumerate() {
        let n = first + n;
        let current = n == selected;
        let marked = grid.marked.contains(&i);
        let heading = format!("{:*^rule$}", format!(" {}. row{} ", n + 1, if marked { " ●" } else { "" }));
        lines.push(Line::from(Span::styled(heading, if current { bold(t.accent) } else { dim() })));
        for (c, name) in results.columns.iter().enumerate() {
            let name = format!("{:>names$}", truncate(name, names));
            let value = match &results.rows[i][c] {
                Some(v) => Span::styled(
                    one_line(&display_prefix(v, room + 1)),
                    class_style(type_class(results.column_types.get(c).map_or("", String::as_str))),
                ),
                None => Span::styled(NULL_SYMBOL, dim()),
            };
            let picked = current && c == grid.selected_col;
            let line = Line::from(vec![
                Span::styled(format!(" {name}"), if picked { bold(t.accent) } else { muted() }),
                Span::styled(": ", dim()),
                value,
            ]);
            lines.push(if picked {
                line.style(bold(t.text_bright).bg(t.selected))
            } else if current {
                line.style(Style::default().bg(t.active))
            } else {
                line
            });
        }
    }
    f.render_widget(Paragraph::new(lines).scroll((u16::try_from(scroll).unwrap_or(u16::MAX), 0)), area);
    app.hits.result_cols.clear();
    draw_rows_footer(f, app, footer, 0, cols);
}

/// The row-number cell: `●` and highlighted when marked, an orange `◆`
/// when watch mode found it new or changed.
fn row_number(marked: bool, selected: bool, n: usize, changed: bool) -> Cell<'static> {
    let t = theme();
    let style = if marked {
        bold(t.on_accent).bg(t.warn)
    } else if selected {
        bold(t.accent).bg(t.selected)
    } else if changed {
        bold(t.warn)
    } else {
        dim()
    };
    let sign = if marked {
        "●"
    } else if changed {
        "◆"
    } else {
        " "
    };
    Cell::from(Span::styled(format!("{sign}{:0>2}", n + 1), style))
}

/// The footer under the table: the cell-edit prompt when one is open,
/// otherwise the selected cell and which rows/columns are shown.
fn draw_rows_footer(f: &mut Frame, app: &App, footer: Rect, offset: usize, count: usize) {
    let t = theme();
    let Some(results) = &app.tab.grid.results else { return };
    // Editing a cell: the footer becomes the prompt.
    if let Some(edit) = &app.cell_edit {
        f.render_widget(Block::default().style(Style::default().bg(t.active)), footer);
        let mut spans = vec![
            Span::styled(" SET ", bold(t.on_accent).bg(t.accent)),
            Span::styled(format!(" {} = ", edit.column), bold(t.accent)),
        ];
        spans.extend(input_spans(&edit.text, fg(t.text_bright)));
        let left = Line::from(spans);
        let null = if edit.nullable { "  ^N null" } else { "" };
        let right = Line::from(Span::styled(format!("↵ save{null}  Esc cancel "), muted()));
        let right_w = right.width() as u16;
        let [l, r] = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Min(0), Constraint::Length(right_w.min(footer.width))])
            .areas(footer);
        f.render_widget(Paragraph::new(left), l);
        f.render_widget(Paragraph::new(right).alignment(Alignment::Right), r);
        return;
    }

    // Footer: the selected cell, and which rows are shown.
    let cell = app.tab.grid.table_state.selected().map(|r| {
        let col = &results.columns[app.tab.grid.selected_col.min(results.columns.len() - 1)];
        let ty = results.column_types.get(app.tab.grid.selected_col).filter(|t| *t != "?").cloned().unwrap_or_default();
        format!(" CELL {:0>2}:{col}  {}", r + 1, ty.to_lowercase())
    });
    let left = Line::from(vec![
        Span::styled(cell.unwrap_or_default(), fg(t.accent)),
        Span::styled(format!("   NULL AS {NULL_SYMBOL}"), dim()),
    ]);
    let shown = app.tab.grid.view.len();
    let plus = if app.run.stream.is_some() || app.tab.grid.cut_short { "+" } else { "" };
    let mut right = vec![Span::styled(format!("1–{shown} / {}{plus}", results.total_rows), bold(t.text))];
    if app.run.fetching_more {
        right.push(Span::styled("  · loading…", fg(t.warn)));
    } else if app.run.stream.is_some() {
        right.push(Span::styled("  · scroll or M for more", dim()));
    } else if results.truncated() || app.tab.grid.cut_short {
        right.push(Span::styled("  · M loads more", dim()));
    }
    // Which columns show, with arrows where more are hidden.
    let total = results.columns.len();
    if count < total {
        let left = if offset > 0 { "◀ " } else { "" };
        let right_more = if offset + count < total { " ▶" } else { "" };
        right.push(Span::styled("  · ", dim()));
        right.push(Span::styled(
            format!("{left}cols {}–{} of {total}{right_more}", offset + 1, offset + count),
            fg(t.text),
        ));
    } else {
        right.push(Span::styled(format!("  · {total} cols"), dim()));
    }
    // The query's time (not a plan's: that's the EXPLAIN's).
    if !app.tab.plan_swapped {
        right.push(Span::styled(format!("  · {}", crate::app::format_elapsed(results.elapsed)), dim()));
    }
    right.push(Span::raw(" "));
    let right = Line::from(right);
    let right_w = right.width() as u16;
    let [l, r] = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(0), Constraint::Length(right_w.min(footer.width))])
        .areas(footer);
    f.render_widget(Paragraph::new(left), l);
    f.render_widget(Paragraph::new(right).alignment(Alignment::Right), r);
}

/// `text` in lines of at most `width` chars, broken between words.
pub(super) fn wrap_words(text: &str, width: usize) -> Vec<String> {
    let mut lines = vec![String::new()];
    for word in text.split_whitespace() {
        let last = lines.last_mut().expect("never empty");
        if !last.is_empty() && last.chars().count() + 1 + word.chars().count() > width {
            lines.push(word.to_string());
        } else {
            if !last.is_empty() {
                last.push(' ');
            }
            last.push_str(word);
        }
    }
    lines
}

/// How NULL is shown in the table (the design's `NULL AS ∅`).
const NULL_SYMBOL: &str = "∅";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ages_read_roughly() {
        let age = |s| age(chrono::TimeDelta::seconds(s));
        assert_eq!(
            [age(2), age(42), age(150), age(7300), age(200_000)],
            ["just now", "42 s ago", "2 min ago", "2 h ago", "2 days ago"]
        );
    }

    #[test]
    fn words_wrap_at_the_width() {
        assert_eq!(wrap_words("one two three four", 9), ["one two", "three", "four"]);
        assert_eq!(wrap_words("averyveryverylongword x", 5), ["averyveryverylongword", "x"]);
    }

    #[test]
    fn type_classes() {
        assert!(type_class("INT4") == TypeClass::Number && type_class("numeric") == TypeClass::Number);
        assert!(type_class("UUID") == TypeClass::Id);
        assert!(type_class("TIMESTAMPTZ") == TypeClass::Time && type_class("DATETIME") == TypeClass::Time);
        assert!(type_class("BOOL") == TypeClass::Bool);
        assert!(type_class("JSONB") == TypeClass::Json);
        assert!(type_class("VARCHAR") == TypeClass::Text && type_class("") == TypeClass::Text);
    }
}
