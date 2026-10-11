//! Asking for query parameter values before a run.

use super::*;
use crate::params::{self, Param};

/// The popup that asks for parameter values.
pub struct ParamForm {
    sql: String,
    offset: usize,
    explain: bool,
    pub params: Vec<Param>,
    pub values: Vec<String>,
    pub field: usize,
}

impl App {
    /// Opens the parameter form if `sql` has placeholders. True if it did.
    pub(super) fn ask_for_params(&mut self, sql: &str, offset: usize, explain: bool) -> bool {
        let found = params::find(sql, self.engine());
        if found.is_empty() {
            return false;
        }
        let values = found.iter().map(|p| self.param_values.get(&p.name).cloned().unwrap_or_default()).collect();
        self.param_form = Some(ParamForm { sql: sql.to_string(), offset, explain, params: found, values, field: 0 });
        true
    }

    pub(super) fn handle_param_key(&mut self, key: KeyEvent) {
        let Some(form) = &mut self.param_form else { return };
        if let Some(i) = list::step(form.field, form.params.len(), key, Nav::FIELDS) {
            form.field = i;
            return;
        }
        match key.code {
            KeyCode::Esc => self.param_form = None,
            KeyCode::Enter => self.submit_params(),
            _ => {
                input::edit_at_end(&mut form.values[form.field], key);
            }
        }
    }

    /// Pastes into the focused field (newlines become spaces).
    pub(super) fn paste_param(&mut self, text: &str) {
        if let Some(form) = &mut self.param_form {
            input::paste_at_end(&mut form.values[form.field], text);
        }
    }

    fn submit_params(&mut self) {
        let Some(form) = self.param_form.take() else { return };
        for (p, v) in form.params.iter().zip(&form.values) {
            self.param_values.insert(p.name.clone(), v.clone());
        }
        let filled = params::fill(&form.sql, &form.params, &form.values, self.engine());
        if form.explain {
            let infos = analyze::analyze(&parse(&filled.sql, self.engine()));
            self.run_now(filled.sql.clone(), form.offset, infos, RunPurpose::Explain, Some(filled));
        } else {
            self.check_and_run(filled.sql.clone(), form.offset, RunPurpose::Query, Some(filled));
        }
    }
}
