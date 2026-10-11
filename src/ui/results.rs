//! `QUERY RESULT`: tabs (rows / messages / plan), the table with a
//! row-number column and two-line headers, and a footer.

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
    let info = match (&app.tab.grid.results, app.executing) {
        (_, true) => "running…".to_string(),
        (Some(r), false) => format!("ran in {} ms", r.elapsed.as_millis()),
        (None, false) => String::new(),
    };
    let info = if app.zoomed { format!("ZOOMED · {info}") } else { info };
    let block = pane("Query result", &info, focused);
    app.hits.results = area;
    app.hits.result_rows = Rect::default();
    let inner = block.inner(area);
    f.render_widget(block, area);
    let [tabs, body, footer] = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(0), Constraint::Length(1)])
        .areas(inner);

    draw_tabs(f, app, tabs);
    f.render_widget(Block::default().style(Style::default().bg(t.inset)), footer);
    match app.tab.result_tab {
        ResultTab::Messages => draw_messages(f, app, body),
        ResultTab::Plan => draw_plan(f, app, body),
        ResultTab::Rows => draw_rows(f, app, body, footer),
    }
    // The last run's outcome (or progress), until the next key; the cell
    // info comes back after it.
    if app.status_in_results && !app.status.is_empty() && app.cell_edit.is_none() {
        let style = if app.status_is_error { bold(t.error) } else { fg(t.text) };
        f.render_widget(Block::default().style(Style::default().bg(t.inset)), footer);
        f.render_widget(
            Paragraph::new(Span::styled(format!(" {}", truncate(&app.status, footer.width as usize)), style)),
            footer,
        );
    }
}

fn draw_tabs(f: &mut Frame, app: &mut App, area: Rect) {
    let t = theme();
    f.render_widget(Block::default().style(Style::default().bg(t.inset)), area);
    let rows = app.tab.grid.results.as_ref().map(|r| {
        if r.columns.is_empty() {
            format!("{} affected", r.rows_affected.unwrap_or(0))
        } else if app.row_stream.is_some() {
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
    let label = if app.tab.grid.result_sets.len() > 1 {
        format!("RESULT {}/{}", app.tab.grid.result_set + 1, app.tab.grid.result_sets.len())
    } else {
        "RESULT".into()
    };
    left.extend(tab(label, rows, app.tab.result_tab == ResultTab::Rows));
    tab_hits.push((start, area.x + width(&left), ResultTab::Rows));
    let errors = app.messages.iter().filter(|m| m.is_error).count();
    let msg_extra = (!app.messages.is_empty()).then(|| {
        if errors > 0 { format!("{} · {errors} err", app.messages.len()) } else { app.messages.len().to_string() }
    });
    let tab_x = area.x + width(&left);
    left.extend(tab("MESSAGES".into(), msg_extra, app.tab.result_tab == ResultTab::Messages));
    tab_hits.push((tab_x, area.x + width(&left), ResultTab::Messages));
    let tab_x = area.x + width(&left);
    left.extend(tab("QUERY PLAN".into(), None, app.tab.result_tab == ResultTab::Plan));
    tab_hits.push((tab_x, area.x + width(&left), ResultTab::Plan));
    app.hits.result_tabs = tab_hits;
    app.hits.result_tabs_y = area.y;

    let right = if app.tab.grid.filter_editing {
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
    let right_w = right.width() as u16;
    let [l, r] = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(0), Constraint::Length(right_w.min(area.width))])
        .areas(area);
    f.render_widget(Paragraph::new(Line::from(left)), l);
    f.render_widget(Paragraph::new(right).alignment(Alignment::Right), r);
}

fn draw_messages(f: &mut Frame, app: &App, area: Rect) {
    let t = theme();
    if app.messages.is_empty() {
        f.render_widget(Paragraph::new(" No messages yet.").style(dim()), area);
        return;
    }
    let lines: Vec<Line> = app
        .messages
        .iter()
        .map(|m| {
            Line::from(vec![
                Span::styled(format!(" {}  ", m.time), dim()),
                Span::styled(one_line(&m.text), if m.is_error { fg(t.error) } else { fg(t.text) }),
            ])
        })
        .collect();
    // Newest at the bottom; scrolling moves up from there.
    let bottom = (lines.len() as u16).saturating_sub(area.height);
    let scroll = bottom.saturating_sub(app.tab.tab_scroll);
    f.render_widget(Paragraph::new(lines).scroll((scroll, 0)), area);
}

fn draw_plan(f: &mut Frame, app: &App, area: Rect) {
    let t = theme();
    match &app.tab.plan {
        None => f.render_widget(
            Paragraph::new(" Press F7 in the editor to see the plan for the statement at the cursor.").style(dim()),
            area,
        ),
        Some(plan) => {
            // What stands out first, numbered; the same number marks its step.
            let mut lines: Vec<Line> = Vec::new();
            let room = (area.width as usize).saturating_sub(5).max(20);
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
                lines.push(line);
            }
            f.render_widget(Paragraph::new(lines).scroll((app.tab.tab_scroll, 0)), area);
        }
    }
}

fn draw_rows(f: &mut Frame, app: &mut App, area: Rect, footer: Rect) {
    let t = theme();
    let Some(results) = &app.tab.grid.results else {
        f.render_widget(Paragraph::new(" No results yet. Write a query above and press F5.").style(dim()), area);
        return;
    };
    if results.columns.is_empty() {
        let mut msg = results.rows_affected.map_or_else(|| " OK".to_string(), |n| format!(" {n} row(s) affected"));
        if let Some(note) = &app.tab.grid.result_note {
            msg = format!("{msg} — {note}");
        }
        f.render_widget(Paragraph::new(msg).style(bold(t.ok)), area);
        return;
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

    // Two-line headers: NAME over type; the sorted column picked out.
    let mut header_cells =
        vec![Cell::from(Text::from(vec![Line::from(" #"), Line::default()])).style(dim().bg(t.inset))];
    for (i, name) in results.columns[visible.clone()].iter().enumerate() {
        let col = offset + i;
        let sorted = match app.tab.grid.sort {
            Some((c, desc)) if c == col => Some(if desc { " ▼" } else { " ▲" }),
            _ => None,
        };
        // sqlx names types it doesn't know (e.g. custom enums) "?".
        // Unknown types: "?" from sqlx, "NULL" for SQLite expressions.
        let ty = results
            .column_types
            .get(col)
            .filter(|t| *t != "?" && !t.eq_ignore_ascii_case("null"))
            .map(|s| s.to_lowercase())
            .unwrap_or_default();
        let (name_style, bg) = if sorted.is_some() { (bold(t.accent), t.active) } else { (bold(t.text), t.inset) };
        let mut first = Line::from(vec![Span::styled(name.to_uppercase(), name_style)]);
        if let Some(arrow) = sorted {
            first.push_span(Span::styled(arrow, fg(t.accent)));
        }
        let text = Text::from(vec![first, Line::from(Span::styled(ty, dim()))]);
        let text = if classes[col] == TypeClass::Number { text.alignment(Alignment::Right) } else { text };
        header_cells.push(Cell::from(text).style(Style::default().bg(bg)));
    }
    let header = Row::new(header_cells).height(2).style(Style::default().bg(t.inset));

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
        let mut cells = vec![row_number(app.tab.grid.marked.contains(&i), Some(n) == selected_row, n)];
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
        Row::new(cells)
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

/// The row-number cell: `●` and highlighted when marked.
fn row_number(marked: bool, selected: bool, n: usize) -> Cell<'static> {
    let t = theme();
    let style = if marked {
        bold(t.on_accent).bg(t.warn)
    } else if selected {
        bold(t.accent).bg(t.selected)
    } else {
        dim()
    };
    Cell::from(Span::styled(format!("{}{:0>2}", if marked { "●" } else { " " }, n + 1), style))
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
    let plus = if app.row_stream.is_some() { "+" } else { "" };
    let mut right = vec![Span::styled(format!("1–{shown} / {}{plus}", results.total_rows), bold(t.text))];
    if app.fetching_more {
        right.push(Span::styled("  · loading…", fg(t.warn)));
    } else if app.row_stream.is_some() {
        right.push(Span::styled("  · scroll or M for more", dim()));
    } else if results.truncated() {
        right.push(Span::styled("  · M loads more", dim()));
    }
    if count < results.columns.len() {
        right.push(Span::styled(
            format!("  · cols {}–{} of {}", offset + 1, offset + count, results.columns.len()),
            dim(),
        ));
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
fn wrap_words(text: &str, width: usize) -> Vec<String> {
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
