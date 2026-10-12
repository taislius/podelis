//! `QUERY`: line-number gutter, underlined current line, syntax
//! colours, and an execution-controls bar; plus the autocomplete popup.

use std::cell::RefCell;
use std::ops::Range;

use super::*;
use crate::analyze::HintSource;
use crate::db::Engine;
use crate::editor::Pos;
use crate::keys::Action;

pub(super) fn highlight_style(hl: Highlight) -> Style {
    let t = theme();
    match hl {
        Highlight::Plain => fg(t.text),
        Highlight::Keyword => fg(t.keyword),
        Highlight::Function => fg(t.function),
        Highlight::Table => fg(t.ident),
        Highlight::String => fg(t.string),
        Highlight::Number => fg(t.number),
        Highlight::Comment => fg(t.dim).add_modifier(Modifier::ITALIC),
    }
}

/// Draws the editor pane and returns the cursor's screen position.
pub(super) fn draw_editor(f: &mut Frame, app: &mut App, area: Rect) -> Option<(u16, u16)> {
    let t = theme();
    let focused = app.focus == Focus::Editor;
    let info = [
        app.zoomed.then_some("ZOOMED"),
        app.executing.then_some("RUNNING…"),
        app.vim_mode().map(crate::app::VimMode::label),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(" · ");
    // No line/column readout: the gutter already marks the current line.
    let mut block = pane("Query", &info, focused);
    if app.active_mode == ConnectionMode::Production {
        block = block.border_style(fg(t.error));
    }
    let inner = block.inner(area);
    f.render_widget(block, area);
    let [tabs, code, controls] = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(0), Constraint::Length(1)])
        .areas(inner);
    // Run buttons on the right of the tab row; the tabs get the rest.
    let buttons = run_buttons(app, tabs.width);
    let buttons_w = buttons.iter().map(|(label, _)| label.chars().count() as u16).sum::<u16>();
    let [tabs, run] = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(0), Constraint::Length(buttons_w)])
        .areas(tabs);
    app.hits.run_buttons = draw_run_buttons(f, &buttons, run);
    let (tab_hits, new_tab) = draw_tabs(f, app, tabs);
    app.hits.editor = area;
    app.hits.tabs = tab_hits;
    app.hits.tabs_y = tabs.y;
    app.hits.new_tab = new_tab;

    let gutter_width = (app.tab.editor.lines.len().to_string().len() as u16 + 2).max(4);
    let [gutter, text_area] = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(gutter_width.min(code.width)), Constraint::Min(0)])
        .areas(code);
    // One column of padding between gutter and text.
    let text_area = Rect {
        x: text_area.x.saturating_add(1).min(text_area.right()),
        width: text_area.width.saturating_sub(1),
        ..text_area
    };

    app.hits.editor_text = text_area;
    app.tab.editor.scroll_to_cursor(text_area.height as usize, text_area.width as usize);
    app.editor_height = text_area.height as usize;
    let (scroll_row, cursor_row) = (app.tab.editor.scroll_row, app.tab.editor.cursor_row);

    // Gutter, with the current line's number picked out.
    let numbers: Vec<Line> = (scroll_row..app.tab.editor.lines.len())
        .take(gutter.height as usize)
        .map(|row| {
            let style = if row == cursor_row { bold(t.accent).bg(t.selected) } else { dim() };
            Line::from(Span::styled(format!("{:>w$} ", row + 1, w = gutter_width as usize - 1), style))
        })
        .collect();
    f.render_widget(Paragraph::new(numbers).style(Style::default().bg(t.inset)), gutter);

    let lines = editor_lines(app, scroll_row, text_area.height as usize);
    f.render_widget(Paragraph::new(lines).scroll((0, app.tab.editor.scroll_col as u16)), text_area);

    // The current line: a faint underline across the pane, under the text
    // (cells already underlined, e.g. an error, keep theirs).
    if cursor_row >= scroll_row && ((cursor_row - scroll_row) as u16) < text_area.height {
        let y = text_area.y + (cursor_row - scroll_row) as u16;
        let buf = f.buffer_mut();
        for x in gutter.right()..code.right() {
            if let Some(cell) = buf.cell_mut((x, y))
                && !cell.modifier.contains(Modifier::UNDERLINED)
            {
                cell.modifier.insert(Modifier::UNDERLINED);
                cell.underline_color = t.line;
            }
        }
    }

    draw_controls(f, app, controls);

    if !focused || text_area.width == 0 || text_area.height == 0 {
        return None;
    }
    let x = text_area.x + (app.tab.editor.cursor_display_col() - app.tab.editor.scroll_col) as u16;
    let y = text_area.y + (app.tab.editor.cursor_row - app.tab.editor.scroll_row) as u16;
    // Under a popup (the command menu, a confirmation…) the terminal's
    // cursor would blink through it; autocomplete is typed in the editor.
    let covered = app.help_visible || app.top_overlay().is_some_and(|o| o != crate::app::Overlay::Completion);
    if !covered {
        f.set_cursor_position((x, y));
    }
    Some((x, y))
}

/// The visible rows of the editor's text (`count` from `first`) as styled
/// lines: syntax colours, then the error and hint underlines, find matches,
/// the bracket pair and the selection. Only the text up to the last visible
/// row is highlighted, so a long script costs little more than a short one.
fn editor_lines(app: &App, first: usize, count: usize) -> Vec<Line<'static>> {
    let t = theme();
    let text = app.tab.editor.text();
    let selection = app.tab.editor.selection();
    let selected = Style::default().bg(t.selection);
    let error =
        app.tab.error_marker.map(|(a, b)| (a, b, fg(t.error).add_modifier(Modifier::UNDERLINED | Modifier::BOLD)));
    // Underline the word the syntax hint points at.
    let hint = app.tab.syntax_hint.as_ref().and_then(|h| h.offset).map(|offset| {
        let start = offset.min(text.len());
        let word = text[start..].find(|c: char| c.is_whitespace()).unwrap_or(text.len() - start);
        let guessed = word.max(text[start..].chars().next().map_or(0, char::len_utf8));
        let end = (start + app.tab.syntax_hint.as_ref().and_then(|h| h.len).unwrap_or(guessed)).min(text.len());
        let color =
            if app.tab.syntax_hint.as_ref().is_some_and(|h| h.source == HintSource::Server) { t.error } else { t.warn };
        (
            app.tab.editor.pos_of_offset(start),
            app.tab.editor.pos_of_offset(end),
            fg(color).add_modifier(Modifier::UNDERLINED),
        )
    });
    // Find matches on screen (a common word can match tens of thousands of
    // times in a long script), and the bracket pair at the cursor.
    let found = Style::default().bg(t.selected).add_modifier(Modifier::UNDERLINED);
    let lines = &app.tab.editor.lines;
    let shown_start: usize = lines.iter().take(first).map(|l| l.len() + 1).sum();
    let shown_end: usize = shown_start + lines.iter().skip(first).take(count).map(|l| l.len() + 1).sum::<usize>();
    let matches = app.find_matches().0;
    let from = matches.partition_point(|m| m.end < shown_start);
    let to = matches.partition_point(|m| m.start <= shown_end);
    let marks: Vec<(Pos, Pos, Style)> = matches[from..to.max(from)]
        .iter()
        .map(|m| (app.tab.editor.pos_of_offset(m.start), app.tab.editor.pos_of_offset(m.end), found))
        .chain(app.tab.editor.matching_bracket().into_iter().flat_map(|(a, b)| {
            let style = bold(t.accent).bg(t.selected);
            [(a, (a.0, a.1 + 1), style), (b, (b.0, b.1 + 1), style)]
        }))
        .collect();
    // One row past the last visible one, so a name at the end of it can
    // still see a `(` on the next line (and be coloured as a function).
    let last = (first + count).min(app.tab.editor.lines.len());
    let end = app.tab.editor.lines.iter().take(last + 1).map(|l| l.len() + 1).sum::<usize>().min(text.len());
    let lines: Vec<Line> = highlighted_rows(&text, end, app.engine(), first, count)
        .into_iter()
        .enumerate()
        .map(|(i, frags)| {
            let row = first + i;
            let mut spans: Vec<Span<'static>> = frags
                .into_iter()
                .map(|(range, hl)| Span::styled(text[range].to_string(), highlight_style(hl)))
                .collect();
            for ((r0, c0), (r1, c1), style) in [error, hint].into_iter().flatten().chain(marks.iter().copied()) {
                if (r0..=r1).contains(&row) {
                    let from = if row == r0 { c0 } else { 0 };
                    let to = if row == r1 { c1 } else { usize::MAX };
                    spans = overlay(spans, from, to, style);
                }
            }
            if let Some(((r0, c0), (r1, c1))) = selection
                && (r0..=r1).contains(&row)
            {
                let from = if row == r0 { c0 } else { 0 };
                let to = if row == r1 { c1 } else { usize::MAX };
                spans = overlay(spans, from, to, selected);
                if row < r1 {
                    // Show the selected line break.
                    spans.push(Span::styled(" ", selected));
                }
            }
            Line::from(spans)
        })
        .collect();
    lines
}

/// Highlighted text, kept between frames: most frames (cursor moves,
/// scrolling, redraws) don't change the text, and an edit only changes the
/// lines from it on. Highlighting the start of a long script again on every
/// frame made typing lag.
struct Highlighted {
    engine: Engine,
    /// The text that was highlighted: the editor's, up to some row.
    text: String,
    /// Each line's fragments, as byte ranges into `text`.
    lines: Vec<Vec<(Range<usize>, Highlight)>>,
    /// Each line's start offset, and whether it's outside any comment,
    /// string or quoted name (so highlighting can restart there).
    starts: Vec<(usize, bool)>,
}

thread_local! {
    static HIGHLIGHTED: RefCell<Option<Highlighted>> = const { RefCell::new(None) };
}

/// Lines of context re-highlighted before an edit: enough for a table name
/// to see the FROM before it.
const CONTEXT_LINES: usize = 20;

/// Rows `first..first + count` of `text[..end]`, highlighted, reusing the
/// last frame's result for the lines before any change.
fn highlighted_rows(
    text: &str,
    end: usize,
    engine: Engine,
    first: usize,
    count: usize,
) -> Vec<Vec<(Range<usize>, Highlight)>> {
    HIGHLIGHTED.with_borrow_mut(|cache| {
        let cached = cache.take().filter(|c| c.engine == engine);
        let unchanged =
            cached.as_ref().is_some_and(|c| c.text.len() >= end && text.as_bytes().starts_with(c.text.as_bytes()));
        let fresh = if unchanged { cached.expect("checked above") } else { rehighlight(cached, text, end, engine) };
        let rows = fresh.lines.iter().skip(first).take(count).cloned().collect();
        *cache = Some(fresh);
        rows
    })
}

/// Highlights `text[..end]`, keeping `old`'s lines before the first change
/// (less one: a name there may see a `(` on the changed line) and redoing
/// the rest from a clean line start `CONTEXT_LINES` earlier.
fn rehighlight(old: Option<Highlighted>, text: &str, end: usize, engine: Engine) -> Highlighted {
    let (mut lines, mut starts, restart) = match old {
        Some(old) => {
            let same = common_prefix(old.text.as_bytes(), &text.as_bytes()[..end]);
            // The line the first change is on, then the one before it.
            let changed = old.starts.partition_point(|&(offset, _)| offset <= same).saturating_sub(1);
            let keep = changed.saturating_sub(1);
            let restart = (0..=keep.saturating_sub(CONTEXT_LINES)).rev().find(|&r| old.starts[r].1).unwrap_or(0);
            let (mut lines, mut starts) = (old.lines, old.starts);
            lines.truncate(keep);
            starts.truncate(keep);
            (lines, starts, restart)
        }
        None => (Vec::new(), Vec::new(), 0),
    };
    let from = starts.get(restart).map_or(0, |&(offset, _)| offset);
    let part = &text[from..end];
    let base = part.as_ptr() as usize;
    let (new_lines, clean) = sql::highlight_lines_and_starts(part, engine);
    // Line starts of the re-highlighted part, as offsets into `text`.
    let mut offset = from;
    let new_starts: Vec<(usize, bool)> = part
        .split('\n')
        .zip(clean)
        .map(|(line, clean)| {
            let start = offset;
            offset += line.len() + 1;
            (start, clean)
        })
        .collect();
    let skip = lines.len() - restart;
    lines.extend(new_lines.into_iter().skip(skip).map(|frags| {
        frags
            .into_iter()
            .map(|(s, hl)| {
                let start = from + (s.as_ptr() as usize - base);
                (start..start + s.len(), hl)
            })
            .collect()
    }));
    starts.extend(new_starts.into_iter().skip(skip));
    Highlighted { engine, text: text[..end].to_string(), lines, starts }
}

/// How many leading bytes `a` and `b` share, comparing in chunks.
fn common_prefix(a: &[u8], b: &[u8]) -> usize {
    const CHUNK: usize = 4096;
    let chunks = a.chunks(CHUNK).zip(b.chunks(CHUNK)).take_while(|(x, y)| x == y).count();
    let at = chunks * CHUNK;
    at + a[at.min(a.len())..].iter().zip(&b[at.min(b.len())..]).take_while(|(x, y)| x == y).count()
}

/// ` Find: foo█ │ Replace: bar   3/12 · ↵ next …` in place of the controls.
fn draw_find_bar(f: &mut Frame, app: &App, bar: &crate::app::FindBar, area: Rect) {
    let t = theme();
    let (matches, current) = app.find_matches();
    let field = |label: &str, input: &LineInput, active: bool| -> Vec<Span<'static>> {
        let mut spans =
            vec![Span::styled(format!(" {label} "), if active { bold(t.on_accent).bg(t.accent) } else { muted() })];
        spans.push(Span::raw(" "));
        if active {
            spans.extend(
                input_spans(input, fg(t.text_bright))
                    .into_iter()
                    .map(|s| Span::styled(s.content.into_owned(), s.style)),
            );
        } else {
            spans.push(Span::styled(input.text().to_string(), fg(t.text)));
        }
        spans.push(Span::raw(" "));
        spans
    };
    let count = match (matches.len(), current) {
        (0, _) if !bar.query.is_empty() => "no matches".to_string(),
        (0, _) => String::new(),
        (n, Some(i)) => format!("{}/{n}", i + 1),
        (n, None) => format!("{n} found"),
    };
    let mut spans = field("FIND", &bar.query, !bar.in_replace);
    spans.extend(field("REPLACE", &bar.replacement, bar.in_replace));
    spans.push(Span::styled(format!(" {count} "), bold(t.accent)));
    let help = if bar.in_replace {
        " ↵ replace · Ctrl+A all · Tab find · Esc"
    } else {
        " ↵/↓ next · ↑ prev · Tab replace · Esc"
    };
    spans.push(Span::styled(help, dim()));
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// The tab strip: ` SQL name ● │ SQL other │ + `, the active tab
/// highlighted; names shrink when many tabs are open.
/// Each tab's x range and index, for clicks.
type TabHits = Vec<(u16, u16, usize)>;

/// Returns where each tab, and the `+`, ended up.
fn draw_tabs(f: &mut Frame, app: &App, area: Rect) -> (TabHits, Option<(u16, u16)>) {
    let t = theme();
    f.render_widget(Block::default().style(Style::default().bg(t.inset)), area);
    let tab_count = app.tabs.len();
    let bar = if app.active_mode == ConnectionMode::Production {
        t.error
    } else if app.focus == Focus::Editor {
        t.accent
    } else {
        t.line
    };
    // The active tab's left line, drawn thick and joined to the pane's top
    // border above it; for the first tab it's the pane's own left border.
    let mark_line = |f: &mut Frame, x: u16, corner: bool| {
        if area.y == 0 || area.height == 0 || x >= f.area().right() {
            return;
        }
        for (y, symbol) in [(area.y - 1, if corner { "┎" } else { "┰" }), (area.y, "┃")] {
            let cell = &mut f.buffer_mut()[(x, y)];
            cell.set_symbol(symbol);
            cell.set_fg(bar);
        }
    };
    if app.active_tab == 0 && area.x > 0 {
        mark_line(f, area.x - 1, true);
    }
    // Room per tab: " SQL " + name + " ●" + "│".
    let budget = (area.width as usize).saturating_sub(4) / tab_count.max(1);
    let name_room = budget.saturating_sub(9).clamp(4, 24);
    let mut spans = Vec::new();
    let mut hits = Vec::new();
    let width = |spans: &[Span]| spans.iter().map(Span::width).sum::<usize>() as u16;
    for (i, tab) in app.tabs.iter().enumerate() {
        let start = area.x + width(&spans);
        let active = i == app.active_tab;
        // No background: the thick line and the brighter name mark the active tab.
        let bg = t.inset;
        // Orange warns that the tab isn't in the library; saved tabs stay quiet.
        let saved_marker = if tab.saved_id.is_some() { muted() } else { fg(t.warn) };
        spans.push(Span::styled(" SQL ", saved_marker.bg(bg)));
        let name_style = if active { fg(t.text_bright) } else { muted() };
        spans.push(Span::styled(truncate(&tab.name, name_room), name_style.bg(bg)));
        // Only a saved query's changes: other tabs are drafts, kept anyway.
        if tab.saved_id.is_some() && app.tab_dirty(i) {
            spans.push(Span::styled(" ●", fg(t.warn).bg(bg)));
        }
        spans.push(Span::styled(" ", Style::default().bg(bg)));
        hits.push((start, area.x + width(&spans), i));
        // The lines either side of the active tab are drawn thick, the colour
        // of the pane's border.
        if i + 1 == app.active_tab || active {
            let x = area.x + width(&spans);
            if x < area.right() {
                mark_line(f, x, false);
            }
            spans.push(Span::styled("┃", fg(bar)));
        } else {
            spans.push(Span::styled("│", fg(t.line)));
        }
    }
    let plus = area.x + width(&spans);
    spans.push(Span::styled(" + ", dim()));
    let new_tab = Some((plus, plus + 3)).filter(|(x0, _)| *x0 < area.right());
    // Key hints are in the status bar, not here.
    f.render_widget(Paragraph::new(Line::from(spans)), area);
    (hits, new_tab)
}

/// The run buttons that fit next to the tabs (`width`: the whole row),
/// Run always; Explain, then Statement, give way first.
fn run_buttons(app: &App, width: u16) -> Vec<(String, Action)> {
    let key = |a: Action| app.keymap.short_label(a);
    let mut buttons = vec![
        (format!(" ▶ {} Run ", key(Action::Run)), Action::Run),
        (format!("  {} Statement ", key(Action::RunStatement)), Action::RunStatement),
        (format!("  {} Explain ", key(Action::Explain)), Action::Explain),
    ];
    // Leave the tabs room for at least one tab and the `+`.
    let room = (width as usize).saturating_sub(24);
    while buttons.len() > 1 && buttons.iter().map(|(l, _)| l.chars().count()).sum::<usize>() > room {
        buttons.pop();
    }
    if buttons.iter().map(|(l, _)| l.chars().count()).sum::<usize>() > width as usize {
        buttons.clear();
    }
    buttons
}

/// Draws the run buttons; returns each one's x range and action, for clicks.
fn draw_run_buttons(f: &mut Frame, buttons: &[(String, Action)], area: Rect) -> Vec<(u16, u16, Action)> {
    let t = theme();
    let mut x = area.x;
    let mut hits = Vec::new();
    let mut spans = Vec::new();
    for (label, action) in buttons {
        let w = label.chars().count() as u16;
        // Quiet like the rest of the chrome; Run, the main one, in bold.
        let style = if *action == Action::Run { bold(t.text).bg(t.inset) } else { muted().bg(t.inset) };
        spans.push(Span::styled(label.clone(), style));
        hits.push((x, x + w, *action));
        x += w;
    }
    f.render_widget(Paragraph::new(Line::from(spans)).style(Style::default().bg(t.inset)), area);
    hits
}

/// Notes about typing (completions, the syntax hint) on the left, the write
/// modes on the right.
fn draw_controls(f: &mut Frame, app: &mut App, area: Rect) {
    let t = theme();
    f.render_widget(Block::default().style(Style::default().bg(t.inset)), area);
    app.hits.tx_mode = Rect::default();
    app.hits.syntax_hint = Rect::default();
    if let Some(bar) = &app.find {
        return draw_find_bar(f, app, bar, area);
    }
    let right = Line::from(tx_status(app, area.width));
    let right_w = right.width() as u16;
    let [l, r] = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(0), Constraint::Length(right_w.min(area.width))])
        .areas(area);
    let left = if let Some(note) = &app.editor_note {
        Line::from(Span::styled(format!(" {}", truncate(note, l.width as usize)), fg(t.text)))
    } else if let Some(hint) = &app.tab.syntax_hint {
        let (label, color, rule) = match hint.source {
            HintSource::Server => ("SERVER", t.error, ""),
            HintSource::Parser => ("HINT", t.warn, ""),
            HintSource::Lint(rule) => ("LINT", t.warn, rule),
        };
        // "LINT 2/3" when there are several; Ctrl+K w goes to the next.
        let label = match hint.nth {
            Some((nth, total)) if total > 1 => format!("{label} {nth}/{total}"),
            _ => label.to_string(),
        };
        // Which line it's about: often not the one being edited.
        let label = match hint.offset {
            Some(offset) => format!("{label} line {}", app.tab.editor.pos_of_offset(offset).0 + 1),
            None => label,
        };
        let message = one_line(&hint.message);
        let room = (l.width as usize).saturating_sub(label.chars().count() + 4);
        // The rule's name, so it's clear which check fired, when the message fits beside it.
        let rule = Some(format!(" · {rule}"))
            .filter(|r| !rule.is_empty() && message.chars().count() + r.chars().count() <= room)
            .unwrap_or_default();
        Line::from(vec![
            Span::styled(format!(" {label} "), bold(t.on_accent).bg(color)),
            Span::styled(format!(" {}", truncate(&message, room)), fg(color)),
            Span::styled(rule, dim()),
        ])
    } else {
        // The run keys are buttons in the tab row.
        Line::default()
    };
    f.render_widget(Paragraph::new(left), l);
    f.render_widget(Paragraph::new(right).alignment(Alignment::Right), r);
    // Clicking a hint goes to where it points.
    if app.editor_note.is_none() && app.tab.syntax_hint.as_ref().is_some_and(|h| h.offset.is_some()) {
        app.hits.syntax_hint = l;
    }
    // Clicking the write modes cycles them.
    if app.txn == TxnState::Idle {
        app.hits.tx_mode = r;
    }
}

/// The transaction modes to pick from, or, while a transaction is open,
/// what to do about it. (The status bar shows the
/// connection's production or read-only mode.)
fn tx_status(app: &App, width: u16) -> Vec<Span<'static>> {
    let t = theme();
    let key = |a: Action| app.keymap.short_label(a);
    let mut spans = match app.txn {
        TxnState::Open => vec![Span::styled(
            format!("● uncommitted · {} commit · {} undo", key(Action::Commit), key(Action::Rollback)),
            bold(t.warn),
        )],
        TxnState::Aborted => {
            vec![Span::styled(format!("✕ transaction failed · {} roll back", key(Action::Rollback)), bold(t.error))]
        }
        TxnState::Idle if app.active_mode == ConnectionMode::ReadOnly => {
            vec![Span::styled("Transaction:", muted()), Span::styled(" read-only", dim())]
        }
        // All three modes when there's room next to the run key, else the current one.
        TxnState::Idle => {
            let mut spans = vec![Span::styled("Transaction:", muted())];
            let all = width >= 72;
            for mode in [TxMode::AutoCommit, TxMode::Manual, TxMode::RollbackOnly] {
                if mode == app.tx_mode {
                    spans.push(Span::raw(" "));
                    // The current mode in bold orange, without a filled background.
                    spans.push(Span::styled(format!(" {} ", mode.short()), bold(t.warn)));
                } else if all {
                    spans.push(Span::styled(format!("  {} ", mode.short()), dim()));
                }
            }
            spans
        }
    };
    spans.push(Span::raw(" "));
    spans
}

/// The autocomplete list, just below the cursor (or above it when there's
/// no room).
pub(super) fn draw_completion(f: &mut Frame, app: &App, (cx, cy): (u16, u16), area: Rect) {
    let t = theme();
    let Some(popup_state) = &app.completion else { return };
    const MAX_ROWS: usize = 8;
    let rows = popup_state.items.len().min(MAX_ROWS);
    let first = popup_state.selected.saturating_sub(MAX_ROWS - 1).min(popup_state.items.len() - rows);
    let label_w = popup_state.items.iter().map(|i| i.label.chars().count()).max().unwrap_or(0);
    let detail_w = popup_state.items.iter().map(|i| i.detail.chars().count()).max().unwrap_or(0);
    let width = fit((label_w + detail_w + 5) as u16, 16, area.width);
    let height = rows as u16 + 2;
    let x = cx.saturating_sub(popup_state.prefix_len as u16).min(area.right().saturating_sub(width));
    let y = if cy + 1 + height <= area.bottom() { cy + 1 } else { cy.saturating_sub(height) };
    let rect = Rect { x, y, width, height: height.min(area.height) };

    let items: Vec<ListItem> = popup_state.items[first..first + rows]
        .iter()
        .map(|item| {
            let pad = label_w.saturating_sub(item.label.chars().count()) + 2;
            ListItem::new(Line::from(vec![
                Span::styled(format!("{}{}", item.label, " ".repeat(pad)), fg(t.text)),
                Span::styled(item.detail.clone(), dim()),
            ]))
        })
        .collect();
    let mut state = ListState::default().with_selected(Some(popup_state.selected - first));
    let more = if popup_state.items.len() > MAX_ROWS {
        format!(" {}/{} ", popup_state.selected + 1, popup_state.items.len())
    } else {
        String::new()
    };
    f.render_widget(Clear, rect);
    f.render_stateful_widget(
        List::new(items)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(fg(t.accent))
                    .style(Style::default().bg(t.panel))
                    .title_bottom(Line::from(Span::styled(more, dim())).right_aligned()),
            )
            .highlight_style(bold(t.text_bright).bg(t.selected)),
        rect,
        &mut state,
    );
}

/// Patches `style` onto chars `from..to` (char indices) of a line's spans.
pub(super) fn overlay(spans: Vec<Span<'static>>, from: usize, to: usize, style: Style) -> Vec<Span<'static>> {
    let mut out = Vec::with_capacity(spans.len() + 2);
    let mut pos = 0;
    for span in spans {
        let len = span.content.chars().count();
        let (start, end) = (from.clamp(pos, pos + len), to.clamp(pos, pos + len));
        if start == end {
            out.push(span);
        } else {
            let chars: Vec<char> = span.content.chars().collect();
            let piece = |a: usize, b: usize| chars[a - pos..b - pos].iter().collect::<String>();
            if start > pos {
                out.push(Span::styled(piece(pos, start), span.style));
            }
            out.push(Span::styled(piece(start, end), span.style.patch(style)));
            if end < pos + len {
                out.push(Span::styled(piece(end, pos + len), span.style));
            }
        }
        pos += len;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every row of `text`, highlighted from scratch.
    fn full(text: &str) -> Vec<Vec<(Range<usize>, Highlight)>> {
        HIGHLIGHTED.with_borrow_mut(|cache| *cache = None);
        highlighted_rows(text, text.len(), Engine::Postgres, 0, usize::MAX)
    }

    #[test]
    fn edits_rehighlight_from_just_before_them_and_match_a_full_pass() {
        let mut text: String = (0..300)
            .map(|i| {
                // Most lines end with a bare name, so a `(` at the start of
                // the next one makes it a function call.
                if i % 3 == 0 {
                    format!("select a{i}, count(*) from t{i} where note = 'x{i}'; -- row {i}\n")
                } else {
                    format!("select a{i} from\n t{i} where b = fn{i}\n")
                }
            })
            .collect();
        let row = |text: &str, n: usize| text.match_indices('\n').nth(n - 1).map_or(0, |(i, _)| i + 1);
        type Edit<'a> = Box<dyn Fn(&mut String) + 'a>;
        let edits: Vec<Edit<'_>> = vec![
            // Typing near the end.
            Box::new(|t| t.insert(row(t, 290) + 7, 'x')),
            // Opening a block comment far up colours everything after it.
            Box::new(|t| t.insert_str(row(t, 100), "/* ")),
            // Closing it again.
            Box::new(|t| t.insert_str(row(t, 150), " */")),
            // A `(` after a name on the line before the edit.
            Box::new(|t| t.insert(row(t, 200), '(')),
            // Deleting lines.
            Box::new(|t| t.replace_range(row(t, 50)..row(t, 60), "")),
            // A string that runs across lines.
            Box::new(|t| t.insert_str(row(t, 250) + 3, "'open\nstill ")),
        ];
        // Seed the cache, then edit, comparing each time.
        let _ = highlighted_rows(&text, text.len(), Engine::Postgres, 280, 40);
        for (n, edit) in edits.iter().enumerate() {
            edit(&mut text);
            let incremental = highlighted_rows(&text, text.len(), Engine::Postgres, 0, usize::MAX);
            assert_eq!(incremental, full(&text), "edit {n} differs from a full highlight");
            // Leave the cache from the incremental pass for the next edit.
            let _ = highlighted_rows(&text, text.len(), Engine::Postgres, 0, usize::MAX);
        }
        // Scrolling past the cached part extends it.
        let half = row(&text, 150);
        let _ = full(&text[..half]);
        let rows = highlighted_rows(&text, text.len(), Engine::Postgres, 0, usize::MAX);
        assert_eq!(rows, full(&text));
    }
}
