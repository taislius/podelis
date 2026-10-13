use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::db::{AgentAccess, ConnectionMode, Engine, SslMode, TxMode};

const KEYRING_SERVICE: &str = "podelis";
/// Project that connections saved before projects existed are moved into.
pub const DEFAULT_PROJECT: &str = "Default";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectionProfile {
    pub name: String,
    pub engine: Engine,
    pub host: String,
    pub port: String,
    pub user: String,
    pub database: String,
    #[serde(default)]
    pub ssl_mode: SslMode,
    #[serde(default)]
    pub mode: ConnectionMode,
    /// The transaction mode it starts in, if not the mode's default
    /// (manual commit for production, auto-commit otherwise).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transaction: Option<TxMode>,
    /// SSH destination to tunnel through; empty for a direct connection.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub ssh: String,
    /// Seconds a query may run before it's cancelled; empty for no limit.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub timeout: String,
    /// What AI agents may do here (`podelis mcp`); not shared by default.
    #[serde(default, skip_serializing_if = "AgentAccess::is_none")]
    pub agents: AgentAccess,
    /// Program and arguments that print the password (e.g. `["pass",
    /// "show", "db/prod"]`); the keychain isn't used for it then.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub password_command: Vec<String>,
    /// Saved before projects existed: the password is still in the keychain
    /// under the bare connection name. Cleared the next time it's saved.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub legacy_keychain: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Project {
    /// Stable id: keys the project's keychain entries and history file, so
    /// renaming the project doesn't have to touch either.
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub connections: Vec<ConnectionProfile>,
}

#[derive(Default, Serialize, Deserialize)]
struct ConnectionsFile {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_project: Option<String>,
    #[serde(default)]
    projects: Vec<Project>,
    /// The pre-projects format; migrated into `DEFAULT_PROJECT` on load.
    #[serde(default, skip_serializing)]
    connections: Vec<ConnectionProfile>,
}

pub struct ConnectionStore {
    path: PathBuf,
    pub projects: Vec<Project>,
    /// Id of the project opened most recently.
    pub last_project: Option<String>,
    /// Set when the file on disk couldn't be parsed or moved aside; saving
    /// is refused so it is never overwritten with an empty list.
    save_blocked: bool,
    /// Id of a project created by migrating the old format on this load.
    pub migrated_project: Option<String>,
}

impl ConnectionStore {
    /// Loads the store. The second value is a warning to show the user if
    /// the existing file couldn't be read.
    pub fn load() -> (Self, Option<String>) {
        Self::load_from(config_dir().join("connections.toml"))
    }

    fn empty(path: PathBuf, save_blocked: bool) -> Self {
        Self { path, projects: vec![], last_project: None, save_blocked, migrated_project: None }
    }

    pub fn load_from(path: PathBuf) -> (Self, Option<String>) {
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return (Self::empty(path, false), None),
            Err(e) => {
                let warning = format!("Couldn't read {}: {e}. Saving is disabled.", path.display());
                return (Self::empty(path, true), Some(warning));
            }
        };
        let file = match toml::from_str::<ConnectionsFile>(&text) {
            Ok(file) => file,
            Err(e) => {
                let backup = backup_path(&path);
                let first_line = e.to_string().lines().next().unwrap_or_default().to_string();
                return match fs::rename(&path, &backup) {
                    Ok(()) => {
                        let warning =
                            format!("connections.toml was invalid ({first_line}); moved it to {}", backup.display());
                        (Self::empty(path, false), Some(warning))
                    }
                    Err(rename_err) => {
                        let warning = format!(
                            "connections.toml is invalid ({first_line}) and couldn't be backed up \
                             ({rename_err}). Saving is disabled."
                        );
                        (Self::empty(path, true), Some(warning))
                    }
                };
            }
        };

        let mut store = Self {
            path,
            projects: file.projects,
            last_project: file.last_project,
            save_blocked: false,
            migrated_project: None,
        };
        if !file.connections.is_empty() {
            let id = match store.projects.iter().position(|p| p.name == DEFAULT_PROJECT) {
                Some(i) => store.projects[i].id.clone(),
                None => store.add_project_unsaved(DEFAULT_PROJECT),
            };
            let project = store.project_mut(&id).unwrap();
            for mut profile in file.connections {
                if !project.connections.iter().any(|c| c.name == profile.name) {
                    profile.legacy_keychain = true;
                    project.connections.push(profile);
                }
            }
            store.migrated_project = Some(id);
            // Persist now so the new project id is stable across runs.
            if let Err(e) = store.save() {
                return (store, Some(format!("Couldn't save migrated connections: {e:#}")));
            }
        }
        (store, None)
    }

    fn save(&self) -> Result<()> {
        if self.save_blocked {
            bail!("refusing to overwrite unreadable {}", self.path.display());
        }
        let file = ConnectionsFile {
            last_project: self.last_project.clone(),
            projects: self.projects.clone(),
            connections: vec![],
        };
        let text = toml::to_string_pretty(&file).context("failed to serialize connections")?;
        write_atomic(&self.path, &text).context("failed to write connections file")
    }

    pub fn project(&self, id: &str) -> Option<&Project> {
        self.projects.iter().find(|p| p.id == id)
    }

    fn project_mut(&mut self, id: &str) -> Option<&mut Project> {
        self.projects.iter_mut().find(|p| p.id == id)
    }

    fn project_or_err(&mut self, id: &str) -> Result<&mut Project> {
        self.project_mut(id).context("project no longer exists")
    }

    pub fn project_by_name(&self, name: &str) -> Option<&Project> {
        self.projects.iter().find(|p| p.name.eq_ignore_ascii_case(name))
    }

    fn check_project_name(&self, name: &str, except_id: Option<&str>) -> Result<()> {
        if name.is_empty() {
            bail!("project name can't be empty");
        }
        if self.projects.iter().any(|p| p.name.eq_ignore_ascii_case(name) && Some(p.id.as_str()) != except_id) {
            bail!("a project named '{name}' already exists");
        }
        Ok(())
    }

    fn add_project_unsaved(&mut self, name: &str) -> String {
        let id = uuid::Uuid::new_v4().to_string();
        self.projects.push(Project { id: id.clone(), name: name.to_string(), connections: vec![] });
        id
    }

    /// Creates a project and returns its id.
    pub fn add_project(&mut self, name: &str) -> Result<String> {
        let name = name.trim();
        self.check_project_name(name, None)?;
        let id = self.add_project_unsaved(name);
        self.save()?;
        Ok(id)
    }

    pub fn rename_project(&mut self, id: &str, name: &str) -> Result<()> {
        let name = name.trim();
        self.check_project_name(name, Some(id))?;
        self.project_or_err(id)?.name = name.to_string();
        self.save()
    }

    /// Removes a project, returning it so the caller can clean up its
    /// keychain entries and history.
    pub fn delete_project(&mut self, id: &str) -> Result<Project> {
        let idx = self.projects.iter().position(|p| p.id == id).context("project no longer exists")?;
        let project = self.projects.remove(idx);
        if self.last_project.as_deref() == Some(id) {
            self.last_project = None;
        }
        self.save()?;
        Ok(project)
    }

    pub fn set_last_project(&mut self, id: &str) -> Result<()> {
        if self.last_project.as_deref() == Some(id) {
            return Ok(());
        }
        self.last_project = Some(id.to_string());
        self.save()
    }

    /// True if `name` belongs to a connection in the project other than the
    /// one being edited.
    pub fn name_taken(&self, project_id: &str, original: Option<&str>, name: &str) -> bool {
        Some(name) != original && self.project(project_id).is_some_and(|p| p.connections.iter().any(|c| c.name == name))
    }

    /// Saves `profile` in the project, replacing the connection called
    /// `original` (so a rename doesn't leave the old entry behind) or
    /// appending a new one.
    pub fn upsert(&mut self, project_id: &str, original: Option<&str>, profile: ConnectionProfile) -> Result<()> {
        if self.name_taken(project_id, original, &profile.name) {
            bail!("a connection named '{}' already exists in this project", profile.name);
        }
        let project = self.project_or_err(project_id)?;
        let key = original.unwrap_or(&profile.name).to_string();
        if let Some(existing) = project.connections.iter_mut().find(|c| c.name == key) {
            *existing = profile;
        } else {
            project.connections.push(profile);
        }
        self.save()
    }

    pub fn delete(&mut self, project_id: &str, name: &str) -> Result<()> {
        self.project_or_err(project_id)?.connections.retain(|c| c.name != name);
        self.save()
    }

    /// Moves a connection between projects. Doesn't touch the keychain; the
    /// caller moves the password (see `keychain_account`).
    pub fn move_connection(&mut self, from: &str, name: &str, to: &str) -> Result<()> {
        if from == to {
            return Ok(());
        }
        if self.name_taken(to, None, name) {
            bail!("the target project already has a connection named '{name}'");
        }
        let source = self.project_or_err(from)?;
        let idx = source.connections.iter().position(|c| c.name == name).context("connection no longer exists")?;
        let mut profile = source.connections.remove(idx);
        // The caller stores the password under the new project's key.
        profile.legacy_keychain = false;
        self.project_or_err(to)?.connections.push(profile);
        self.save()
    }
}

/// Keychain account a connection's password is stored under.
pub fn keychain_account(project_id: &str, profile: &ConnectionProfile) -> String {
    if profile.legacy_keychain { profile.name.clone() } else { project_account(project_id, &profile.name) }
}

/// Keychain account for a connection saved in the current format.
pub fn project_account(project_id: &str, name: &str) -> String {
    format!("{project_id}/{name}")
}

/// Where all of Podėlis's files live. `PODELIS_CONFIG_DIR` overrides the
/// OS default, e.g. for a portable setup or testing.
pub fn config_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("PODELIS_CONFIG_DIR").filter(|d| !d.is_empty()) {
        return PathBuf::from(dir);
    }
    dirs::config_dir().unwrap_or_else(std::env::temp_dir).join("podelis")
}

fn backup_path(path: &Path) -> PathBuf {
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
    path.with_extension(format!("toml.bak-{stamp}"))
}

/// Writes via a temp file + rename so a crash mid-write can't truncate the
/// existing file. Files are readable only by the user (they can hold SQL
/// with sensitive data), and new folders likewise.
pub fn write_atomic(path: &Path, text: &str) -> Result<()> {
    if let Some(parent) = path.parent()
        && !parent.exists()
    {
        fs::create_dir_all(parent).context("failed to create config directory")?;
        restrict(parent, 0o700);
    }
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, text)?;
    restrict(&tmp, 0o600);
    fs::rename(&tmp, path)?;
    Ok(())
}

fn restrict(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    let _ = fs::set_permissions(path, fs::Permissions::from_mode(mode));
}

fn keyring_entry(account: &str) -> Result<keyring::Entry> {
    keyring::Entry::new(KEYRING_SERVICE, account).map_err(|e| match e {
        // keyring's wording for "there's no Secret Service to talk to".
        keyring::Error::NoDefaultStore if cfg!(target_os = "linux") => {
            anyhow::anyhow!("there's no OS keychain here: on Linux, no Secret Service is running")
        }
        keyring::Error::NoDefaultStore => anyhow::anyhow!("there's no OS keychain here"),
        e => anyhow::Error::new(e).context("failed to access OS keychain"),
    })
}

pub fn set_password(account: &str, password: &str) -> Result<()> {
    #[cfg(test)]
    if let Some(fake) = fake_keychain() {
        fake.lock().unwrap().insert(account.to_string(), password.to_string());
        return Ok(());
    }
    keyring_entry(account)?.set_password(password).context("failed to store password in OS keychain")
}

/// Tests keep passwords in memory, away from the real keychain (which may
/// also prompt), unless `PODELIS_TEST_KEYCHAIN=1` asks for it.
#[cfg(test)]
fn fake_keychain() -> Option<&'static std::sync::Mutex<std::collections::HashMap<String, String>>> {
    static FAKE: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<String, String>>> =
        std::sync::OnceLock::new();
    (std::env::var("PODELIS_TEST_KEYCHAIN").as_deref() != Ok("1")).then(|| FAKE.get_or_init(Default::default))
}

/// The stored password; an empty string if there is none.
pub fn get_password(account: &str) -> Result<String> {
    #[cfg(test)]
    if let Some(fake) = fake_keychain() {
        return Ok(fake.lock().unwrap().get(account).cloned().unwrap_or_default());
    }
    match keyring_entry(account)?.get_password() {
        Ok(p) => Ok(p),
        Err(keyring::Error::NoEntry) => Ok(String::new()),
        Err(e) => Err(anyhow::Error::new(e).context("failed to read password from OS keychain")),
    }
}

/// The password saved for a connection, read when connecting. A keychain
/// that can't be read (none on a server, or locked) isn't fatal: the
/// password is then empty, so the database's own sources (`PGPASSWORD`,
/// `~/.pgpass`) can still supply it, and the second value says why.
pub fn saved_password(project_id: &str, profile: &ConnectionProfile) -> (String, Option<String>) {
    if profile.engine.is_file_based() || !profile.password_command.is_empty() {
        return (String::new(), None);
    }
    match get_password(&keychain_account(project_id, profile)) {
        Ok(password) => (password, None),
        Err(e) => (String::new(), Some(format!("{e:#}"))),
    }
}

/// Deletes the stored password; succeeds if there wasn't one.
pub fn delete_password(account: &str) -> Result<()> {
    #[cfg(test)]
    if let Some(fake) = fake_keychain() {
        fake.lock().unwrap().remove(account);
        return Ok(());
    }
    match keyring_entry(account)?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(anyhow::Error::new(e).context("failed to delete password from OS keychain")),
    }
}

/// Stores `password` (or removes the entry if it's empty).
pub fn store_password(account: &str, password: &str) -> Result<()> {
    if password.is_empty() { delete_password(account) } else { set_password(account, password) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile(name: &str) -> ConnectionProfile {
        ConnectionProfile {
            name: name.to_string(),
            engine: Engine::Postgres,
            host: "localhost".to_string(),
            port: "5432".to_string(),
            user: "me".to_string(),
            database: "db".to_string(),
            ssl_mode: SslMode::Prefer,
            mode: ConnectionMode::Normal,
            transaction: None,
            ssh: String::new(),
            timeout: String::new(),
            agents: AgentAccess::None,
            password_command: Vec::new(),
            legacy_keychain: false,
        }
    }

    fn temp_store() -> (ConnectionStore, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let (store, warning) = ConnectionStore::load_from(dir.path().join("c.toml"));
        assert!(warning.is_none());
        (store, dir)
    }

    fn names(store: &ConnectionStore, project: &str) -> Vec<String> {
        store.project(project).unwrap().connections.iter().map(|c| c.name.clone()).collect()
    }

    /// Opt-in (`PODELIS_TEST_KEYCHAIN=1`): writes, reads, moves and deletes
    /// real OS keychain entries under throwaway accounts, the way saving,
    /// moving and deleting connections does. macOS may ask for permission.
    #[test]
    fn live_keychain_round_trip() {
        if std::env::var("PODELIS_TEST_KEYCHAIN").as_deref() != Ok("1") {
            return;
        }
        let from = format!("podelis-test-{}", uuid::Uuid::new_v4());
        let to = format!("podelis-test-{}", uuid::Uuid::new_v4());
        let p = profile("conn");
        let (a, b) = (keychain_account(&from, &p), keychain_account(&to, &p));
        store_password(&a, "s3cret").unwrap();
        assert_eq!(get_password(&a).unwrap(), "s3cret");
        // Move, as `m` does: copy to the new account, delete the old one.
        store_password(&b, &get_password(&a).unwrap()).unwrap();
        delete_password(&a).unwrap();
        assert_eq!(get_password(&a).unwrap(), "");
        assert_eq!(get_password(&b).unwrap(), "s3cret");
        // An empty password removes the entry; deleting twice is fine.
        store_password(&b, "").unwrap();
        assert_eq!(get_password(&b).unwrap(), "");
        delete_password(&b).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn files_are_private_to_the_user() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("new").join("history.jsonl");
        write_atomic(&file, "secret sql").unwrap();
        let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&file), 0o600);
        assert_eq!(mode(&dir.path().join("new")), 0o700);
    }

    #[test]
    fn missing_file_is_empty_and_saveable() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("connections.toml");
        let (mut store, warning) = ConnectionStore::load_from(path.clone());
        assert!(warning.is_none() && store.projects.is_empty());
        let id = store.add_project("p").unwrap();
        store.upsert(&id, None, profile("a")).unwrap();
        store.set_last_project(&id).unwrap();
        let (reloaded, _) = ConnectionStore::load_from(path);
        assert_eq!(reloaded.projects, store.projects);
        assert_eq!(reloaded.last_project.as_deref(), Some(id.as_str()));
    }

    #[test]
    fn password_command_is_saved_as_words_and_skips_the_keychain() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("connections.toml");
        let (mut store, _) = ConnectionStore::load_from(path.clone());
        let id = store.add_project("p").unwrap();
        let mut with_command = profile("a");
        with_command.password_command = vec!["pass".into(), "show".into(), "db/a".into()];
        store.upsert(&id, None, with_command.clone()).unwrap();
        store.upsert(&id, None, profile("b")).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("password_command = [") && text.contains(r#""db/a""#), "{text}");
        assert_eq!(text.matches("password_command").count(), 1, "only where it's set");
        let (reloaded, _) = ConnectionStore::load_from(path);
        assert_eq!(reloaded.projects, store.projects);
        // A password in the keychain under its name isn't read.
        store_password(&project_account(&id, "a"), "stale").unwrap();
        assert_eq!(saved_password(&id, &with_command), (String::new(), None));
        store_password(&project_account(&id, "b"), "s3cret").unwrap();
        assert_eq!(saved_password(&id, &profile("b")), ("s3cret".to_string(), None));
        delete_password(&project_account(&id, "a")).unwrap();
        delete_password(&project_account(&id, "b")).unwrap();
    }

    #[test]
    fn invalid_file_is_backed_up_not_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("connections.toml");
        fs::write(&path, "this is [not toml").unwrap();
        let (mut store, warning) = ConnectionStore::load_from(path.clone());
        assert!(warning.unwrap().contains("moved it to"));
        let backups: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .filter_map(std::result::Result::ok)
            .filter(|e| e.file_name().to_string_lossy().contains(".bak-"))
            .collect();
        assert_eq!(backups.len(), 1);
        assert_eq!(fs::read_to_string(backups[0].path()).unwrap(), "this is [not toml");
        store.add_project("p").unwrap();
    }

    #[test]
    fn legacy_file_migrates_into_default_project() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("c.toml");
        fs::write(
            &path,
            "[[connections]]\nname = \"x\"\nengine = \"MySql\"\nhost = \"h\"\nport = \"3306\"\n\
             user = \"u\"\ndatabase = \"d\"\n",
        )
        .unwrap();
        let (store, warning) = ConnectionStore::load_from(path.clone());
        assert!(warning.is_none());
        let id = store.migrated_project.clone().unwrap();
        let project = store.project(&id).unwrap();
        assert_eq!(project.name, DEFAULT_PROJECT);
        let conn = &project.connections[0];
        assert_eq!((conn.engine, conn.ssl_mode, conn.legacy_keychain), (Engine::MySql, SslMode::Prefer, true));
        // Password still found under the old bare-name keychain account.
        assert_eq!(keychain_account(&id, conn), "x");

        // The file was rewritten in the new format; reloading doesn't re-migrate.
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("[[projects]]") && !text.contains("\n[[connections]]"));
        let (again, _) = ConnectionStore::load_from(path);
        assert!(again.migrated_project.is_none());
        assert_eq!(again.projects, store.projects);
    }

    #[test]
    fn same_connection_name_in_different_projects() {
        let (mut store, _dir) = temp_store();
        let a = store.add_project("a").unwrap();
        let b = store.add_project("b").unwrap();
        store.upsert(&a, None, profile("local")).unwrap();
        store.upsert(&b, None, profile("local")).unwrap();
        let pa = &store.project(&a).unwrap().connections[0];
        let pb = &store.project(&b).unwrap().connections[0];
        assert_ne!(keychain_account(&a, pa), keychain_account(&b, pb));
    }

    #[test]
    fn project_names_are_unique_and_non_empty() {
        let (mut store, _dir) = temp_store();
        let a = store.add_project("Shop").unwrap();
        assert!(store.add_project("shop").is_err());
        assert!(store.add_project("  ").is_err());
        let b = store.add_project("other").unwrap();
        assert!(store.rename_project(&b, "SHOP").is_err());
        store.rename_project(&a, "shop-backend").unwrap();
        assert_eq!(store.project_by_name("Shop-Backend").unwrap().id, a);
    }

    #[test]
    fn delete_project_clears_last_project() {
        let (mut store, _dir) = temp_store();
        let a = store.add_project("a").unwrap();
        store.upsert(&a, None, profile("x")).unwrap();
        store.set_last_project(&a).unwrap();
        let removed = store.delete_project(&a).unwrap();
        assert_eq!(removed.connections.len(), 1);
        assert!(store.projects.is_empty() && store.last_project.is_none());
    }

    #[test]
    fn rename_replaces_instead_of_duplicating() {
        let (mut store, _dir) = temp_store();
        let p = store.add_project("p").unwrap();
        store.upsert(&p, None, profile("old")).unwrap();
        store.upsert(&p, None, profile("other")).unwrap();
        store.upsert(&p, Some("old"), profile("new")).unwrap();
        assert_eq!(names(&store, &p), ["new", "other"]);
    }

    #[test]
    fn rename_onto_existing_name_is_refused() {
        let (mut store, _dir) = temp_store();
        let p = store.add_project("p").unwrap();
        store.upsert(&p, None, profile("a")).unwrap();
        store.upsert(&p, None, profile("b")).unwrap();
        assert!(store.upsert(&p, Some("a"), profile("b")).is_err());
        assert!(store.upsert(&p, None, profile("b")).is_err());
        assert!(store.upsert(&p, Some("b"), profile("b")).is_ok());
    }

    #[test]
    fn move_connection_between_projects() {
        let (mut store, _dir) = temp_store();
        let a = store.add_project("a").unwrap();
        let b = store.add_project("b").unwrap();
        store.upsert(&a, None, profile("x")).unwrap();
        store.upsert(&a, None, profile("y")).unwrap();
        store.upsert(&b, None, profile("y")).unwrap();
        store.move_connection(&a, "x", &b).unwrap();
        assert_eq!(names(&store, &a), ["y"]);
        assert_eq!(names(&store, &b), ["y", "x"]);
        assert!(store.move_connection(&a, "y", &b).is_err());
        assert_eq!(names(&store, &a), ["y"]);
    }
}
