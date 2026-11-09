//! The app's badge (`>_  PODĖLIS v…`), and the card shown while the last
//! session's connections reopen.

use super::*;

/// The version in the badge. Tests draw a fixed one, so a release doesn't
/// change every snapshot.
const VERSION: &str = if cfg!(test) { "0.0.0" } else { env!("CARGO_PKG_VERSION") };

/// The app's badge: `>_  PODĖLIS v1.2.3`, dark on green.
pub(super) fn brand() -> Vec<Span<'static>> {
    let t = theme();
    vec![
        Span::styled(" >_  PODĖLIS ", bold(t.on_accent).bg(t.accent)),
        Span::styled(format!("v{VERSION} "), fg(t.on_accent).bg(t.accent)),
    ]
}

/// While the last session's connections are reopened (if that takes a
/// moment): which are back, which failed, which are still coming.
pub(super) fn draw_splash(f: &mut Frame, app: &App, area: Rect) {
    let t = theme();
    let Some(splash) = &app.splash else { return };
    let mut lines = vec![
        Line::default(),
        Line::from(brand()).alignment(Alignment::Center),
        Line::default(),
        Line::from(vec![
            Span::styled(format!("{} ", t!("splash.reopening")), muted()),
            Span::styled(splash.project.clone(), bold(t.text)),
        ])
        .alignment(Alignment::Center),
        Line::default(),
    ];
    for (name, error) in &splash.done {
        lines.push(Line::from(match error {
            None => vec![Span::styled("  ✓ ", fg(t.ok)), Span::styled(name.clone(), fg(t.text))],
            Some(e) => vec![
                Span::styled("  ✕ ", fg(t.error)),
                Span::styled(name.clone(), fg(t.text)),
                Span::styled(format!("  {}", truncate(&one_line(e), 36)), fg(t.error)),
            ],
        }));
    }
    for name in app.reconnecting() {
        lines.push(Line::from(vec![Span::styled("  ◌ ", fg(t.warn)), Span::styled(format!("{name}…"), muted())]));
    }
    lines.push(Line::default());
    lines.push(Line::from(Span::styled(t!("splash.any_key"), dim())).alignment(Alignment::Center));
    let width = fit(lines.iter().map(Line::width).max().unwrap_or(0) as u16 + 6, 40, area.width);
    let rect = centered_box(area, width, lines.len() as u16 + 2);
    f.render_widget(Clear, rect);
    f.render_widget(Paragraph::new(lines).block(popup("", t.accent)), rect);
}
