//! The bars around the screens: the top bar (the badge, the connection,
//! its state) and the status bar (messages, the AI, the keys to press).

use super::splash::brand;
use super::*;
use crate::config::keys::Action;
use crate::config::settings::Hints;

/// A connection's name, or for an unsaved one its file or host/database.
pub(super) fn draw_app_bar(f: &mut Frame, app: &mut App, area: Rect) {
    let t = theme();
    f.render_widget(Block::default().style(Style::default().bg(t.panel)), area);

    // Identity and breadcrumb on the left.
    let mut left = brand();
    left.push(Span::styled("  │  ", fg(t.line)));
    let project = app.project().map(|p| p.name.clone());
    let crumbs: Vec<String> = match app.screen {
        Screen::Projects => vec![t!("bar.projects_and_connections").into()],
        Screen::Connections => vec![project.clone().unwrap_or_default(), t!("hint.connections").into()],
        Screen::Connect => vec![
            project.clone().unwrap_or_default(),
            if app.connection.form.original_name.is_some() {
                app.connection.form.name.clone()
            } else {
                t!("bar.new_connection").into()
            },
        ],
        // The status bar shows the connection.
        Screen::Query => vec![project.clone().unwrap_or_default()],
    };
    // No project (a connection opened from the command line): no crumb.
    let crumbs: Vec<String> = crumbs.into_iter().filter(|c| !c.is_empty()).collect();
    let last = crumbs.len().saturating_sub(1);
    app.hits.project = Rect::default();
    // The first crumb is the project (except on the project list itself).
    let named_project = project.is_some() && !matches!(app.screen, Screen::Projects);
    for (i, crumb) in crumbs.into_iter().enumerate() {
        if i > 0 {
            left.push(Span::styled(" / ", dim()));
        } else if named_project {
            left.push(Span::styled(format!("{} ", t!("bar.project")), muted()));
        }
        // The project is a switch: clicking it shows the projects.
        if named_project && i == 0 {
            let x = area.x + Line::from(left.clone()).width() as u16;
            let label = format!(" {crumb} ▼ ");
            app.hits.project = Rect { x, width: label.chars().count() as u16, ..area };
            left.push(Span::styled(label, muted().bg(t.inset)));
            continue;
        }
        left.push(if i == last { Span::styled(crumb, bold(t.text)) } else { Span::styled(crumb, muted()) });
    }

    // Connecting from the form; on the query screen the status bar has
    // the connection.
    let right = if app.connection.form.connecting && !matches!(app.screen, Screen::Query) {
        vec![Span::styled(format!(" ◌ {} ", t!("bar.connecting")), bold(t.warn).bg(t.active)), Span::raw(" ")]
    } else {
        Vec::new()
    };
    let right_width = Line::from(right.clone()).width() as u16;
    let [l, r] = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(0), Constraint::Length(right_width.min(area.width))])
        .areas(area);
    f.render_widget(Paragraph::new(Line::from(left)), l);
    f.render_widget(Paragraph::new(Line::from(right)).alignment(Alignment::Right), r);
}

/// Where the connection goes: `user@host:port` (and the SSH host), or the
/// SQLite file.
fn endpoint(form: &crate::app::ConnectForm) -> String {
    if form.engine.is_file_based() {
        return home_short(&form.host);
    }
    let user = if form.user.is_empty() { String::new() } else { format!("{}@", form.user) };
    let via = if form.ssh.trim().is_empty() { String::new() } else { format!(" via {}", form.ssh.trim()) };
    format!("{user}{}:{}{via}", form.host, form.port)
}

/// `path` with the home folder written `~`.
fn home_short(path: &str) -> String {
    match std::env::var("HOME").ok().filter(|h| h.len() > 1).and_then(|h| path.strip_prefix(&h).map(str::to_string)) {
        Some(rest) if rest.is_empty() || rest.starts_with('/') => format!("~{rest}"),
        _ => path.to_string(),
    }
}

/// The status bar's connection: state, name, where it goes, the server,
/// latency and mode. All of it, once, in one place.
fn connection_status(app: &App, room: usize) -> Vec<Span<'static>> {
    let t = theme();
    if !app.connected() {
        return if app.reconnecting().is_empty() {
            let key = app.keymap.short_label(Action::OpenConnection);
            vec![
                Span::styled(format!("○ {}", t!("bar.not_connected")), muted()),
                Span::styled(format!(" · {key} {}  ", t!("hint.connect")), dim()),
            ]
        } else {
            vec![Span::styled(format!("◌ {}  ", t!("bar.reconnecting")), bold(t.warn))]
        };
    }
    let (dot, name_style) =
        if app.keepalive.connection_lost { (fg(t.error), bold(t.error)) } else { (fg(t.ok), bold(t.ok)) };
    let mut spans = vec![
        Span::styled(if app.settings.keepalive > 0 { "♥ " } else { "● " }, dot),
        Span::styled(app.connection.form.short_name(), name_style),
    ];
    if app.keepalive.connection_lost {
        spans.push(Span::styled(format!(" · {}", t!("bar.connection_lost")), bold(t.error)));
    }
    match app.connection.mode {
        ConnectionMode::Production => spans.extend([Span::raw(" "), badge(t!("bar.production"), t.on_accent, t.error)]),
        ConnectionMode::ReadOnly => spans.extend([Span::raw(" "), badge(t!("bar.read_only"), t.on_accent, t.ok)]),
        ConnectionMode::Normal => {}
    }
    let engine = app.connection.form.engine.label();
    let version = app.connection.database.as_ref().map(|d| d.version().to_string()).unwrap_or_default();
    let mut rest = format!(" · {engine} {version}");
    if let Some(latency) = app.connection.latency {
        rest = format!("{} · {} ms", rest.trim_end(), latency.as_millis());
    }
    // Long paths and hosts give way to the messages in the middle: a
    // file's path from its end (the file's name stays), a host from its.
    let used: usize = spans.iter().map(Span::width).sum();
    let room = room.saturating_sub(used + 4);
    let at = endpoint(&app.connection.form);
    let details = if app.connection.form.engine.is_file_based() {
        let fits = room.saturating_sub(rest.trim_end().chars().count()).max(12);
        format!("{}{}", truncate_start(&at, fits), rest.trim_end())
    } else {
        truncate(&format!("{at}{}", rest.trim_end()), room)
    };
    spans.push(Span::styled(format!("  {details}  "), muted()));
    spans
}

/// Vim's mode, the connection, then a status message or key hints, then
/// metrics.
pub(super) fn draw_status_bar(f: &mut Frame, app: &mut App, area: Rect) {
    let t = theme();
    f.render_widget(Block::default().style(Style::default().bg(t.inset)), area);

    // The focused pane's border shows where you are; only vim's mode needs
    // saying.
    let vim = matches!(app.screen, Screen::Query) && app.focus == Focus::Editor;
    let mut left = match app.vim_mode().filter(|_| vim) {
        Some(mode) => vec![Span::styled(format!(" {} ", mode.label()), bold(t.on_accent).bg(t.accent)), Span::raw(" ")],
        None => Vec::new(),
    };
    left.push(Span::raw(" "));
    let mode_w = Line::from(left.clone()).width() as u16;
    if matches!(app.screen, Screen::Query) {
        // Leave the middle for messages: up to half the bar for the
        // connection, down to a third while a long message needs the room.
        let width = area.width as usize;
        let message = app.notes.status.chars().count() + 6;
        let room = width.saturating_sub(message).clamp(width / 3, width / 2).max(24);
        left.extend(connection_status(app, room));
    }

    // Rows and timing are in the results pane. What agents did that's worth
    // a look since the agent window was last open (a click opens it).
    let mut right: Vec<Span> = Vec::new();
    if let Some(note) = app.agent_note().filter(|_| matches!(app.screen, Screen::Query)) {
        right.push(Span::styled(format!(" {note} "), bold(t.warn)));
    }
    // While no message needs the room: whether AI is on, beside the
    // connection's badges (READ-ONLY…), and Ctrl+K at the right edge.
    let in_results = app.notes.status_in_results && app.hits.results.width > 0;
    let quiet = app.notes.status.is_empty() || in_results;
    if matches!(app.screen, Screen::Query) && quiet {
        left.extend(ai_status(app));
        right.extend(chip(&app.keymap.short_label(Action::Palette), t!("hint.commands"), false));
    }

    let left_w = Line::from(left.clone()).width() as u16;
    let right_w = Line::from(right.clone()).width() as u16;
    let [l, mid, r] = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(left_w.min(area.width)),
            Constraint::Min(0),
            Constraint::Length(right_w.min(area.width)),
        ])
        .areas(area);
    f.render_widget(Paragraph::new(Line::from(left)), l);
    f.render_widget(Paragraph::new(Line::from(right)).alignment(Alignment::Right), r);
    // Clicking the connection opens the connection info.
    app.hits.connection = Rect { x: l.x + mode_w, width: l.width.saturating_sub(mode_w), ..l };
    app.hits.agent_note = if right_w > 0 { r } else { Rect::default() };

    // A message takes the middle; otherwise key hints, at the right with
    // Ctrl+K's, so every key is in one place. A run's outcome is in the
    // results pane, unless that's hidden (another pane zoomed).
    let middle = if quiet {
        Line::from(key_hints(app, mid.width)).alignment(Alignment::Right)
    } else {
        Line::from(Span::styled(
            truncate(&one_line(&app.notes.status), mid.width as usize),
            if app.notes.status_is_error { bold(t.error) } else { fg(t.text) },
        ))
    };
    f.render_widget(Paragraph::new(middle), mid);
}

/// Whether AI is on, and with which model (`Ctrl+K q` asks it).
fn ai_status(app: &App) -> Vec<Span<'static>> {
    match app.ai_state() {
        crate::app::AiState::Off => vec![Span::styled(format!(" {} ", t!("bar.ai_off")), dim())],
        crate::app::AiState::On { model, asks_to_share } => {
            let mut spans =
                vec![Span::styled(format!(" {} ", t!("bar.ai")), muted()), Span::styled(model, fg(theme().text))];
            if asks_to_share {
                spans.push(Span::styled(format!(" · {}", t!("bar.ai_asks_to_share")), muted()));
            }
            spans.push(Span::raw(" "));
            spans
        }
    }
}

fn owned<'a>(hints: &[(&str, &'a str)]) -> Vec<(String, &'a str)> {
    hints.iter().map(|(k, l)| ((*k).to_string(), *l)).collect()
}

/// The pane's menu (what right-click opens): its key on the query screen.
/// Not while a plain `m` would type into a filter or cell being edited.
fn menu_chip(app: &App) -> Option<Vec<Span<'static>>> {
    let key = match app.focus {
        _ if !matches!(app.screen, Screen::Query) => return None,
        Focus::Editor => app.keymap.short_label(Action::Menu),
        _ if app.typing() => return None,
        _ => "m".to_string(),
    };
    Some(chip(&key, t!("hint.menu_click"), false))
}

/// As many key hints as fit in `width` cells, most important first.
fn key_hints(app: &App, width: u16) -> Vec<Span<'static>> {
    let keys = &app.keymap;
    let editor_hints = [
        (keys.short_label(Action::RunStatement), t!("hint.statement")),
        // Right after a word Tab completes; Esc first makes it move on.
        ((if app.tab_completes() { "ESC TAB" } else { "TAB" }).to_string(), t!("hint.next_pane")),
        (keys.short_label(Action::NewTab), t!("hint.new_tab")),
        (keys.short_label(Action::SaveTab), t!("hint.save")),
        (keys.short_label(Action::Library), t!("hint.library")),
        (keys.short_label(Action::HistoryPrev), t!("hint.history")),
        (keys.short_label(Action::Explain), t!("hint.explain")),
        (keys.short_label(Action::Palette), t!("hint.keys")),
    ];
    let hints: Vec<(String, &str)> = match app.screen {
        Screen::Projects => owned(&[
            ("↵", t!("hint.open")),
            ("n", t!("hint.new")),
            ("r", t!("hint.rename")),
            ("d", t!("hint.delete")),
            ("→", t!("hint.connections")),
            ("Esc", t!("hint.back")),
            ("?", t!("hint.keys")),
        ]),
        Screen::Connections => owned(&[
            ("↵", t!("hint.connect")),
            ("e", t!("hint.edit")),
            ("n", t!("hint.new")),
            ("m", t!("hint.move")),
            ("d", t!("hint.delete")),
            ("c", t!("hint.containers")),
            ("←", t!("hint.projects")),
            ("Esc", t!("hint.back")),
        ]),
        Screen::Connect => owned(&[
            ("↵", t!("hint.connect")),
            ("^S", t!("hint.save")),
            ("^T", t!("hint.test")),
            ("↑↓", t!("hint.field")),
            ("←→", t!("hint.option")),
            ("Tab", t!("hint.next_pane")),
        ]),
        Screen::Query => match app.focus {
            Focus::Results => owned(&[
                ("↵", t!("hint.view")),
                ("s", t!("hint.sort")),
                ("/", t!("hint.filter")),
                ("c", t!("hint.edit")),
                ("E", t!("hint.export")),
                ("m", t!("hint.menu")),
                ("[ ]", t!("hint.tabs")),
                ("?", t!("hint.keys")),
            ]),
            Focus::Schema => owned(&[
                ("↵", t!("hint.expand")),
                ("s", t!("hint.select")),
                ("d", t!("hint.definition")),
                ("/", t!("hint.filter")),
                ("o", t!("hint.order")),
                ("r", t!("hint.refresh")),
                ("m", t!("hint.menu")),
                ("?", t!("hint.keys")),
            ]),
            Focus::Editor => editor_hints.to_vec(),
        },
    };
    let mut chips = Vec::new();
    if matches!(app.screen, Screen::Query) && app.panes.zoomed {
        let back = if app.focus == Focus::Editor { keys.short_label(Action::Zoom) } else { "z".to_string() };
        chips.push(chip(&back, t!("hint.unzoom"), true));
    }
    if app.settings.hints == Hints::Full {
        if matches!(app.screen, Screen::Query) {
            chips.push(chip(&keys.short_label(Action::Run), t!("hint.run"), true));
        }
        chips.extend(hints.iter().map(|(key, label)| chip(key, label, false)));
    } else {
        // The main key of the screen (the editor shows the run keys), then
        // where the rest are.
        if !matches!(app.screen, Screen::Query) {
            chips.extend(hints.iter().take(1).map(|(key, label)| chip(key, label, false)));
        }
        // Every key (Ctrl+K is at the right edge): `?`, or F1 where `?`
        // would be typed.
        let help = if app.typing()
            || (matches!(app.screen, Screen::Query) && app.focus == Focus::Editor && app.editor_inserting())
        {
            "F1"
        } else {
            "?"
        };
        chips.push(chip(help, t!("hint.all_keys"), false));
        // The pane's menu (what right-click opens), each item with its key.
        chips.extend(menu_chip(app));
    }
    let mut spans = Vec::new();
    let mut used = 0;
    for c in chips {
        let w: usize = c.iter().map(Span::width).sum();
        if used + w > width as usize {
            break;
        }
        used += w;
        spans.extend(c);
    }
    spans
}
