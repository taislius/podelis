//! Application bar (top), status bar (bottom) and the help overlay.

use super::*;
use crate::keys::Action;
use crate::settings::Hints;

/// A connection's name, or for an unsaved one its file or host/database.
fn connection_label(form: &crate::app::ConnectForm) -> String {
    if !form.name.is_empty() {
        return form.name.clone();
    }
    if form.engine.is_file_based() {
        return std::path::Path::new(&form.host)
            .file_name()
            .map_or_else(|| form.host.clone(), |n| n.to_string_lossy().into_owned());
    }
    if form.database.is_empty() { form.host.clone() } else { format!("{}/{}", form.host, form.database) }
}

pub(super) fn draw_app_bar(f: &mut Frame, app: &mut App, area: Rect) {
    let t = theme();
    f.render_widget(Block::default().style(Style::default().bg(t.panel)), area);

    // Identity and breadcrumb on the left.
    let mut left = brand();
    left.push(Span::styled("  │  ", fg(t.line)));
    let project = app.project().map(|p| p.name.clone());
    let crumbs: Vec<String> = match app.screen {
        Screen::Projects => vec!["projects & connections".into()],
        Screen::Connections => vec![project.clone().unwrap_or_default(), "connections".into()],
        Screen::Connect => vec![
            project.clone().unwrap_or_default(),
            if app.connect_form.original_name.is_some() {
                app.connect_form.name.clone()
            } else {
                "new connection".into()
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
            left.push(Span::styled("project ", muted()));
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
    let right = if app.connect_form.connecting && !matches!(app.screen, Screen::Query) {
        vec![Span::styled(" ◌ CONNECTING… ", bold(t.warn).bg(t.active)), Span::raw(" ")]
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
        return form.host.clone();
    }
    let user = if form.user.is_empty() { String::new() } else { format!("{}@", form.user) };
    let via = if form.ssh.trim().is_empty() { String::new() } else { format!(" via {}", form.ssh.trim()) };
    format!("{user}{}:{}{via}", form.host, form.port)
}

/// The status bar's connection: state, name, where it goes, the server,
/// latency and mode. All of it, once, in one place.
fn connection_status(app: &App, room: usize) -> Vec<Span<'static>> {
    let t = theme();
    if !app.connected() {
        return if app.reconnecting().is_empty() {
            let key = app.keymap.short_label(Action::OpenConnection);
            vec![Span::styled("○ not connected", muted()), Span::styled(format!(" · {key} connect  "), dim())]
        } else {
            vec![Span::styled("◌ reconnecting…  ", bold(t.warn))]
        };
    }
    let (dot, name_style) = if app.connection_lost { (fg(t.error), bold(t.error)) } else { (fg(t.ok), bold(t.ok)) };
    let mut spans = vec![
        Span::styled(if app.settings.keepalive > 0 { "♥ " } else { "● " }, dot),
        Span::styled(connection_label(&app.connect_form), name_style),
    ];
    if app.connection_lost {
        spans.push(Span::styled(" · connection lost", bold(t.error)));
    }
    match app.active_mode {
        ConnectionMode::Production => spans.extend([Span::raw(" "), badge("PRODUCTION", t.on_accent, t.error)]),
        ConnectionMode::ReadOnly => spans.extend([Span::raw(" "), badge("READ-ONLY", t.on_accent, t.ok)]),
        ConnectionMode::Normal => {}
    }
    let engine = app.connect_form.engine.label();
    let version = app.database.as_ref().map(|d| d.version().to_string()).unwrap_or_default();
    let mut details = format!("{} · {engine} {version}", endpoint(&app.connect_form));
    if let Some(latency) = app.connect_latency {
        details = format!("{} · {} ms", details.trim_end(), latency.as_millis());
    }
    // Long paths and hosts give way to the messages in the middle.
    let used: usize = spans.iter().map(Span::width).sum();
    spans.push(Span::styled(format!("  {}  ", truncate(details.trim_end(), room.saturating_sub(used + 4))), muted()));
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
        // Leave the middle for messages.
        left.extend(connection_status(app, (area.width as usize / 2).max(24)));
    }

    // Rows and timing are in the results pane. What agents did that's worth
    // a look since the agent window was last open (a click opens it).
    let mut right: Vec<Span> = Vec::new();
    if let Some(note) = app.agent_note().filter(|_| matches!(app.screen, Screen::Query)) {
        right.push(Span::styled(format!(" {note} "), bold(t.warn)));
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

    // A message takes the middle; otherwise key hints. A run's outcome is
    // in the results pane, unless that's hidden (another pane zoomed).
    let in_results = app.status_in_results && app.hits.results.width > 0;
    let middle = if app.status.is_empty() || in_results {
        Line::from(key_hints(app, mid.width)).alignment(Alignment::Center)
    } else {
        Line::from(Span::styled(
            truncate(&one_line(&app.status), mid.width as usize),
            if app.status_is_error { bold(t.error) } else { fg(t.text) },
        ))
    };
    f.render_widget(Paragraph::new(middle), mid);
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
    Some(chip(&key, "menu · right-click", false))
}

/// As many key hints as fit in `width` cells, most important first.
fn key_hints(app: &App, width: u16) -> Vec<Span<'static>> {
    let keys = &app.keymap;
    let editor_hints = [
        (keys.short_label(Action::RunStatement), "statement"),
        // Right after a word Tab completes; Esc first makes it move on.
        ((if app.tab_completes() { "ESC TAB" } else { "TAB" }).to_string(), "next pane"),
        (keys.short_label(Action::NewTab), "new tab"),
        (keys.short_label(Action::SaveTab), "save"),
        (keys.short_label(Action::Library), "library"),
        (keys.short_label(Action::HistoryPrev), "history"),
        (keys.short_label(Action::Explain), "explain"),
        (keys.short_label(Action::Palette), "keys"),
    ];
    let hints: Vec<(String, &str)> = match app.screen {
        Screen::Projects => owned(&[
            ("↵", "open"),
            ("n", "new"),
            ("r", "rename"),
            ("d", "delete"),
            ("→", "connections"),
            ("Esc", "back"),
            ("?", "keys"),
        ]),
        Screen::Connections => owned(&[
            ("↵", "connect"),
            ("e", "edit"),
            ("n", "new"),
            ("m", "move"),
            ("d", "delete"),
            ("c", "containers"),
            ("←", "projects"),
            ("Esc", "back"),
        ]),
        Screen::Connect => owned(&[
            ("↵", "connect"),
            ("^S", "save"),
            ("^T", "test"),
            ("↑↓", "field"),
            ("←→", "option"),
            ("Tab", "next pane"),
        ]),
        Screen::Query => match app.focus {
            Focus::Results => owned(&[
                ("↵", "view"),
                ("s", "sort"),
                ("/", "filter"),
                ("c", "edit"),
                ("e", "export"),
                ("m", "menu"),
                ("[ ]", "tabs"),
                ("?", "keys"),
            ]),
            Focus::Schema => owned(&[
                ("↵", "expand"),
                ("s", "select"),
                ("d", "definition"),
                ("/", "filter"),
                ("o", "order"),
                ("r", "refresh"),
                ("m", "menu"),
                ("?", "keys"),
            ]),
            Focus::Editor => editor_hints.to_vec(),
        },
    };
    let mut chips = Vec::new();
    if matches!(app.screen, Screen::Query) && app.zoomed {
        let back = if app.focus == Focus::Editor { keys.short_label(Action::Zoom) } else { "z".to_string() };
        chips.push(chip(&back, "unzoom", true));
    }
    if app.settings.hints == Hints::Full {
        if matches!(app.screen, Screen::Query) {
            chips.push(chip(&keys.short_label(Action::Run), "run", true));
        }
        chips.extend(hints.iter().map(|(key, label)| chip(key, label, false)));
    } else {
        // The main key of the screen (the editor shows the run keys), then
        // where the rest are.
        if !matches!(app.screen, Screen::Query) {
            chips.extend(hints.iter().take(1).map(|(key, label)| chip(key, label, false)));
        }
        let help = match app.screen {
            Screen::Query => keys.short_label(Action::Palette),
            _ if app.typing() => "F1".to_string(),
            _ => "?".to_string(),
        };
        chips.push(chip(&help, "keys", false));
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

/// The keys of the screen by section, as (keys, what they do).
// Long only because it lists the keys.
#[allow(clippy::too_many_lines)]
pub(super) fn help_sections(app: &App) -> Vec<(&'static str, Vec<(String, String)>)> {
    let k = |a: Action| app.keymap.label(a);
    let undo_redo = format!("{} / {}", k(Action::Undo), k(Action::Redo));
    let history = format!("{} / {}", k(Action::HistoryPrev), k(Action::HistoryNext));
    let tabs = format!("{} / {}", k(Action::NewTab), k(Action::CloseTab));
    let switch = format!("{} / {} · Ctrl+K 1…9", k(Action::PrevTab), k(Action::NextTab));
    let complete = format!("Tab · {}", k(Action::Complete));
    let tx_keys = format!("{} / {}", k(Action::Commit), k(Action::Rollback));
    let comment_format = format!("{} · {}", k(Action::Comment), k(Action::Format));
    let keys_menu = format!("{} · ? (outside the editor)", k(Action::Palette));
    let sections: Vec<(&'static str, Vec<(&str, &str)>)> = match app.screen {
        Screen::Query => vec![
            (
                "Run",
                vec![
                    (k(Action::Run), "run the selection, or everything"),
                    (k(Action::RunStatement), "run the statement at the cursor"),
                    (k(Action::Explain), "show the query plan (not executed)"),
                    (k(Action::ExplainAnalyze), "explain analyze (runs it; writes rolled back)"),
                    (k(Action::TxMode), "transaction mode: auto-commit · manual commit · rollback-only"),
                    (&tx_keys, "commit / roll back the open transaction"),
                    ("Esc", "cancel the running query"),
                ],
            ),
            (
                "Editor",
                vec![
                    (&complete, "autocomplete"),
                    (&undo_redo, "undo / redo"),
                    ("Shift+move · Ctrl+A", "select · select all"),
                    ("Ctrl+C / Ctrl+X / Ctrl+V", "copy / cut / paste"),
                    (&history, "query history"),
                    (k(Action::Find), "find and replace"),
                    (k(Action::InsertSnippet), "insert a snippet / saved query"),
                    (k(Action::Menu), "menu of what you can do here"),
                    (&comment_format, "comment lines · format SQL"),
                    ("vim keys", "editor_mode = \"vim\" in config.toml"),
                    ("rebind keys", "[keys] in config.toml"),
                ],
            ),
            (
                "Results",
                vec![
                    ("↵ · v", "full value of the cell"),
                    ("x · f", "row vertically · follow foreign key"),
                    ("X · …\\G", "all rows one column per line (end a query with \\G)"),
                    ("| · o", "pipe rows through a command · open in viewer"),
                    ("s · /", "sort · filter"),
                    ("[ · ] · 1 2 3", "rows / messages / plan tabs"),
                    ("< · >", "previous / next result set of a script"),
                    (", · . · p", "earlier / later result kept from a run · pin it"),
                    ("↑↓ · ↵ · y · Y", "in MESSAGES: select · whole message · copy it · copy all"),
                    ("c", "edit the cell (saved by primary key)"),
                    ("a · Space · d", "add a row · mark rows · delete marked rows"),
                    ("C", "copy rows as a Markdown table"),
                    ("m · right-click", "menu of what you can do here"),
                    ("y · Y", "copy cell · row"),
                    ("e · J · i · M", "export CSV · JSON · INSERTs · load more"),
                ],
            ),
            (
                "Navigator",
                vec![
                    ("↵ · → / ←", "expand / collapse"),
                    ("s", "SELECT the table (added to the editor and run)"),
                    ("d", "definition of the table, view or function"),
                    ("/ · r", "filter · refresh"),
                    ("o", "order by name, rows or size"),
                    ("PgUp · PgDn", "a page up · down"),
                    (".", "show / hide system tables"),
                    ("z · m", "zoom · menu of what you can do here"),
                    ("Space", "command menu (like Ctrl+K)"),
                ],
            ),
            (
                "Tabs & library",
                vec![
                    (&tabs, "new tab (same connection) / close"),
                    (&switch, "switch tab"),
                    (k(Action::SaveTab), "save tab to the library (group/name)"),
                    (k(Action::RenameTab), "rename / move to another group"),
                    (k(Action::Library), "search saved queries and history"),
                ],
            ),
            (
                "Panes",
                vec![
                    ("Tab / Shift+Tab", "next / previous pane (in the editor after a word: Esc Tab)"),
                    (k(Action::ToggleSchema), "show/hide the navigator"),
                    (k(Action::Zoom), "zoom the pane to full screen (z in results/navigator)"),
                ],
            ),
            (
                "AI",
                vec![
                    (k(Action::Ask), "ask in plain language: a model writes the SQL (lands in the editor, not run)"),
                    (k(Action::Memory), "project memory: what models are told; confirm their guesses"),
                ],
            ),
            (
                "Connection & project",
                vec![
                    (&keys_menu, "one-letter actions and this pane's keys; type to search all"),
                    (k(Action::OpenConnection), "open a connection (new tab if connected)"),
                    (k(Action::Disconnect), "disconnect this tab"),
                    (k(Action::Projects), "projects and connections (or click the project in the top bar)"),
                ],
            ),
        ],
        Screen::Projects | Screen::Connections | Screen::Connect => vec![
            (
                "Projects",
                vec![
                    ("↵", "open (the open project: its connections)"),
                    ("n · r · d", "new · rename · delete"),
                    ("Tab / Shift+Tab", "next / previous pane (all three, round)"),
                    ("→", "its connections (opening it first)"),
                    ("Esc", "back to the tabs"),
                    ("q", "quit"),
                ],
            ),
            (
                "Connections",
                vec![
                    ("↵", "connect (in a new tab if this one is connected)"),
                    ("e · →", "edit its settings"),
                    ("n · d · m", "new · delete · move to another project"),
                    ("c", "find database containers (Docker, Podman)"),
                    ("←", "projects"),
                    ("Esc", "back to the tabs"),
                ],
            ),
            (
                "Settings",
                vec![
                    ("↑↓", "move between fields"),
                    ("← →", "change engine, SSL mode, mode"),
                    ("↵ · Ctrl+S", "connect · save"),
                    ("Ctrl+T", "test the connection step by step"),
                    ("paste a URL", "fills the whole form"),
                    ("Esc", "back to the connections (unsaved changes are dropped)"),
                ],
            ),
        ],
    };
    sections
        .into_iter()
        .map(|(title, keys)| (title, keys.into_iter().map(|(k, w)| (k.to_string(), w.to_string())).collect()))
        .collect()
}

/// The focused pane's section of the help.
pub(super) fn pane_keys(app: &App) -> (&'static str, Vec<(String, String)>) {
    let title = match app.focus {
        Focus::Editor => "Editor",
        Focus::Results => "Results",
        Focus::Schema => "Navigator",
    };
    help_sections(app).into_iter().find(|(t, _)| *t == title).unwrap_or((title, Vec::new()))
}

pub(super) fn draw_help(f: &mut Frame, app: &App, area: Rect) {
    let t = theme();
    let sections = help_sections(app);
    // Configured keys can be long: size the key column to the longest.
    let key_w = sections.iter().flat_map(|(_, keys)| keys).map(|(k, _)| k.chars().count()).max().unwrap_or(0).max(22);
    let mut lines = Vec::new();
    for (title, keys) in sections {
        lines.push(Line::from(Span::styled(title.to_uppercase(), bold(t.accent))));
        for (key, what) in keys {
            lines.push(Line::from(vec![
                Span::styled(format!("  {key:<key_w$} "), bold(t.text)),
                Span::styled(what, muted()),
            ]));
        }
        lines.push(Line::default());
    }
    lines.push(Line::from(Span::styled("Esc or ? closes this", dim())));
    let height = fit(lines.len() as u16 + 2, 5, area.height);
    let width = lines.iter().map(Line::width).max().unwrap_or(0) as u16 + 4;
    let rect = centered_box(area, fit(width.max(64), 20, area.width), height);
    f.render_widget(Clear, rect);
    f.render_widget(Paragraph::new(lines).block(popup("KEYS", t.accent)), rect);
}

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
        Line::from(vec![Span::styled("reopening ", muted()), Span::styled(splash.project.clone(), bold(t.text))])
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
    lines.push(Line::from(Span::styled("any key continues", dim())).alignment(Alignment::Center));
    let width = fit(lines.iter().map(Line::width).max().unwrap_or(0) as u16 + 6, 40, area.width);
    let rect = centered_box(area, width, lines.len() as u16 + 2);
    f.render_widget(Clear, rect);
    f.render_widget(Paragraph::new(lines).block(popup("", t.accent)), rect);
}
