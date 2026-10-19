//! The settings screen (`Ctrl+K ,`): each setting with its value, what the
//! selected one does, and what the last change did.

use super::results::wrap_words;
use super::*;
use crate::app::settings_view::{Field, Kind};

pub(super) fn draw_settings(f: &mut Frame, app: &App, area: Rect) {
    let t = theme();
    let Some(view) = &app.settings_view else { return };
    let label_w = Field::ALL.iter().map(|f| f.label().chars().count()).max().unwrap_or(0);
    let width = fit(78, 40, area.width);
    let value_w = usize::from(width).saturating_sub(label_w + 10);
    let mut lines: Vec<Line> = Field::ALL
        .iter()
        .enumerate()
        .map(|(i, &field)| {
            let focused = i == view.selected;
            let marker = if focused { Span::styled(" › ", bold(t.accent)) } else { Span::raw("   ") };
            let name = Span::styled(
                format!("{:<label_w$}  ", field.label()),
                if focused { bold(t.accent) } else { fg(t.text) },
            );
            if let (Some(input), true) = (&view.editing, focused) {
                let spans = input_spans(input, fg(t.text_bright).bg(t.active));
                return Line::from([vec![marker, name], spans].concat());
            }
            let shown = truncate(&app.setting_shown(field), value_w);
            let changeable = matches!(field.kind(), Kind::Choice | Kind::Toggle);
            let text = if focused && changeable { format!("‹ {shown} ›") } else { shown };
            let value = Span::styled(text, if focused { bold(t.text_bright) } else { muted() });
            Line::from(vec![marker, name, value])
        })
        .collect();
    lines.push(Line::default());
    let field = Field::ALL[view.selected.min(Field::ALL.len() - 1)];
    let room = usize::from(width).saturating_sub(4);
    for part in wrap_words(field.about(), room) {
        lines.push(Line::from(Span::styled(format!(" {part}"), dim())));
    }
    lines.push(Line::default());
    let (said, style) = match &view.said {
        Some((said, is_error)) => (said.as_str(), message_style(*is_error)),
        None => (t!("settings.in_the_file"), dim()),
    };
    for part in wrap_words(said, room) {
        lines.push(Line::from(Span::styled(format!(" {part}"), style)));
    }
    let keys = if view.editing.is_some() { t!("settings.typing_keys") } else { t!("settings.keys") };
    lines.push(Line::from(Span::styled(format!(" {keys}"), dim())));
    let rect = centered_box(area, width, (lines.len() as u16 + 2).min(area.height));
    f.render_widget(Clear, rect);
    f.render_widget(Paragraph::new(lines).block(popup(t!("settings.title"), t.accent)), rect);
}
