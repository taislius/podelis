//! The project's memory in the app (`Ctrl+K k`): what models are told
//! about its data. Facts a model suggested wait (orange) until the user
//! confirms them (`c`); `a` adds one, `d` forgets one. The gateway keeps
//! them, so every change goes through it.

use serde_json::{Value, json};

use super::ask::AskFailure;
use super::*;
use crate::gateway::client::{CallError, connect_or_start, spawn_gateway};

/// The memory window.
#[derive(Default)]
pub struct MemoryView {
    /// The project it shows.
    pub project_id: String,
    /// As the gateway listed them, oldest first.
    pub facts: Vec<Value>,
    pub selected: usize,
    pub loading: bool,
    /// `a`: the fact being typed, `subject what…`.
    pub adding: Option<LineInput>,
}

/// What a gateway call from the app was for, and the project it was
/// about: an answer for another one (after switching) is dropped.
#[derive(Debug)]
pub enum GatewayPurpose {
    /// The facts, for the memory window.
    MemoryList(String),
    /// A change, said in the status bar with the fact (as the window
    /// shows it, where the answer doesn't), then the list again.
    MemoryChanged(&'static str, String, Option<String>),
}

impl App {
    /// `Ctrl+K k`: the project's memory.
    pub(super) fn open_memory(&mut self) {
        let Some(project_id) = self.current_project.clone() else {
            return self.set_status("Open a project first: memory is a project's".into(), true);
        };
        self.memory_view = Some(MemoryView { project_id: project_id.clone(), loading: true, ..MemoryView::default() });
        self.load_memory(project_id);
    }

    fn load_memory(&mut self, project_id: String) {
        self.gateway_call("memory.list", json!({}), GatewayPurpose::MemoryList(project_id));
    }

    /// Calls the gateway (started if it isn't running) for the open
    /// project in the background; the answer comes as `GatewayDone`.
    fn gateway_call(&mut self, method: &'static str, mut params: Value, purpose: GatewayPurpose) {
        // By id: two projects' names may differ only in case.
        if let Some(id) = &self.current_project {
            params["project"] = json!(format!("id:{id}"));
        }
        let config_dir = self.data_dir.clone().unwrap_or_else(crate::config::store::config_dir);
        let send = self.sender();
        tokio::spawn(async move {
            let result = match connect_or_start(&config_dir, "podelis", || spawn_gateway(&config_dir)).await {
                Ok(mut client) => client.call(method, params).await,
                Err(e) => Err(e),
            };
            let result = result.map_err(|e| match e {
                CallError::Rpc(e) => AskFailure { code: Some(e.code), message: e.message, data: e.data },
                CallError::Io(e) => {
                    AskFailure { code: None, message: format!("the gateway couldn't be reached: {e}"), data: None }
                }
            });
            send(DbEventKind::GatewayDone(purpose, Box::new(result)));
        });
    }

    pub(super) fn gateway_done(&mut self, purpose: GatewayPurpose, result: Result<Value, AskFailure>) {
        let project = match &purpose {
            GatewayPurpose::MemoryList(p) | GatewayPurpose::MemoryChanged(_, p, _) => p.clone(),
        };
        // For a project left since: not this one's.
        if self.current_project.as_deref() != Some(project.as_str()) {
            return;
        }
        let same_window = self.memory_view.as_ref().is_some_and(|v| v.project_id == project);
        match (purpose, result) {
            (GatewayPurpose::MemoryList(_), Ok(listed)) => {
                if let Some(view) = self.memory_view.as_mut().filter(|_| same_window) {
                    view.facts = listed["facts"].as_array().cloned().unwrap_or_default();
                    view.selected = view.selected.min(view.facts.len().saturating_sub(1));
                    view.loading = false;
                }
            }
            (GatewayPurpose::MemoryChanged(said, project, shown), Ok(fact)) => {
                let what = shown.or_else(|| fact_text(&fact)).unwrap_or_else(|| format!("#{}", fact["id"]));
                self.set_status(format!("{said} {what}"), false);
                if same_window {
                    self.load_memory(project);
                }
            }
            (_, Err(e)) => {
                if let Some(view) = &mut self.memory_view {
                    view.loading = false;
                }
                self.set_status(format!("Memory: {}", e.message), true);
            }
        }
    }

    pub(super) fn handle_memory_key(&mut self, key: KeyEvent) {
        let Some(view) = &mut self.memory_view else { return };
        if let Some(input) = &mut view.adding {
            match input.handle(key) {
                InputResult::Cancel => view.adding = None,
                InputResult::Submit => {
                    let text = view.adding.take().map(|i| i.text().trim().to_string()).unwrap_or_default();
                    let project = view.project_id.clone();
                    return self.add_fact(&text, project);
                }
                InputResult::Changed | InputResult::Moved | InputResult::Ignored => {}
            }
            return;
        }
        if let Some(i) = list::step(view.selected, view.facts.len(), key, Nav::LIST) {
            view.selected = i;
            return;
        }
        let id = view.facts.get(view.selected).and_then(|f| f["id"].as_i64());
        let shown = view.facts.get(view.selected).and_then(fact_text);
        let project = view.project_id.clone();
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => self.memory_view = None,
            KeyCode::Char('a') => view.adding = Some(LineInput::new("")),
            KeyCode::Char('c') if id.is_some() => {
                self.gateway_call(
                    "memory.confirm",
                    json!({ "id": id }),
                    GatewayPurpose::MemoryChanged("Confirmed", project.clone(), shown),
                );
            }
            KeyCode::Char('d') if id.is_some() => {
                self.gateway_call(
                    "memory.forget",
                    json!({ "id": id }),
                    GatewayPurpose::MemoryChanged("Forgot", project.clone(), shown),
                );
            }
            _ => {}
        }
    }

    /// `a` in the memory window: `subject what…`, as `podelis memory add`.
    fn add_fact(&mut self, text: &str, project: String) {
        if crate::gateway::memory::parse_fact(text).is_none() {
            return self.set_status("Type what it's about, then what's said: orders.status 3 = cancelled".into(), true);
        }
        let params = json!({ "text": text });
        self.gateway_call("memory.add", params, GatewayPurpose::MemoryChanged("Remembered", project, None));
    }
}

/// A fact as the window shows it, `subject relation value`, shortened.
fn fact_text(fact: &Value) -> Option<String> {
    let part = |k: &str| fact[k].as_str().map(str::to_string);
    let text = format!("{} {} {}", part("subject")?, part("relation")?, part("value")?);
    Some(if text.chars().count() > 60 { text.chars().take(59).collect::<String>() + "…" } else { text })
}
