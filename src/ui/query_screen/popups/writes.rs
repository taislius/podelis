//! Writes: the transaction mode's picker, the review before running, the
//! sandbox, the export form.

use super::*;

/// Ctrl+K x: auto-commit, manual commit or rollback-only.
pub(super) fn draw_tx_picker(f: &mut Frame, app: &mut App, area: Rect) {
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
    app.hits.tx_picker = rect;
    f.render_widget(Clear, rect);
    f.render_widget(Paragraph::new(lines).block(popup(t!("tx_picker.title"), t.accent)), rect);
}

/// `E`: where the rows go, in what format, which of them.
pub(super) fn draw_export_form(f: &mut Frame, app: &mut App, area: Rect) {
    use crate::app::export_form::{ExportClick as Click, ExportLine as Part, ExportTo as To};
    let t = theme();
    let Some(form) = &app.popups.export_form else { return };
    let (choice, current) = (form.choice, form.line);
    // Wide enough for the whole name, as far as the screen goes.
    let name_w = u16::try_from(form.name.text().chars().count()).unwrap_or(u16::MAX).saturating_add(16);
    let width = fit(name_w.clamp(60, 120), 30, area.width.saturating_sub(4).max(30));
    let room = usize::from(width.saturating_sub(15));
    let shown = t!("export_form.shown", n = app.tab.grid.view.len());
    // A line: its label, then pieces of text, some to click.
    type Piece = (Vec<Span<'static>>, bool, Option<Click>);
    let text = |s: String| vec![Span::raw(s)];
    let radio = |on: bool, label: String, click: Click| -> Piece {
        (text(format!("{} {label}", if on { "●" } else { "○" })), on, Some(click))
    };
    let gap = || -> Piece { (text("   ".to_string()), false, None) };
    let mut lines: Vec<(&str, Option<Part>, Vec<Piece>)> = vec![
        (
            t!("export_form.to"),
            Some(Part::To),
            vec![
                radio(choice.to == To::File, t!("export_form.file").to_string(), Click::To(To::File)),
                gap(),
                radio(choice.to == To::Clipboard, t!("export_form.clipboard").to_string(), Click::To(To::Clipboard)),
            ],
        ),
        (
            t!("export_form.format"),
            Some(Part::Format),
            vec![(text(format!("‹ {} ›", choice.format.name())), true, Some(Click::NextFormat))],
        ),
    ];
    if choice.to == To::Clipboard {
        let marked = app.tab.grid.marked.len();
        let which = if marked > 0 { t!("export_form.marked", n = marked) } else { shown };
        lines.push((t!("export_form.rows"), None, vec![(text(which), false, None)]));
    } else {
        if app.export_every_offered() {
            lines.push((
                t!("export_form.rows"),
                Some(Part::Rows),
                vec![
                    radio(!choice.every, shown, Click::Every(false)),
                    gap(),
                    radio(choice.every, t!("export_form.every").to_string(), Click::Every(true)),
                ],
            ));
        } else {
            lines.push((t!("export_form.rows"), None, vec![(text(shown), false, None)]));
        }
        // Being edited: the cursor in it; otherwise its end, where the
        // name is.
        let name = if current == Part::Name {
            input_window(&form.name, room, fg(t.text_bright))
        } else {
            text(truncate_start(form.name.text(), room))
        };
        lines.push((t!("export_form.file_name"), Some(Part::Name), vec![(name, false, Some(Click::Name))]));
        if let Some(error) = &form.error {
            lines.push(("", None, vec![(vec![Span::styled(error.clone(), fg(t.warn))], false, None)]));
        }
    }
    let run = if choice.to == To::File { t!("export_form.run_file") } else { t!("export_form.run_clipboard") };
    let height = lines.len() as u16 + 4;
    let rect = centered_box(area, width, height);
    let mut spots = Vec::new();
    let mut drawn: Vec<Line> = Vec::new();
    for (row, (label, part, pieces)) in lines.into_iter().enumerate() {
        let here = part == Some(current);
        let top = rect.y + 1 + row as u16;
        let label_style = if here { bold(t.accent) } else { muted() };
        let mut spans = vec![Span::styled(format!(" {label:<9}  "), label_style)];
        let mut left = rect.x + 1 + spans[0].width() as u16;
        for (piece, on, click) in pieces {
            let style = if on && here {
                bold(t.text_bright).bg(t.active)
            } else if on || click.is_none() || click == Some(Click::Name) {
                fg(t.text)
            } else {
                muted()
            };
            let wide: u16 = piece.iter().map(|s| s.width() as u16).sum();
            if let Some(click) = click {
                spots.push((Rect { x: left, y: top, width: wide, height: 1 }, click));
            }
            left += wide;
            spans.extend(piece.into_iter().map(|s| s.patch_style(style)));
        }
        drawn.push(Line::from(spans));
    }
    drawn.push(Line::default());
    let run = format!(" {run} ");
    let help = t!("export_form.help");
    let run_w = Span::raw(run.as_str()).width() as u16;
    spots.push((Rect { x: rect.x + 2, y: rect.y + 1 + drawn.len() as u16, width: run_w, height: 1 }, Click::Run));
    drawn.push(Line::from(vec![
        Span::raw(" "),
        Span::styled(run, bold(t.text_bright).bg(t.active)),
        Span::styled(format!("  {help}"), dim()),
    ]));
    app.hits.export_form = rect;
    app.hits.export_form_spots = spots;
    f.render_widget(Clear, rect);
    f.render_widget(Paragraph::new(drawn).block(popup(t!("export_form.title"), t.accent)), rect);
}

/// The sandbox: its steps, what they changed per table, and how to end it.
pub(super) fn draw_sandbox(f: &mut Frame, app: &mut App, area: Rect) {
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
    let key = |command, what: &str, bg| {
        use crate::app::windows::sandbox::SANDBOX_KEYS;
        let k = crate::app::key_table::first_key(SANDBOX_KEYS, command);
        vec![Span::styled(format!(" {k} "), bold(t.on_accent).bg(bg)), Span::styled(format!(" {what}  "), fg(t.text))]
    };
    use crate::app::windows::sandbox::SandboxKey as S;
    // Its keys under the steps, kept there: the steps scroll above them.
    let mut chips = vec![
        key(S::Undo, t!("sandbox.undo"), t.accent),
        key(S::Discard, t!("sandbox.discard"), t.error),
        key(S::KeepScript, t!("sandbox.keep_script"), t.accent),
        key(S::RunForReal, t!("sandbox.run_for_real"), t.ok),
    ];
    // How to close it, and that it stays open: with the keys, each part
    // whole (on a line of its own where they don't fit together).
    let close = crate::app::key_table::first_key(crate::app::windows::sandbox::SANDBOX_KEYS, S::Close);
    let esc = t!("sandbox.esc", key = close);
    chips.extend(esc.split(" · ").map(|part| vec![Span::styled(format!(" {part}  "), dim())]));
    let keys = key_lines(chips, width.saturating_sub(2));
    let keys_h = keys.len() as u16;
    // Short of the whole screen: the status bar stays clear of its border.
    // Wrapped (a long line isn't cut at the border), once: its lines say how
    // tall it is and how far it scrolls.
    let text = Paragraph::new(lines).wrap(Wrap { trim: false });
    let body_lines = u16::try_from(text.line_count(width.saturating_sub(2))).unwrap_or(u16::MAX);
    let height = body_lines.saturating_add(keys_h).saturating_add(2).min(area.height.saturating_sub(2));
    let rect = centered_box(area, width, height);
    f.render_widget(Clear, rect);
    let block = popup(t!("sandbox.title"), t.warn);
    let inner = block.inner(rect);
    f.render_widget(block, rect);
    let [body, footer] = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(keys_h.min(inner.height))])
        .areas(inner);
    // Scrolled no further than its last line shows.
    let most = body_lines.saturating_sub(body.height);
    let Some(view) = &mut app.popups.sandbox_view else { return };
    view.scroll = view.scroll.min(most);
    f.render_widget(text.scroll((view.scroll, 0)), body);
    f.render_widget(Paragraph::new(keys), footer);
}

/// After writes ran in a transaction: commit or roll back? Its keys on as
/// many lines as the width needs, none cut.
pub(super) fn draw_review(f: &mut Frame, app: &App, area: Rect) {
    let t = theme();
    let Some(review) = &app.popups.review else { return };
    let rows = review.rows_affected;
    let chips = vec![
        vec![
            Span::styled(" c ", bold(t.on_accent).bg(t.ok)),
            Span::styled(format!(" {}   ", t!("review.commit")), fg(t.text)),
        ],
        vec![
            Span::styled(" r ", bold(t.on_accent).bg(t.error)),
            Span::styled(format!(" {}   ", t!("review.rollback")), fg(t.text)),
        ],
        vec![
            Span::styled(" Esc ", bold(t.text).bg(t.active)),
            Span::styled(format!(" {}", t!("review.keep_open")), muted()),
        ],
    ];
    // As wide as all on one line, if the screen has room.
    let one_line: usize = chips.iter().flatten().map(Span::width).sum();
    let width = fit(one_line as u16 + 4, 40, area.width.saturating_sub(4));
    let mut lines = vec![Line::from(Span::styled(tn!("review.changed", rows), bold(t.warn))), Line::default()];
    lines.extend(key_lines(chips, width.saturating_sub(2)));
    let text = Paragraph::new(lines).wrap(Wrap { trim: false });
    let height = (text.line_count(width.saturating_sub(2)) as u16 + 2).min(area.height);
    let rect = centered_box(area, width, height);
    f.render_widget(Clear, rect);
    f.render_widget(text.block(popup(t!("review.title"), t.warn)), rect);
}
