//! The project's memory in the app (`Ctrl+K y k`): what models are told
//! about its data. Facts a model suggested wait (orange) until the user
//! confirms them (`c`); `a` adds one, `e` changes one, `d` forgets one. The
//! gateway keeps them, so every change goes through it.
//!
//! On a tab with a saved connection it's that database's **ontology**: its
//! tables (each with its columns that have facts), its terms, a model's
//! suggestions and the stale facts, a header line each with their facts
//! under it. `a` on a line starts a fact about it, `p` moves a fact between
//! this database and every database in the project, and `E` opens it all
//! in the user's editor as YAML, then says what loading it back changes
//! before anything is changed.

use crate::config::store::ProjectId;
use std::path::PathBuf;

use serde_json::Value;

use crate::app::query_screen::ask::AskFailure;
use crate::app::*;
use crate::gateway::client::{CallError, connect_or_start, spawn_gateway};
use crate::gateway::methods::{self as m, Method, Scoped};
use podelis_ai::memory::Fact;
use podelis_ai::ontology::outline::SectionKind;

/// The memory window.
pub struct MemoryView {
    /// The project it shows.
    pub project_id: ProjectId,
    /// As the gateway listed them, oldest first; in an ontology, a header
    /// before each section's facts.
    pub lines: Vec<MemoryLine>,
    pub selected: usize,
    pub loading: bool,
    /// `a`: the fact being typed, `subject what…`; or, with `editing`, the
    /// one being changed (`e`).
    pub adding: Option<LineInput>,
    /// The id of the fact `adding` changes (`e`), not a new one.
    pub editing: Option<podelis_ai::memory::FactId>,
    /// It shows a database's ontology (that database's key), not the
    /// project's facts in a list.
    pub database: Option<String>,
    /// The edits read back from the editor, waiting for `y`: what they'd
    /// change (`add`, `confirm`, `move`, `forget`), and the file.
    pub plan: Option<(m::ImportPlan, PathBuf)>,
    /// How far the plan is scrolled, and whether its last line has been
    /// in sight (`y` waits for that: nothing is changed unseen).
    pub plan_scroll: usize,
    /// How many of the plan's lines the window last had room for (it's
    /// drawn after the keys are handled).
    pub plan_shown: std::cell::Cell<usize>,
    /// Edits that didn't load (why, and the file they're kept in), for `E`
    /// to open again.
    pub failed_edit: Option<(String, PathBuf)>,
}

/// A window for a made-up project, for tests to fill in.
#[cfg(test)]
impl Default for MemoryView {
    fn default() -> Self {
        Self::new("p".into())
    }
}

/// A line of the memory window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemoryLine {
    /// An ontology's table, column or term (as a fact about it is
    /// written), or its suggestions or stale facts (no subject).
    Header { kind: SectionKind, subject: String },
    /// A fact, and the kind of section it's in (`None`: the project's list).
    Fact { fact: Fact, section: Option<SectionKind> },
}

impl MemoryLine {
    pub fn fact(&self) -> Option<&Fact> {
        match self {
            MemoryLine::Fact { fact, .. } => Some(fact),
            MemoryLine::Header { .. } => None,
        }
    }
}

impl MemoryView {
    /// The window for project `project_id`, nothing in it yet.
    pub fn new(project_id: ProjectId) -> Self {
        Self {
            project_id,
            lines: Vec::new(),
            selected: 0,
            loading: false,
            adding: None,
            editing: None,
            database: None,
            plan: None,
            plan_scroll: 0,
            plan_shown: std::cell::Cell::new(0),
            failed_edit: None,
        }
    }

    /// Its facts, without the headers.
    #[cfg(test)]
    pub fn facts(&self) -> Vec<&Fact> {
        self.lines.iter().filter_map(MemoryLine::fact).collect()
    }

    /// The selected line's fact, if it's a fact (not a header).
    fn fact(&self) -> Option<&Fact> {
        self.lines.get(self.selected).and_then(MemoryLine::fact)
    }

    /// What a fact added on the selected line is about: its header's, or
    /// its fact's, subject.
    fn subject(&self) -> Option<String> {
        let subject = match self.lines.get(self.selected)? {
            MemoryLine::Header { subject, .. } => subject,
            MemoryLine::Fact { fact, .. } => &fact.subject,
        };
        Some(subject.clone()).filter(|s| !s.is_empty())
    }
}

/// What a change to a fact did, for the status bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    Confirmed,
    Forgot,
    Changed,
    Remembered,
    /// Moved to every database of the project, or to this one.
    Moved {
        every: bool,
    },
}

impl Change {
    fn said(self, fact: &str) -> String {
        match self {
            Change::Confirmed => t!("memory.did_confirm", fact = fact),
            Change::Forgot => t!("memory.did_forget", fact = fact),
            Change::Changed => t!("memory.did_change", fact = fact),
            Change::Remembered => t!("memory.did_remember", fact = fact),
            Change::Moved { every: true } => t!("memory.did_move_every", fact = fact),
            Change::Moved { every: false } => t!("memory.did_move_this", fact = fact),
        }
    }
}

/// What a gateway call from the app was for, and the project it was
/// about: an answer for another one (after switching) is dropped.
#[derive(Debug)]
pub enum GatewayPurpose {
    /// The facts, for the memory window.
    MemoryList(ProjectId),
    /// A change, said in the status bar with the fact (as the window
    /// shows it, where the answer doesn't), then the list again.
    MemoryChanged(Change, ProjectId, Option<String>),
    /// The database's ontology, for the window.
    OntologyList(ProjectId),
    /// The ontology as YAML, to open in the editor.
    OntologyEdit(ProjectId),
    /// What the edited file (the path) would change.
    OntologyPlan(ProjectId, PathBuf),
    /// The edited file loaded (the path, removed then).
    OntologyApplied(ProjectId, PathBuf),
}

impl App {
    /// `Ctrl+K y k`: the project's memory; on a saved connection, its
    /// database's ontology.
    pub(in crate::app) fn open_memory(&mut self) {
        let Some(project_id) = self.current_project.clone() else {
            return self.set_status(t!("memory.need_project").into(), true);
        };
        let database = self.memory_database();
        self.popups.memory_view = Some(MemoryView { loading: true, database, ..MemoryView::new(project_id.clone()) });
        self.load_memory(project_id);
    }

    /// The tab's saved connection, whose database the memory window is
    /// about; none when the tab isn't on one (then: the whole project).
    pub(crate) fn memory_connection(&self) -> Option<String> {
        let name = self.connection.form.name.trim();
        (!name.is_empty() && self.profiles().iter().any(|p| p.name.eq_ignore_ascii_case(name)))
            .then(|| name.to_string())
    }

    /// The database the memory window is about, as the memory names it.
    pub(crate) fn memory_database(&self) -> Option<String> {
        let name = self.memory_connection()?;
        self.profiles()
            .iter()
            .find(|p| p.name.eq_ignore_ascii_case(&name))
            .map(crate::config::store::ConnectionProfile::database_key)
    }

    pub(super) fn load_memory(&mut self, project_id: ProjectId) {
        if self.popups.memory_view.as_ref().is_some_and(|v| v.database.is_some()) {
            // The schema as the gateway has it (read again after a while).
            let params =
                m::OntologyParams { format: Some("outline".into()), refresh: Some(false), ..Default::default() };
            self.gateway_call::<m::Ontology>(params, GatewayPurpose::OntologyList(project_id));
        } else {
            self.gateway_call::<m::MemoryList>(m::MemoryListParams::default(), GatewayPurpose::MemoryList(project_id));
        }
    }

    /// Calls the gateway (started if it isn't running) for the open
    /// project in the background; the answer comes as `GatewayDone`.
    pub(super) fn gateway_call<M: Method>(&mut self, mut params: M::Params, purpose: GatewayPurpose)
    where
        M::Params: Scoped,
    {
        let scope = params.scope_mut();
        // By id: two projects' names may differ only in case.
        if let Some(id) = &self.current_project {
            scope.project = Some(format!("id:{id}"));
        }
        // About the tab's database, when it's on a saved connection: what's
        // added is about that database, and its facts (and those about every
        // database) are listed.
        scope.connection = self.memory_connection();
        // A relative SQLite path is from where the app runs.
        scope.dir = std::env::current_dir().ok().map(|dir| dir.display().to_string());
        // The answer is read as `M`'s when it comes (`gateway_done`).
        let params = serde_json::to_value(&params).unwrap_or_default();
        let config_dir = self.data_dir.clone().unwrap_or_else(crate::config::store::config_dir);
        let send = self.sender();
        tokio::spawn(async move {
            let result = match connect_or_start(&config_dir, "podelis", || spawn_gateway(&config_dir)).await {
                Ok(mut client) => client.call(M::NAME, params).await,
                Err(e) => Err(e),
            };
            let result = result.map_err(|e| match e {
                CallError::Rpc(e) => AskFailure { code: Some(e.code), message: e.message, data: e.data },
                CallError::Io(e) => {
                    AskFailure { code: None, message: t!("ask.gateway_unreachable", e = e), data: None }
                }
            });
            send(Background::GatewayDone(purpose, Box::new(result)).into());
        });
    }

    pub(in crate::app) fn gateway_done(&mut self, purpose: GatewayPurpose, result: Result<Value, AskFailure>) {
        let project = match &purpose {
            GatewayPurpose::MemoryList(p)
            | GatewayPurpose::MemoryChanged(_, p, _)
            | GatewayPurpose::OntologyList(p)
            | GatewayPurpose::OntologyEdit(p)
            | GatewayPurpose::OntologyPlan(p, _)
            | GatewayPurpose::OntologyApplied(p, _) => p.clone(),
        };
        // For a project left since: not this one's.
        if self.current_project.as_ref() != Some(&project) {
            return;
        }
        let same_window = self.popups.memory_view.as_ref().is_some_and(|v| v.project_id == project);
        let result = result.and_then(|answer| Answer::read(&purpose, answer));
        match (purpose, result) {
            (GatewayPurpose::MemoryList(_), Ok(Answer::Facts(facts))) => {
                if let Some(view) = self.popups.memory_view.as_mut().filter(|_| same_window) {
                    view.lines = facts.into_iter().map(|fact| MemoryLine::Fact { fact, section: None }).collect();
                    view.selected = view.selected.min(view.lines.len().saturating_sub(1));
                    view.loading = false;
                }
            }
            (GatewayPurpose::OntologyList(_), Ok(Answer::Ontology(built))) => {
                if let Some(view) = self.popups.memory_view.as_mut().filter(|_| same_window) {
                    view.lines = lines_of(built.outline.unwrap_or_default());
                    view.selected = view.selected.min(view.lines.len().saturating_sub(1));
                    view.loading = false;
                }
            }
            (GatewayPurpose::MemoryChanged(change, project, shown), Ok(Answer::Changed(what))) => {
                self.set_status(change.said(&shown.unwrap_or(what)), false);
                if same_window {
                    self.load_memory(project);
                }
            }
            (GatewayPurpose::OntologyEdit(project), Ok(Answer::Ontology(built))) => {
                self.edit_in_editor(
                    project,
                    built.yaml.as_deref().unwrap_or_default(),
                    Some(built.database.as_str()).filter(|d| !d.is_empty()),
                );
            }
            (GatewayPurpose::OntologyPlan(_, file), Ok(_)) if !same_window => {
                let _ = std::fs::remove_file(&file);
            }
            (GatewayPurpose::OntologyPlan(_, file), Ok(Answer::Plan(plan))) => {
                if plan.is_empty() {
                    let _ = std::fs::remove_file(&file);
                    return self.set_status(t!("memory.plan_nothing").into(), false);
                }
                if let Some(view) = self.popups.memory_view.as_mut().filter(|_| same_window) {
                    view.plan = Some((plan, file));
                    view.plan_scroll = 0;
                }
            }
            (GatewayPurpose::OntologyApplied(project, file), Ok(Answer::Applied(done))) => {
                let _ = std::fs::remove_file(&file);
                let said = t!(
                    "memory.applied",
                    added = done.added,
                    confirmed = done.confirmed,
                    moved = done.moved,
                    forgotten = done.forgotten
                );
                self.set_status(said, false);
                if same_window {
                    self.load_memory(project);
                }
            }
            // The edits didn't load: kept, to open again or throw away.
            (GatewayPurpose::OntologyPlan(_, file) | GatewayPurpose::OntologyApplied(_, file), Err(e)) => {
                self.edit_failed(e.message, file);
            }
            (_, Err(e)) => {
                if let Some(view) = &mut self.popups.memory_view {
                    view.loading = false;
                }
                self.set_status(t!("memory.failed", e = e.message), true);
            }
            // `Answer::read` gives each purpose its own kind of answer, or
            // an error: none other comes here.
            (_, Ok(_)) => self.set_status(t!("memory.failed", e = "?"), true),
        }
    }

    pub(in crate::app) fn handle_memory_key(&mut self, key: KeyEvent) {
        let Some(view) = &mut self.popups.memory_view else { return };
        if view.failed_edit.is_some() {
            return self.handle_failed_edit_key(key);
        }
        if view.plan.is_some() {
            return self.handle_edits_plan_key(key);
        }
        if let Some(input) = &mut view.adding {
            match input.handle(key) {
                InputResult::Cancel => {
                    view.adding = None;
                    view.editing = None;
                }
                InputResult::Submit => {
                    // Not a fact (no value): said, and what's typed stays.
                    if podelis_ai::memory::parse_fact(input.text()).is_none() {
                        return self.set_status(t!("memory.add_hint").into(), true);
                    }
                    let text = view.adding.take().map(|i| i.text().trim().to_string()).unwrap_or_default();
                    let project = view.project_id.clone();
                    return match view.editing.take() {
                        Some(id) => self.update_fact(id, &text, project),
                        None => self.add_fact(&text, project),
                    };
                }
                InputResult::Changed | InputResult::Moved | InputResult::Ignored => {}
            }
            return;
        }
        if let Some(i) = list::step(view.selected, view.lines.len(), key, Nav::LIST) {
            view.selected = i;
            return;
        }
        let fact = view.fact().cloned();
        let id = fact.as_ref().map(|f| f.id);
        let shown = fact.as_ref().map(|f| shortened(f.line()));
        let project = view.project_id.clone();
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => self.popups.memory_view = None,
            // About the selected table, column or term, typed for you.
            KeyCode::Char('a') => {
                let start =
                    view.subject().filter(|_| view.database.is_some()).map(|s| format!("{s} ")).unwrap_or_default();
                view.adding = Some(LineInput::new(start));
                view.editing = None;
            }
            // The whole fact to change, as typed when adding it.
            KeyCode::Char('e') | KeyCode::Enter if id.is_some() => {
                let text = fact.as_ref().map(Fact::line).unwrap_or_default();
                view.adding = Some(LineInput::new(text));
                view.editing = id;
            }
            KeyCode::Char('c') if id.is_some() => {
                self.gateway_call::<m::MemoryConfirm>(
                    m::FactParams { id, ..Default::default() },
                    GatewayPurpose::MemoryChanged(Change::Confirmed, project, shown),
                );
            }
            KeyCode::Char('d') if id.is_some() => {
                self.gateway_call::<m::MemoryForget>(
                    m::FactParams { id, ..Default::default() },
                    GatewayPurpose::MemoryChanged(Change::Forgot, project, shown),
                );
            }
            // Between this database and every database in the project.
            KeyCode::Char('p') if id.is_some() && view.database.is_some() => {
                let every = fact.as_ref().is_some_and(|f| !f.database.is_empty());
                self.gateway_call::<m::MemoryMove>(
                    m::MemoryMoveParams { id, every, ..Default::default() },
                    GatewayPurpose::MemoryChanged(Change::Moved { every }, project, shown),
                );
            }
            // All of it in the editor, as YAML.
            KeyCode::Char('E') if view.database.is_some() => {
                let params =
                    m::OntologyParams { format: Some("yaml".into()), refresh: Some(false), ..Default::default() };
                self.gateway_call::<m::Ontology>(params, GatewayPurpose::OntologyEdit(project));
            }
            _ => {}
        }
    }

    /// `e` in the memory window: fact `id` now says `text` (read as when
    /// adding); confirmed, being the user's word now.
    fn update_fact(&mut self, id: podelis_ai::memory::FactId, text: &str, project: ProjectId) {
        let params = m::MemoryUpdateParams { id: Some(id), text: Some(text.to_string()), ..Default::default() };
        let purpose = GatewayPurpose::MemoryChanged(Change::Changed, project, None);
        self.gateway_call::<m::MemoryUpdate>(params, purpose);
    }

    /// `a` in the memory window: `subject what…`, as `podelis memory add`.
    fn add_fact(&mut self, text: &str, project: ProjectId) {
        let params = m::MemoryAddParams { text: Some(text.to_string()), ..Default::default() };
        let purpose = GatewayPurpose::MemoryChanged(Change::Remembered, project, None);
        self.gateway_call::<m::MemoryAdd>(params, purpose);
    }
}

/// The ontology's outline (as the gateway sends it) as the window's lines:
/// a header for each table, column, term or group, then its facts, each
/// with the subject a fact added there is about.
fn lines_of(outline: Vec<m::OutlineSection>) -> Vec<MemoryLine> {
    let mut lines = Vec::new();
    for section in outline {
        let Some(kind) = SectionKind::parse(&section.kind) else { continue };
        lines.push(MemoryLine::Header { kind, subject: section.subject });
        lines.extend(section.facts.into_iter().map(|fact| MemoryLine::Fact { fact, section: Some(kind) }));
    }
    lines
}

/// An answer to `M` this version couldn't read, and why.
fn not_understood<M: Method>(why: &str) -> AskFailure {
    AskFailure {
        code: None,
        message: format!("the gateway's answer to {} couldn't be read: {why}", M::NAME),
        data: None,
    }
}

/// A gateway answer, read as what its purpose asked for.
enum Answer {
    Facts(Vec<Fact>),
    Ontology(m::OntologyResult),
    /// A fact changed, as the status bar says it (`#id` when the answer
    /// is only its id).
    Changed(String),
    Plan(m::ImportPlan),
    Applied(m::ImportApplied),
}

impl Answer {
    fn read(purpose: &GatewayPurpose, answer: Value) -> Result<Self, AskFailure> {
        fn read<M: Method>(answer: Value) -> Result<M::Result, AskFailure> {
            serde_json::from_value(answer).map_err(|e| not_understood::<M>(&e.to_string()))
        }
        Ok(match purpose {
            GatewayPurpose::MemoryList(_) => Answer::Facts(read::<m::MemoryList>(answer)?.facts),
            GatewayPurpose::OntologyList(_) | GatewayPurpose::OntologyEdit(_) => {
                Answer::Ontology(read::<m::Ontology>(answer)?)
            }
            // Added or changed: the fact; confirmed, forgotten or moved: its id.
            GatewayPurpose::MemoryChanged(..) => match serde_json::from_value::<Fact>(answer.clone()) {
                Ok(fact) => Answer::Changed(shortened(fact.line())),
                Err(_) => Answer::Changed(format!("#{}", read::<m::MemoryForget>(answer)?.id)),
            },
            // What it would change when that was asked, what it changed
            // when it was told to: the other is an answer not understood.
            GatewayPurpose::OntologyPlan(..) => match read::<m::OntologyImport>(answer)? {
                m::ImportResult::Plan(plan) => Answer::Plan(plan),
                m::ImportResult::Applied(_) => return Err(not_understood::<m::OntologyImport>("changes, not a plan")),
            },
            GatewayPurpose::OntologyApplied(..) => match read::<m::OntologyImport>(answer)? {
                m::ImportResult::Applied(done) => Answer::Applied(done),
                m::ImportResult::Plan(_) => {
                    return Err(not_understood::<m::OntologyImport>("a plan, not what changed"));
                }
            },
        })
    }
}

/// `subject relation value`, shortened for the status bar.
fn shortened(text: String) -> String {
    if text.chars().count() > 60 { text.chars().take(59).collect::<String>() + "…" } else { text }
}
