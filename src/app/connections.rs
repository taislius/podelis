use super::*;
use crate::db::AgentAccess;

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
    Mode,
    Transaction,
    Timeout,
    Agents,
    Ssh,
}

const SERVER_FIELDS: [ConnectField; 14] = [
    ConnectField::Name,
    ConnectField::Engine,
    ConnectField::Host,
    ConnectField::Port,
    ConnectField::User,
    ConnectField::Password,
    ConnectField::PasswordCommand,
    ConnectField::Database,
    ConnectField::Ssl,
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

/// The fields a connection to `engine` has, in order.
pub fn fields_for(engine: Engine) -> &'static [ConnectField] {
    if engine.is_file_based() { &FILE_FIELDS } else { &SERVER_FIELDS }
}

pub struct ConnectForm {
    /// Name of the saved profile this form was opened from, so saving under
    /// a new name renames it instead of creating a duplicate.
    pub original_name: Option<String>,
    /// The original profile's password is under the pre-projects keychain key.
    pub(super) original_legacy: bool,
    pub name: String,
    pub engine: Engine,
    pub host: String,
    pub port: String,
    pub user: String,
    pub password: String,
    /// The password command as typed (see [`crate::db::password_command`]);
    /// when set, it supplies the password instead of the Password field.
    pub password_command: String,
    pub database: String,
    pub ssl_mode: SslMode,
    pub mode: ConnectionMode,
    /// The transaction mode chosen for the connection; None follows the
    /// mode's default (see [`ConnectForm::tx_mode`]).
    pub transaction: Option<TxMode>,
    /// SSH destination to tunnel through; empty for a direct connection.
    pub ssh: String,
    /// Query timeout in seconds, as typed; empty for none.
    pub timeout: String,
    /// What AI agents may do on it (`podelis mcp`).
    pub agents: AgentAccess,
    pub focus: ConnectField,
    pub error: Option<String>,
    pub info: Option<String>,
    pub connecting: bool,
    /// A SQLite file the user agreed to create (by connecting again).
    pub create_file: Option<String>,
    /// Why the saved password couldn't be read from the keychain; said
    /// again if connecting without it fails.
    pub keychain_note: Option<String>,
}

impl ConnectForm {
    pub(crate) fn new() -> Self {
        Self {
            original_name: None,
            original_legacy: false,
            name: String::new(),
            // The first engine (by its `order`): PostgreSQL.
            engine: Engine::ALL[0],
            host: "localhost".to_string(),
            port: Engine::ALL[0].default_port().to_string(),
            user: String::new(),
            password: String::new(),
            password_command: String::new(),
            database: String::new(),
            ssl_mode: SslMode::default(),
            mode: ConnectionMode::default(),
            transaction: None,
            ssh: String::new(),
            timeout: String::new(),
            agents: AgentAccess::None,
            focus: ConnectField::Name,
            error: None,
            info: None,
            connecting: false,
            create_file: None,
            keychain_note: None,
        }
    }

    pub(crate) fn from_profile(profile: &ConnectionProfile, password: String) -> Self {
        Self {
            original_name: Some(profile.name.clone()),
            original_legacy: profile.legacy_keychain,
            name: profile.name.clone(),
            engine: profile.engine,
            host: profile.host.clone(),
            port: profile.port.clone(),
            user: profile.user.clone(),
            password,
            password_command: crate::db::password_command::join(&profile.password_command),
            database: profile.database.clone(),
            ssl_mode: profile.ssl_mode,
            mode: profile.mode,
            transaction: profile.transaction,
            ssh: profile.ssh.clone(),
            timeout: profile.timeout.clone(),
            agents: profile.agents,
            ..Self::new()
        }
    }

    #[cfg(test)]
    pub fn to_profile_for_tests(&self) -> ConnectionProfile {
        self.to_profile()
    }

    pub(super) fn to_profile(&self) -> ConnectionProfile {
        ConnectionProfile {
            name: self.name.trim().to_string(),
            engine: self.engine,
            host: self.host.clone(),
            port: self.port.clone(),
            user: self.user.clone(),
            database: self.database.clone(),
            ssl_mode: self.ssl_mode,
            mode: self.mode,
            // Only a choice: the mode's default follows the mode.
            transaction: self.transaction.filter(|t| *t != TxMode::for_connection(self.mode)),
            ssh: self.ssh.trim().to_string(),
            timeout: self.timeout.trim().to_string(),
            agents: self.agents,
            password_command: self.password_command_words().unwrap_or_default(),
            legacy_keychain: false,
        }
    }

    pub(super) fn field_mut(&mut self, field: ConnectField) -> Option<&mut String> {
        match field {
            ConnectField::Name => Some(&mut self.name),
            ConnectField::Engine
            | ConnectField::Ssl
            | ConnectField::Mode
            | ConnectField::Transaction
            | ConnectField::Agents => None,
            ConnectField::Ssh => Some(&mut self.ssh),
            ConnectField::Timeout => Some(&mut self.timeout),
            ConnectField::Host => Some(&mut self.host),
            ConnectField::Port => Some(&mut self.port),
            ConnectField::User => Some(&mut self.user),
            ConnectField::Password => Some(&mut self.password),
            ConnectField::PasswordCommand => Some(&mut self.password_command),
            ConnectField::Database => Some(&mut self.database),
        }
    }

    /// A failed connect's message, plus why the saved password is missing
    /// if the keychain couldn't be read: the likelier cause then.
    pub fn connect_failure(&self, err: &str) -> String {
        let supplied = !self.password.is_empty() || !self.password_command.trim().is_empty();
        match &self.keychain_note {
            Some(note) if !supplied => format!("{err} (no saved password: {note})"),
            _ => err.to_string(),
        }
    }

    /// The password command's program and arguments (none for a file).
    pub fn password_command_words(&self) -> Result<Vec<String>, String> {
        if self.engine.is_file_based() {
            return Ok(Vec::new());
        }
        let words = crate::db::password_command::split(&self.password_command)?;
        if words.first().is_some_and(String::is_empty) {
            return Err("The password command's program is empty".to_string());
        }
        Ok(words)
    }

    /// The fields shown for the selected engine, in order.
    pub fn visible_fields(&self) -> &'static [ConnectField] {
        fields_for(self.engine)
    }

    pub(super) fn next_field(&mut self) {
        let fields = self.visible_fields();
        let idx = fields.iter().position(|f| *f == self.focus).unwrap_or(0);
        self.focus = fields[(idx + 1) % fields.len()];
    }

    pub(super) fn prev_field(&mut self) {
        let fields = self.visible_fields();
        let idx = fields.iter().position(|f| *f == self.focus).unwrap_or(0);
        self.focus = fields[(idx + fields.len() - 1) % fields.len()];
    }

    /// The transaction mode the connection starts in: the one chosen, or
    /// the mode's default (manual commit for production).
    pub fn tx_mode(&self) -> TxMode {
        self.transaction.unwrap_or_else(|| TxMode::for_connection(self.mode))
    }

    pub(super) fn cycle_mode(&mut self, forward: bool) {
        self.mode = self.mode.cycle(forward);
    }

    pub(super) fn cycle_transaction(&mut self, forward: bool) {
        let current = self.tx_mode();
        self.transaction = Some(if forward { current.next() } else { current.next().next() });
    }

    pub(super) fn cycle_engine(&mut self, forward: bool) {
        let old = self.engine;
        self.engine = self.engine.cycle(forward);
        if self.port == old.default_port() || self.port.is_empty() {
            self.port = self.engine.default_port().to_string();
        }
        // A server host makes no sense as a file name and vice versa.
        if self.engine.is_file_based() && self.host == "localhost" {
            self.host.clear();
        } else if old.is_file_based() && !self.engine.is_file_based() && self.host.is_empty() {
            self.host = "localhost".to_string();
        }
    }

    /// Fills the form from a connection URL such as
    /// `postgres://user:pass@host:5432/db?sslmode=require`,
    /// `mysql://user@host/db` or `sqlite:path/to/file.db`.
    pub fn apply_url(&mut self, text: &str) -> Result<(), String> {
        let url = url::Url::parse(text).map_err(|e| format!("Not a valid connection URL: {e}"))?;
        let decode = |s: &str| percent_decode(s).unwrap_or_else(|| s.to_string());
        let engine =
            Engine::from_scheme(url.scheme()).ok_or_else(|| format!("Unsupported URL scheme '{}'", url.scheme()))?;
        self.engine = engine;
        if engine.is_file_based() {
            // sqlite:file.db, sqlite://relative.db, sqlite:///abs/path.db
            let rest = text.split_once(':').map_or("", |(_, r)| r);
            let path = rest.strip_prefix("//").unwrap_or(rest);
            let path = path.split('?').next().unwrap_or(path);
            self.host = decode(path);
            self.port.clear();
            return Ok(());
        }
        self.host = url.host_str().unwrap_or("localhost").trim_start_matches('[').trim_end_matches(']').to_string();
        self.port = url.port().map_or_else(|| engine.default_port().to_string(), |p| p.to_string());
        self.user = decode(url.username());
        self.password = url.password().map(decode).unwrap_or_default();
        self.database = decode(url.path().trim_start_matches('/'));
        for (key, value) in url.query_pairs() {
            if matches!(key.as_ref(), "sslmode" | "ssl-mode" | "ssl_mode") {
                self.ssl_mode = match value.to_ascii_lowercase().replace('_', "-").as_str() {
                    "disable" | "disabled" => SslMode::Disable,
                    "require" | "required" => SslMode::Require,
                    "verify-ca" => SslMode::VerifyCa,
                    "verify-full" | "verify-identity" => SslMode::VerifyFull,
                    _ => SslMode::Prefer,
                };
            }
        }
        Ok(())
    }

    /// The query timeout, if one is set.
    pub fn query_timeout(&self) -> Result<Option<Duration>, String> {
        let text = self.timeout.trim().trim_end_matches('s').trim();
        if text.is_empty() {
            return Ok(None);
        }
        match text.parse::<u64>() {
            Ok(0) => Ok(None),
            Ok(secs) => Ok(Some(Duration::from_secs(secs))),
            Err(_) => Err("Timeout must be a number of seconds (or empty for none)".to_string()),
        }
    }

    pub fn params(&self) -> Result<ConnectParams, String> {
        self.query_timeout()?;
        let host = self.host.trim();
        let ssh = Some(self.ssh.trim().to_string()).filter(|s| !s.is_empty());
        if self.engine.is_file_based() {
            if host.is_empty() {
                return Err("Enter the database file path (or :memory:)".to_string());
            }
            return Ok(ConnectParams {
                engine: self.engine,
                host: host.to_string(),
                port: 0,
                user: String::new(),
                password: String::new(),
                database: String::new(),
                ssl_mode: SslMode::Disable,
                read_only: self.mode == ConnectionMode::ReadOnly,
                ssh: None,
                password_command: Vec::new(),
            });
        }
        if host.is_empty() {
            return Err("Host is required".to_string());
        }
        let port = match self.port.trim().parse::<u16>() {
            Ok(p) if p > 0 => p,
            _ => return Err("Port must be a number between 1 and 65535".to_string()),
        };
        // A password command replaces the typed (or saved) password.
        let password_command = self.password_command_words()?;
        let password = if password_command.is_empty() { self.password.clone() } else { String::new() };
        Ok(ConnectParams {
            engine: self.engine,
            host: host.to_string(),
            port,
            user: self.user.clone(),
            password,
            database: self.database.trim().to_string(),
            ssl_mode: self.ssl_mode,
            read_only: self.mode == ConnectionMode::ReadOnly,
            ssh,
            password_command,
        })
    }

    /// Its name for the status bar and messages: the saved name, else
    /// the file's name or `host/database`.
    pub fn short_name(&self) -> String {
        if !self.name.is_empty() {
            return self.name.clone();
        }
        if self.engine.is_file_based() {
            return std::path::Path::new(&self.host)
                .file_name()
                .map_or_else(|| self.host.clone(), |n| n.to_string_lossy().into_owned());
        }
        if self.database.is_empty() { self.host.clone() } else { format!("{}/{}", self.host, self.database) }
    }

    pub fn summary(&self) -> String {
        if self.engine.is_file_based() {
            return format!("{} {}", self.engine.label(), self.host);
        }
        let via = if self.ssh.trim().is_empty() { String::new() } else { format!(" via {}", self.ssh.trim()) };
        format!("{} as {}@{}:{}/{}", self.engine.label(), self.user, self.host, self.port, self.database) + &via
    }
}

fn percent_decode(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = s.get(i + 1..i + 3)?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// Whether pasted text is a connection URL rather than a field value.
pub(super) fn looks_like_url(text: &str) -> bool {
    let t = text.trim();
    ["postgres://", "postgresql://", "mysql://", "mariadb://", "sqlite:", "sqlite3:"]
        .iter()
        .any(|p| t.len() > p.len() && t[..p.len()].eq_ignore_ascii_case(p))
}

impl App {
    pub(super) fn handle_connections_key(&mut self, key: KeyEvent) {
        // A single key bound to `projects` (none by default: F11 is macOS's
        // Show Desktop) works here too; ← does the same.
        if self.keymap.action(key) == Some(crate::keys::Action::Projects) {
            return self.open_manager(Screen::Projects);
        }
        if self.moving_to.is_some() {
            return self.handle_move_key(key);
        }
        if self.containers.is_some() {
            return self.handle_containers_key(key);
        }
        if self.loading_profile {
            if key.code == KeyCode::Esc {
                self.invalidate_tasks();
                self.loading_profile = false;
                self.conns.auto_connect = false;
            }
            return;
        }
        if self.confirm_delete {
            self.confirm_delete = false;
            if matches!(key.code, KeyCode::Char('y' | 'Y')) {
                self.delete_selected();
            } else {
                self.connections_message = None;
            }
            return;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            return;
        }
        let len = self.profiles().len();
        let on_profile = self.connections_selected < len;
        // The row after the connections is "new connection".
        if let Some(i) = list::step(self.connections_selected, len + 1, key, Nav::LIST) {
            self.connections_selected = i;
            return;
        }
        match key.code {
            KeyCode::Esc => self.leave_connections(),
            KeyCode::Char('q') => self.request_quit(),
            KeyCode::Left | KeyCode::Char('h') | KeyCode::BackTab => {
                self.connections_message = None;
                self.screen = Screen::Projects;
            }
            KeyCode::Char('m') if on_profile => {
                if self.move_targets().is_empty() {
                    self.connections_message = Some(("Create another project first (← then n)".to_string(), true));
                } else {
                    self.connections_message = None;
                    self.moving_to = Some(0);
                }
            }
            KeyCode::Char('d') if on_profile => self.confirm_delete = true,
            KeyCode::Char('c') => self.find_containers(),
            // Into the settings pane, to change them.
            KeyCode::Right | KeyCode::Char('l' | 'e') | KeyCode::Tab => self.edit_selected_connection(),
            KeyCode::Enter if on_profile => {
                self.free_tab_for_form();
                self.conns.auto_connect = true;
                self.load_profile(self.profiles()[self.connections_selected].clone());
            }
            KeyCode::Char('n') | KeyCode::Enter => self.new_connection(),
            _ => {}
        }
    }

    /// Lists the running database containers, off the UI thread (it runs
    /// `docker`), to pick one.
    fn find_containers(&mut self) {
        self.connections_message = None;
        self.containers_request += 1;
        let request = self.containers_request;
        self.containers = Some(Box::new(ContainerPicker { request, loading: true, ..ContainerPicker::default() }));
        let send = self.sender();
        // No runtime in unit tests: they hand over what was found.
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            drop(
                runtime.spawn_blocking(move || send(DbEventKind::ContainersFound(request, crate::containers::find()))),
            );
        }
    }

    pub(crate) fn containers_found(&mut self, request: u64, result: Result<Vec<crate::containers::Container>, String>) {
        let Some(picker) = self.containers.as_mut().filter(|p| p.request == request) else { return };
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
        let Some(picker) = &mut self.containers else { return };
        if let Some(i) = list::step(picker.selected, picker.found.len(), key, Nav::LIST) {
            picker.selected = i;
            picker.note = None;
            return;
        }
        match key.code {
            KeyCode::Esc | KeyCode::Char('c' | 'q') => self.containers = None,
            KeyCode::Enter => {
                let Some(container) = picker.found.get(picker.selected).cloned() else { return };
                let Some(profile) = container.profile else {
                    picker.note =
                        Some(format!("'{}' doesn't publish its database port: start it with -p", container.name));
                    return;
                };
                self.containers = None;
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
            self.connections_message = Some(("Open a project first".to_string(), true));
            return;
        };
        if let Some(saved) = self.saved_like(&profile).cloned() {
            if let Some(i) = self.profiles().iter().position(|p| p.name == saved.name) {
                self.connections_selected = i;
            }
            let said = format!("'{container}' is already saved as '{}'", saved.name);
            self.load_profile(saved);
            self.connections_message = Some((said, false));
            return;
        }
        profile.name = self.free_name(&project_id, &profile.name);
        self.new_connection();
        self.connect_form = ConnectForm { original_name: None, ..ConnectForm::from_profile(&profile, String::new()) };
        self.connect_form.info = Some(format!("From the container '{container}': Enter connects, Ctrl+S saves"));
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
    fn free_name(&self, project_id: &str, base: &str) -> String {
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
        self.connections_message = None;
        self.connections_selected = self.profiles().len();
        self.connect_form = ConnectForm::new();
        self.screen = Screen::Connect;
    }

    /// The selected connection's settings, to change (its password comes
    /// from the keychain first), or a new one's.
    pub(super) fn edit_selected_connection(&mut self) {
        match self.profiles().get(self.connections_selected).cloned() {
            Some(profile) => self.load_profile(profile),
            None => self.new_connection(),
        }
    }

    /// Reads the profile's password off the UI thread: the OS keychain may
    /// block on an unlock prompt.
    pub(super) fn load_profile(&mut self, profile: ConnectionProfile) {
        let Some(project_id) = self.current_project.clone() else { return };
        self.loading_profile = true;
        self.connections_message = None;
        let send = self.sender();
        tokio::task::spawn_blocking(move || {
            let (password, note) = store::saved_password(&project_id, &profile);
            send(DbEventKind::PasswordLoaded(profile, password, note));
        });
    }

    pub(super) fn handle_move_key(&mut self, key: KeyEvent) {
        let Some(idx) = self.moving_to else { return };
        if let Some(i) = list::step(idx, self.move_targets().len(), key, Nav::LIST) {
            self.moving_to = Some(i);
            return;
        }
        match key.code {
            KeyCode::Esc => self.moving_to = None,
            KeyCode::Enter => {
                self.moving_to = None;
                if let Some(target) = self.move_targets().get(idx).map(|p| p.id.clone()) {
                    self.move_selected(&target);
                }
            }
            _ => {}
        }
    }

    /// Moves the selected connection, and its keychain entry, to another project.
    pub(super) fn move_selected(&mut self, target: &str) {
        let (Some(from), Some(profile)) =
            (self.current_project.clone(), self.profiles().get(self.connections_selected).cloned())
        else {
            return;
        };
        let target_name = self.store.project(target).map(|p| p.name.clone()).unwrap_or_default();
        if self.store.name_taken(target, None, &profile.name) {
            self.connections_message =
                Some((format!("'{target_name}' already has a connection named '{}'", profile.name), true));
            return;
        }
        let old_account = store::keychain_account(&from, &profile);
        let new_account = store::project_account(target, &profile.name);
        // Nothing to move for a password command. A keychain that can't be
        // read stops the move: the password would be lost with the old entry.
        let (password, note) = store::saved_password(&from, &profile);
        if let Some(note) = note {
            self.connections_message = Some((format!("Couldn't move password: {note}"), true));
            return;
        }
        if !password.is_empty()
            && let Err(e) = store::store_password(&new_account, &password)
        {
            self.connections_message = Some((format!("Couldn't move password: {e:#}"), true));
            return;
        }
        match self.store.move_connection(&from, &profile.name, target) {
            Ok(()) => {
                let _ = store::delete_password(&old_account);
                self.connections_message = Some((format!("Moved '{}' to '{target_name}'", profile.name), false));
                let len = self.profiles().len();
                if self.connections_selected > 0 && self.connections_selected >= len {
                    self.connections_selected -= 1;
                }
            }
            Err(e) => {
                let _ = store::delete_password(&new_account);
                self.connections_message = Some((format!("Couldn't move: {e:#}"), true));
            }
        }
    }

    pub(super) fn delete_selected(&mut self) {
        let Some(project_id) = self.current_project.clone() else { return };
        let Some(profile) = self.profiles().get(self.connections_selected).cloned() else { return };
        let name = profile.name.clone();
        match self.store.delete(&project_id, &name) {
            Ok(()) => {
                let _ = store::delete_password(&store::keychain_account(&project_id, &profile));
                self.connections_message = Some((format!("Deleted '{name}'"), false));
                if self.connections_selected > 0 && self.connections_selected >= self.profiles().len() {
                    self.connections_selected -= 1;
                }
            }
            Err(e) => self.connections_message = Some((format!("Couldn't delete: {e:#}"), true)),
        }
    }

    pub(super) fn handle_connect_key(&mut self, key: KeyEvent) {
        if self.connect_form.connecting {
            if key.code == KeyCode::Esc {
                self.invalidate_tasks();
                self.connect_form.connecting = false;
                self.connect_form.info = Some("Connection attempt cancelled".to_string());
            }
            return;
        }

        if self.cell_view.is_some() {
            return self.handle_cell_view_key(key);
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char('s') => self.save_connection(),
                KeyCode::Char('t') => self.test_connection(),
                KeyCode::Char('y') => self.copy_connect_field(),
                // Where the terminal doesn't paste by itself.
                KeyCode::Char('v') => match crate::clipboard::read() {
                    Ok(text) => self.handle_paste(&text),
                    Err(e) => {
                        self.connect_form.error = Some(crate::clipboard::read_failed(&e));
                    }
                },
                // Clearing and deleting a word, as in every input.
                KeyCode::Char('u' | 'w') => {
                    let form = &mut self.connect_form;
                    if let Some(field) = form.field_mut(form.focus) {
                        input::edit_at_end(field, key);
                    }
                }
                _ => {}
            }
            return;
        }

        let form = &mut self.connect_form;
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
            _ => {
                if let Some(field) = form.field_mut(form.focus) {
                    input::edit_at_end(field, key);
                }
            }
        }
    }

    /// Ctrl+Y in the form: copies the focused field's value (not the
    /// password).
    pub(super) fn copy_connect_field(&mut self) {
        let form = &mut self.connect_form;
        let focus = form.focus;
        form.error = None;
        if focus == ConnectField::Password {
            form.info = Some("The password isn't copied".to_string());
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
            form.info = Some("Nothing to copy: the field is empty".to_string());
            return;
        }
        form.info = Some(format!("Copied: {value}"));
        self.clipboard = Some(value);
    }

    pub(super) fn save_connection(&mut self) {
        let Some(project_id) = self.current_project.clone() else { return };
        let form = &mut self.connect_form;
        form.info = None;
        form.error = None;
        let profile = form.to_profile();
        let name = profile.name.clone();
        if name.is_empty() {
            form.error = Some("Enter a name (top field) before saving".to_string());
            return;
        }
        if let Err(e) = form.password_command_words() {
            form.error = Some(e);
            return;
        }
        let original = form.original_name.clone();
        if self.store.name_taken(&project_id, original.as_deref(), &name) {
            form.error = Some(format!("A connection named '{name}' already exists in this project"));
            return;
        }
        // An empty password removes the entry rather than storing "", and
        // so does a password command (it supplies the password instead).
        // Removing one where there's no keychain (a server) is no failure.
        // SQLite has no password: nothing to keep in the keychain.
        let account = store::project_account(&project_id, &name);
        let keep = if profile.password_command.is_empty() { form.password.as_str() } else { "" };
        if !form.engine.is_file_based()
            && let Err(e) = store::store_password(&account, keep)
            && !keep.is_empty()
        {
            let pgpass = form.engine.dialect().password_files_note();
            let instead = match crate::db::password_command::installed(crate::doctor::on_path).first() {
                Some(tool) => format!(
                    "{} is installed: set Passwd cmd to {} and leave Password empty{pgpass}",
                    tool.label,
                    tool.example(&name)
                ),
                None => format!("leave Password empty and use Passwd cmd{pgpass}"),
            };
            form.error = Some(format!("Failed to save: {e:#}. No keychain here: {instead}"));
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
                let _ = store::delete_password(&old_account);
            }
        }
        form.original_legacy = false;
        let renamed_from = original.filter(|o| *o != name);
        form.info = Some(match renamed_from {
            Some(old) => format!("Renamed '{old}' to '{name}'"),
            None => format!("Saved connection '{name}'"),
        });
        form.original_name = Some(name);
    }

    /// A new connection made in a project is kept once it works (so it's
    /// there next time, and the session can reopen it); unnamed, it's
    /// named after where it goes. Returns what to say about it.
    pub(super) fn keep_new_connection(&mut self) -> Option<(String, bool)> {
        let project_id = self.current_project.clone()?;
        if self.connect_form.original_name.is_some() {
            return None;
        }
        if self.connect_form.name.trim().is_empty() {
            // Already saved (same server, user, database and tunnel): that
            // one is used rather than a copy.
            let form = &self.connect_form;
            if let Some(saved) = self.saved_like(&form.to_profile()).map(|p| (p.name.clone(), p.legacy_keychain)) {
                let (name, legacy) = saved;
                self.connect_form.name.clone_from(&name);
                self.connect_form.original_name = Some(name.clone());
                self.connect_form.original_legacy = legacy;
                return Some((format!("Already saved as '{name}'"), false));
            }
            let base = if form.engine.is_file_based() {
                std::path::Path::new(&form.host)
                    .file_name()
                    .map_or_else(|| form.host.clone(), |n| n.to_string_lossy().into_owned())
            } else if form.database.is_empty() {
                form.host.clone()
            } else {
                format!("{}/{}", form.host, form.database)
            };
            self.connect_form.name = self.free_name(&project_id, &base);
        }
        self.save_connection();
        let form = &mut self.connect_form;
        let said = match form.error.take() {
            Some(e) => (format!("Connected, but not saved: {e} (Ctrl+S in the connection form)"), true),
            None => (format!("Saved as '{}' in this project", form.name), false),
        };
        form.info = None;
        Some(said)
    }

    /// Ctrl+T: connects step by step and shows where it breaks.
    pub(super) fn test_connection(&mut self) {
        if !self.may_reach_outside() {
            return;
        }
        let params = match self.connect_form.params() {
            Ok(p) => p,
            Err(e) => {
                self.connect_form.error = Some(e);
                return;
            }
        };
        self.connect_form.error = None;
        self.connect_form.info = Some("Testing the connection…".to_string());
        let title = format!("CONNECTION TEST · {}", self.connect_form.summary());
        let send = self.sender();
        tokio::spawn(async move {
            let steps = crate::db::diagnose(params).await;
            send(DbEventKind::Diagnosed(title, crate::db::report(&steps)));
        });
    }

    pub(super) fn start_connect(&mut self) {
        let params = match self.connect_form.params() {
            Ok(p) => p,
            Err(e) => {
                self.connect_form.error = Some(e);
                return;
            }
        };
        // A missing SQLite file is usually a typo: say so, and create it only
        // when asked again for the same path.
        // The approval is used up here, so a later attempt asks again.
        let approved = self.connect_form.create_file.take();
        if let Some(path) = params.file_to_create()
            && approved.as_deref() != Some(path)
        {
            let form = &mut self.connect_form;
            form.create_file = Some(path.to_string());
            form.error = None;
            form.info = Some(format!("{path} doesn't exist: Enter again to create a new database there"));
            return;
        }
        if !self.may_reach_outside() {
            return;
        }
        // Edited over a connected tab: that tab keeps its connection, and
        // this one opens in a new tab.
        if self.form_stash.is_some() {
            let form = std::mem::take(&mut self.connect_form);
            self.restore_active_form();
            self.free_tab_for_form();
            self.connect_form = form;
        }
        self.connect_form.connecting = true;
        self.connect_started = Some(Instant::now());
        self.connect_form.error = None;
        self.connect_form.info = None;
        let send = self.sender();
        self.task = Some(tokio::spawn(async move {
            match Database::connect(params).await {
                Ok(db) => send(DbEventKind::Connected(db)),
                Err(e) => send(DbEventKind::ConnectFailed(crate::db::error_message(&e))),
            }
        }));
    }
}

impl Default for ConnectForm {
    fn default() -> Self {
        Self::new()
    }
}
