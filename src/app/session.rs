//! One connection per tab, and reopening the last session on startup.
//!
//! The active tab's connection is `App::connection`, as the active tab's
//! state is `App::tab`; other tabs' connections are parked as
//! [`Connection`]s, and switching swaps the two. Tabs opened with Ctrl+T share
//! their tab's connection (and so its session and transaction).

use crate::config::store::ProjectId;
use serde::{Deserialize, Serialize};

use super::*;

pub use super::conn_state::{ConnState, KeepAlive};

/// A connection and what goes with it: its form, modes, transaction,
/// navigator, health, summary and connect time. The active value is
/// `ProjectConnections::active`; other open connections are parked in slots.
#[derive(Default)]
pub struct Connection {
    pub database: Option<Database>,
    pub form: ConnectForm,
    /// Its mode (normal, read-only, production).
    pub mode: ConnectionMode,
    /// What happens to writes on it (Ctrl+K w).
    pub tx_mode: TxMode,
    pub txn: TxnState,
    /// Where it stands: opening, open, lost, failed.
    pub state: ConnState,
    /// The heartbeat's timing and check for this connection's session.
    pub keepalive: KeepAlive,
    /// The schema navigator's catalog and state.
    pub schema: Option<SchemaPanel>,
    /// Where it goes, for the status bar.
    pub summary: String,
    /// How long connecting took, for the application bar.
    pub latency: Option<Duration>,
    /// The sandbox open on it (`TxMode::Sandbox`), while its transaction is.
    pub sandbox: Option<super::Sandbox>,
    /// Its tables a rollback doesn't undo (MySQL's `MyISAM`, `MEMORY`…),
    /// read with the catalog: the sandbox and rollback-only refuse to
    /// write them, since the write would stay.
    pub unrollable: Vec<Unrollable>,
}

/// A table a rollback doesn't undo: its schema (database) and name,
/// lowercased.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unrollable {
    pub schema: String,
    pub table: String,
}

impl Unrollable {
    /// Whether `name`, as a statement writes it (`audit`, `shop.audit`,
    /// quoted or not), may be this table. A bare name is, whatever the
    /// database: which one the session is in can change (`USE`) since the
    /// list was read, and a write wrongly let through would stay. Named
    /// with its database, it's that database's alone.
    pub fn is(&self, name: &str) -> bool {
        let parts: Vec<String> = name.split('.').map(|p| p.trim().trim_matches(['`', '"']).to_lowercase()).collect();
        match parts.as_slice() {
            [table] => *table == self.table,
            [schema, table] => *schema == self.schema && *table == self.table,
            _ => false,
        }
    }
}

/// Connection IDs used by tabs, parked connections and session restoration.
#[derive(Default)]
pub struct Conns {
    /// ID of the active connection slot, whose value is `ProjectConnections::active`.
    pub active: Option<ConnId>,
    parked: HashMap<ConnId, Connection>,
    next_id: ConnId,
    /// Each tab's saved connection name, from the tabs file.
    pub tab_connections: Vec<Option<String>>,
    /// UI state saved with the tabs.
    pub saved_session: SessionUi,
    /// The last opening's number ([`ConnState::Opening`]'s `attempt`).
    last_attempt: u64,
    /// A run asked for on a tab whose connection is being opened again
    /// (`retry_tab_connection`): done once it's open.
    pub after_reconnect: Option<crate::config::keys::Action>,
    /// Expanded schema tables to restore, by connection name.
    restore_expanded: HashMap<String, Vec<(String, String)>>,
    /// Connect as soon as the saved connection's password is loaded (it was
    /// named on the command line).
    pub auto_connect: bool,
}

/// The project's active connection and its other open connection slots.
/// Tabs refer to these slots by ID; they do not own the connection values.
#[derive(Default)]
pub struct ProjectConnections {
    pub active: Connection,
    pub(super) slots: Conns,
}

impl ProjectConnections {
    pub(super) fn switch_active(&mut self, target: Option<ConnId>) -> bool {
        self.slots.switch_active(&mut self.active, target)
    }

    pub(super) fn drain(&mut self) -> Vec<Connection> {
        self.slots.drain(&mut self.active)
    }

    pub(super) fn attach_active(&mut self, slot: Connection) -> ConnId {
        self.slots.attach_active(&mut self.active, slot)
    }

    /// Connection `id`: the shown one, or a parked one.
    pub(super) fn slot_mut(&mut self, id: ConnId) -> Option<&mut Connection> {
        if self.slots.active == Some(id) { Some(&mut self.active) } else { self.slots.parked.get_mut(&id) }
    }

    /// Where connection `id` stands (the shown one's, or a parked one's).
    pub(super) fn state_of(&self, id: ConnId) -> Option<&ConnState> {
        if self.slots.active == Some(id) {
            Some(&self.active.state)
        } else {
            self.slots.parked.get(&id).map(|s| &s.state)
        }
    }

    /// Every connection slot: the shown one first.
    pub(super) fn all(&self) -> impl Iterator<Item = (ConnId, &Connection)> {
        let shown = self.slots.active.map(|id| (id, &self.active));
        shown.into_iter().chain(self.slots.parked.iter().map(|(id, slot)| (*id, slot)))
    }
}

impl std::ops::Deref for ProjectConnections {
    type Target = Connection;

    fn deref(&self) -> &Self::Target {
        &self.active
    }
}

impl std::ops::DerefMut for ProjectConnections {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.active
    }
}

impl Conns {
    /// Parks the active slot and activates `target`. A missing target leaves
    /// both the active ID and slot untouched.
    fn switch_active(&mut self, active: &mut Connection, target: Option<ConnId>) -> bool {
        if target == self.active || target.is_some_and(|id| !self.parked.contains_key(&id)) {
            return false;
        }

        let current = std::mem::take(active);
        if let Some(id) = self.active {
            self.parked.insert(id, current);
        }
        if let Some(id) = target {
            *active = self.parked.remove(&id).expect("the target connection was checked above");
        }
        self.active = target;
        true
    }

    /// Gives a new connection a unique ID and makes its slot active.
    fn attach_active(&mut self, active: &mut Connection, slot: Connection) -> ConnId {
        debug_assert!(self.active.is_none(), "detach the current connection before attaching another");
        let id = self.allocate_id();
        *active = slot;
        self.active = Some(id);
        id
    }

    /// Stores a connection that will be activated when one of its tabs is shown.
    fn park_new(&mut self, slot: Connection) -> ConnId {
        let id = self.allocate_id();
        let previous = self.parked.insert(id, slot);
        debug_assert!(previous.is_none(), "connection IDs are never reused");
        id
    }

    /// Removes the active and parked slots, leaving no active connection.
    fn drain(&mut self, active: &mut Connection) -> Vec<Connection> {
        let mut slots = Vec::with_capacity(self.parked.len() + 1);
        slots.push(std::mem::take(active));
        slots.extend(self.parked.drain().map(|(_, slot)| slot));
        self.active = None;
        slots
    }

    fn allocate_id(&mut self) -> ConnId {
        self.next_id.next()
    }
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

/// The project an address opened on the first run goes into.
const DEFAULT_PROJECT: &str = "default";

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
        self.connection.slots.active.is_some() && self.connection.state.is_open()
    }

    /// Marks the active tab connected without a database, for UI tests.
    #[cfg(test)]
    pub fn pretend_connected(&mut self) {
        self.connection.slots.active = Some(ConnId::from_test_id(u64::MAX));
        // As one whose tables loaded (a test of the states sets its own).
        self.connection.state = ConnState::Ready { verified: true };
        self.tabs[self.active_tab].conn = Some(ConnId::from_test_id(u64::MAX));
    }

    /// A transaction is open on any tab's connection.
    pub(super) fn any_open_transaction(&self) -> bool {
        self.connection.txn != TxnState::Idle || self.connection.slots.parked.values().any(|s| s.txn != TxnState::Idle)
    }

    /// Leaves the project for `target` (or, with `None`, the project list),
    /// asking first if a transaction would be rolled back. With `panes`,
    /// `target`'s connections take the keys rather than its tabs.
    pub(super) fn switch_project(&mut self, target: Option<ProjectId>, panes: bool) {
        // Leaving stops every tab's query: each has to end first.
        if !self.running_tabs().is_empty() {
            return self.set_status(t!("session.wait").to_string(), true);
        }
        if self.any_open_transaction() {
            self.popups.confirm = Some(Confirm {
                message: t!("session.leave_rollback").to_string(),
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
    pub(super) fn go_to_project(&mut self, target: Option<ProjectId>, panes: bool) {
        self.leave_project();
        if let Some(id) = target.filter(|id| self.store.project(id).is_some()) {
            self.open_project(&id);
            self.resume_project();
            if panes {
                // Leaving it from here keeps where it was left.
                self.manager.from_query = matches!(self.screen, Screen::Query);
                self.screen = Screen::Connections;
            }
        }
    }

    /// Saves the tabs and session, closes the connections and shows the
    /// project list. Opening the project again brings it all back.
    pub(super) fn leave_project(&mut self) {
        // Its answer would go to a tab of another project.
        self.stop_asking();
        self.popups.memory_view = None;
        self.save_draft();
        if let Some(id) = &self.current_project {
            self.manager.project_selected = self.store.projects.iter().position(|p| &p.id == id).unwrap_or(0);
        }
        self.close_all_connections();
        self.reset_agents();
        // Opening it again reloads its tabs and session from disk.
        self.manager.left_project = self.current_project.take();
        self.popups.menu = None;
        self.panes.zoomed = false;
        self.screen = Screen::Projects;
    }

    /// Connections being opened again (the last session's, or a retry),
    /// by name.
    pub fn reconnecting(&self) -> Vec<String> {
        let mut names: Vec<String> =
            self.connection.all().filter(|(_, c)| c.state.is_opening()).map(|(_, c)| c.form.name.clone()).collect();
        names.sort();
        names
    }

    /// The shown tab's connection, when it couldn't be opened again: its
    /// name, and why.
    pub fn tab_connection_failure(&self) -> Option<(&str, &str)> {
        self.connection.slots.active?;
        self.connection.state.failure().map(|why| (self.connection.form.name.as_str(), why))
    }

    /// Makes connection `target` the active one, parking the current one.
    /// Puts `target`'s state in the app's fields, parking the active one's.
    pub(super) fn swap_conns(&mut self, target: Option<ConnId>) {
        self.connection.switch_active(target);
    }

    pub(super) fn switch_conn(&mut self, target: Option<ConnId>) {
        if !self.connection.switch_active(target) {
            return;
        }
        // (A check on its way is its connection's, and still awaited: its
        // answer finds it, shown or parked, so none starts beside it.)
        // Events from the other connection's background work no longer
        // apply; restart what was cut short.
        // (A check on its way is for the connection before: see `pinging`.)
        self.generation.bump();
        self.tab.syntax_hint = None;
        let has_database = self.connection.database.is_some();
        match &mut self.connection.schema {
            Some(panel) if panel.loading => self.load_catalog(),
            Some(panel) => panel.tables.iter_mut().for_each(|t| t.details_loading = false),
            None if has_database => self.load_catalog(),
            None => {}
        }
    }

    /// Connection `id`'s transaction state.
    pub(super) fn conn_txn(&self, id: ConnId) -> Option<TxnState> {
        if self.connection.slots.active == Some(id) {
            return Some(self.connection.txn);
        }
        self.connection.slots.parked.get(&id).map(|slot| slot.txn)
    }

    /// A connection's name, by its id (the shown one's or a parked one's):
    /// its saved name, else its file's name or `host/database`.
    pub(super) fn conn_name(&self, id: ConnId) -> Option<String> {
        if self.connection.slots.active == Some(id) {
            return Some(self.connection.form.short_name());
        }
        self.connection.slots.parked.get(&id).map(|slot| slot.form.short_name())
    }

    /// Where connection `id` goes (`PostgreSQL db.example.com:5432/shop`),
    /// for the connections list.
    pub(super) fn conn_address(&self, id: ConnId) -> String {
        let form = if self.connection.slots.active == Some(id) {
            Some(&self.connection.form)
        } else {
            self.connection.slots.parked.get(&id).map(|s| &s.form)
        };
        form.map(|f| f.summary()).unwrap_or_default()
    }

    /// From the connections list: to the shown tab if it's on `id`, else
    /// the first tab on it.
    pub(super) fn go_to_connection(&mut self, id: ConnId) {
        if self.tabs[self.active_tab].conn == Some(id) {
            return;
        }
        if let Some(i) = self.tabs.iter().position(|t| t.conn == Some(id)) {
            self.switch_tab(i);
        }
    }

    /// Ctrl+D in the connections list: asks before closing connection `id`
    /// (its tabs are disconnected, an open transaction rolled back). Not
    /// while a query runs on it: that's stopped in its tab first.
    pub(super) fn request_close_connection(&mut self, id: ConnId) {
        let Some(name) = self.conn_name(id) else { return };
        let tabs: Vec<usize> = (0..self.tabs.len()).filter(|&i| self.tabs[i].conn == Some(id)).collect();
        if let Some(&busy) = tabs.iter().find(|&&i| self.tab_running(i)) {
            self.popups.palette = None;
            let tab = self.tabs[busy].name.clone();
            return self.set_status(t!("conn_list.busy", name = name, tab = tab), true);
        }
        let names: Vec<&str> = tabs.iter().map(|&i| self.tabs[i].name.as_str()).collect();
        let names = if names.is_empty() { t!("conn_list.no_tabs").to_string() } else { names.join(", ") };
        let open = self.conn_txn(id) == Some(TxnState::Open);
        self.popups.palette = None;
        self.popups.confirm = Some(Confirm {
            message: if open {
                t!("conn_list.close_rollback", name = name, tabs = names)
            } else {
                t!("conn_list.close_ask", name = name, tabs = names)
            },
            details: Vec::new(),
            danger: open,
            action: ConfirmAction::CloseConnection(id),
            offer_transaction: false,
        });
    }

    /// Ctrl+X in the connections list: asks before closing every open
    /// connection (open transactions rolled back). Not while a query runs.
    pub(super) fn request_close_all_connections(&mut self) {
        let mut ids: Vec<ConnId> = self.connection.slots.active.into_iter().collect();
        ids.extend(self.parked_connections().into_iter().map(|(id, ..)| id));
        self.popups.palette = None;
        if let Some(busy) = (0..self.tabs.len()).find(|&i| self.tab_running(i)) {
            let tab = self.tabs[busy].name.clone();
            return self.set_status(t!("conn_list.busy_all", tab = tab), true);
        }
        let open = ids.iter().any(|&id| self.conn_txn(id) == Some(TxnState::Open));
        let n = ids.len();
        self.popups.confirm = Some(Confirm {
            message: if open {
                t!("conn_list.close_all_rollback", n = n)
            } else {
                t!("conn_list.close_all_ask", n = n)
            },
            details: Vec::new(),
            danger: open,
            action: ConfirmAction::CloseAllConnections,
            offer_transaction: false,
        });
    }

    /// Closes every open connection, as `close_connection` closes one.
    pub(super) fn close_every_connection(&mut self) {
        let mut ids: Vec<ConnId> = self.connection.slots.active.into_iter().collect();
        ids.extend(self.parked_connections().into_iter().map(|(id, ..)| id));
        let n = ids.len();
        for id in ids {
            self.close_connection(id);
        }
        self.set_status(t!("conn_list.closed_all", n = n), false);
    }

    /// Closes connection `id`: every tab on it is disconnected (and isn't
    /// reopened by its next run), the connection closed; the shown tab
    /// stays where it is.
    pub(super) fn close_connection(&mut self, id: ConnId) {
        let Some(name) = self.conn_name(id) else { return };
        if self.connection.slots.active == Some(id) {
            self.invalidate_tasks();
            self.tab.run.set_stream(None);
            self.tab.run.fetching_more = false;
            self.tab.run.executing = false;
            self.tab.run.cancelling = false;
            self.tab.run.cancel_failed = false;
        }
        for i in 0..self.tabs.len() {
            if self.tabs[i].conn == Some(id) {
                self.tabs[i].conn = None;
                // Closed on purpose: a run there doesn't open it again.
                if let Some(saved) = self.connection.slots.tab_connections.get_mut(i) {
                    *saved = None;
                }
            }
        }
        if self.connection.slots.active == Some(id) {
            self.switch_conn(None);
        }
        self.release_unused_conns();
        self.save_draft();
        self.set_status(t!("conn_list.closed", name = name), false);
    }

    /// The other tabs' connections: id, database and form.
    pub(super) fn parked_connections(&self) -> Vec<(ConnId, &Database, &ConnectForm)> {
        let mut out: Vec<_> = self
            .connection
            .slots
            .parked
            .iter()
            .filter_map(|(id, slot)| Some((*id, slot.database.as_ref()?, &slot.form)))
            .collect();
        out.sort_by_key(|(id, ..)| *id);
        out
    }

    /// Closes parked connections that no tab uses any more.
    pub(super) fn release_unused_conns(&mut self) {
        let unused: Vec<ConnId> = self
            .connection
            .slots
            .parked
            .keys()
            .copied()
            .filter(|id| self.tabs.iter().all(|t| t.conn != Some(*id)))
            .collect();
        for id in unused {
            if let Some(db) = self.connection.slots.parked.remove(&id).and_then(|s| s.database) {
                tokio::spawn(async move { db.close().await });
            }
        }
    }

    /// Closes every connection (when the project's tabs are replaced).
    pub(super) fn close_all_connections(&mut self) {
        self.stop_all_runs();
        self.tab.run.set_stream(None);
        self.tab.run.executing = false;
        for db in self.connection.drain().into_iter().filter_map(|slot| slot.database) {
            tokio::spawn(async move { db.close().await });
        }
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
        let id = self.connection.attach_active(Connection { database: Some(db), ..Connection::default() });
        self.tabs[self.active_tab].conn = Some(id);
    }

    /// The shown connection was opened by hand: the tabs of a slot that
    /// failed to open the same saved connection use it too (the failure
    /// isn't theirs any more), and the failed slot goes.
    pub(super) fn take_over_failed_slots(&mut self) {
        let Some(opened) = self.connection.slots.active else { return };
        let name = self.connection.form.name.clone();
        if name.is_empty() {
            return;
        }
        // The slot's name, or the tab's from the last session: a failed
        // slot shown in the tab where it was opened by hand gave that its
        // form, name and all.
        let failed: Vec<ConnId> = self
            .connection
            .all()
            .filter(|(id, slot)| *id != opened && slot.state.failure().is_some())
            .map(|(id, _)| id)
            .collect();
        let mut moved = false;
        for (i, tab) in self.tabs.iter_mut().enumerate() {
            let Some(conn) = tab.conn.filter(|c| failed.contains(c)) else { continue };
            let slot_name = self.connection.all().find(|(id, _)| *id == conn).map(|(_, s)| s.form.name.as_str());
            let tab_name = self.connection.slots.tab_connections.get(i).and_then(Option::as_deref);
            if slot_name == Some(name.as_str()) || tab_name == Some(name.as_str()) {
                tab.conn = Some(opened);
                moved = true;
            }
        }
        if moved {
            self.release_unused_conns();
        }
    }

    /// Ctrl+D: disconnects this tab. The connection stays open while other
    /// tabs use it.
    pub(super) fn disconnect(&mut self) {
        self.invalidate_tasks();
        // Closing the stream first lets close() take the session.
        self.tab.run.set_stream(None);
        self.tab.run.fetching_more = false;
        self.tab.run.executing = false;
        self.tab.run.cancelling = false;
        self.tab.run.cancel_failed = false;
        self.tab.grid.results = None;
        self.tab.grid.result_sets.clear();
        self.tabs[self.active_tab].conn = None;
        // Disconnected on purpose: a run here doesn't open it again
        // (`tab_connection_to_retry`).
        if let Some(name) = self.connection.slots.tab_connections.get_mut(self.active_tab) {
            *name = None;
        }
        self.switch_conn(None);
        self.release_unused_conns();
        self.save_draft();
        self.open_manager(Screen::Connections);
        self.manager.drop_blank_tab = true;
        self.manager.connection_message = Some(("Disconnected".to_string(), false));
    }

    /// Ctrl+O: picks a connection for this tab, or for a new tab if this
    /// one is connected.
    pub(super) fn open_connection(&mut self) {
        if self.tab.run.executing {
            return self.set_status(t!("session.wait").into(), true);
        }
        self.open_manager(Screen::Connections);
        self.free_tab_for_form();
        self.manager.connection_message = Some((t!("session.pick").to_string(), false));
    }

    /// Esc on the projects and connections: back to the project's tabs,
    /// connected or not, dropping an empty tab opened for a connection.
    pub(super) fn leave_connections(&mut self) {
        self.manager.connection_message = None;
        self.restore_active_form();
        self.screen = Screen::Query;
        let blank = self.tab.editor.is_empty() && self.tabs[self.active_tab].saved_id.is_none();
        if std::mem::take(&mut self.manager.drop_blank_tab)
            && self.connection.slots.active.is_none()
            && blank
            && self.tabs.len() > 1
        {
            self.close_tab();
        }
    }

    /// Names of the connections open in tabs (the connections pane marks
    /// them).
    pub fn open_connection_names(&self) -> Vec<&str> {
        let active = self.connection.slots.active.map(|_| self.active_form().name.as_str());
        let parked =
            self.connection.slots.parked.values().filter(|s| s.database.is_some()).map(|s| s.form.name.as_str());
        active.into_iter().chain(parked).collect()
    }

    /// The saved connection tab `i` uses, by name (unsaved ones have none).
    pub(super) fn tab_connection_name(&self, i: usize) -> Option<String> {
        // (A connection being opened again, or that couldn't be, has its
        // slot: its name is kept.)
        let name = match self.tabs.get(i)?.conn {
            None => return None,
            Some(id) if Some(id) == self.connection.slots.active => &self.active_form().name,
            Some(id) => &self.connection.slots.parked.get(&id)?.form.name,
        };
        self.profiles().iter().any(|p| p.name == *name).then(|| name.clone())
    }

    pub(super) fn session_ui(&self) -> SessionUi {
        let mut expanded = self.connection.slots.restore_expanded.clone();
        let mut add = |form: &ConnectForm, schema: &Option<SchemaPanel>| {
            if let Some(panel) = schema
                && !form.name.is_empty()
            {
                let open = panel.tables.iter().filter(|t| t.expanded);
                expanded
                    .insert(form.name.clone(), open.map(|t| (t.info.schema.clone(), t.info.name.clone())).collect());
            }
        };
        add(self.active_form(), &self.connection.schema);
        for slot in self.connection.slots.parked.values() {
            add(&slot.form, &slot.schema);
        }
        expanded.retain(|_, tables| !tables.is_empty());
        let focus = match self.focus {
            Focus::Editor => "editor",
            Focus::Results => "results",
            Focus::Schema => "schema",
        };
        SessionUi {
            on_query: matches!(self.screen, Screen::Query) || self.manager.from_query,
            focus: focus.into(),
            schema_visible: self.panes.schema_visible,
            expanded,
        }
    }

    /// On startup: opens what the command line asked for (a project, a
    /// connection), or else restores the last session.
    pub fn start(&mut self, project: Option<&str>, connection: Option<&str>) {
        if let Some(name) = project
            && let Err(e) = self.open_project_by_name(name)
        {
            self.manager.project_message = Some((e, true));
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
        let target = crate::config::draft::classify(connection);
        // An address with no project yet (the first run): one called
        // "default" keeps its tabs and history, as any session's.
        if self.current_project.is_none() && !matches!(target, crate::config::draft::Target::Saved(_)) {
            let id = match self.store.project_by_name(DEFAULT_PROJECT) {
                Some(p) => Ok(p.id.clone()),
                None => self.store.add_project(DEFAULT_PROJECT),
            };
            match id {
                Ok(id) => self.open_project(&id),
                Err(e) => {
                    self.notes.status =
                        format!("Couldn't create the “{DEFAULT_PROJECT}” project ({e:#}): tabs won't be kept");
                }
            }
        }
        match target {
            crate::config::draft::Target::Saved(name) => {
                let profile = self.profiles().iter().find(|p| p.name.eq_ignore_ascii_case(&name)).cloned();
                if let Some(profile) = profile {
                    self.connection.slots.auto_connect = true;
                    self.load_profile(profile);
                } else {
                    let msg = t!("session.no_saved", name = name);
                    if self.current_project.is_some() {
                        self.manager.connection_message = Some((msg, true));
                    } else {
                        self.manager.project_message = Some((msg, true));
                    }
                }
            }
            address => match ConnectionDraft::from_address(&address).map(|d| ConnectForm::from(d.with_absolute_file()))
            {
                Ok(form) => {
                    self.connection.form = form;
                    self.screen = Screen::Connect;
                    self.start_connect();
                }
                Err(e) => self.manager.project_message = Some((e, true)),
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
        if self.current_project.is_none() {
            return;
        }
        let session = self.connection.slots.saved_session.clone();
        if !session.on_query {
            return;
        }
        let mut names: Vec<String> = self.connection.slots.tab_connections.iter().flatten().cloned().collect();
        names.sort_unstable();
        names.dedup();
        let profiles: Vec<ConnectionProfile> =
            self.profiles().iter().filter(|p| names.contains(&p.name)).cloned().collect();
        if profiles.is_empty() || !self.may_reach_outside() {
            return;
        }
        self.screen = Screen::Query;
        self.panes.schema_visible = session.schema_visible;
        self.focus = match session.focus.as_str() {
            "results" => Focus::Results,
            "schema" if session.schema_visible => Focus::Schema,
            _ => Focus::Editor,
        };
        self.connection.slots.restore_expanded = session.expanded;
        let project = self.project().map(|p| p.name.clone()).unwrap_or_default();
        self.splash = Some(Splash { project, started: Instant::now(), shown: false, done: Vec::new() });
        self.connect_saved(profiles);
    }

    /// The saved connection the active tab had in the last session, when
    /// opening it again then failed (the server was down, the VPN off):
    /// a run there tries it again rather than saying "not connected".
    pub(super) fn tab_connection_to_retry(&self) -> Option<ConnectionProfile> {
        if self.connection.database.is_some() || !self.reconnecting().is_empty() {
            return None;
        }
        // Its slot says it failed; or it has none, but had one last time.
        let name = if self.connection.state.failure().is_some() && self.connection.slots.active.is_some() {
            &self.connection.form.name
        } else if self.tabs[self.active_tab].conn.is_none() {
            self.connection.slots.tab_connections.get(self.active_tab)?.as_ref()?
        } else {
            return None;
        };
        self.profiles().iter().find(|p| &p.name == name).cloned()
    }

    /// Opens the tab's connection again (`tab_connection_to_retry`), then
    /// does `action` (a run) once it's open.
    pub(super) fn retry_tab_connection(&mut self, profile: ConnectionProfile, action: crate::config::keys::Action) {
        self.connection.slots.after_reconnect = Some(action);
        self.connect_saved(vec![profile]);
    }

    /// Opens `profiles` in the background, as the session had them. Each
    /// gets its slot at once ([`ConnState::Opening`]), which the tabs that
    /// used it point at from then: what arrives (`Restored`) is for that
    /// slot and that attempt, or for nothing (closed, or tried again since).
    fn connect_saved(&mut self, profiles: Vec<ConnectionProfile>) {
        let Some(project_id) = self.current_project.clone() else { return };
        let project = self.project().map(|p| p.name.clone()).unwrap_or_default();
        for profile in profiles {
            crate::system::connection_log::record("reopening", &profile.name, profile.engine.label());
            self.connection.slots.last_attempt += 1;
            let attempt = self.connection.slots.last_attempt;
            // A retry opens the slot its tabs have; else a new one.
            let failed = self.connection.all().find(|(_, c)| c.form.name == profile.name && !c.state.is_open());
            let id = if let Some((id, _)) = failed {
                id
            } else {
                let form = ConnectForm::from_profile(&profile, String::new());
                self.connection.slots.park_new(Connection {
                    mode: form.mode,
                    tx_mode: form.tx_mode(),
                    summary: format!("[{project}] {}", form.summary()),
                    form,
                    ..Connection::default()
                })
            };
            if let Some(slot) = self.connection.slot_mut(id) {
                slot.state = ConnState::Opening { attempt };
            }
            for (tab, saved_name) in self.tabs.iter_mut().zip(&self.connection.slots.tab_connections) {
                if tab.conn.is_none() && saved_name.as_deref() == Some(profile.name.as_str()) {
                    tab.conn = Some(id);
                }
            }
            let send = self.sender();
            let project_id = project_id.clone();
            tokio::spawn(async move {
                let (for_password, saved) = (project_id.clone(), profile.clone());
                let (password, note) =
                    tokio::task::spawn_blocking(move || store::saved_password(&for_password, &saved))
                        .await
                        .unwrap_or_default();
                let mut form = ConnectForm::from_profile(&profile, password);
                form.keychain_note = note;
                let result = match form.params() {
                    // Don't recreate a database file that was moved or deleted.
                    Ok(params) if params.file_to_create().is_some() => Err(t!("session.file_gone", path = params.host)),
                    Ok(params) => Database::connect(params).await.map_err(|e| {
                        crate::system::connection_log::record(
                            "reopen failed",
                            &profile.name,
                            &crate::db::error_details(&e),
                        );
                        crate::db::error_message(&e)
                    }),
                    Err(e) => Err(e),
                };
                send(Background::Restored { project_id, id, attempt, form: Box::new(form), result }.into());
            });
        }
        // The shown tab's connection is the one opening for it.
        if let Some(id) = self.tabs[self.active_tab].conn
            && self.connection.slots.active != Some(id)
        {
            self.switch_conn(Some(id));
        }
        self.set_status(t!("session.reconnecting", names = self.reconnecting().join(", ")), false);
    }

    /// What opening connection `id` again (attempt `attempt`) came to.
    pub(super) fn apply_restored(
        &mut self,
        project_id: &ProjectId,
        id: ConnId,
        attempt: u64,
        form: ConnectForm,
        result: Result<Database, String>,
    ) {
        let awaited = self.current_project.as_ref() == Some(project_id)
            && self.connection.slot_mut(id).is_some_and(|slot| slot.state == ConnState::Opening { attempt });
        if !awaited {
            if let Ok(db) = result {
                tokio::spawn(async move { db.close().await });
            }
            return;
        }
        if let Some(splash) = &mut self.splash {
            splash.done.push((form.name.clone(), result.as_ref().err().cloned()));
        }
        let name = form.name.clone();
        let Some(slot) = self.connection.slot_mut(id) else { return };
        let db = match result {
            Ok(db) => db,
            Err(e) => {
                tracing::info!(connection = %name, error = %e, "couldn't reopen the connection");
                crate::system::connection_log::record("not reopened", &name, &e);
                let why = form.connect_failure(&e);
                slot.state = ConnState::Failed { why: why.clone() };
                slot.form = form;
                self.after_reopening();
                self.connection.slots.after_reconnect = None;
                let msg = t!("session.reconnect_failed", name = name, e = why, again = t!("session.again"));
                self.log(msg.clone(), true);
                return self.set_status(msg, true);
            }
        };
        tracing::info!(connection = %name, "reopened the connection");
        crate::system::connection_log::record("reopened", &name, db.version());
        let summary = form.summary();
        let project = self.project().map(|p| p.name.clone()).unwrap_or_default();
        let Some(slot) = self.connection.slot_mut(id) else { return };
        slot.database = Some(db);
        slot.state = ConnState::default();
        slot.mode = form.mode;
        slot.tx_mode = form.tx_mode();
        slot.summary = format!("[{project}] {summary}");
        slot.form = form;
        self.log(t!("session.reconnected", summary = summary), false);
        // Shown: its tables now (a parked one's load as its tab is shown).
        if self.connection.slots.active == Some(id) {
            self.load_catalog();
        }
        self.after_reopening();
        // (A retry for a run: the run says how it went.)
        if self.reconnecting().is_empty()
            && !self.notes.status_is_error
            && self.connection.slots.after_reconnect.is_none()
        {
            self.set_status(t!("session.restored").to_string(), false);
        }
        if let Some(action) =
            self.connection.slots.after_reconnect.take().filter(|_| self.connection.database.is_some())
        {
            self.run_action(action);
        }
    }

    /// One reopened (or not): once none is left opening, the card's job is
    /// done, unless one failed: then it stays (shown at once) until a key,
    /// saying which and why.
    fn after_reopening(&mut self) {
        if !self.reconnecting().is_empty() {
            return;
        }
        if let Some(splash) = &mut self.splash {
            if splash.done.iter().any(|(_, e)| e.is_some()) {
                splash.shown = true;
            } else {
                self.splash = None;
            }
        }
    }

    /// Re-expands the tables that were open last time, once the catalog of
    /// a restored connection is loaded.
    pub(super) fn restore_expanded_tables(&mut self) {
        let name = self.connection.form.name.clone();
        let Some(open) = self.connection.slots.restore_expanded.remove(&name) else { return };
        let Some(panel) = &mut self.connection.schema else { return };
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

#[cfg(test)]
mod conns_tests {
    use super::*;

    fn slot(summary: &str) -> Connection {
        Connection { summary: summary.to_string(), ..Connection::default() }
    }

    #[test]
    fn connection_owner_allocates_and_switches_slots_together() {
        let mut conns = Conns::default();
        let mut active = Connection::default();
        let first = conns.attach_active(&mut active, slot("first"));
        let second = conns.park_new(slot("second"));

        assert_ne!(first, second, "a new slot gets an id of its own");
        assert!(conns.switch_active(&mut active, Some(second)));
        assert_eq!(conns.active, Some(second));
        assert_eq!(active.summary, "second");
        assert_eq!(conns.parked.get(&first).unwrap().summary, "first");

        assert!(conns.switch_active(&mut active, None));
        assert_eq!(conns.active, None);
        assert_eq!(active.summary, "");
        assert_eq!(conns.parked.get(&second).unwrap().summary, "second");
    }

    #[test]
    fn a_missing_connection_does_not_disturb_the_active_slot() {
        let mut conns = Conns::default();
        let mut active = Connection::default();
        let id = conns.attach_active(&mut active, slot("active"));

        assert!(!conns.switch_active(&mut active, Some(ConnId::from_test_id(999))));
        assert_eq!(conns.active, Some(id));
        assert_eq!(active.summary, "active");
    }

    #[test]
    fn draining_connections_clears_active_and_parked_state() {
        let mut conns = Conns::default();
        let mut active = Connection::default();
        conns.attach_active(&mut active, slot("active"));
        conns.park_new(slot("parked"));

        let slots = conns.drain(&mut active);
        assert_eq!(slots.len(), 2);
        assert!(conns.active.is_none() && conns.parked.is_empty());
        assert_eq!(active.summary, "");
    }
}

#[cfg(test)]
mod project_connections_tests {
    use super::*;

    fn slot(summary: &str) -> Connection {
        Connection { summary: summary.to_string(), ..Connection::default() }
    }

    #[test]
    fn project_owner_moves_active_connections_and_drains_them_as_a_unit() {
        let mut project = ProjectConnections::default();
        let alpha = project.attach_active(slot("alpha"));
        let beta = project.slots.park_new(slot("beta"));

        assert!(project.switch_active(Some(beta)));
        assert_eq!(project.active.summary, "beta");
        assert_eq!(project.slots.active, Some(beta));
        assert_eq!(project.slots.parked.get(&alpha).unwrap().summary, "alpha");

        let drained = project.drain();
        assert_eq!(drained.len(), 2);
        assert!(project.slots.active.is_none() && project.slots.parked.is_empty());
        assert_eq!(project.active.summary, "");
    }
}
