//! One connection per tab, and reopening the last session on startup.
//!
//! The active tab's connection lives in `App`'s usual fields (`database`,
//! `schema`…), like the rest of the active tab's state; other tabs'
//! connections are parked as [`ConnSlot`]s. Tabs opened with Ctrl+T share
//! their tab's connection (and so its session and transaction).

use serde::{Deserialize, Serialize};

use super::*;

/// A connection's state. Field names match the `App` fields they swap with.
#[derive(Default)]
pub struct ConnSlot {
    database: Option<Database>,
    connect_form: ConnectForm,
    active_mode: ConnectionMode,
    tx_mode: TxMode,
    txn: TxnState,
    schema: Option<SchemaPanel>,
    connection_summary: String,
    connect_latency: Option<Duration>,
}

fn swap_conn(app: &mut App, slot: &mut ConnSlot) {
    std::mem::swap(&mut app.database, &mut slot.database);
    std::mem::swap(&mut app.connect_form, &mut slot.connect_form);
    std::mem::swap(&mut app.active_mode, &mut slot.active_mode);
    std::mem::swap(&mut app.tx_mode, &mut slot.tx_mode);
    std::mem::swap(&mut app.txn, &mut slot.txn);
    std::mem::swap(&mut app.schema, &mut slot.schema);
    std::mem::swap(&mut app.connection_summary, &mut slot.connection_summary);
    std::mem::swap(&mut app.connect_latency, &mut slot.connect_latency);
}

/// Which connection each tab uses, and the session being restored.
#[derive(Default)]
pub struct Conns {
    /// Id of the connection whose state is in `App`'s fields (the active
    /// tab's), and the other tabs' connections.
    pub active: Option<u64>,
    parked: HashMap<u64, ConnSlot>,
    next_id: u64,
    /// Each tab's saved connection name, from the tabs file.
    pub tab_connections: Vec<Option<String>>,
    /// UI state saved with the tabs.
    pub saved_session: SessionUi,
    /// Connections being reopened at startup, by name.
    pub restoring: Vec<String>,
    /// Expanded schema tables to restore, by connection name.
    restore_expanded: HashMap<String, Vec<(String, String)>>,
    /// Connect as soon as the saved connection's password is loaded (it was
    /// named on the command line).
    pub auto_connect: bool,
}

/// The card shown while a session's connections are reopened, if that
/// takes long enough to notice.
pub struct Splash {
    pub project: String,
    started: Instant,
    /// Drawn (after `SPLASH_DELAY`; a fast reopen never shows it).
    pub shown: bool,
    /// Reopened so far: the name, and the error if it failed.
    pub done: Vec<(String, Option<String>)>,
}

const SPLASH_DELAY: Duration = Duration::from_millis(200);

#[cfg(test)]
impl Splash {
    pub fn for_tests(shown: bool) -> Self {
        Splash { project: "shop".into(), started: Instant::now(), shown, done: Vec::new() }
    }
}

/// UI state saved with a project's tabs.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SessionUi {
    /// The query screen was open (not the connections list).
    #[serde(default)]
    pub on_query: bool,
    #[serde(default)]
    pub focus: String,
    #[serde(default)]
    pub schema_visible: bool,
    /// Expanded schema tables (schema, table) by connection name.
    #[serde(default)]
    pub expanded: HashMap<String, Vec<(String, String)>>,
}

impl App {
    /// When the reopening card should appear.
    pub fn splash_due(&self) -> Option<Instant> {
        self.splash.as_ref().filter(|s| !s.shown).map(|s| s.started + SPLASH_DELAY)
    }

    /// Reopening is taking a while: show the card.
    pub fn show_splash(&mut self) {
        if let Some(splash) = &mut self.splash {
            splash.shown = true;
        }
    }

    /// Whether the active tab has a connection.
    pub fn connected(&self) -> bool {
        self.conns.active.is_some()
    }

    /// Marks the active tab connected without a database, for UI tests.
    #[cfg(test)]
    pub fn pretend_connected(&mut self) {
        self.conns.active = Some(u64::MAX);
        self.tabs[self.active_tab].conn = Some(u64::MAX);
    }

    /// A transaction is open on any tab's connection.
    pub(super) fn any_open_transaction(&self) -> bool {
        self.txn != TxnState::Idle || self.conns.parked.values().any(|s| s.txn != TxnState::Idle)
    }

    /// Leaves the project for `target` (or, with `None`, the project list),
    /// asking first if a transaction would be rolled back. With `panes`,
    /// `target`'s connections take the keys rather than its tabs.
    pub(super) fn switch_project(&mut self, target: Option<String>, panes: bool) {
        if self.executing {
            return self.set_status("Wait for the query to finish (or press Esc) first".to_string(), true);
        }
        if self.any_open_transaction() {
            self.confirm = Some(Confirm {
                message: "A transaction is open. Leave the project and roll it back?".to_string(),
                details: vec![],
                danger: true,
                action: ConfirmAction::LeaveProject(target, panes),
                offer_transaction: false,
            });
            return;
        }
        self.go_to_project(target, panes);
    }

    /// Leaves this project and opens `target` where it was left (or shows
    /// the project list). With `panes`, its tabs come back behind its
    /// connections.
    pub(super) fn go_to_project(&mut self, target: Option<String>, panes: bool) {
        self.leave_project();
        if let Some(id) = target.filter(|id| self.store.project(id).is_some()) {
            self.open_project(&id);
            self.resume_project();
            if panes {
                // Leaving it from here keeps where it was left.
                self.manager_from_query = matches!(self.screen, Screen::Query);
                self.screen = Screen::Connections;
            }
        }
    }

    /// Saves the tabs and session, closes the connections and shows the
    /// project list. Opening the project again brings it all back.
    pub(super) fn leave_project(&mut self) {
        // Its answer would go to a tab of another project.
        self.stop_asking();
        self.memory_view = None;
        self.save_draft();
        if let Some(id) = &self.current_project {
            self.projects_selected = self.store.projects.iter().position(|p| &p.id == id).unwrap_or(0);
        }
        self.close_all_connections();
        self.reset_agents();
        // Opening it again reloads its tabs and session from disk.
        self.left_project = self.current_project.take();
        self.menu = None;
        self.zoomed = false;
        self.screen = Screen::Projects;
    }

    /// Connections still being reopened at startup.
    pub fn reconnecting(&self) -> &[String] {
        &self.conns.restoring
    }

    /// Makes connection `target` the active one, parking the current one.
    /// Puts `target`'s state in the app's fields, parking the active one's.
    pub(super) fn swap_conns(&mut self, target: Option<u64>) {
        if target == self.conns.active {
            return;
        }
        let mut current = ConnSlot::default();
        swap_conn(self, &mut current);
        if let Some(id) = self.conns.active {
            self.conns.parked.insert(id, current);
        }
        if let Some(mut slot) = target.and_then(|id| self.conns.parked.remove(&id)) {
            swap_conn(self, &mut slot);
        }
        self.conns.active = target;
    }

    pub(super) fn switch_conn(&mut self, target: Option<u64>) {
        if target == self.conns.active {
            return;
        }
        self.swap_conns(target);
        self.connection_lost = false;
        // Events from the other connection's background work no longer
        // apply; restart what was cut short.
        self.generation += 1;
        self.tab.syntax_hint = None;
        match &mut self.schema {
            Some(panel) if panel.loading => self.load_catalog(),
            Some(panel) => panel.tables.iter_mut().for_each(|t| t.details_loading = false),
            None if self.database.is_some() => self.load_catalog(),
            None => {}
        }
    }

    /// The other tabs' connections: id, database and form.
    pub(super) fn parked_connections(&self) -> Vec<(u64, &Database, &ConnectForm)> {
        let mut out: Vec<_> = self
            .conns
            .parked
            .iter()
            .filter_map(|(id, slot)| Some((*id, slot.database.as_ref()?, &slot.connect_form)))
            .collect();
        out.sort_by_key(|(id, ..)| *id);
        out
    }

    /// Closes parked connections that no tab uses any more.
    pub(super) fn release_unused_conns(&mut self) {
        let unused: Vec<u64> =
            self.conns.parked.keys().copied().filter(|id| self.tabs.iter().all(|t| t.conn != Some(*id))).collect();
        for id in unused {
            if let Some(db) = self.conns.parked.remove(&id).and_then(|s| s.database) {
                tokio::spawn(async move { db.close().await });
            }
        }
    }

    /// Closes every connection (when the project's tabs are replaced).
    pub(super) fn close_all_connections(&mut self) {
        self.invalidate_tasks();
        self.row_stream = None;
        self.executing = false;
        let mut current = ConnSlot::default();
        swap_conn(self, &mut current);
        let slots = std::iter::once(current).chain(self.conns.parked.drain().map(|(_, s)| s));
        for db in slots.filter_map(|s| s.database) {
            tokio::spawn(async move { db.close().await });
        }
        self.conns.active = None;
        self.conns.restoring.clear();
        self.splash = None;
        for tab in &mut self.tabs {
            tab.conn = None;
        }
    }

    /// Makes a freshly opened connection the active tab's, replacing (and,
    /// if no other tab uses it, closing) the one it had.
    pub(super) fn attach_connection(&mut self, db: Database) {
        self.tabs[self.active_tab].conn = None;
        self.switch_conn(None);
        self.release_unused_conns();
        let id = self.conns.next_id;
        self.conns.next_id += 1;
        self.tabs[self.active_tab].conn = Some(id);
        self.conns.active = Some(id);
        self.database = Some(db);
    }

    /// Ctrl+D: disconnects this tab. The connection stays open while other
    /// tabs use it.
    pub(super) fn disconnect(&mut self) {
        self.invalidate_tasks();
        // Closing the stream first lets close() take the session.
        self.row_stream = None;
        self.fetching_more = false;
        self.executing = false;
        self.cancelling = false;
        self.tab.grid.results = None;
        self.tab.grid.result_sets.clear();
        self.tabs[self.active_tab].conn = None;
        self.switch_conn(None);
        self.release_unused_conns();
        self.save_draft();
        self.open_manager(Screen::Connections);
        self.drop_blank_tab = true;
        self.connections_message = Some(("Disconnected".to_string(), false));
    }

    /// Ctrl+O: picks a connection for this tab, or for a new tab if this
    /// one is connected.
    pub(super) fn open_connection(&mut self) {
        if self.executing {
            return self.set_status("Wait for the query to finish (or press Esc) first".into(), true);
        }
        self.open_manager(Screen::Connections);
        self.free_tab_for_form();
        self.connections_message = Some(("Pick a connection for this tab".to_string(), false));
    }

    /// Esc on the projects and connections: back to the project's tabs,
    /// connected or not, dropping an empty tab opened for a connection.
    pub(super) fn leave_connections(&mut self) {
        self.connections_message = None;
        self.restore_active_form();
        self.screen = Screen::Query;
        let blank = self.tab.editor.is_empty() && self.tabs[self.active_tab].saved_id.is_none();
        if std::mem::take(&mut self.drop_blank_tab) && self.conns.active.is_none() && blank && self.tabs.len() > 1 {
            self.close_tab();
        }
    }

    /// Names of the connections open in tabs (the connections pane marks
    /// them).
    pub fn open_connection_names(&self) -> Vec<&str> {
        let active = self.conns.active.map(|_| self.active_form().name.as_str());
        let parked = self.conns.parked.values().filter(|s| s.database.is_some()).map(|s| s.connect_form.name.as_str());
        active.into_iter().chain(parked).collect()
    }

    /// The saved connection tab `i` uses, by name (unsaved ones have none).
    pub(super) fn tab_connection_name(&self, i: usize) -> Option<String> {
        let name = match self.tabs.get(i)?.conn {
            None => {
                return self
                    .conns
                    .tab_connections
                    .get(i)
                    .cloned()
                    .flatten()
                    .filter(|n| self.conns.restoring.contains(n));
            }
            Some(id) if Some(id) == self.conns.active => &self.active_form().name,
            Some(id) => &self.conns.parked.get(&id)?.connect_form.name,
        };
        self.profiles().iter().any(|p| p.name == *name).then(|| name.clone())
    }

    pub(super) fn session_ui(&self) -> SessionUi {
        let mut expanded = self.conns.restore_expanded.clone();
        let mut add = |form: &ConnectForm, schema: &Option<SchemaPanel>| {
            if let Some(panel) = schema
                && !form.name.is_empty()
            {
                let open = panel.tables.iter().filter(|t| t.expanded);
                expanded
                    .insert(form.name.clone(), open.map(|t| (t.info.schema.clone(), t.info.name.clone())).collect());
            }
        };
        add(self.active_form(), &self.schema);
        for slot in self.conns.parked.values() {
            add(&slot.connect_form, &slot.schema);
        }
        expanded.retain(|_, tables| !tables.is_empty());
        let focus = match self.focus {
            Focus::Editor => "editor",
            Focus::Results => "results",
            Focus::Schema => "schema",
        };
        SessionUi {
            on_query: matches!(self.screen, Screen::Query) || self.manager_from_query,
            focus: focus.into(),
            schema_visible: self.schema_visible,
            expanded,
        }
    }

    /// On startup: opens what the command line asked for (a project, a
    /// connection), or else restores the last session.
    pub fn start(&mut self, project: Option<&str>, connection: Option<&str>) {
        if let Some(name) = project
            && let Err(e) = self.open_project_by_name(name)
        {
            self.projects_message = Some((e, true));
            return;
        }
        let Some(connection) = connection else {
            if project.is_none() {
                self.restore_session();
            }
            return;
        };
        if self.current_project.is_none()
            && let Some(id) = self.store.last_project.clone().filter(|id| self.store.project(id).is_some())
        {
            self.open_project(&id);
        }
        match crate::cli::classify(connection) {
            crate::cli::Target::Saved(name) => {
                let profile = self.profiles().iter().find(|p| p.name.eq_ignore_ascii_case(&name)).cloned();
                if let Some(profile) = profile {
                    self.conns.auto_connect = true;
                    self.load_profile(profile);
                } else {
                    let msg = format!("No saved connection named '{name}' here");
                    if self.current_project.is_some() {
                        self.connections_message = Some((msg, true));
                    } else {
                        self.projects_message = Some((msg, true));
                    }
                }
            }
            address => match crate::cli::form_for_address(&address) {
                Ok(form) => {
                    self.connect_form = form;
                    self.screen = Screen::Connect;
                    self.start_connect();
                }
                Err(e) => self.projects_message = Some((e, true)),
            },
        }
    }

    /// On startup: reopens the last project and, if its query screen was
    /// open, reconnects its tabs' connections (passwords from the keychain).
    pub fn restore_session(&mut self) {
        if !self.settings.restore_session || self.data_dir.is_none() {
            return;
        }
        let Some(project_id) = self.store.last_project.clone() else { return };
        if self.store.project(&project_id).is_none() {
            return;
        }
        self.open_project(&project_id);
        self.resume_project();
    }

    /// After opening a project: if its query screen was open last time,
    /// goes back to it and reconnects its tabs' connections.
    pub(super) fn resume_project(&mut self) {
        if !self.settings.restore_session {
            return;
        }
        let Some(project_id) = self.current_project.clone() else { return };
        let session = self.conns.saved_session.clone();
        if !session.on_query {
            return;
        }
        let mut names: Vec<String> = self.conns.tab_connections.iter().flatten().cloned().collect();
        names.sort_unstable();
        names.dedup();
        let profiles: Vec<ConnectionProfile> =
            self.profiles().iter().filter(|p| names.contains(&p.name)).cloned().collect();
        if profiles.is_empty() || !self.may_reach_outside() {
            return;
        }
        self.screen = Screen::Query;
        self.schema_visible = session.schema_visible;
        self.focus = match session.focus.as_str() {
            "results" => Focus::Results,
            "schema" if session.schema_visible => Focus::Schema,
            _ => Focus::Editor,
        };
        self.conns.restore_expanded = session.expanded;
        self.conns.restoring = profiles.iter().map(|p| p.name.clone()).collect();
        self.set_status(format!("Reconnecting to {}…", self.conns.restoring.join(", ")), false);
        let project = self.project().map(|p| p.name.clone()).unwrap_or_default();
        self.splash = Some(Splash { project, started: Instant::now(), shown: false, done: Vec::new() });
        for profile in profiles {
            let send = self.sender();
            let project_id = project_id.clone();
            tokio::spawn(async move {
                let (id, saved) = (project_id.clone(), profile.clone());
                let (password, note) =
                    tokio::task::spawn_blocking(move || store::saved_password(&id, &saved)).await.unwrap_or_default();
                let mut form = ConnectForm::from_profile(&profile, password);
                form.keychain_note = note;
                let result = match form.params() {
                    // Don't recreate a database file that was moved or deleted.
                    Ok(params) if params.file_to_create().is_some() => {
                        Err(format!("{} doesn't exist (moved or deleted?)", params.host))
                    }
                    Ok(params) => Database::connect(params).await.map_err(|e| crate::db::error_message(&e)),
                    Err(e) => Err(e),
                };
                send(DbEventKind::Restored { project_id, form: Box::new(form), result });
            });
        }
    }

    /// A connection reopened by [`restore_session`] arrived.
    pub(super) fn apply_restored(&mut self, project_id: &str, form: ConnectForm, result: Result<Database, String>) {
        if self.current_project.as_deref() != Some(project_id) || !self.conns.restoring.contains(&form.name) {
            if let Ok(db) = result {
                tokio::spawn(async move { db.close().await });
            }
            return;
        }
        self.conns.restoring.retain(|n| *n != form.name);
        if let Some(splash) = &mut self.splash {
            splash.done.push((form.name.clone(), result.as_ref().err().cloned()));
        }
        // All reopened: the card's job is done (failures stay in the status bar).
        if self.conns.restoring.is_empty() {
            self.splash = None;
        }
        let db = match result {
            Ok(db) => db,
            Err(e) => {
                let msg =
                    format!("Couldn't reconnect '{}': {} (Ctrl+O to connect)", form.name, form.connect_failure(&e));
                self.log(msg.clone(), true);
                return self.set_status(msg, true);
            }
        };
        let id = self.conns.next_id;
        self.conns.next_id += 1;
        for (tab, name) in self.tabs.iter_mut().zip(&self.conns.tab_connections) {
            if name.as_deref() == Some(form.name.as_str()) {
                tab.conn = Some(id);
            }
        }
        let project = self.project().map(|p| p.name.clone()).unwrap_or_default();
        self.log(format!("Reconnected to {}", form.summary()), false);
        let slot = ConnSlot {
            database: Some(db),
            active_mode: form.mode,
            tx_mode: form.tx_mode(),
            connection_summary: format!("[{project}] {}", form.summary()),
            connect_form: form,
            ..ConnSlot::default()
        };
        self.conns.parked.insert(id, slot);
        if self.tabs[self.active_tab].conn == Some(id) {
            self.switch_conn(Some(id));
        }
        if self.conns.restoring.is_empty() && !self.status_is_error {
            self.set_status("Session restored".to_string(), false);
        }
    }

    /// Re-expands the tables that were open last time, once the catalog of
    /// a restored connection is loaded.
    pub(super) fn restore_expanded_tables(&mut self) {
        let Some(open) = self.conns.restore_expanded.remove(&self.connect_form.name) else { return };
        let Some(panel) = &mut self.schema else { return };
        let wanted: Vec<usize> = (0..panel.tables.len())
            .filter(|&t| open.iter().any(|(s, n)| *s == panel.tables[t].info.schema && *n == panel.tables[t].info.name))
            .collect();
        for &t in &wanted {
            panel.tables[t].expanded = true;
        }
        for t in wanted {
            self.request_details(t);
        }
    }
}
