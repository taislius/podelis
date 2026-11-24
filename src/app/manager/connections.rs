use super::keys::{self, ConnectionsKey};
use crate::app::*;
use crate::config::store::ProjectId;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectField {
    Name,
    Engine,
    Host,
    Port,
    User,
    Password,
    PasswordCommand,
    Database,
    Ssl,
    /// TLS's CA file, client certificate and key.
    TlsCa,
    TlsCert,
    TlsKey,
    Mode,
    Transaction,
    Timeout,
    Agents,
    Ssh,
}

const SERVER_FIELDS: [ConnectField; 17] = [
    ConnectField::Name,
    ConnectField::Engine,
    ConnectField::Host,
    ConnectField::Port,
    ConnectField::User,
    ConnectField::Password,
    ConnectField::PasswordCommand,
    ConnectField::Database,
    ConnectField::Ssl,
    ConnectField::TlsCa,
    ConnectField::TlsCert,
    ConnectField::TlsKey,
    ConnectField::Mode,
    ConnectField::Transaction,
    ConnectField::Timeout,
    ConnectField::Agents,
    ConnectField::Ssh,
];

/// SQLite has no server: just a file path (shown in the Host field).
const FILE_FIELDS: [ConnectField; 7] = [
    ConnectField::Name,
    ConnectField::Engine,
    ConnectField::Host,
    ConnectField::Mode,
    ConnectField::Transaction,
    ConnectField::Timeout,
    ConnectField::Agents,
];

impl ConnectField {
    /// One of TLS's files (CA, certificate, key).
    pub fn is_tls_file(self) -> bool {
        matches!(self, ConnectField::TlsCa | ConnectField::TlsCert | ConnectField::TlsKey)
    }
}

/// Whether TLS's files are offered: where TLS is required, or one is set.
pub fn shows_tls_files(mode: crate::db::SslMode, files: &crate::db::TlsFiles) -> bool {
    use crate::db::SslMode;
    matches!(mode, SslMode::Require | SslMode::VerifyCa | SslMode::VerifyFull) || !files.is_empty()
}

/// The connect form's keys with Ctrl (its letters are typed into fields):
/// Ctrl+S saves the connection, Ctrl+T tests it, Ctrl+Y copies the field.
pub const SAVE_KEY: char = 's';
pub const TEST_KEY: char = 't';
pub const COPY_KEY: char = 'y';

/// Ctrl+`c` as a sentence says it (`Ctrl+S`).
pub fn ctrl_words(c: char) -> String {
    format!("Ctrl+{}", c.to_ascii_uppercase())
}

/// The fields a connection to `engine` has, in order.
pub fn fields_for(engine: Engine) -> &'static [ConnectField] {
    if engine.is_file_based() { &FILE_FIELDS } else { &SERVER_FIELDS }
}

/// The connect form: a [`ConnectionDraft`] being edited, and the form's own
/// state (which field has focus, what's said, whether it's connecting).
/// It derefs to the draft, so `form.host` is the draft's.
pub struct ConnectForm {
    /// The connection's settings as typed.
    pub draft: ConnectionDraft,
    /// Name of the saved profile this form was opened from, so saving under
    /// a new name renames it instead of creating a duplicate.
    pub original_name: Option<String>,
    /// The original profile's password is under the pre-projects keychain key.
    pub(in crate::app) original_legacy: bool,
    pub focus: ConnectField,
    /// Where the cursor is in the focused field (a byte offset; `None`:
    /// at its end, as when a field is moved to).
    pub cursor: Option<usize>,
    /// Opened on another database than its saved connection's (`D` in the
    /// navigator): not that saved connection, whose name it keeps.
    pub other_database: bool,
    /// The password it was opened with (from the keychain), to tell a
    /// changed one.
    pub(in crate::app) opened_password: Option<String>,
    pub error: Option<String>,
    pub info: Option<String>,
    pub connecting: bool,
    /// A SQLite file the user agreed to create (by connecting again).
    pub create_file: Option<String>,
    /// Why the saved password couldn't be read from the keychain; said
    /// again if connecting without it fails.
    pub keychain_note: Option<String>,
}

impl std::ops::Deref for ConnectForm {
    type Target = ConnectionDraft;

    fn deref(&self) -> &ConnectionDraft {
        &self.draft
    }
}

impl std::ops::DerefMut for ConnectForm {
    fn deref_mut(&mut self) -> &mut ConnectionDraft {
        &mut self.draft
    }
}

impl From<ConnectionDraft> for ConnectForm {
    fn from(draft: ConnectionDraft) -> Self {
        Self {
            draft,
            original_name: None,
            original_legacy: false,
            focus: ConnectField::Name,
            cursor: None,
            other_database: false,
            opened_password: None,
            error: None,
            info: None,
            connecting: false,
            create_file: None,
            keychain_note: None,
        }
    }
}

impl ConnectForm {
    pub(crate) fn new() -> Self {
        ConnectionDraft::new().into()
    }

    /// The same connection's settings and name, as a form of its own (the
    /// form's own state, its focus and what it said, starts afresh).
    pub(in crate::app) fn duplicate(&self) -> Self {
        Self {
            original_name: self.original_name.clone(),
            original_legacy: self.original_legacy,
            keychain_note: self.keychain_note.clone(),
            other_database: self.other_database,
            ..self.draft.clone().into()
        }
    }

    pub(crate) fn from_profile(profile: &ConnectionProfile, password: String) -> Self {
        Self {
            original_name: Some(profile.name.clone()),
            original_legacy: profile.legacy_keychain,
            opened_password: Some(password.clone()),
            ..ConnectionDraft::from_profile(profile, password).into()
        }
    }

    pub(in crate::app) fn field_mut(&mut self, field: ConnectField) -> Option<&mut String> {
        let draft = &mut self.draft;
        match field {
            ConnectField::Name => Some(&mut draft.name),
            ConnectField::Engine
            | ConnectField::Ssl
            | ConnectField::Mode
            | ConnectField::Transaction
            | ConnectField::Agents => None,
            ConnectField::Ssh => Some(&mut draft.ssh),
            ConnectField::Timeout => Some(&mut draft.timeout),
            ConnectField::Host => Some(&mut draft.host),
            ConnectField::Port => Some(&mut draft.port),
            ConnectField::User => Some(&mut draft.user),
            ConnectField::Password => Some(&mut draft.password),
            ConnectField::PasswordCommand => Some(&mut draft.password_command),
            ConnectField::Database => Some(&mut draft.database),
            ConnectField::TlsCa => Some(&mut draft.tls_ca),
            ConnectField::TlsCert => Some(&mut draft.tls_cert),
            ConnectField::TlsKey => Some(&mut draft.tls_key),
        }
    }

    /// The focused field's text before the cursor, as it's shown (the
    /// password as as many dots): where to draw the cursor. `None` for a
    /// field of options, or the password while a command supplies it.
    pub fn before_cursor(&self) -> Option<String> {
        let d = &self.draft;
        let text = match self.focus {
            ConnectField::Name => &d.name,
            ConnectField::Ssh => &d.ssh,
            ConnectField::Timeout => &d.timeout,
            ConnectField::Host => &d.host,
            ConnectField::Port => &d.port,
            ConnectField::User => &d.user,
            ConnectField::Password if !d.password_command.trim().is_empty() => return None,
            ConnectField::Password => &d.password,
            ConnectField::PasswordCommand => &d.password_command,
            ConnectField::Database => &d.database,
            ConnectField::TlsCa => &d.tls_ca,
            ConnectField::TlsCert => &d.tls_cert,
            ConnectField::TlsKey => &d.tls_key,
            _ => return None,
        };
        let at = self.cursor.filter(|&c| text.is_char_boundary(c)).unwrap_or(text.len());
        Some(if self.focus == ConnectField::Password {
            "•".repeat(text[..at].chars().count())
        } else {
            text[..at].to_string()
        })
    }

    /// Whether it's saved connection `name` as saved (not another database
    /// of it).
    pub fn is_saved(&self, name: &str) -> bool {
        self.name == name && !self.other_database
    }

    /// Its name as the tabs and the top bar say it: another database's
    /// with that database (`prod/archive`).
    pub fn label(&self) -> String {
        if self.other_database { format!("{}/{}", self.short_name(), self.database) } else { self.short_name() }
    }

    /// A failed connect's message, plus why the saved password is missing
    /// if the keychain couldn't be read: the likelier cause then.
    pub fn connect_failure(&self, err: &str) -> String {
        let supplied = !self.password.is_empty() || !self.password_command.trim().is_empty();
        match &self.keychain_note {
            Some(note) if !supplied => t!("connections.no_saved_password", err = err, note = note),
            _ => err.to_string(),
        }
    }

    /// The fields shown for the selected engine, in order: TLS's files
    /// only where TLS is required (or one is set, or has the focus: one
    /// just emptied stays till the focus moves on).
    pub fn visible_fields(&self) -> Vec<ConnectField> {
        let tls = shows_tls_files(self.ssl_mode, &self.tls()) || self.focus.is_tls_file();
        fields_for(self.engine)
            .iter()
            .copied()
            .filter(|f| tls || !matches!(f, ConnectField::TlsCa | ConnectField::TlsCert | ConnectField::TlsKey))
            .collect()
    }

    /// A key in the focused field, at its cursor.
    pub(in crate::app) fn edit_field(&mut self, key: KeyEvent) {
        let mut cursor = self.cursor;
        if let Some(field) = self.field_mut(self.focus) {
            input::edit_at(field, &mut cursor, key);
        }
        self.cursor = cursor;
    }

    /// Text pasted into the focused field, at its cursor.
    pub(in crate::app) fn paste_into_field(&mut self, text: &str) {
        let mut cursor = self.cursor;
        if let Some(field) = self.field_mut(self.focus) {
            input::paste_at(field, &mut cursor, text);
        }
        self.cursor = cursor;
    }

    /// Whether the form differs from the saved connection it was opened
    /// from (a new one: whether anything's typed).
    pub fn unsaved(&self, saved: Option<&ConnectionProfile>) -> bool {
        match saved {
            // As it would be saved; what isn't the form's (unknown keys,
            // where the password is kept) as it was.
            // Both as the form makes them (a transaction saved as its mode's
            // default reads back as none), and the password as opened.
            Some(saved) => {
                let as_saved = ConnectionDraft::from_profile(saved, String::new()).to_profile();
                self.to_profile() != as_saved || self.opened_password.as_ref().is_some_and(|p| *p != self.password)
            }
            None => !self.host.is_empty() || !self.name.is_empty(),
        }
    }

    pub(in crate::app) fn next_field(&mut self) {
        let fields = self.visible_fields();
        let idx = fields.iter().position(|f| *f == self.focus).unwrap_or(0);
        self.focus = fields[(idx + 1) % fields.len()];
        self.cursor = None;
    }

    pub(in crate::app) fn prev_field(&mut self) {
        let fields = self.visible_fields();
        let idx = fields.iter().position(|f| *f == self.focus).unwrap_or(0);
        self.focus = fields[(idx + fields.len() - 1) % fields.len()];
        self.cursor = None;
    }
}

impl App {
    pub(in crate::app) fn handle_connections_key(&mut self, key: KeyEvent) {
        // A single key bound to `projects` (none by default: F11 is macOS's
        // Show Desktop) works here too; ← does the same.
        if self.keymap.action(key) == Some(crate::config::keys::Action::Projects) {
            return self.open_manager(Screen::Projects);
        }
        if self.manager.moving_to.is_some() {
            return self.handle_move_key(key);
        }
        if self.manager.containers.is_some() {
            return self.handle_containers_key(key);
        }
        if self.manager.loading_profile {
            if key.code == KeyCode::Esc {
                self.invalidate_tasks();
                self.manager.loading_profile = false;
                self.connection.slots.auto_connect = false;
            }
            return;
        }
        if self.manager.confirm_delete {
            self.manager.confirm_delete = false;
            if crate::app::confirm::is_yes(key) {
                self.delete_selected();
            } else {
                self.manager.connection_message = None;
            }
            return;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            return;
        }
        let len = self.profiles().len();
        let on_profile = self.manager.connection_selected < len;
        // The row after the connections is "new connection".
        if let Some(i) = list::step(self.manager.connection_selected, len + 1, key, Nav::LIST) {
            self.manager.connection_selected = i;
            return;
        }
        let Some(command) = keys::lookup(keys::CONNECTIONS_KEYS, key) else { return };
        match command {
            ConnectionsKey::Back => self.leave_connections(),
            ConnectionsKey::Projects => {
                self.manager.connection_message = None;
                self.screen = Screen::Projects;
            }
            ConnectionsKey::Move if on_profile => {
                if self.move_targets().is_empty() {
                    self.manager.connection_message = Some((t!("connections.need_other_project").to_string(), true));
                } else {
                    self.manager.connection_message = None;
                    self.manager.moving_to = Some(0);
                }
            }
            ConnectionsKey::Delete if on_profile => self.manager.confirm_delete = true,
            ConnectionsKey::Containers => self.find_containers(),
            ConnectionsKey::Edit => self.edit_selected_connection(),
            ConnectionsKey::Connect if on_profile => {
                let profile = self.profiles()[self.manager.connection_selected].clone();
                self.open_saved_here(profile);
            }
            ConnectionsKey::NewTab if on_profile => {
                self.free_tab_for_form();
                self.connection.slots.auto_connect = true;
                self.load_profile(self.profiles()[self.manager.connection_selected].clone());
            }
            // Enter on "+ New connection" makes one.
            ConnectionsKey::New | ConnectionsKey::Connect => self.new_connection(),
            ConnectionsKey::Move | ConnectionsKey::Delete | ConnectionsKey::NewTab => {}
        }
    }

    /// Enter on a saved connection: this tab's from now on. Already open
    /// (another tab's): this tab joins it; otherwise it's opened. The tab's
    /// connection before stays open for its other tabs. Not while this tab
    /// has a transaction open or a query running: `t` opens it in a new tab.
    pub(in crate::app) fn open_saved_here(&mut self, profile: ConnectionProfile) {
        let connected = self.connection.slots.active.is_some();
        if connected && (self.tab.run.executing || self.connection.txn != TxnState::Idle) {
            if self.screen == Screen::Query {
                return self.set_status(t!("session.busy_here").to_string(), true);
            }
            self.manager.connection_message = Some((t!("session.busy_here").to_string(), true));
            return;
        }
        // Joined only if it's open to what the saved one reaches now (not to
        // a host or database it named before an edit).
        let wanted = ConnectForm::from_profile(&profile, String::new());
        let open = self
            .connection
            .all()
            .find(|(_, slot)| {
                slot.form.is_saved(&profile.name) && slot.database.is_some() && slot.form.same_target(&wanted)
            })
            .map(|(id, slot)| (id, slot.txn));
        // Another tab's transaction is open on it: joining would run this
        // tab's SQL in it.
        if let Some((_, txn)) = open
            && txn != TxnState::Idle
        {
            let said = t!("session.join_in_transaction", name = profile.name);
            if self.screen == Screen::Query {
                return self.set_status(said, true);
            }
            self.manager.connection_message = Some((said, true));
            return;
        }
        if let Some((id, _)) = open {
            // Picked over one still on its way: that one doesn't land.
            self.latest_swap = None;
            self.tabs[self.active_tab].conn = Some(id);
            self.switch_conn(Some(id));
            self.release_unused_conns();
            self.manager.connection_message = None;
            self.screen = Screen::Query;
            self.focus = Focus::Editor;
            self.save_draft();
            return;
        }
        // A connected tab keeps its connection while the new one opens, in
        // the background: once it's open the tab swaps to it (the old one
        // released, unless other tabs use it); if it doesn't open, the tab
        // is as it was, and says why.
        if connected {
            return self.swap_to(profile);
        }
        self.connection.slots.auto_connect = true;
        self.load_profile(profile);
    }

    /// Opens `profile` for the shown tab, which keeps its connection until
    /// the new one is open (`DbEventKind::DatabaseOpened` for this tab).
    fn swap_to(&mut self, profile: ConnectionProfile) {
        let Some(project_id) = self.current_project.clone() else { return };
        if !self.may_reach_outside() {
            return;
        }
        let name = profile.name.clone();
        // A file that isn't there is made only when asked (the form asks):
        // not here, behind the tab's back.
        let missing = ConnectForm::from_profile(&profile, String::new())
            .params()
            .ok()
            .and_then(|p| p.file_to_create().map(str::to_string));
        if let Some(path) = missing {
            let said = t!("session.swap_no_file", name = name, path = path);
            self.manager.connection_message = Some((said, true));
            return;
        }
        self.manager.connection_message = None;
        self.screen = Screen::Query;
        self.focus = Focus::Editor;
        self.set_status(t!("palette.opening_database", name = name), false);
        crate::system::connection_log::record("connecting", &name, profile.engine.label());
        let send = self.sender();
        let swap = self.new_swap();
        tokio::spawn(async move {
            let failed = |error: String| DbEventKind::DatabaseFailed { name: name.clone(), error, swap };
            let keychain = {
                let profile = profile.clone();
                tokio::task::spawn_blocking(move || store::saved_password(&project_id, &profile)).await
            };
            let Ok((password, note)) = keychain else { return send(failed("the keychain didn't answer".into())) };
            let mut form = ConnectForm::from_profile(&profile, password);
            // Why the password is missing, said with a failure it explains.
            form.keychain_note = note;
            let params = match form.params() {
                Ok(params) => params,
                Err(e) => return send(failed(e)),
            };
            match Database::connect(params).await {
                Ok(db) => send(DbEventKind::DatabaseOpened(db, Box::new(form), swap)),
                Err(e) => {
                    crate::system::connection_log::record("connect failed", &name, &crate::db::error_details(&e));
                    let message = crate::db::error_message(&e);
                    let mut said = form.connect_failure(&message);
                    // A password to type: the form, in a new tab (this one
                    // keeps its connection).
                    if crate::db::is_auth_failure(&message) && form.password_command.trim().is_empty() {
                        said = format!("{said} · {}", t!("session.swap_password"));
                    }
                    send(failed(said));
                }
            }
        });
    }

    /// Lists the running database containers, off the UI thread (it runs
    /// `docker`), to pick one.
    fn find_containers(&mut self) {
        self.manager.connection_message = None;
        let request = self.manager.containers_request.next();
        self.manager.containers =
            Some(Box::new(ContainerPicker { request, loading: true, ..ContainerPicker::default() }));
        let send = self.sender();
        // No runtime in unit tests: they hand over what was found.
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            drop(runtime.spawn_blocking(move || {
                send(Background::ContainersFound(request, crate::system::containers::find()).into());
            }));
        }
    }

    pub(in crate::app) fn containers_found(
        &mut self,
        request: ContainerRequestId,
        result: Result<Vec<crate::system::containers::Container>, String>,
    ) {
        let Some(picker) = self.manager.containers.as_mut().filter(|p| p.request == request) else { return };
        picker.loading = false;
        match result {
            Ok(found) => {
                picker.found = found;
                picker.error = None;
            }
            Err(e) => picker.error = Some(e),
        }
        picker.selected = picker.selected.min(picker.found.len().saturating_sub(1));
    }

    fn handle_containers_key(&mut self, key: KeyEvent) {
        let Some(picker) = &mut self.manager.containers else { return };
        if let Some(i) = list::step(picker.selected, picker.found.len(), key, Nav::LIST) {
            picker.selected = i;
            picker.note = None;
            return;
        }
        match key.code {
            KeyCode::Esc | KeyCode::Char('c' | 'q') => self.manager.containers = None,
            KeyCode::Enter => {
                let Some(container) = picker.found.get(picker.selected).cloned() else { return };
                let Some(profile) = container.profile else {
                    picker.note = Some(t!("connections.port_not_published", name = container.name));
                    return;
                };
                self.manager.containers = None;
                self.fill_from_container(&container.name, profile);
            }
            _ => {}
        }
    }

    /// The settings pane, filled in from a container: a connection already
    /// saved for it to change (opened as `e` would, password and all), or a
    /// new one under a name not yet taken. Enter then connects (and saves it).
    fn fill_from_container(&mut self, container: &str, mut profile: ConnectionProfile) {
        let Some(project_id) = self.current_project.clone() else {
            self.manager.connection_message = Some((t!("connections.need_project").to_string(), true));
            return;
        };
        if let Some(saved) = self.saved_like(&profile).cloned() {
            if let Some(i) = self.profiles().iter().position(|p| p.name == saved.name) {
                self.manager.connection_selected = i;
            }
            let said = t!("connections.container_saved", container = container, name = saved.name);
            self.load_profile(saved);
            self.manager.connection_message = Some((said, false));
            return;
        }
        profile.name = self.free_name(&project_id, &profile.name);
        self.new_connection();
        self.connection.form = ConnectionDraft::from_profile(&profile, String::new()).into();
        self.connection.form.info =
            Some(t!("connections.from_container", container = container, save = ctrl_words(SAVE_KEY)));
    }

    /// A saved connection of the open project to the same place: server,
    /// user, database and SSH tunnel (so a local container never matches
    /// one that tunnels elsewhere).
    fn saved_like(&self, profile: &ConnectionProfile) -> Option<&ConnectionProfile> {
        self.profiles().iter().find(|p| {
            p.engine == profile.engine
                && p.host.trim() == profile.host.trim()
                && p.port.trim() == profile.port.trim()
                && p.user.trim() == profile.user.trim()
                && p.database.trim() == profile.database.trim()
                && p.ssh.trim() == profile.ssh.trim()
        })
    }

    /// `base`, or `base (2)`, `base (3)`… whichever the project hasn't got.
    fn free_name(&self, project_id: &ProjectId, base: &str) -> String {
        let mut name = base.to_string();
        let mut n = 2;
        while self.store.name_taken(project_id, None, &name) {
            name = format!("{base} ({n})");
            n += 1;
        }
        name
    }

    /// An empty form in the settings pane, for a new connection.
    fn new_connection(&mut self) {
        self.stash_active_form();
        self.manager.connection_message = None;
        self.manager.connection_selected = self.profiles().len();
        self.connection.form = ConnectForm::new();
        self.screen = Screen::Connect;
    }

    /// The selected connection's settings, to change (its password comes
    /// from the keychain first), or a new one's.
    pub(in crate::app) fn edit_selected_connection(&mut self) {
        match self.profiles().get(self.manager.connection_selected).cloned() {
            Some(profile) => self.load_profile(profile),
            None => self.new_connection(),
        }
    }

    /// Reads the profile's password off the UI thread: the OS keychain may
    /// block on an unlock prompt.
    pub(in crate::app) fn load_profile(&mut self, profile: ConnectionProfile) {
        let Some(project_id) = self.current_project.clone() else { return };
        // Opened another way: one still on its way for the tab doesn't land.
        self.latest_swap = None;
        self.manager.loading_profile = true;
        self.manager.connection_message = None;
        let send = self.sender();
        tokio::task::spawn_blocking(move || {
            let (password, note) = store::saved_password(&project_id, &profile);
            send(DbEventKind::PasswordLoaded { profile, password, note });
        });
    }

    pub(in crate::app) fn handle_move_key(&mut self, key: KeyEvent) {
        let Some(idx) = self.manager.moving_to else { return };
        if let Some(i) = list::step(idx, self.move_targets().len(), key, Nav::LIST) {
            self.manager.moving_to = Some(i);
            return;
        }
        match key.code {
            KeyCode::Esc => self.manager.moving_to = None,
            KeyCode::Enter => {
                self.manager.moving_to = None;
                if let Some(target) = self.move_targets().get(idx).map(|p| p.id.clone()) {
                    self.move_selected(&target);
                }
            }
            _ => {}
        }
    }

    /// Moves the selected connection, and its keychain entry, to another project.
    pub(in crate::app) fn move_selected(&mut self, target: &ProjectId) {
        let (Some(from), Some(profile)) =
            (self.current_project.clone(), self.profiles().get(self.manager.connection_selected).cloned())
        else {
            return;
        };
        let target_name = self.store.project(target).map(|p| p.name.clone()).unwrap_or_default();
        if self.store.name_taken(target, None, &profile.name) {
            self.manager.connection_message =
                Some((t!("connections.target_has_name", target = target_name, name = profile.name), true));
            return;
        }
        let old_account = store::keychain_account(&from, &profile);
        let new_account = store::project_account(target, &profile.name);
        // Nothing to move for a password command. A keychain that can't be
        // read stops the move: the password would be lost with the old entry.
        let (password, note) = store::saved_password(&from, &profile);
        if let Some(note) = note {
            self.manager.connection_message = Some((t!("connections.move_password_failed", note = note), true));
            return;
        }
        if !password.is_empty()
            && let Err(e) = crate::db::keychain::store_password(&new_account, &password)
        {
            self.manager.connection_message = Some((format!("Couldn't move password: {e:#}"), true));
            return;
        }
        match self.store.move_connection(&from, &profile.name, target) {
            Ok(()) => {
                let _ = crate::db::keychain::delete_password(&old_account);
                self.manager.connection_message =
                    Some((t!("connections.moved", name = profile.name, target = target_name), false));
                let len = self.profiles().len();
                if self.manager.connection_selected > 0 && self.manager.connection_selected >= len {
                    self.manager.connection_selected -= 1;
                }
            }
            Err(e) => {
                let _ = crate::db::keychain::delete_password(&new_account);
                self.manager.connection_message = Some((format!("Couldn't move: {e:#}"), true));
            }
        }
    }

    pub(in crate::app) fn delete_selected(&mut self) {
        let Some(project_id) = self.current_project.clone() else { return };
        let Some(profile) = self.profiles().get(self.manager.connection_selected).cloned() else { return };
        let name = profile.name.clone();
        match self.store.delete(&project_id, &name) {
            Ok(()) => {
                let _ = crate::db::keychain::delete_password(&store::keychain_account(&project_id, &profile));
                self.manager.connection_message = Some((t!("connections.deleted", name = name), false));
                if self.manager.connection_selected > 0 && self.manager.connection_selected >= self.profiles().len() {
                    self.manager.connection_selected -= 1;
                }
            }
            Err(e) => self.manager.connection_message = Some((format!("Couldn't delete: {e:#}"), true)),
        }
    }

    pub(in crate::app) fn handle_connect_key(&mut self, key: KeyEvent) {
        if self.connection.form.connecting {
            if key.code == KeyCode::Esc {
                self.invalidate_tasks();
                self.connection.form.connecting = false;
                self.connection.form.info = Some(t!("connections.cancelled").to_string());
            }
            return;
        }

        if self.popups.cell_view.is_some() {
            return self.handle_cell_view_key(key);
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char(c) if c == SAVE_KEY => self.save_connection(),
                KeyCode::Char(c) if c == TEST_KEY => self.test_connection(),
                KeyCode::Char(c) if c == COPY_KEY => self.copy_connect_field(),
                // Where the terminal doesn't paste by itself.
                KeyCode::Char('v') => match crate::system::clipboard::read() {
                    Ok(text) => self.handle_paste(&text),
                    Err(e) => {
                        self.connection.form.error = Some(crate::system::clipboard::read_failed(&e));
                    }
                },
                // Clearing and deleting a word, as in every input.
                KeyCode::Char('u' | 'w') => self.connection.form.edit_field(key),
                _ => {}
            }
            return;
        }

        let form = &mut self.connection.form;
        match key.code {
            // Esc and Shift+Tab back to the connections, Tab round to the
            // projects; changes not saved are dropped.
            KeyCode::Esc | KeyCode::BackTab => {
                self.restore_active_form();
                self.screen = Screen::Connections;
            }
            KeyCode::Tab => self.open_manager(Screen::Projects),
            KeyCode::Down => form.next_field(),
            KeyCode::Up => form.prev_field(),
            KeyCode::Left if form.focus == ConnectField::Engine => form.cycle_engine(false),
            KeyCode::Right if form.focus == ConnectField::Engine => form.cycle_engine(true),
            KeyCode::Left if form.focus == ConnectField::Mode => form.cycle_mode(false),
            KeyCode::Right if form.focus == ConnectField::Mode => form.cycle_mode(true),
            KeyCode::Left if form.focus == ConnectField::Agents => form.agents = form.agents.cycle(false),
            KeyCode::Right if form.focus == ConnectField::Agents => form.agents = form.agents.cycle(true),
            KeyCode::Left if form.focus == ConnectField::Transaction => form.cycle_transaction(false),
            KeyCode::Right if form.focus == ConnectField::Transaction => form.cycle_transaction(true),
            KeyCode::Left if form.focus == ConnectField::Ssl => form.ssl_mode = form.ssl_mode.cycle(false),
            KeyCode::Right if form.focus == ConnectField::Ssl => form.ssl_mode = form.ssl_mode.cycle(true),
            KeyCode::Enter => self.start_connect(),
            _ => form.edit_field(key),
        }
    }

    /// Ctrl+Y in the form: copies the focused field's value (not the
    /// password).
    pub(in crate::app) fn copy_connect_field(&mut self) {
        let form = &mut self.connection.form;
        let focus = form.focus;
        form.error = None;
        if focus == ConnectField::Password {
            form.info = Some(t!("connections.password_not_copied").to_string());
            return;
        }
        let value = match focus {
            ConnectField::Engine => form.engine.label().to_string(),
            ConnectField::Ssl => form.ssl_mode.label().to_string(),
            ConnectField::Mode => form.mode.label().to_string(),
            ConnectField::Transaction => form.tx_mode().label().to_string(),
            ConnectField::Agents => form.agents.label().to_string(),
            _ => form.field_mut(focus).map(|f| f.clone()).unwrap_or_default(),
        };
        if value.is_empty() {
            form.info = Some(t!("connections.nothing_to_copy").to_string());
            return;
        }
        form.info = Some(t!("connections.copied", value = value));
        self.for_terminal.clipboard = Some(value);
    }

    pub(in crate::app) fn save_connection(&mut self) {
        let Some(project_id) = self.current_project.clone() else { return };
        let form = &mut self.connection.form;
        form.info = None;
        form.error = None;
        let profile = form.to_profile();
        let name = profile.name.clone();
        if name.is_empty() {
            form.error = Some(t!("connections.need_name").to_string());
            return;
        }
        if let Err(e) = form.password_command_words() {
            form.error = Some(e);
            return;
        }
        let original = form.original_name.clone();
        if self.store.name_taken(&project_id, original.as_deref(), &name) {
            form.error = Some(t!("connections.name_taken", name = name));
            return;
        }
        // An empty password removes the entry rather than storing "", and
        // so does a password command (it supplies the password instead).
        // Removing one where there's no keychain (a server) is no failure.
        // SQLite has no password: nothing to keep in the keychain.
        let account = store::project_account(&project_id, &name);
        let keep = if profile.password_command.is_empty() { form.password.as_str() } else { "" };
        if !form.engine.is_file_based()
            && let Err(e) = crate::db::keychain::store_password(&account, keep)
            && !keep.is_empty()
        {
            let pgpass = form.engine.backend().password_files_note();
            let instead = match crate::db::password_command::installed(crate::system::programs::on_path).first() {
                Some(tool) => {
                    t!("connections.tool_installed", tool = tool.label, example = tool.example(&name), pgpass = pgpass)
                }
                None => t!("connections.use_password_command", pgpass = pgpass),
            };
            form.error = Some(t!("connections.no_keychain", e = format!("{e:#}"), instead = instead));
            return;
        }
        if let Err(e) = self.store.upsert(&project_id, original.as_deref(), profile) {
            form.error = Some(format!("Failed to save: {e:#}"));
            return;
        }
        // Drop the password stored under the old name (or pre-projects key).
        if let Some(old) = &original {
            let old_account = if form.original_legacy { old.clone() } else { store::project_account(&project_id, old) };
            if old_account != account {
                let _ = crate::db::keychain::delete_password(&old_account);
            }
        }
        form.original_legacy = false;
        let renamed_from = original.filter(|o| *o != name);
        form.info = Some(match renamed_from {
            Some(old) => t!("connections.renamed", old = old, name = name),
            None => t!("connections.saved", name = name),
        });
        form.original_name = Some(name);
    }

    /// A new connection made in a project is kept once it works (so it's
    /// there next time, and the session can reopen it); unnamed, it's
    /// named after where it goes. Returns what to say about it.
    pub(in crate::app) fn keep_new_connection(&mut self) -> Option<(String, bool)> {
        let project_id = self.current_project.clone()?;
        if self.connection.form.original_name.is_some() {
            return None;
        }
        if self.connection.form.name.trim().is_empty() {
            // Already saved (same server, user, database and tunnel): that
            // one is used rather than a copy.
            let form = &self.connection.form;
            if let Some(saved) = self.saved_like(&form.to_profile()).map(|p| (p.name.clone(), p.legacy_keychain)) {
                let (name, legacy) = saved;
                self.connection.form.name.clone_from(&name);
                self.connection.form.original_name = Some(name.clone());
                self.connection.form.original_legacy = legacy;
                return Some((t!("connections.already_saved", name = name), false));
            }
            let base = form.short_name();
            self.connection.form.name = self.free_name(&project_id, &base);
        }
        self.save_connection();
        let form = &mut self.connection.form;
        let said = match form.error.take() {
            Some(e) => (t!("connections.not_saved", e = e, save = ctrl_words(SAVE_KEY)), true),
            None => (t!("connections.saved_in_project", name = form.name), false),
        };
        form.info = None;
        Some(said)
    }

    /// Ctrl+T: connects step by step and shows where it breaks.
    pub(in crate::app) fn test_connection(&mut self) {
        if !self.may_reach_outside() {
            return;
        }
        let params = match self.connection.form.params() {
            Ok(p) => p,
            Err(e) => {
                self.connection.form.error = Some(e);
                return;
            }
        };
        self.connection.form.error = None;
        self.connection.form.info = Some(t!("connections.testing").to_string());
        let title = format!("{} · {}", t!("connections.test_title"), self.connection.form.summary());
        let send = self.sender();
        tokio::spawn(async move {
            let steps = crate::db::diagnose(params).await;
            send(DbEventKind::Diagnosed { title, report: crate::db::report(&steps) });
        });
    }

    pub(in crate::app) fn start_connect(&mut self) {
        let params = match self.connection.form.params() {
            Ok(p) => p,
            Err(e) => {
                self.connection.form.error = Some(e);
                return;
            }
        };
        // A missing SQLite file is usually a typo: say so, and create it only
        // when asked again for the same path.
        // The approval is used up here, so a later attempt asks again.
        let approved = self.connection.form.create_file.take();
        if let Some(path) = params.file_to_create()
            && approved.as_deref() != Some(path)
        {
            let form = &mut self.connection.form;
            form.create_file = Some(path.to_string());
            form.error = None;
            form.info = Some(t!("connections.create_file", path = path));
            return;
        }
        if !self.may_reach_outside() {
            return;
        }
        // Edited over a connected tab: that tab keeps its connection, and
        // this one opens in a new tab.
        if self.manager.form_stash.is_some() {
            let form = std::mem::take(&mut self.connection.form);
            self.restore_active_form();
            self.free_tab_for_form();
            self.connection.form = form;
        }
        self.connection.form.connecting = true;
        self.connect_started = Some(Instant::now());
        self.connection.form.error = None;
        self.connection.form.info = None;
        let send = self.sender();
        let name = self.connection.form.short_name();
        crate::system::connection_log::record("connecting", &name, &self.connection.form.summary());
        self.tab.run.task = Some(tokio::spawn(async move {
            match Database::connect(params).await {
                Ok(db) => send(DbEventKind::Connected(db)),
                Err(e) => {
                    crate::system::connection_log::record("connect failed", &name, &crate::db::error_details(&e));
                    send(DbEventKind::ConnectFailed(crate::db::error_message(&e)));
                }
            }
        }));
    }
}

impl Default for ConnectForm {
    fn default() -> Self {
        Self::new()
    }
}
