//! Projects and connections, side by side in three panes: the projects,
//! the open project's connections, and the selected connection's settings
//! (edited in place).

use super::connect::{Settings, form_lines, form_message};
use super::*;

/// Narrowest the connections pane gets beside the others.
const CONNECTIONS_MIN: u16 = 34;
/// The settings pane: room for the labels and a host name.
const SETTINGS_WIDTH: u16 = 50;

/// First run (no projects yet): what Podėlis is and how to start.
fn draw_welcome(f: &mut Frame, app: &App, body: Rect) {
    let t = theme();
    let key = |k: &str, what: &str| {
        Line::from(vec![Span::styled(format!("  {k:<34}"), bold(t.accent)), Span::styled(what.to_string(), fg(t.text))])
    };
    let mut lines = vec![
        Line::default(),
        Line::from(chrome::brand()).alignment(Alignment::Center),
        Line::default(),
        Line::from(Span::styled("A terminal SQL client for PostgreSQL, MySQL and SQLite", fg(t.text)))
            .alignment(Alignment::Center),
        Line::default(),
        key("n  or  ↵", "create a project, then add its connections"),
        Line::from(Span::styled(format!("  {:<34}(paste a connection URL into the form)", ""), muted())),
        key("podelis postgres://me@host/db", "open a database right away"),
        key("podelis app.db", "open a SQLite file"),
        Line::default(),
        Line::from(Span::styled("? keys · q quits", dim())).alignment(Alignment::Center),
    ];
    if let Some((msg, is_error)) = &app.manager.project_message {
        lines.insert(lines.len() - 1, Line::from(Span::styled(format!("  {msg}"), message_style(*is_error))));
    }
    let width = fit(lines.iter().map(Line::width).max().unwrap_or(0) as u16 + 6, 30, body.width);
    let rect = centered_box(body, width, lines.len() as u16 + 2);
    f.render_widget(Paragraph::new(lines).block(popup("Welcome", t.accent)), rect);
}

/// Where each pane goes; a narrow terminal shows the focused pane and the
/// next one (or just the focused one).
struct Panes {
    projects: Option<Rect>,
    connections: Option<Rect>,
    settings: Option<Rect>,
}

fn layout(app: &App, body: Rect) -> Panes {
    let longest = app.store.projects.iter().map(|p| p.name.chars().count()).max().unwrap_or(0) as u16;
    let projects_w = fit(longest + 10, 24, 34);
    let split = |widths: [Constraint; 2]| -> [Rect; 2] {
        Layout::default().direction(Direction::Horizontal).constraints(widths).areas(body)
    };
    if body.width >= projects_w + CONNECTIONS_MIN + SETTINGS_WIDTH {
        let [p, c, s] = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(projects_w), Constraint::Min(0), Constraint::Length(SETTINGS_WIDTH)])
            .areas(body);
        return Panes { projects: Some(p), connections: Some(c), settings: Some(s) };
    }
    if body.width < 64 {
        let only = Some(body);
        return match app.screen {
            Screen::Projects => Panes { projects: only, connections: None, settings: None },
            Screen::Connections => Panes { projects: None, connections: only, settings: None },
            _ => Panes { projects: None, connections: None, settings: only },
        };
    }
    if app.screen == Screen::Projects {
        let [p, c] = split([Constraint::Length(projects_w), Constraint::Min(0)]);
        return Panes { projects: Some(p), connections: Some(c), settings: None };
    }
    let [c, s] = split([Constraint::Min(0), Constraint::Length(SETTINGS_WIDTH.min(body.width / 2 + 6))]);
    Panes { projects: None, connections: Some(c), settings: Some(s) }
}

pub(super) fn draw_manager(f: &mut Frame, app: &mut App, body: Rect) {
    app.hits.manager_projects = (Rect::default(), 0);
    app.hits.manager_connections = (Rect::default(), 0);
    app.hits.manager_settings = Rect::default();
    if app.store.projects.is_empty() && app.manager.project_prompt.is_none() && app.screen == Screen::Projects {
        return draw_welcome(f, app, body);
    }
    let panes = layout(app, body);
    if let Some(area) = panes.projects {
        draw_projects(f, app, area);
    }
    if let Some(area) = panes.connections {
        draw_connections(f, app, area);
    }
    if let Some(area) = panes.settings {
        draw_settings(f, app, area);
        app.hits.manager_settings = area;
    }
}

/// A pane's rows above its messages: the list, then a prompt line (if
/// any), then the message wrapped to as many lines as it needs (up to 4).
fn split_pane(inner: Rect, prompt: bool, message: Option<&str>) -> [Rect; 3] {
    let width = inner.width.max(1) as usize;
    let message_h = message.map_or(0, |m| m.chars().count().div_ceil(width).clamp(1, 4) as u16);
    Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),
            Constraint::Length(u16::from(prompt)),
            Constraint::Length(message_h + u16::from(message_h > 0)),
        ])
        .areas(inner)
}

/// A list row: the selection has the accent marker and background in the
/// focused pane, and just a quieter background elsewhere. `dot` marks
/// something about the row (the open project, a connection in use).
fn row(name: &str, detail: &str, dot: Option<Color>, selected: bool, focused: bool, width: u16) -> ListItem<'static> {
    let t = theme();
    let marker = if selected && focused { "▸ " } else { "  " };
    let name_style = if selected { bold(t.text_bright) } else { fg(t.text) };
    let dot_w = if dot.is_some() { 2 } else { 0 };
    let room = (width as usize).saturating_sub(4 + dot_w);
    let name = truncate(name, room.max(4));
    let detail = truncate(detail, room.saturating_sub(name.chars().count() + 2));
    let used = 2 + dot_w + name.chars().count() + detail.chars().count() + 1;
    let mut spans = vec![Span::styled(marker, bold(t.accent))];
    if let Some(color) = dot {
        spans.push(Span::styled("● ", fg(color)));
    }
    spans.extend([
        Span::styled(name, name_style),
        Span::raw(" ".repeat((width as usize).saturating_sub(used).max(1))),
        Span::styled(detail, dim()),
    ]);
    let item = ListItem::new(Line::from(spans));
    match (selected, focused) {
        (true, true) => item.style(Style::default().bg(t.active)),
        (true, false) => item.style(Style::default().bg(t.inset)),
        _ => item,
    }
}

fn new_row(label: &str, selected: bool, focused: bool) -> ListItem<'static> {
    let t = theme();
    let style = if selected && focused { bold(t.accent) } else { fg(t.muted) };
    let line = Line::from(vec![
        Span::styled(if selected && focused { "▸ " } else { "  " }, bold(t.accent)),
        Span::styled(label.to_string(), style),
    ]);
    let item = ListItem::new(line);
    match (selected, focused) {
        (true, true) => item.style(Style::default().bg(t.active)),
        (true, false) => item.style(Style::default().bg(t.inset)),
        _ => item,
    }
}

fn draw_message(f: &mut Frame, area: Rect, message: Option<(String, Style)>) {
    if let Some((msg, style)) = message {
        let area = Rect {
            x: area.x + 1,
            y: area.y + 1,
            width: area.width.saturating_sub(2),
            height: area.height.saturating_sub(1),
        };
        f.render_widget(Paragraph::new(msg).style(style).wrap(Wrap { trim: true }), area);
    }
}

fn draw_projects(f: &mut Frame, app: &mut App, area: Rect) {
    let t = theme();
    let focused = app.screen == Screen::Projects;
    let n = app.store.projects.len();
    let block = pane("Projects", &n.to_string(), focused);
    let inner = block.inner(area);
    f.render_widget(block, area);

    let message = if app.manager.confirm_delete_project {
        let project = app.store.projects.get(app.manager.project_selected);
        let name = project.map(|p| p.name.as_str()).unwrap_or_default();
        let n = project.map_or(0, |p| p.connections.len());
        Some((format!("Delete project '{name}', its {n} connection(s) and their passwords? y/n"), bold(t.warn)))
    } else {
        app.manager.project_message.as_ref().map(|(msg, is_error)| (msg.clone(), message_style(*is_error)))
    };
    let [list_area, prompt_area, message_area] =
        split_pane(inner, app.manager.project_prompt.is_some(), message.as_ref().map(|(m, _)| m.as_str()));

    let mut items: Vec<ListItem> = app
        .store
        .projects
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let open = app.current_project.as_ref() == Some(&p.id);
            let dot = open.then_some(t.ok);
            row(
                &p.name,
                if open { "open" } else { "" },
                dot,
                i == app.manager.project_selected,
                focused,
                list_area.width,
            )
        })
        .collect();
    items.push(new_row("+ New project", app.manager.project_selected >= n, focused));
    let mut state = ListState::default().with_selected(Some(app.manager.project_selected));
    f.render_stateful_widget(List::new(items), list_area, &mut state);
    app.hits.manager_projects = (list_area, state.offset());

    if let Some(prompt) = &app.manager.project_prompt {
        let label = match prompt.kind {
            PromptKind::NewProject => "Name",
            PromptKind::RenameProject(_) => "Rename to",
        };
        let room = (prompt_area.width as usize).saturating_sub(label.chars().count() + 3);
        // A long name scrolls to keep the cursor in view.
        let (before, _) = prompt.text.split();
        let skip = before.chars().count().saturating_sub(room);
        let text: String = prompt.text.text().chars().skip(skip).collect();
        let line = Line::from(vec![
            Span::styled(format!(" {label}: "), bold(t.accent)),
            Span::styled(text, fg(t.text_bright)),
        ]);
        f.render_widget(Paragraph::new(line).style(Style::default().bg(t.active)), prompt_area);
        let x = prompt_area.x + (label.chars().count() + 3 + before.chars().count() - skip) as u16;
        if x < prompt_area.right() {
            f.set_cursor_position((x, prompt_area.y));
        }
    }
    draw_message(f, message_area, message);
}

/// Where a connection goes: engine, then host:port/database or the file.
fn where_to(p: &ConnectionProfile) -> String {
    let place = if p.engine.is_file_based() {
        std::path::Path::new(&p.host).file_name().map_or_else(|| p.host.clone(), |n| n.to_string_lossy().into_owned())
    } else if p.database.is_empty() {
        p.host.clone()
    } else {
        format!("{}/{}", p.host, p.database)
    };
    format!("{} {place}", p.engine.label())
}

fn draw_connections(f: &mut Frame, app: &mut App, area: Rect) {
    let t = theme();
    let focused = app.screen == Screen::Connections;
    let live = app.viewing_open_project();
    // Another project selected in the projects pane: its connections, to
    // look at (Enter there opens it).
    let project = if live { app.project() } else { app.store.projects.get(app.manager.project_selected) };
    let Some(project) = project else {
        let block = pane("Connections", "", false);
        let inner = block.inner(area);
        f.render_widget(block, area);
        let hint = if app.store.projects.is_empty() { "" } else { "↵ names a new project" };
        f.render_widget(Paragraph::new(format!(" {hint}")).style(dim()), inner);
        return;
    };
    let profiles = &project.connections;
    let block = pane("Connections", &project.name, focused);
    let inner = block.inner(area);
    f.render_widget(block, area);

    if let Some(picker) = &app.manager.containers {
        let title = Line::from(Span::styled(" Database containers:", bold(t.accent)));
        let (text, style) = if picker.loading {
            ("Looking for running containers…".to_string(), muted())
        } else if let Some(e) = &picker.error {
            (e.clone(), message_style(true))
        } else if picker.found.is_empty() {
            ("No running PostgreSQL or MySQL containers · Esc".to_string(), dim())
        } else if let Some(note) = &picker.note {
            (note.clone(), message_style(true))
        } else {
            ("↵ fills in a new connection · Esc".to_string(), dim())
        };
        let [list_area, _, message_area] = split_pane(inner, false, Some(&text));
        let message = Some((text, style));
        let items: Vec<ListItem> = picker
            .found
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let place = c.profile.as_ref().map_or_else(
                    || "port not published".to_string(),
                    |p| format!("{} · {}:{}", p.engine.label(), p.host, p.port),
                );
                row(&c.name, &place, None, i == picker.selected, true, list_area.width)
            })
            .collect();
        let mut state = ListState::default().with_selected(Some(picker.selected));
        f.render_stateful_widget(List::new(items).block(Block::default().title(title)), list_area, &mut state);
        draw_message(f, message_area, message);
        return;
    }
    if let Some(target) = app.manager.moving_to {
        let name = profiles.get(app.manager.connection_selected).map(|p| p.name.as_str()).unwrap_or_default();
        let items: Vec<ListItem> = app
            .move_targets()
            .iter()
            .enumerate()
            .map(|(i, p)| row(&p.name, "", None, i == target, true, inner.width))
            .collect();
        let mut state = ListState::default().with_selected(Some(target));
        let title = Line::from(Span::styled(format!(" Move '{name}' to:"), bold(t.accent)));
        f.render_stateful_widget(List::new(items).block(Block::default().title(title)), inner, &mut state);
        return;
    }

    let message = if !live {
        Some((format!("→ or ↵ opens '{}'", project.name), dim()))
    } else if app.manager.loading_profile {
        Some(("Reading the password from the keychain… (Esc to cancel)".to_string(), muted()))
    } else if app.manager.confirm_delete {
        let name = profiles.get(app.manager.connection_selected).map(|p| p.name.as_str()).unwrap_or_default();
        Some((format!("Delete '{name}' and its saved password? y/n"), bold(t.warn)))
    } else {
        app.manager.connection_message.as_ref().map(|(msg, is_error)| (msg.clone(), message_style(*is_error)))
    };
    let [list_area, _, message_area] = split_pane(inner, false, message.as_ref().map(|(m, _)| m.as_str()));

    let in_use = app.open_connection_names();
    let mut items: Vec<ListItem> = profiles
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let mode = match p.mode {
                ConnectionMode::Production => " · prod",
                ConnectionMode::ReadOnly => " · read-only",
                ConnectionMode::Normal => "",
            };
            let dot = (live && in_use.contains(&p.name.as_str())).then_some(t.ok);
            let selected = live && i == app.manager.connection_selected;
            row(&p.name, &format!("{}{mode}", where_to(p)), dot, selected, focused, list_area.width)
        })
        .collect();
    if live {
        items.push(new_row("+ New connection", app.manager.connection_selected >= profiles.len(), focused));
    } else if profiles.is_empty() {
        items.push(ListItem::new(Span::styled("  no connections yet", dim())));
    }
    let selected = live.then_some(app.manager.connection_selected);
    let mut state = ListState::default().with_selected(selected);
    f.render_stateful_widget(List::new(items), list_area, &mut state);
    app.hits.manager_connections = (list_area, state.offset());
    draw_message(f, message_area, message);
}

fn draw_settings(f: &mut Frame, app: &App, area: Rect) {
    let editing = app.screen == Screen::Connect;
    if editing {
        let form = &app.connection.form;
        let title = if form.original_name.is_some() { "Edit connection" } else { "New connection" };
        let block = pane(title, form.engine.label(), true);
        let inner = block.inner(area);
        f.render_widget(block, area);
        let mask = "•".repeat(form.password.chars().count());
        let lines = form_lines(&Settings::from(form), Some(form.focus), &mask);
        let n = lines.len() as u16;
        f.render_widget(Paragraph::new(lines), inner);
        let (message, style) = form_message(form);
        let rest = Rect {
            x: inner.x + 1,
            y: inner.y + n + 1,
            width: inner.width.saturating_sub(2),
            height: inner.height.saturating_sub(n + 1),
        };
        f.render_widget(Paragraph::new(message).style(style).wrap(Wrap { trim: true }), rest);
        return;
    }
    // The container being picked: what its connection would be.
    if let Some(c) = app.manager.containers.as_ref().and_then(|p| p.found.get(p.selected)) {
        let block = pane("Container", &c.image, false);
        let inner = block.inner(area);
        f.render_widget(block, area);
        let lines = match &c.profile {
            Some(profile) => {
                let mut lines = form_lines(&Settings::from(profile), None, "");
                lines.push(Line::default());
                lines.push(Line::from(Span::styled(
                    " The password stays in the container: read when connecting",
                    dim(),
                )));
                lines
            }
            None => vec![
                Line::from(Span::styled(" Its database port isn't published, so", muted())),
                Line::from(Span::styled(" there's no address here to connect to.", muted())),
                Line::default(),
                Line::from(Span::styled(" Start it with -p, e.g. -p 5433:5432", dim())),
            ],
        };
        f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
        return;
    }
    let profile = app.viewing_open_project().then(|| app.profiles().get(app.manager.connection_selected)).flatten();
    let Some(profile) = profile else {
        let block = pane("Settings", "", false);
        let inner = block.inner(area);
        f.render_widget(block, area);
        if app.viewing_open_project() && app.manager.moving_to.is_none() {
            let lines = vec![
                Line::from(Span::styled(" ↵ adds a connection", dim())),
                Line::from(Span::styled(" (paste a connection URL into its form)", dim())),
            ];
            f.render_widget(Paragraph::new(lines), inner);
        }
        return;
    };
    let block = pane("Settings", profile.engine.label(), false);
    let inner = block.inner(area);
    f.render_widget(block, area);
    let mut lines = form_lines(&Settings::from(profile), None, "in the keychain");
    lines.push(Line::default());
    lines.push(Line::from(Span::styled(" ↵ connects · e edits", dim())));
    f.render_widget(Paragraph::new(lines), inner);
}
