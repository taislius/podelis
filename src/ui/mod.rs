//! Drawing, following the "SQL operator console" design: an application bar
//! on top, panes (`NAVIGATOR` …) in the middle and a
//! status bar with key hints at the bottom.

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{
    Block, BorderType, Borders, Cell, Clear, List, ListItem, ListState, Paragraph, Row, Table, TableState, Wrap,
};

use crate::app::{
    App, ConnectField, Focus, LibraryItem, LineInput, Overlay, PromptKind, ResultTab, SchemaNode, SchemaSection,
    Screen, TabPromptKind, TxMode, display_prefix, visible_columns,
};
use crate::config::store::ConnectionProfile;
use crate::config::theme::theme;
use crate::db::{ConnectionMode, TxnState};
use crate::language::Highlight;

/// Narrowest the schema navigator gets.
const SCHEMA_WIDTH: u16 = 30;

mod agents;
mod bars;
mod connect;
mod help;
mod manager;
mod popups;
mod query_screen;
mod settings;
mod splash;
#[cfg(test)]
mod tests;

pub fn draw(f: &mut Frame, app: &mut App) {
    let area = f.area();
    f.render_widget(Block::default().style(Style::default().bg(theme().canvas).fg(theme().text)), area);
    let [bar, body, status] = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(0), Constraint::Length(1)])
        .areas(area);
    bars::draw_app_bar(f, app, bar);
    match app.screen {
        Screen::Projects | Screen::Connections | Screen::Connect => manager::draw_manager(f, app, body),
        Screen::Query => query_screen::draw_query(f, app, body),
    }
    bars::draw_status_bar(f, app, status);
    // Leaving the project with a transaction open asks first.
    if !matches!(app.screen, Screen::Query) && app.popups.confirm.is_some() {
        popups::draw_confirm(f, app, area);
    }
    // The text view also shows over other screens (the connection test).
    if !matches!(app.screen, Screen::Query) && app.popups.cell_view.is_some() {
        popups::draw_cell_view(f, app, area);
    }
    if app.splash.as_ref().is_some_and(|s| s.shown) {
        splash::draw_splash(f, app, area);
    }
    if app.help.visible {
        help::draw_help(f, app, area);
    }
}

/// A text input's spans: the text, with the cursor as a block at the end
/// or as the character under it, reversed.
pub(super) fn input_spans(input: &LineInput, style: Style) -> Vec<Span<'_>> {
    let (before, after) = input.split();
    let mut rest = after.chars();
    let cursor = match rest.next() {
        Some(c) => Span::styled(c.to_string(), style.add_modifier(Modifier::REVERSED)),
        None => Span::styled("█", style),
    };
    vec![Span::styled(before, style), cursor, Span::styled(rest.as_str(), style)]
}

/// `input_spans` in `room` chars: when it's longer, the part around the
/// cursor, `…` where it's cut.
pub(super) fn input_window(input: &LineInput, room: usize, style: Style) -> Vec<Span<'static>> {
    let (before, after) = input.split();
    let (before, after): (Vec<char>, Vec<char>) = (before.chars().collect(), after.chars().collect());
    // The cursor takes a cell of its own at the end.
    let total = before.len() + after.len().max(1);
    if total <= room || room < 3 {
        return input_spans(input, style).into_iter().map(|s| Span::styled(s.content.into_owned(), s.style)).collect();
    }
    // As much before the cursor as fits with it, and a few after it.
    let after_kept = after.len().min((room / 4).max(1));
    let start = before.len().saturating_sub(room.saturating_sub(after_kept + 1));
    let cut_start = start > 0;
    let mut spans = Vec::new();
    if cut_start {
        spans.push(Span::styled("…", style));
    }
    let shown_before: String = before[start + usize::from(cut_start)..].iter().collect();
    spans.push(Span::styled(shown_before, style));
    let used = spans.iter().map(|s| s.content.chars().count()).sum::<usize>();
    let left = room.saturating_sub(used);
    match after.first() {
        Some(c) => spans.push(Span::styled(c.to_string(), style.add_modifier(Modifier::REVERSED))),
        None => spans.push(Span::styled("█", style)),
    }
    let rest: Vec<char> = after.iter().skip(1).copied().collect();
    let fits = left.saturating_sub(1);
    if rest.len() > fits {
        let shown: String = rest[..fits.saturating_sub(1)].iter().collect();
        spans.push(Span::styled(shown + "…", style));
    } else {
        spans.push(Span::styled(rest.into_iter().collect::<String>(), style));
    }
    spans
}

/// `value` raised to `min`, then capped at `max` (which wins when the
/// terminal is smaller than `min`; `u16::clamp` would panic instead).
pub(super) fn fit(value: u16, min: u16, max: u16) -> u16 {
    value.max(min).min(max)
}

pub(super) fn centered_box(area: Rect, width: u16, height: u16) -> Rect {
    let vertical_pad = area.height.saturating_sub(height) / 2;
    let horizontal_pad = area.width.saturating_sub(width) / 2;
    Rect {
        x: area.x + horizontal_pad,
        y: area.y + vertical_pad,
        width: width.min(area.width),
        height: height.min(area.height),
    }
}

pub(super) fn fg(color: ratatui::style::Color) -> Style {
    Style::default().fg(color)
}

pub(super) fn dim() -> Style {
    fg(theme().dim)
}

pub(super) fn muted() -> Style {
    fg(theme().muted)
}

pub(super) fn bold(color: ratatui::style::Color) -> Style {
    fg(color).add_modifier(Modifier::BOLD)
}

pub(super) fn message_style(is_error: bool) -> Style {
    fg(if is_error { theme().error } else { theme().ok })
}

/// A pane, e.g. `SQL`, with optional info on the right of its top border.
/// The focused pane gets an accent border and title.
pub(super) fn pane<'a>(title: &'a str, info: &str, focused: bool) -> Block<'a> {
    let t = theme();
    // Unfocused panes recede: a muted title and a quiet border, so only
    // the focused one draws the eye.
    let (label, label_bg) = if focused { (bold(t.accent), t.active) } else { (muted(), t.inset) };
    let border = if focused { t.accent } else { t.line };
    let mut block = Block::default()
        .borders(Borders::ALL)
        .border_style(fg(border))
        .style(Style::default().bg(t.panel).fg(t.text))
        .title(Span::styled(format!(" {} ", title.to_uppercase()), label.bg(label_bg)));
    if !info.is_empty() {
        block = block.title(Line::from(Span::styled(format!(" {info} "), muted())).right_aligned());
    }
    block
}

/// A popup frame (dialogs, help, cell view).
pub(super) fn popup(title: &str, color: ratatui::style::Color) -> Block<'_> {
    let t = theme();
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Plain)
        .border_style(fg(color))
        .style(Style::default().bg(t.panel).fg(t.text));
    if title.is_empty() { block } else { block.title(Span::styled(format!(" {title} "), bold(t.text_bright))) }
}

/// A boxed key hint, e.g. `[F5] run`; `primary` hints are filled with the accent.
pub(super) fn chip(key: &str, label: &str, primary: bool) -> Vec<Span<'static>> {
    let t = theme();
    let key_style = if primary { bold(t.on_accent).bg(t.accent) } else { bold(t.text).bg(t.active) };
    vec![
        Span::styled(format!(" {key} "), key_style),
        Span::styled(format!(" {label}  "), fg(if primary { t.text } else { t.muted })),
    ]
}

/// A filled label, e.g. ` PRODUCTION `.
pub(super) fn badge(text: &str, fg_color: ratatui::style::Color, bg: ratatui::style::Color) -> Span<'static> {
    Span::styled(format!(" {text} "), bold(fg_color).bg(bg))
}

pub(super) fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Shortens `s` to `max` chars with an ellipsis.
pub(super) fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        s.chars().take(max.saturating_sub(1)).collect::<String>() + "…"
    }
}

/// `s` in `max` chars, in `color`: whole if it fits, else cut with its
/// last few chars fading into the pane's background before the `…`, so
/// the cut reads as "more here" rather than as the statement's end.
pub(super) fn fading(
    s: &str,
    max: usize,
    color: fn(&crate::config::theme::Theme) -> ratatui::style::Color,
) -> Vec<Span<'static>> {
    const STEPS: usize = 6;
    let t = theme();
    if s.chars().count() <= max {
        return vec![Span::styled(s.to_string(), fg(color(t)))];
    }
    let kept: Vec<char> = s.chars().take(max.saturating_sub(1)).collect();
    let steps = STEPS.min(kept.len() / 3);
    let (plain, faded) = kept.split_at(kept.len() - steps);
    let mut spans = vec![Span::styled(plain.iter().collect::<String>(), fg(color(t)))];
    let colors = crate::config::theme::fade(color, |t| t.panel, steps + 1);
    spans.extend(faded.iter().zip(&colors).map(|(c, &shade)| Span::styled(c.to_string(), fg(shade))));
    if max > 0 {
        spans.push(Span::styled("…", fg(colors.last().copied().unwrap_or(t.dim))));
    }
    spans
}

/// Shortens `s` to `cols` screen columns with an ellipsis (wide
/// characters, CJK or emoji, take two).
pub(super) fn truncate_cols(s: &str, cols: usize) -> String {
    use unicode_width::{UnicodeWidthChar as _, UnicodeWidthStr as _};
    if s.width() <= cols {
        return s.to_string();
    }
    let mut out = String::new();
    let mut used = 0;
    for c in s.chars() {
        let w = c.width().unwrap_or(0);
        if used + w + 1 > cols {
            break;
        }
        out.push(c);
        used += w;
    }
    out + "…"
}

/// Shortens `s` to `max` chars from the start, keeping its end (a
/// path's last folders).
pub(super) fn truncate_start(s: &str, max: usize) -> String {
    let len = s.chars().count();
    if len <= max {
        s.to_string()
    } else {
        "…".to_string() + &s.chars().skip(len - max.saturating_sub(1)).collect::<String>()
    }
}

/// A size in bytes, e.g. `12.4 MB` (powers of 1024, as `du -h`).
pub(super) fn human_bytes(n: i64) -> String {
    crate::db::human_bytes(u64::try_from(n).unwrap_or(0))
}

/// Compact counts like the design's `24.8k` / `1.1m`.
#[expect(clippy::cast_precision_loss, reason = "display only, one decimal")]
pub(super) fn human_count(n: i64) -> String {
    match n {
        n if n >= 1_000_000 => format!("{:.1}m", n as f64 / 1e6),
        n if n >= 1_000 => format!("{:.1}k", n as f64 / 1e3),
        n => n.to_string(),
    }
}
