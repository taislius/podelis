//! One-line prompts and small forms: a row's values, a pipe's command, a
//! question for the AI, a tab's name, a query's parameters.

use super::*;

/// The add-row form: one line per column, with its type.
pub(super) fn draw_row_form(f: &mut Frame, app: &App, area: Rect) {
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
pub(super) fn draw_pipe_prompt(f: &mut Frame, command: &LineInput, area: Rect) {
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
pub(super) fn draw_ask_prompt(f: &mut Frame, app: &App, area: Rect) {
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

/// The save-as / rename prompt.
pub(super) fn draw_tab_prompt(f: &mut Frame, app: &App, area: Rect) {
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

/// The query parameters form: one line per placeholder.
pub(super) fn draw_param_form(f: &mut Frame, app: &App, area: Rect) {
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
