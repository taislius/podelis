use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::db::keychain::get_password;
use crate::db::{AgentAccess, ConnectionMode, Engine, SslMode, TxMode};

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
    /// A CA file, a client certificate and key for TLS, if not the default.
    #[serde(default, skip_serializing_if = "crate::db::TlsFiles::is_empty")]
    pub tls: crate::db::TlsFiles,
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
    /// Settings this version doesn't know (a newer one's), kept as they
    /// were so saving doesn't drop them.
    #[serde(flatten)]
    pub unknown: Unknown,
}

impl ConnectionProfile {
    /// The database it's a connection to, as the project's memory (its
    /// ontology) names it: the database's name, so dev, staging and prod
    /// connections to `shop` share what's known of `shop`; for a file
    /// (SQLite, DuckDB), the file's name without its folder or extension.
    /// Without either, the connection's own name. Lowercase.
    pub fn database_key(&self) -> String {
        let key = if self.engine.is_file_based() {
            let host = self.host.trim();
            std::path::Path::new(host)
                .file_stem()
                .map_or_else(|| host.to_string(), |s| s.to_string_lossy().into_owned())
        } else {
            self.database.trim().to_string()
        };
        let key = if key.is_empty() || key == ":memory:" { self.name.trim().to_string() } else { key };
        key.to_lowercase()
    }
}

/// A project's stable id: keys its keychain entries and its files
/// (history, tabs, library, memory…), so renaming the project touches
/// neither. Written as the plain string it always was.
/// It names files and folders, so one read back (connections.toml, the
/// gateway) is taken only without `/`, `\` and the like that would lead
/// out of them, and not starting with `.`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ProjectId(String);

impl TryFrom<String> for ProjectId {
    type Error = String;

    fn try_from(id: String) -> Result<Self, String> {
        let unsafe_char = |c: char| matches!(c, '/' | '\\' | '\0') || c.is_control();
        // Not `.x` either: a hidden file, like `..` a way out of the folder.
        if id.is_empty() || id.len() > 200 || id.starts_with('.') || id.chars().any(unsafe_char) {
            return Err(format!("not a project id: {id:?}"));
        }
        Ok(Self(id))
    }
}

impl From<ProjectId> for String {
    fn from(id: ProjectId) -> Self {
        id.0
    }
}

impl ProjectId {
    fn new() -> Self {
        Self(uuid::Uuid::new_v4().to_string())
    }

    /// As it's written in file names, keychain accounts and messages to
    /// the gateway.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// One read back from where it was written (a gateway request): `None`
    /// when it can't be one. Whether a project has it is the store's to say.
    pub fn from_written(id: impl Into<String>) -> Option<Self> {
        Self::try_from(id.into()).ok()
    }
}

/// An id made up by a test.
#[cfg(test)]
impl From<&str> for ProjectId {
    fn from(id: &str) -> Self {
        Self::try_from(id.to_string()).expect("a test's id is safe")
    }
}

impl std::fmt::Display for ProjectId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Project {
    /// Stable id: keys the project's keychain entries and history file, so
    /// renaming the project doesn't have to touch either.
    pub id: ProjectId,
    pub name: String,
    #[serde(default)]
    pub connections: Vec<ConnectionProfile>,
    /// Which AI models may be shown its databases' schemas (table and
    /// column names, comments) when asking in plain language.
    #[serde(default, skip_serializing_if = "ShareSchema::is_default")]
    pub share_schema: ShareSchema,
    /// Settings this version doesn't know, kept (see `ConnectionProfile`).
    #[serde(flatten)]
    pub unknown: Unknown,
}

/// `profile` as opened from `dir`: a SQLite file with a relative path is
/// in that folder (the gateway runs elsewhere than the app or agent that
/// asks). Other connections are as they are.
pub fn opened_from(profile: &ConnectionProfile, dir: Option<&str>) -> ConnectionProfile {
    let mut opened = profile.clone();
    if opened.engine.is_file_based()
        && opened.host != ":memory:"
        && std::path::Path::new(&opened.host).is_relative()
        && let Some(dir) = dir.filter(|d| !d.is_empty())
    {
        opened.host = std::path::Path::new(dir).join(&opened.host).to_string_lossy().into_owned();
    }
    opened
}

/// Where a project's schemas may go, to write SQL from a question.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ShareSchema {
    /// Only to models on this machine: nothing leaves it.
    #[default]
    Local,
    /// To any model set up, a provider's too.
    All,
}

impl ShareSchema {
    #[expect(clippy::trivially_copy_pass_by_ref, reason = "serde's `skip_serializing_if` passes a reference")]
    fn is_default(&self) -> bool {
        *self == ShareSchema::default()
    }
}

/// Settings in `connections.toml` that this version doesn't know (written
/// by a newer one): kept as they were and written back, so opening an
/// older Podėlis by mistake doesn't lose them.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Unknown(toml::Table);

// Settings hold no NaN, so equality is total.
impl Eq for Unknown {}

#[derive(Default, Serialize, Deserialize)]
struct ConnectionsFile {
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "project_if_valid")]
    last_project: Option<ProjectId>,
    #[serde(default)]
    projects: Vec<StoredProject>,
    /// The pre-projects format; migrated into `DEFAULT_PROJECT` on load.
    #[serde(default, skip_serializing)]
    connections: Vec<ConnectionProfile>,
    /// Settings this version doesn't know, kept (see `ConnectionProfile`).
    #[serde(flatten)]
    unknown: Unknown,
}

/// A project as the file has it: one this version can't read (its id
/// isn't one, a setting of another shape) is kept as it is and written
/// back, rather than failing the whole file.
#[derive(Serialize, Deserialize)]
#[serde(untagged)]
enum StoredProject {
    Readable(Project),
    Unreadable(toml::Value),
}

/// A project this version can't read, kept as it is, and where it was:
/// after how many of the readable ones (so a save leaves it in its place).
#[derive(Clone)]
struct Kept {
    after: usize,
    project: toml::Value,
}

impl Kept {
    fn name(&self) -> Option<&str> {
        self.project.get("name").and_then(toml::Value::as_str)
    }
}

/// The projects this version reads, and those it keeps as they are.
fn sort_out(stored: Vec<StoredProject>) -> (Vec<Project>, Vec<Kept>) {
    let mut readable = Vec::new();
    let mut unreadable = Vec::new();
    for project in stored {
        match project {
            StoredProject::Readable(p) => readable.push(p),
            StoredProject::Unreadable(project) => unreadable.push(Kept { after: readable.len(), project }),
        }
    }
    (readable, unreadable)
}

/// What's said of the projects kept as they are, if there are any.
fn unreadable_warning(unreadable: &[Kept]) -> Option<String> {
    if unreadable.is_empty() {
        return None;
    }
    let names: Vec<&str> = unreadable.iter().map(|k| k.name().unwrap_or("(no name)")).collect();
    Some(format!(
        "connections.toml: {} project(s) couldn't be read and are left in it as they are: {}",
        names.len(),
        names.join(", ")
    ))
}

/// `last_project`, dropped when it can't be a project's id.
fn project_if_valid<'de, D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Option<ProjectId>, D::Error> {
    Ok(Option::<String>::deserialize(d)?.and_then(ProjectId::from_written))
}

pub struct ConnectionStore {
    path: PathBuf,
    pub projects: Vec<Project>,
    /// Projects in the file this version can't read: written back as found,
    /// where they were.
    unreadable: Vec<Kept>,
    /// Id of the project opened most recently.
    pub last_project: Option<ProjectId>,
    /// Set when the file on disk couldn't be parsed or moved aside; saving
    /// is refused so it is never overwritten with an empty list.
    save_blocked: bool,
    /// Id of a project created by migrating the old format on this load.
    pub migrated_project: Option<ProjectId>,
    /// The file's own settings this version doesn't know, written back.
    unknown: Unknown,
}

impl ConnectionStore {
    /// Loads the store. The second value is a warning to show the user if
    /// the existing file couldn't be read.
    pub fn load() -> (Self, Option<String>) {
        Self::load_from(config_dir().join("connections.toml"))
    }

    fn empty(path: PathBuf, save_blocked: bool) -> Self {
        Self {
            path,
            projects: vec![],
            unreadable: vec![],
            last_project: None,
            save_blocked,
            migrated_project: None,
            unknown: Unknown::default(),
        }
    }

    /// Reads the store without changing anything: a file that can't be
    /// parsed is reported, not moved aside, and saving is refused. For
    /// readers in the background (the gateway), which mustn't touch the
    /// user's file.
    pub fn peek(path: PathBuf) -> (Self, Option<String>) {
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return (Self::empty(path, true), None),
            Err(e) => {
                let warning = format!("couldn't read {}: {e}", path.display());
                return (Self::empty(path, true), Some(warning));
            }
        };
        match toml::from_str::<ConnectionsFile>(&text) {
            Ok(file) => {
                let (projects, unreadable) = sort_out(file.projects);
                let warning = unreadable_warning(&unreadable);
                let store = Self {
                    path,
                    unknown: file.unknown,
                    projects,
                    unreadable,
                    last_project: file.last_project,
                    save_blocked: true,
                    migrated_project: None,
                };
                (store, warning)
            }
            Err(e) => {
                let first_line = e.to_string().lines().next().unwrap_or_default().to_string();
                (Self::empty(path, true), Some(format!("connections.toml is invalid: {first_line}")))
            }
        }
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

        let (projects, unreadable) = sort_out(file.projects);
        let warning = unreadable_warning(&unreadable);
        let mut store = Self {
            path,
            projects,
            unreadable,
            last_project: file.last_project,
            save_blocked: false,
            migrated_project: None,
            unknown: file.unknown,
        };
        if !file.connections.is_empty() {
            let id = match store.projects.iter().position(|p| p.name == DEFAULT_PROJECT) {
                Some(i) => store.projects[i].id.clone(),
                None => store.add_project_unsaved(DEFAULT_PROJECT),
            };
            let project = store.project_mut(&id).expect("the project was just found or added");
            for mut profile in file.connections {
                if !project.connections.iter().any(|c| c.name == profile.name) {
                    profile.legacy_keychain = true;
                    project.connections.push(profile);
                }
            }
            store.migrated_project = Some(id);
            // Persist now so the new project id is stable across runs.
            if let Err(e) = store.save() {
                // Said with what else there was to say (projects left out).
                let failed = format!("Couldn't save migrated connections: {e:#}");
                let said = warning.map_or(failed.clone(), |w| format!("{failed}; {w}"));
                return (store, Some(said));
            }
        }
        (store, warning)
    }

    fn save(&self) -> Result<()> {
        if self.save_blocked {
            bail!("refusing to overwrite unreadable {}", self.path.display());
        }
        // Each project kept as it was goes back where it was among the others.
        let mut projects = Vec::with_capacity(self.projects.len() + self.unreadable.len());
        let kept_after = |i: usize| self.unreadable.iter().filter(move |k| k.after == i);
        for (i, project) in self.projects.iter().enumerate() {
            projects.extend(kept_after(i).map(|k| StoredProject::Unreadable(k.project.clone())));
            projects.push(StoredProject::Readable(project.clone()));
        }
        let at_end = self.unreadable.iter().filter(|k| k.after >= self.projects.len());
        projects.extend(at_end.map(|k| StoredProject::Unreadable(k.project.clone())));
        let file = ConnectionsFile {
            last_project: self.last_project.clone(),
            projects,
            connections: vec![],
            unknown: self.unknown.clone(),
        };
        let text = toml::to_string_pretty(&file).context("failed to serialize connections")?;
        write_atomic(&self.path, &text).context("failed to write connections file")
    }

    pub fn project(&self, id: &ProjectId) -> Option<&Project> {
        self.projects.iter().find(|p| p.id == *id)
    }

    fn project_mut(&mut self, id: &ProjectId) -> Option<&mut Project> {
        self.projects.iter_mut().find(|p| p.id == *id)
    }

    fn project_or_err(&mut self, id: &ProjectId) -> Result<&mut Project> {
        self.project_mut(id).context("project no longer exists")
    }

    pub fn project_by_name(&self, name: &str) -> Option<&Project> {
        self.projects.iter().find(|p| p.name.eq_ignore_ascii_case(name))
    }

    fn check_project_name(&self, name: &str, except_id: Option<&ProjectId>) -> Result<()> {
        if name.is_empty() {
            bail!("project name can't be empty");
        }
        let mut kept = self.unreadable.iter().filter_map(Kept::name);
        if self.projects.iter().any(|p| p.name.eq_ignore_ascii_case(name) && Some(&p.id) != except_id)
            || kept.any(|n| n.eq_ignore_ascii_case(name))
        {
            bail!("a project named '{name}' already exists");
        }
        Ok(())
    }

    fn add_project_unsaved(&mut self, name: &str) -> ProjectId {
        let id = ProjectId::new();
        self.projects.push(Project {
            unknown: Unknown::default(),
            id: id.clone(),
            name: name.to_string(),
            connections: vec![],
            share_schema: ShareSchema::default(),
        });
        id
    }

    /// Creates a project and returns its id.
    pub fn add_project(&mut self, name: &str) -> Result<ProjectId> {
        let name = name.trim();
        self.check_project_name(name, None)?;
        let id = self.add_project_unsaved(name);
        self.save()?;
        Ok(id)
    }

    pub fn rename_project(&mut self, id: &ProjectId, name: &str) -> Result<()> {
        let name = name.trim();
        self.check_project_name(name, Some(id))?;
        self.project_or_err(id)?.name = name.to_string();
        self.save()
    }

    /// Where the project's schemas may be sent to write SQL.
    pub fn set_share_schema(&mut self, id: &ProjectId, share: ShareSchema) -> Result<()> {
        self.project_or_err(id)?.share_schema = share;
        self.save()
    }

    /// Removes a project, returning it so the caller can clean up its
    /// keychain entries and history.
    pub fn delete_project(&mut self, id: &ProjectId) -> Result<Project> {
        let idx = self.projects.iter().position(|p| p.id == *id).context("project no longer exists")?;
        let project = self.projects.remove(idx);
        // The projects kept as they were stay where they were among the rest.
        for kept in &mut self.unreadable {
            if kept.after > idx {
                kept.after -= 1;
            }
        }
        if self.last_project.as_ref() == Some(id) {
            self.last_project = None;
        }
        self.save()?;
        Ok(project)
    }

    pub fn set_last_project(&mut self, id: &ProjectId) -> Result<()> {
        if self.last_project.as_ref() == Some(id) {
            return Ok(());
        }
        self.last_project = Some(id.clone());
        self.save()
    }

    /// True if `name` belongs to a connection in the project other than the
    /// one being edited.
    pub fn name_taken(&self, project_id: &ProjectId, original: Option<&str>, name: &str) -> bool {
        Some(name) != original && self.project(project_id).is_some_and(|p| p.connections.iter().any(|c| c.name == name))
    }

    /// Saves `profile` in the project, replacing the connection called
    /// `original` (so a rename doesn't leave the old entry behind) or
    /// appending a new one.
    pub fn upsert(&mut self, project_id: &ProjectId, original: Option<&str>, profile: ConnectionProfile) -> Result<()> {
        if self.name_taken(project_id, original, &profile.name) {
            bail!("a connection named '{}' already exists in this project", profile.name);
        }
        let project = self.project_or_err(project_id)?;
        let key = original.unwrap_or(&profile.name).to_string();
        if let Some(existing) = project.connections.iter_mut().find(|c| c.name == key) {
            // Edited here: what a newer version set stays.
            let unknown = std::mem::take(&mut existing.unknown);
            *existing = ConnectionProfile { unknown, ..profile };
        } else {
            project.connections.push(profile);
        }
        self.save()
    }

    pub fn delete(&mut self, project_id: &ProjectId, name: &str) -> Result<()> {
        self.project_or_err(project_id)?.connections.retain(|c| c.name != name);
        self.save()
    }

    /// Moves a connection between projects. Doesn't touch the keychain; the
    /// caller moves the password (see `keychain_account`).
    pub fn move_connection(&mut self, from: &ProjectId, name: &str, to: &ProjectId) -> Result<()> {
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
pub fn keychain_account(project_id: &ProjectId, profile: &ConnectionProfile) -> String {
    if profile.legacy_keychain { profile.name.clone() } else { project_account(project_id, &profile.name) }
}

/// Keychain account for a connection saved in the current format.
pub fn project_account(project_id: &ProjectId, name: &str) -> String {
    format!("{project_id}/{name}")
}

/// Where all of Podėlis's files live. `PODELIS_CONFIG_DIR` overrides the
/// OS default, e.g. for a portable setup or testing.
pub fn config_dir() -> PathBuf {
    config_dir_from_env().unwrap_or_else(|| dirs::config_dir().unwrap_or_else(std::env::temp_dir).join("podelis"))
}

/// The folder `PODELIS_CONFIG_DIR` names, if it's set.
fn config_dir_from_env() -> Option<PathBuf> {
    std::env::var_os("PODELIS_CONFIG_DIR").filter(|d| !d.is_empty()).map(PathBuf::from)
}

/// Makes the config folder the user's alone (0700), if it was made more
/// open (by an older version, or `mkdir`): its files are 0600 and its
/// folders 0700 already, but others could still list the projects' ids.
/// A folder given by `PODELIS_CONFIG_DIR`, or a symlink (into a dotfiles
/// or synced folder, say), is left as it is.
pub fn secure_config_dir() {
    if config_dir_from_env().is_none() {
        secure_dir(&config_dir());
    }
}

fn secure_dir(dir: &Path) {
    use std::os::unix::fs::PermissionsExt;
    // Not followed: a link's target is another folder's to decide.
    if let Ok(meta) = fs::symlink_metadata(dir)
        && meta.is_dir()
        && meta.permissions().mode() & 0o077 != 0
    {
        restrict(dir, 0o700);
    }
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
    // A name of this write's own: two Podėlis writing the same file at once
    // can't write into (or rename) each other's half-written temp file.
    static WRITES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = WRITES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp = path.with_extension(format!("tmp-{}-{n}", std::process::id()));
    let written = (|| -> Result<()> {
        // The user's alone from the start, not after it's written.
        use std::io::Write as _;
        use std::os::unix::fs::OpenOptionsExt as _;
        let mut file = fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&tmp)?;
        file.write_all(text.as_bytes())?;
        drop(file);
        restrict(&tmp, 0o600);
        fs::rename(&tmp, path)?;
        Ok(())
    })();
    if written.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    written
}

fn restrict(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    let _ = fs::set_permissions(path, fs::Permissions::from_mode(mode));
}

/// The password saved for a connection, read when connecting. A keychain
/// that can't be read (none on a server, or locked) isn't fatal: the
/// password is then empty, so the database's own sources (`PGPASSWORD`,
/// `~/.pgpass`) can still supply it, and the second value says why.
pub fn saved_password(project_id: &ProjectId, profile: &ConnectionProfile) -> (String, Option<String>) {
    if profile.engine.is_file_based() || !profile.password_command.is_empty() {
        return (String::new(), None);
    }
    match get_password(&keychain_account(project_id, profile)) {
        Ok(password) => (password, None),
        Err(e) => (String::new(), Some(format!("{e:#}"))),
    }
}

#[cfg(test)]
/// A saved connection `name` to the file database at `path`.
#[cfg(feature = "sqlite")]
pub(crate) fn file_profile(name: &str, path: &std::path::Path) -> ConnectionProfile {
    ConnectionProfile {
        unknown: Unknown::default(),
        name: name.into(),
        engine: crate::db::tests::file_engine(),
        host: path.to_string_lossy().into_owned(),
        port: String::new(),
        user: String::new(),
        database: String::new(),
        ssl_mode: crate::db::SslMode::Disable,
        tls: crate::db::TlsFiles::default(),
        mode: crate::db::ConnectionMode::Normal,
        transaction: None,
        ssh: String::new(),
        timeout: String::new(),
        password_command: Vec::new(),
        agents: crate::db::AgentAccess::None,
        legacy_keychain: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile(name: &str) -> ConnectionProfile {
        ConnectionProfile {
            unknown: Unknown::default(),
            name: name.to_string(),
            engine: Engine::Postgres,
            host: "localhost".to_string(),
            port: "5432".to_string(),
            user: "me".to_string(),
            database: "db".to_string(),
            ssl_mode: SslMode::Prefer,
            tls: crate::db::TlsFiles::default(),
            mode: ConnectionMode::Normal,
            transaction: None,
            ssh: String::new(),
            timeout: String::new(),
            agents: AgentAccess::None,
            password_command: Vec::new(),
            legacy_keychain: false,
        }
    }

    #[test]
    fn connections_to_one_database_share_its_key() {
        let on = |name: &str, host: &str, database: &str| ConnectionProfile {
            host: host.into(),
            database: database.into(),
            ..profile(name)
        };
        // dev and prod of `Shop`: one database, whatever the host.
        assert_eq!(on("dev", "localhost", "Shop").database_key(), "shop");
        assert_eq!(on("prod", "db.example.com", "shop").database_key(), "shop");
        // No database named: the connection's own name.
        assert_eq!(on("Warehouse", "dw", " ").database_key(), "warehouse");
        // A file: its name, without folder or extension.
        let file = ConnectionProfile { engine: Engine::Sqlite, ..on("local", "/data/Shop.db", "") };
        assert_eq!(file.database_key(), "shop");
        let memory = ConnectionProfile { engine: Engine::Sqlite, ..on("scratch", ":memory:", "") };
        assert_eq!(memory.database_key(), "scratch");
    }

    fn temp_store() -> (ConnectionStore, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let (store, warning) = ConnectionStore::load_from(dir.path().join("c.toml"));
        assert!(warning.is_none());
        (store, dir)
    }

    #[test]
    fn a_project_that_cant_be_read_is_kept_and_the_others_load() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("c.toml");
        let text = "last_project = \"../x\"\n\n[[projects]]\nid = \"good\"\nname = \"kept\"\n\n\
                    [[projects]]\nid = \"../x\"\nname = \"odd\"\nfuture = 1\n\n\
                    [[projects]]\nid = \"last\"\nname = \"third\"\n";
        fs::write(&path, text).unwrap();
        let (mut store, warning) = ConnectionStore::load_from(path.clone());
        assert!(warning.as_deref().is_some_and(|w| w.contains("odd")), "{warning:?}");
        assert_eq!(store.projects.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), ["kept", "third"]);
        assert_eq!(store.last_project, None, "not a project's id: dropped");
        assert!(!path.with_extension("toml.bak").exists() && path.exists(), "the file isn't moved aside");
        // Its name stays its own.
        assert!(store.add_project("ODD").is_err());
        // Saving writes it back as it was, where it was.
        store.add_project("new").unwrap();
        let saved = fs::read_to_string(&path).unwrap();
        assert!(saved.contains("id = \"../x\"") && saved.contains("future = 1"), "{saved}");
        let order: Vec<usize> =
            ["\"kept\"", "\"odd\"", "\"third\"", "\"new\""].iter().map(|n| saved.find(n).unwrap()).collect();
        assert!(order.is_sorted(), "{saved}");
        // Deleting one before it: it stays between the same two.
        let first = store.projects[0].id.clone();
        store.delete_project(&first).unwrap();
        let saved = fs::read_to_string(&path).unwrap();
        let order: Vec<usize> = ["\"odd\"", "\"third\"", "\"new\""].iter().map(|n| saved.find(n).unwrap()).collect();
        assert!(order.is_sorted(), "{saved}");
        let (again, _) = ConnectionStore::load_from(path);
        assert_eq!(again.projects.len(), 2);
    }

    #[test]
    fn a_project_id_that_would_lead_out_of_its_folders_isnt_taken() {
        for bad in ["", ".", "..", "../x", ".x", "a/b", "a\\b", "a\0b"] {
            assert!(ProjectId::try_from(bad.to_string()).is_err(), "{bad:?}");
            assert!(toml::from_str::<Project>(&format!("id = {bad:?}\nname = \"p\"\n")).is_err(), "{bad:?}");
        }
        let id = ProjectId::new();
        assert_eq!(ProjectId::from_written(id.as_str()), Some(id));
        assert!(ProjectId::from_written("podelis-test-1 (2)").is_some());
    }

    fn names(store: &ConnectionStore, project: &ProjectId) -> Vec<String> {
        store.project(project).unwrap().connections.iter().map(|c| c.name.clone()).collect()
    }

    /// Opt-in (`PODELIS_TEST_KEYCHAIN=1`): the OS keychain stores, finds
    /// and forgets a connection's password under its real account name
    /// (project and connection, with what names may hold), as connecting
    /// reads it. (Moving one with `m` is the app's test
    /// `moving_a_connection_moves_its_password`.) Its entry goes however
    /// the test ends: it'd sit beside the user's own.
    #[test]
    fn live_keychain_accounts_of_connections() {
        if std::env::var("PODELIS_TEST_KEYCHAIN").as_deref() != Ok("1") {
            return;
        }
        struct Forget(String);
        impl Drop for Forget {
            fn drop(&mut self) {
                let _ = crate::db::keychain::delete_password(&self.0);
            }
        }
        let p = profile("shop db / ąčę: \"prod\"");
        let project = ProjectId::from_written(format!("podelis-test-{} (2)", uuid::Uuid::new_v4())).unwrap();
        let entry = Forget(keychain_account(&project, &p));
        crate::db::keychain::store_password(&entry.0, "s3cret").unwrap();
        assert_eq!(saved_password(&project, &p), ("s3cret".to_string(), None));
        crate::db::keychain::store_password(&entry.0, "").unwrap();
        assert_eq!(saved_password(&project, &p).0, "");
    }

    #[test]
    fn settings_a_newer_version_wrote_are_kept() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("connections.toml");
        let id = uuid::Uuid::new_v4().to_string();
        let text = format!(
            "last_project = \"{id}\"\nfuture_top = 1\n\n[[projects]]\nid = \"{id}\"\nname = \"p\"\nshare_schema = \"all\"\n\
             future_project = [\"a\", \"b\"]\n\n[[projects.connections]]\nname = \"db\"\nengine = \"Sqlite\"\nhost = \"x.db\"\n\
             port = \"\"\nuser = \"\"\ndatabase = \"\"\nfuture_connection = {{ k = true }}\n"
        );
        fs::write(&path, text).unwrap();
        let (mut store, warning) = ConnectionStore::load_from(path.clone());
        assert!(warning.is_none(), "{warning:?}");
        // Edited here, as the connection form does (it knows nothing of them).
        let mut edited = store.projects[0].connections[0].clone();
        edited.unknown = Unknown::default();
        edited.database = "main".into();
        store.upsert(&ProjectId::from_written(id.clone()).unwrap(), Some("db"), edited).unwrap();
        let saved = fs::read_to_string(&path).unwrap();
        for kept in [
            "future_top = 1",
            "future_project = [",
            "future_connection",
            "share_schema = \"all\"",
            "database = \"main\"",
        ] {
            assert!(saved.contains(kept), "{kept} in:\n{saved}");
        }
        // A setting this version knows, set back to its default, stays gone.
        store.set_share_schema(&ProjectId::from_written(id).unwrap(), ShareSchema::Local).unwrap();
        assert!(!fs::read_to_string(&path).unwrap().contains("share_schema"));
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
        assert_eq!(reloaded.last_project.as_ref(), Some(&id));
    }

    #[test]
    fn a_config_folder_open_to_others_is_made_the_users_alone() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("podelis");
        fs::create_dir(&folder).unwrap();
        fs::set_permissions(&folder, fs::Permissions::from_mode(0o755)).unwrap();
        secure_dir(&folder);
        assert_eq!(fs::metadata(&folder).unwrap().permissions().mode() & 0o777, 0o700);
        // A symlink to a folder elsewhere: that folder is left as it is.
        let elsewhere = dir.path().join("dotfiles");
        fs::create_dir(&elsewhere).unwrap();
        fs::set_permissions(&elsewhere, fs::Permissions::from_mode(0o755)).unwrap();
        let link = dir.path().join("linked");
        std::os::unix::fs::symlink(&elsewhere, &link).unwrap();
        secure_dir(&link);
        assert_eq!(fs::metadata(&elsewhere).unwrap().permissions().mode() & 0o777, 0o755);
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
        crate::db::keychain::store_password(&project_account(&id, "a"), "stale").unwrap();
        assert_eq!(saved_password(&id, &with_command), (String::new(), None));
        crate::db::keychain::store_password(&project_account(&id, "b"), "s3cret").unwrap();
        assert_eq!(saved_password(&id, &profile("b")), ("s3cret".to_string(), None));
        crate::db::keychain::delete_password(&project_account(&id, "a")).unwrap();
        crate::db::keychain::delete_password(&project_account(&id, "b")).unwrap();
    }

    #[test]
    fn invalid_file_is_backed_up_not_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("connections.toml");
        fs::write(&path, "this is [not toml").unwrap();
        let (mut store, warning) = ConnectionStore::load_from(path);
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
