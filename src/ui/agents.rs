//! The agent activity window: what AI agents did through `podelis mcp`, by
//! session, and one session's calls.

use super::*;
use crate::app::{short_dir, short_time};

pub(super) fn draw_agents(f: &mut Frame, app: &App, area: Rect) {
    let t = theme();
    let Some(view) = &app.agents.view else { return };
    let session = view.session.and_then(|i| app.agents.sessions.get(i));
    let rect = centered_box(area, area.width.saturating_sub(6).max(40), area.height.saturating_sub(4).max(8));
    f.render_widget(Clear, rect);
    let title = match session {
        Some(s) => format!("{} · {}", t!("agents.title"), s.label()),
        None => t!("agents.title").to_string(),
    };
    let keys = match session {
        Some(_) => format!(" {} ", t!("agents.call_keys")),
        None => format!(" {} ", t!("agents.session_keys")),
    };
    let block = popup(&title, t.accent).title_bottom(Line::from(Span::styled(keys, dim())).right_aligned());
    let inner = block.inner(rect);
    f.render_widget(block, rect);
    let [top, list] = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(if session.is_some() { 2 } else { 1 }), Constraint::Min(0)])
        .areas(inner);
    let w = inner.width as usize;
    let rows = app.agents_rows();

    // The search, and what's shown.
    let what = match session {
        Some(s) => t!("agents.calls_of", shown = rows.len(), total = s.calls()),
        None => t!("agents.sessions_of", shown = rows.len(), total = app.agents.sessions.len()),
    };
    let only = if view.flagged_only { format!(" · {}", t!("agents.only_flagged")) } else { String::new() };
    let mut search = vec![Span::styled(" / ", bold(t.accent))];
    if view.filtering || !view.filter.text().is_empty() {
        search.extend(input_spans(&view.filter, fg(t.text_bright)));
    }
    search.push(Span::styled(format!("   {what}{only}"), dim()));
    let mut head = vec![Line::from(search)];
    if let Some(s) = session {
        let mut about = vec![format!("{} {}", s.agent, s.version.as_deref().unwrap_or_default()).trim().to_string()];
        if !s.id.is_empty() {
            about.push(t!("agents.session", id = s.id));
        }
        if let Some(dir) = &s.dir {
            about.push(short_dir(dir));
        }
        about.push(if s.first == s.last {
            short_time(&s.last)
        } else {
            format!("{} – {}", short_time(&s.first), short_time(&s.last))
        });
        head.push(Line::from(Span::styled(truncate(&format!(" {}", about.join(" · ")), w), muted())));
    }
    f.render_widget(Paragraph::new(head).style(Style::default().bg(t.inset)), top);

    if rows.is_empty() {
        let empty = if app.agents.sessions.is_empty() {
            format!(" {}", t!("agents.none_yet"))
        } else {
            format!(" {}", t!("agents.no_matches"))
        };
        f.render_widget(Paragraph::new(empty).style(dim()).wrap(Wrap { trim: false }), list);
        return;
    }
    let items: Vec<ListItem> = rows
        .iter()
        .map(|&i| match session {
            None => session_row(&app.agents.sessions[i], w),
            Some(_) => call_row(&app.agents.log[i], w),
        })
        .collect();
    let mut state = ListState::default().with_selected(Some(view.selected.min(items.len() - 1)));
    f.render_stateful_widget(List::new(items).highlight_style(Style::default().bg(t.active)), list, &mut state);
}

/// `! 10-13 10:05  billing (claude-code 2.0)  ~/src/shop  shop, prod  14 calls  2 refused`
fn session_row(s: &crate::agents::Session, width: usize) -> ListItem<'static> {
    let t = theme();
    let issues: Vec<String> = [
        (s.refused, t!("agents.refused", n = s.refused)),
        (s.failed, t!("agents.failed", n = s.failed)),
        (s.wrote, t!("agents.wrote", n = s.wrote)),
        (s.dry_runs, tn!("agents.dry_runs", s.dry_runs)),
    ]
    .into_iter()
    .filter(|(n, _)| *n > 0)
    .map(|(_, said)| said)
    .collect();
    let flagged = !issues.is_empty();
    let who = match (&s.name, &s.version) {
        (Some(_), Some(v)) => format!("{} ({} {v})", s.label(), s.agent),
        (Some(_), None) => format!("{} ({})", s.label(), s.agent),
        (None, Some(v)) => format!("{} {v}", s.label()),
        (None, None) => s.label().to_string(),
    };
    let who = if s.id.is_empty() { format!("{who} · {}", t!("agents.earlier")) } else { who };
    let dir = s.dir.as_deref().map(short_dir).unwrap_or_default();
    let issues = if flagged { format!("  {}", issues.join(" · ")) } else { String::new() };
    let calls = tn!("agents.calls", s.calls());
    let main = format!("{}  {who}  {dir}  {}  {calls}", short_time(&s.last), s.connections.join(", "));
    let room = width.saturating_sub(2 + issues.chars().count());
    ListItem::new(Line::from(vec![
        Span::styled(if flagged { "! " } else { "» " }, if flagged { bold(t.warn) } else { dim() }),
        Span::styled(truncate(&main, room), fg(t.text)),
        Span::styled(issues, fg(t.warn)),
    ]))
}

/// `! 10:05:12  prod  query  refused: …  3 ms  update users set …`
fn call_row(q: &crate::app::AgentQuery, width: usize) -> ListItem<'static> {
    let t = theme();
    let e = &q.entry;
    let flagged = q.notice().is_some();
    let time = e.at.get(11..19).unwrap_or(&e.at).to_string();
    let head = format!("{time}  {}  {}  ", e.connection, e.tool);
    let outcome = format!("{}  {} ms  ", truncate(&one_line(&q.outcome()), 48), e.ms);
    let room = width.saturating_sub(2 + head.chars().count() + outcome.chars().count());
    ListItem::new(Line::from(vec![
        Span::styled(if flagged { "! " } else { "» " }, if flagged { bold(t.warn) } else { dim() }),
        Span::styled(head, muted()),
        Span::styled(outcome, if flagged { fg(t.warn) } else { dim() }),
        Span::styled(truncate(&one_line(&e.sql), room), fg(t.text)),
    ]))
}
