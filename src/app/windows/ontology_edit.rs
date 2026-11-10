//! The ontology edited in the user's editor (`E` in the memory window):
//! written as YAML to a file of the user's alone, opened with the terminal
//! handed over, read back, and what loading it would change shown (to
//! scroll through) before `y` changes anything. Edits that don't load are
//! kept, to open again.

use std::path::PathBuf;

use serde_json::{Value, json};

use super::memory::GatewayPurpose;
use crate::app::query_screen::pipes::{AfterExternal, External, shell_quote};
use crate::app::*;

impl App {
    /// Keys while edits that didn't load are shown.
    pub(super) fn handle_failed_edit_key(&mut self, key: KeyEvent) {
        let Some(view) = &mut self.popups.memory_view else { return };
        let Some((_, file)) = &view.failed_edit else { return };
        let (project, file) = (view.project_id.clone(), file.clone());
        match key.code {
            KeyCode::Char('E' | 'e') => {
                view.failed_edit = None;
                self.open_editor(project, file);
            }
            KeyCode::Esc | KeyCode::Char('n') => {
                view.failed_edit = None;
                let _ = std::fs::remove_file(&file);
            }
            _ => {}
        }
    }

    /// Keys while the plan of loading edits is shown: it scrolls, and `y`
    /// waits until every change has been in sight.
    pub(super) fn handle_edits_plan_key(&mut self, key: KeyEvent) {
        let Some(view) = &mut self.popups.memory_view else { return };
        let Some((_, file)) = &view.plan else { return };
        let (project, file) = (view.project_id.clone(), file.clone());
        let step = match key.code {
            KeyCode::Down | KeyCode::Char('j') => Some(1_isize),
            KeyCode::Up | KeyCode::Char('k') => Some(-1),
            KeyCode::PageDown | KeyCode::Char(' ') => Some(10),
            KeyCode::PageUp => Some(-10),
            _ => None,
        };
        if let Some(step) = step {
            // No further than the last line in sight: ↑ moves at once after.
            let last = view.plan.as_ref().map_or(0, |(p, _)| plan_len(p)).saturating_sub(view.plan_shown.get());
            view.plan_scroll = view.plan_scroll.saturating_add_signed(step).min(last);
            return;
        }
        match key.code {
            // Not before every change has been in sight.
            KeyCode::Char('y')
                if view.plan_scroll + view.plan_shown.get() < view.plan.as_ref().map_or(0, |(p, _)| plan_len(p)) =>
            {
                self.set_status(t!("memory.plan_scroll").into(), true);
            }
            KeyCode::Char('y') => {
                view.plan = None;
                self.load_edits(project, file, true);
            }
            // Back to the editor, the file as it was left.
            KeyCode::Char('E') => {
                view.plan = None;
                self.open_editor(project, file);
            }
            KeyCode::Esc | KeyCode::Char('n') => {
                view.plan = None;
                let _ = std::fs::remove_file(&file);
            }
            _ => {}
        }
    }

    /// `E`: the ontology written to a file of the user's alone, then opened
    /// in their editor with the terminal handed over.
    pub(super) fn edit_in_editor(&mut self, project: String, yaml: &str, database: Option<&str>) {
        // A folder of their own: `opened/` is emptied of day-old files,
        // and edits kept after a failed load must stay until dealt with.
        let config_dir = self.data_dir.clone().unwrap_or_else(crate::config::store::config_dir);
        let dir = config_dir.join("ontology-edits");
        let name = database.unwrap_or("ontology").replace(['/', '\\', ' '], "_");
        let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
        let file = dir.join(format!("ontology-{name}-{stamp}.yaml"));
        let written = std::fs::create_dir_all(&dir).map_err(anyhow::Error::from).and_then(|()| {
            use std::os::unix::fs::PermissionsExt as _;
            let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
            crate::config::store::write_atomic(&file, yaml)
        });
        match written {
            Ok(()) => self.open_editor(project, file),
            Err(e) => self.set_status(t!("pipes.write_failed", e = format!("{e:#}")), true),
        }
    }

    /// Opens `file` in `$VISUAL`, else `$EDITOR`, else `vi`, in this
    /// terminal; when it exits, the file is read back.
    pub(super) fn open_editor(&mut self, project: String, file: PathBuf) {
        let editor = ["VISUAL", "EDITOR"]
            .iter()
            .find_map(|v| std::env::var(v).ok().filter(|e| !e.trim().is_empty()))
            .unwrap_or_else(|| "vi".to_string());
        let command = format!("{editor} {}", shell_quote(&file.to_string_lossy()));
        self.for_terminal.external =
            Some(External { command, file, then: Some(AfterExternal::OntologyEdited(project)) });
    }

    /// Edits that couldn't be loaded, or an editor that failed: the file is
    /// kept, said where, for `E` to open again.
    pub(in crate::app) fn edit_failed(&mut self, why: String, file: PathBuf) {
        if let Some(view) = &mut self.popups.memory_view {
            view.failed_edit = Some((why, file));
        } else {
            let said = t!("memory.edit_kept", path = file.display());
            self.set_status(format!("{why} · {said}"), true);
        }
    }

    /// After the editor: what the file would change, asked about before
    /// anything is changed.
    pub(in crate::app) fn ontology_edited(&mut self, project: String, file: PathBuf) {
        self.load_edits(project, file, false);
    }

    /// The edited `file` to the gateway: what it'd change, or (`apply`)
    /// changed so.
    fn load_edits(&mut self, project: String, file: PathBuf, apply: bool) {
        let text = match std::fs::read_to_string(&file) {
            Ok(text) => text,
            Err(e) => return self.set_status(t!("memory.editor_failed", e = e), true),
        };
        let params = json!({ "text": text, "apply": apply, "refresh": false });
        let purpose = if apply {
            GatewayPurpose::OntologyApplied(project, file)
        } else {
            GatewayPurpose::OntologyPlan(project, file)
        };
        self.gateway_call("ontology.import", params, purpose);
    }
}

/// How many lines the plan takes.
fn plan_len(plan: &Value) -> usize {
    crate::gateway::ontology::plan_lines(plan).len()
}
