//! The projects pane, the first of the three panes for managing projects
//! and connections (projects │ connections │ settings). `Screen` says
//! which of them has the keys.

use super::*;

/// Which manager pane a click landed in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ManagerHit {
    Project(usize),
    Connection(usize),
    Settings,
}

impl App {
    pub(super) fn handle_projects_key(&mut self, key: KeyEvent) {
        if self.project_prompt.is_some() {
            return self.handle_prompt_key(key);
        }
        if std::mem::take(&mut self.confirm_delete_project) {
            if matches!(key.code, KeyCode::Char('y' | 'Y')) {
                self.delete_selected_project();
            } else {
                self.projects_message = None;
            }
            return;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            return;
        }
        let len = self.store.projects.len();
        let selected = self.store.projects.get(self.projects_selected).map(|p| p.id.clone());
        let is_current = selected.is_some() && selected == self.current_project;
        self.projects_message = None;
        // The row after the projects is "new project".
        if let Some(i) = list::step(self.projects_selected, len + 1, key, Nav::LIST) {
            self.projects_selected = i;
            return;
        }
        match key.code {
            KeyCode::Esc => self.leave_projects(),
            KeyCode::Char('q') => self.request_quit(),
            KeyCode::Char('n') => self.start_prompt(PromptKind::NewProject, String::new()),
            // Into the selected project's connections (opening it first if
            // it isn't the open one), or back round to the settings.
            KeyCode::Right | KeyCode::Char('l') | KeyCode::Tab | KeyCode::BackTab => {
                self.pane_from_projects(key.code != KeyCode::BackTab);
            }
            KeyCode::Enter => match selected {
                Some(_) if is_current => self.screen = Screen::Connections,
                Some(id) => self.enter_project(id, false),
                None => self.start_prompt(PromptKind::NewProject, String::new()),
            },
            KeyCode::Char('r') => {
                if let Some(id) = selected {
                    let name = self.store.project(&id).map(|p| p.name.clone()).unwrap_or_default();
                    self.start_prompt(PromptKind::RenameProject(id), name);
                }
            }
            KeyCode::Char('d') if is_current => {
                self.projects_message =
                    Some(("This project is open: switch to another one to delete it".to_string(), true));
            }
            KeyCode::Char('d') if selected.is_some() => self.confirm_delete_project = true,
            _ => {}
        }
    }

    /// → or Tab (or Shift+Tab, round the other way) from the projects: the
    /// selected project's connections (or the selected connection's
    /// settings) take the keys. Another project is opened first, as Enter
    /// does; "+ New project" asks for a name.
    fn pane_from_projects(&mut self, forward: bool) {
        let Some(selected) = self.store.projects.get(self.projects_selected).map(|p| p.id.clone()) else {
            return self.start_prompt(PromptKind::NewProject, String::new());
        };
        if self.current_project.as_ref() != Some(&selected) {
            return self.enter_project(selected, true);
        }
        if forward {
            self.screen = Screen::Connections;
        } else {
            self.edit_selected_connection();
        }
    }

    /// Opens project `id`: from another project as a switch (its tabs are
    /// saved, and a transaction asks first), and where it was left; with
    /// `panes`, on its connections.
    fn enter_project(&mut self, id: String, panes: bool) {
        if self.current_project.is_some() {
            return self.switch_project(Some(id), panes);
        }
        self.open_project(&id);
        self.resume_project();
        if panes {
            self.screen = Screen::Connections;
        }
    }

    /// Esc on the projects: back to the open project's tabs; with none
    /// open, to the project last left, else quit.
    fn leave_projects(&mut self) {
        if self.current_project.is_some() {
            return self.leave_connections();
        }
        match self.left_project.clone().filter(|id| self.store.project(id).is_some()) {
            Some(id) => {
                self.open_project(&id);
                self.resume_project();
            }
            None => self.should_quit = true,
        }
    }

    pub(super) fn start_prompt(&mut self, kind: PromptKind, text: String) {
        self.projects_message = None;
        self.project_prompt = Some(Prompt { kind, text: LineInput::new(text) });
    }

    pub(super) fn handle_prompt_key(&mut self, key: KeyEvent) {
        let Some(prompt) = &mut self.project_prompt else { return };
        match prompt.text.handle(key) {
            InputResult::Cancel => self.project_prompt = None,
            InputResult::Submit => {
                let text = prompt.text.text().to_string();
                match prompt.kind.clone() {
                    // A new project opens straight away, ready for connections.
                    PromptKind::NewProject => match self.store.add_project(&text) {
                        Ok(id) => {
                            self.project_prompt = None;
                            self.projects_selected = self.store.projects.len() - 1;
                            self.enter_project(id, false);
                        }
                        // Keep the prompt open so the name can be fixed.
                        Err(e) => self.projects_message = Some((format!("{e:#}"), true)),
                    },
                    PromptKind::RenameProject(id) => match self.store.rename_project(&id, &text) {
                        Ok(()) => self.project_prompt = None,
                        Err(e) => self.projects_message = Some((format!("{e:#}"), true)),
                    },
                }
            }
            InputResult::Changed | InputResult::Moved | InputResult::Ignored => {}
        }
    }

    pub(super) fn delete_selected_project(&mut self) {
        let Some(id) = self.store.projects.get(self.projects_selected).map(|p| p.id.clone()) else {
            return;
        };
        match self.store.delete_project(&id) {
            Ok(project) => {
                let count = project.connections.len();
                self.projects_message =
                    Some((format!("Deleted project '{}' and its {count} connection(s)", project.name), false));
                // Its memory for models too (the gateway may keep it open: on
                // Unix the file goes, and nothing asks for this project again).
                let memory = self.data_dir.as_ref().map(|d| crate::gateway::memory::path(d, &id));
                let files = [self.history_path(&id), self.draft_path(&id), self.tabs_path(&id), memory];
                for path in files.into_iter().flatten() {
                    let _ = std::fs::remove_file(path);
                }
                // Keychain calls can block on an unlock prompt.
                if count > 0 {
                    tokio::task::spawn_blocking(move || {
                        for profile in &project.connections {
                            let _ = store::delete_password(&store::keychain_account(&project.id, profile));
                        }
                    });
                }
                if self.left_project.as_deref() == Some(id.as_str()) {
                    self.left_project = None;
                }
                let len = self.store.projects.len();
                if self.projects_selected > 0 && self.projects_selected >= len {
                    self.projects_selected -= 1;
                }
            }
            Err(e) => self.projects_message = Some((format!("Couldn't delete: {e:#}"), true)),
        }
    }

    /// Shows the projects and connections panes with `screen`'s focused:
    /// Ctrl+K p and the project's name in the top bar (projects), Ctrl+O
    /// (connections).
    pub(super) fn open_manager(&mut self, screen: Screen) {
        // Only looking at the projects from the work: leaving the project
        // from here reopens it on its tabs, as if left from them.
        if matches!(self.screen, Screen::Query) {
            self.manager_from_query = screen == Screen::Projects;
            self.drop_blank_tab = false;
        }
        // A password still being read was for a connection left behind.
        if std::mem::take(&mut self.loading_profile) {
            self.invalidate_tasks();
        }
        self.conns.auto_connect = false;
        self.restore_active_form();
        self.completion = None;
        self.menu = None;
        self.project_prompt = None;
        self.confirm_delete_project = false;
        self.confirm_delete = false;
        self.moving_to = None;
        self.containers = None;
        self.projects_message = None;
        if let Some(i) = self.store.projects.iter().position(|p| self.current_project.as_ref() == Some(&p.id)) {
            self.projects_selected = i;
        }
        self.screen = screen;
    }

    /// Before connecting: a connected tab keeps its connection, and the
    /// new one gets a new tab (dropped again on the way back if it stays
    /// empty and unconnected).
    pub(super) fn free_tab_for_form(&mut self) {
        if self.conns.active.is_some() {
            self.new_tab(None, "", None);
            self.tabs[self.active_tab].conn = None;
            self.switch_conn(None);
            self.drop_blank_tab = true;
        }
    }

    /// Before the settings pane edits a connection: the form belongs to the
    /// active tab's connection, so a connected tab's is set aside.
    pub(super) fn stash_active_form(&mut self) {
        if self.conns.active.is_some() && self.form_stash.is_none() {
            self.form_stash = Some(std::mem::take(&mut self.connect_form));
        }
    }

    /// Leaving the settings pane: the active tab's form comes back.
    pub(super) fn restore_active_form(&mut self) {
        if let Some(form) = self.form_stash.take() {
            self.connect_form = form;
        }
    }

    /// The active tab's connection form, even while the settings pane
    /// edits another.
    pub(super) fn active_form(&self) -> &ConnectForm {
        self.form_stash.as_ref().unwrap_or(&self.connect_form)
    }

    /// Whether a click may leave what's on screen now: not while a
    /// connection is being made or a question or popup waits.
    pub(super) fn busy_for_clicks(&self) -> bool {
        self.popup_open()
            || self.connect_form.connecting
            || self.loading_profile
            || self.moving_to.is_some()
            || self.containers.is_some()
            || self.project_prompt.is_some()
    }

    /// Clicks on the projects and connections panes: a click selects (and
    /// focuses that pane), clicking the selected row again or a double
    /// click opens it, and the settings pane edits the connection.
    pub(super) fn click_manager(&mut self, x: u16, y: u16, double: bool) {
        let Some(hit) = self.manager_hit(x, y) else { return };
        if self.busy_for_clicks() {
            return;
        }
        if hit != ManagerHit::Settings {
            self.restore_active_form();
        }
        self.confirm_delete = false;
        self.confirm_delete_project = false;
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        match hit {
            ManagerHit::Project(row) => {
                let again = row == self.projects_selected && matches!(self.screen, Screen::Projects);
                self.projects_selected = row;
                self.screen = Screen::Projects;
                if again || double {
                    self.handle_projects_key(enter);
                }
            }
            ManagerHit::Connection(row) => {
                let again = row == self.connections_selected && matches!(self.screen, Screen::Connections);
                self.connections_selected = row;
                self.screen = Screen::Connections;
                if again || double {
                    self.handle_connections_key(enter);
                }
            }
            // The rows start below the pane's top border.
            ManagerHit::Settings if matches!(self.screen, Screen::Connect) => {
                let top = self.hits.manager_settings.y + 1;
                let field = y.checked_sub(top).and_then(|row| self.connect_form.visible_fields().get(row as usize));
                if let Some(&field) = field {
                    self.connect_form.focus = field;
                }
            }
            ManagerHit::Settings => self.edit_selected_connection(),
        }
    }

    fn manager_hit(&self, x: u16, y: u16) -> Option<ManagerHit> {
        let inside = |r: Rect| super::mouse::inside(r, x, y);
        let (projects, p_offset) = self.hits.manager_projects;
        let (connections, c_offset) = self.hits.manager_connections;
        if inside(projects) {
            let row = (y - projects.y) as usize + p_offset;
            return (row <= self.store.projects.len()).then_some(ManagerHit::Project(row));
        }
        // Only the open project's connections can be picked.
        if inside(connections) && self.viewing_open_project() {
            let row = (y - connections.y) as usize + c_offset;
            return (row <= self.profiles().len()).then_some(ManagerHit::Connection(row));
        }
        (inside(self.hits.manager_settings) && self.viewing_open_project()).then_some(ManagerHit::Settings)
    }

    /// Whether the projects pane's selection is the open project (so the
    /// connections pane shows its connections, not a preview).
    pub fn viewing_open_project(&self) -> bool {
        let selected = self.store.projects.get(self.projects_selected).map(|p| &p.id);
        self.current_project.is_some() && (selected == self.current_project.as_ref() || self.screen != Screen::Projects)
    }
}
