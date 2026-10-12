//! `NAVIGATOR`: filter line, TABLES / VIEWS sections, tables
//! with row estimates, expanded columns with their types.

use super::*;
use crate::app::{SchemaPanel, SchemaSort};

pub(super) fn draw_schema(f: &mut Frame, app: &mut App, area: Rect) {
    let t = theme();
    let focused = app.focus == Focus::Schema;
    let count = app.schema.as_ref().map_or(0, |s| s.tables.len());
    let info = if app.schema.as_ref().is_some_and(|s| s.loading) {
        "loading…".to_string()
    } else {
        match app.schema.as_ref().map_or(SchemaSort::Name, |s| s.sort) {
            SchemaSort::Name => format!("{count} objects"),
            sort => format!("{count} · by {}", sort.label()),
        }
    };
    // " NAVIGATOR " plus the info must fit on the top border.
    let info = if area.width as usize >= 14 + info.chars().count() { info } else { String::new() };
    let info = if app.zoomed { format!("ZOOMED · {info}") } else { info };
    let block = pane("Navigator", &info, focused);
    let inner = block.inner(area);
    f.render_widget(block, area);
    let [filter, database, tree] = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Length(1), Constraint::Min(0)])
        .areas(inner);

    app.hits.schema_filter = filter;
    draw_database_line(f, app, database, focused);

    // Filter line.
    let line = match &app.schema {
        Some(s) if s.filter_editing => {
            Line::from([vec![Span::raw(" / ")], input_spans(&s.filter, fg(t.text_bright))].concat())
        }
        Some(s) if !s.filter.is_empty() => Line::styled(format!(" / {}", s.filter.text()), bold(t.accent)),
        _ => Line::styled(" / filter by name…", dim()),
    };
    f.render_widget(Paragraph::new(line).style(Style::default().bg(t.inset)), filter);

    let Some(panel) = &app.schema else { return };
    if let Some(e) = &panel.error {
        f.render_widget(Paragraph::new(e.as_str()).style(fg(t.error)).wrap(Wrap { trim: true }), tree);
        return;
    }
    if panel.tables.is_empty() {
        let msg = if panel.loading { " Loading…" } else { " No tables" };
        f.render_widget(Paragraph::new(msg).style(dim()), tree);
        return;
    }

    let width = tree.width as usize;
    let show_schema = app.engine().capabilities().schemas;
    // Only the rows on screen are built: a big database has tens of
    // thousands of tables.
    let nodes = panel.nodes();
    let height = tree.height as usize;
    let Some(panel) = &mut app.schema else { return };
    let offset = panel.scroll_into_view(&nodes, height);
    let panel = &*panel;
    let items: Vec<ListItem> =
        nodes.into_iter().skip(offset).take(height).map(|node| schema_item(panel, node, width, show_schema)).collect();

    let highlight = if focused { bold(t.text_bright).bg(t.active) } else { Style::default().bg(t.inset) };
    let mut state = ListState::default().with_selected(panel.selected.checked_sub(offset));
    f.render_stateful_widget(List::new(items).highlight_style(highlight), tree, &mut state);
    app.hits.schema_list = tree;
    app.hits.schema_offset = offset;
}

/// The database the tables belong to, like the design's tree root.
fn draw_database_line(f: &mut Frame, app: &App, database: Rect, focused: bool) {
    let t = theme();
    let form = &app.connect_form;
    let db_name = if form.engine.is_file_based() {
        std::path::Path::new(&form.host).file_name().map_or(form.host.clone(), |n| n.to_string_lossy().into_owned())
    } else if form.database.is_empty() {
        "(default database)".to_string()
    } else {
        form.database.clone()
    };
    // The status bar shows the server and version.
    let name = truncate(&db_name, (database.width as usize).saturating_sub(4));
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(" ▼ ", dim()),
            Span::styled(name, if focused { bold(t.ident) } else { muted() }),
        ])),
        database,
    );
}

/// One row of the tree: a name on the left, details right-aligned.
fn schema_item(panel: &SchemaPanel, node: SchemaNode, width: usize, show_schema: bool) -> ListItem<'static> {
    let t = theme();
    let row = |left: Vec<Span<'static>>, right: String, right_style: Style| -> Line<'static> {
        let used: usize = left.iter().map(|s| s.content.chars().count()).sum::<usize>() + right.chars().count() + 1;
        let mut spans = left;
        spans.push(Span::raw(" ".repeat(width.saturating_sub(used).max(1))));
        spans.push(Span::styled(right, right_style));
        Line::from(spans)
    };
    match node {
        SchemaNode::Section(section) => {
            let title = match section {
                SchemaSection::Tables => " TABLES",
                SchemaSection::Views => " VIEWS",
                SchemaSection::System => " SYSTEM",
                SchemaSection::Routines => " FUNCTIONS",
            };
            let n = match section {
                SchemaSection::Routines => panel.routines.len(),
                _ => panel.tables.iter().filter(|t| section.holds(&t.info)).count(),
            };
            ListItem::new(row(vec![Span::styled(title, bold(t.muted))], n.to_string(), dim()))
                .style(Style::default().bg(t.inset))
        }
        SchemaNode::Table(i) => {
            let table = &panel.tables[i];
            // System tables can share a name with the user's (MySQL's
            // information_schema.tables), so they always show their schema.
            let qualify = table.info.schema != "main" && (table.info.system || show_schema);
            let name = if qualify && table.info.schema != "public" {
                format!("{}.{}", table.info.schema, table.info.name)
            } else {
                table.info.name.clone()
            };
            let (icon, name_style) = match (table.info.is_view, table.info.system) {
                (true, false) => ("◇ ", fg(t.ident)),
                (true, true) => ("◇ ", muted()),
                (false, system) => {
                    (if table.expanded { "▼ " } else { "▶ " }, if system { muted() } else { fg(t.text) })
                }
            };
            let right = if table.info.is_view {
                "view".to_string()
            } else if panel.sort == SchemaSort::Size {
                table.info.bytes.map(human_bytes).unwrap_or_default()
            } else {
                table.info.rows.map(human_count).unwrap_or_default()
            };
            let name = truncate(&name, width.saturating_sub(right.chars().count() + 6));
            ListItem::new(row(
                vec![Span::styled(format!("  {icon}"), dim()), Span::styled(name, name_style)],
                right,
                dim(),
            ))
        }
        SchemaNode::Column(i, c) => {
            let table = &panel.tables[i];
            let col = &table.columns[c];
            let mut right = col.data_type.to_lowercase();
            if col.primary_key {
                right.push_str(" PK");
            }
            if let Some(target) = table.foreign_key(&col.name) {
                right = truncate(&format!("→ {target}"), width / 2);
            }
            let name = truncate(&col.name, width.saturating_sub(right.chars().count() + 9));
            let name_style = if col.primary_key { bold(t.number) } else { fg(t.text) };
            ListItem::new(row(
                vec![Span::styled("      ◇ ", dim()), Span::styled(name, name_style)],
                right,
                if col.primary_key { fg(t.accent) } else { dim() },
            ))
        }
        SchemaNode::Index(i, x) => {
            let ix = &panel.tables[i].details.as_ref().expect("index rows come from details").indexes[x];
            let kind = if ix.primary {
                " PK"
            } else if ix.unique {
                " unique"
            } else {
                ""
            };
            let right = truncate(&format!("({}){kind}", ix.columns), width / 2);
            let name = truncate(&ix.name, width.saturating_sub(right.chars().count() + 9));
            ListItem::new(row(vec![Span::styled("      ≡ ", dim()), Span::styled(name, muted())], right, dim()))
        }
        SchemaNode::Routine(r) => {
            let routine = &panel.routines[r];
            let right = if routine.kind == "procedure" { "proc" } else { "fn" }.to_string();
            let name = if show_schema && routine.schema != "public" {
                format!("{}.{}", routine.schema, routine.name)
            } else {
                routine.name.clone()
            };
            let name = truncate(&name, width.saturating_sub(right.chars().count() + 6));
            ListItem::new(row(vec![Span::styled("  ƒ ", dim()), Span::styled(name, fg(t.ident))], right, dim()))
        }
    }
}
