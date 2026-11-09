//! `E` and `export ▾`: one form for where the rows go, in what format and
//! which of them. It offers what can be done now, and starts from the last
//! choice, so doing the same again is `E`, `Enter`.

use crate::app::commands::{Can, Command};
use crate::app::*;

/// Where the form sends the rows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExportTo {
    File,
    Clipboard,
}

/// What the form is set to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExportChoice {
    pub to: ExportTo,
    pub format: Format,
    /// Every row of the result, not just those shown (to a file).
    pub every: bool,
}

impl Default for ExportChoice {
    fn default() -> Self {
        ExportChoice { to: ExportTo::File, format: Format::Csv, every: false }
    }
}

impl ExportChoice {
    pub fn dest(self) -> Dest {
        match (self.to, self.every) {
            (ExportTo::Clipboard, _) => Dest::Copy,
            (ExportTo::File, false) => Dest::ExportShown,
            (ExportTo::File, true) => Dest::ExportAll,
        }
    }
}

/// The form's lines that can be changed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExportLine {
    To,
    Format,
    Rows,
}

pub struct ExportForm {
    pub choice: ExportChoice,
    pub line: ExportLine,
}

/// A place to click in the form.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExportClick {
    To(ExportTo),
    Every(bool),
    NextFormat,
    Run,
}

impl App {
    fn export_offered(&self, to: Dest, format: Format) -> bool {
        format.supports(to) && !matches!(self.can(Command::Rows { to, format }), Can::No { .. })
    }

    /// The formats `choice`'s rows can go in now, in the one order.
    fn export_formats(&self, choice: ExportChoice) -> Vec<Format> {
        Format::for_dest(choice.dest()).filter(|f| self.export_offered(choice.dest(), *f)).collect()
    }

    /// Whether "every row" can be chosen (a plan's table has only its rows).
    pub fn export_every_offered(&self) -> bool {
        Format::for_dest(Dest::ExportAll).any(|f| self.export_offered(Dest::ExportAll, f))
    }

    /// Makes `choice` one that can be done: what it asks for that can't
    /// moves to what's nearest, in the formats' order.
    fn export_fit(&self, mut choice: ExportChoice) -> ExportChoice {
        choice.every = choice.every && choice.to == ExportTo::File && self.export_every_offered();
        let offered = self.export_formats(choice);
        if !offered.contains(&choice.format) {
            let at = |f: Format| Format::ALL.iter().position(|x| *x == f).unwrap_or(0);
            let from = at(choice.format);
            let nearest = offered.iter().min_by_key(|f| (at(**f).abs_diff(from), at(**f) < from));
            if let Some(format) = nearest {
                choice.format = *format;
            }
        }
        choice
    }

    /// The lines that can be changed now.
    fn export_lines(&self, choice: ExportChoice) -> Vec<ExportLine> {
        let mut lines = vec![ExportLine::To, ExportLine::Format];
        if choice.to == ExportTo::File && self.export_every_offered() {
            lines.push(ExportLine::Rows);
        }
        lines
    }

    pub(in crate::app) fn open_export_form(&mut self) {
        let choice = self.export_fit(self.exports.choice);
        self.popups.completion = None;
        self.popups.export_form = Some(ExportForm { choice, line: ExportLine::To });
    }

    fn set_export_choice(&mut self, choice: ExportChoice) {
        let choice = self.export_fit(choice);
        let lines = self.export_lines(choice);
        if let Some(form) = &mut self.popups.export_form {
            form.choice = choice;
            if !lines.contains(&form.line) {
                form.line = ExportLine::To;
            }
        }
    }

    /// ←/→ on the current line.
    fn change_export_line(&mut self, forward: bool) {
        let Some(form) = &self.popups.export_form else { return };
        let mut choice = form.choice;
        match form.line {
            ExportLine::To => {
                choice.to = if choice.to == ExportTo::File { ExportTo::Clipboard } else { ExportTo::File };
            }
            ExportLine::Rows => choice.every = !choice.every,
            ExportLine::Format => {
                let formats = self.export_formats(choice);
                let at = formats.iter().position(|f| *f == choice.format).unwrap_or(0);
                let step = if forward { 1 } else { formats.len().saturating_sub(1) };
                if let Some(format) = formats.get((at + step) % formats.len().max(1)) {
                    choice.format = *format;
                }
            }
        }
        self.set_export_choice(choice);
    }

    fn run_export_form(&mut self) {
        let Some(form) = self.popups.export_form.take() else { return };
        self.exports.choice = form.choice;
        self.run_command(Command::Rows { to: form.choice.dest(), format: form.choice.format });
    }

    pub(in crate::app) fn handle_export_form_key(&mut self, key: KeyEvent) {
        let Some(form) = &self.popups.export_form else { return };
        let lines = self.export_lines(form.choice);
        let at = lines.iter().position(|l| *l == form.line).unwrap_or(0);
        let line = match key.code {
            KeyCode::Esc => {
                self.popups.export_form = None;
                return;
            }
            KeyCode::Enter => return self.run_export_form(),
            KeyCode::Left => return self.change_export_line(false),
            KeyCode::Right | KeyCode::Char(' ') => return self.change_export_line(true),
            KeyCode::Down | KeyCode::Tab => lines[(at + 1) % lines.len()],
            KeyCode::Up | KeyCode::BackTab => lines[(at + lines.len() - 1) % lines.len()],
            _ => return,
        };
        if let Some(form) = &mut self.popups.export_form {
            form.line = line;
        }
    }

    /// A click in the form sets what's under it, and on its last line runs
    /// it; outside, it closes.
    pub(in crate::app) fn export_form_click(&mut self, x: u16, y: u16) {
        if !mouse::inside(self.hits.export_form, x, y) {
            self.popups.export_form = None;
            return;
        }
        let Some(form) = &self.popups.export_form else { return };
        let mut choice = form.choice;
        let Some((_, click)) = self.hits.export_form_spots.iter().find(|(r, _)| mouse::inside(*r, x, y)) else {
            return;
        };
        match *click {
            ExportClick::Run => return self.run_export_form(),
            ExportClick::To(to) => choice.to = to,
            ExportClick::Every(every) => choice.every = every,
            ExportClick::NextFormat => {
                if let Some(form) = &mut self.popups.export_form {
                    form.line = ExportLine::Format;
                }
                return self.change_export_line(true);
            }
        }
        self.set_export_choice(choice);
    }

    /// Where a file of `format` would go: the name without its time.
    pub fn export_path_shown(&self, format: Format) -> String {
        let dir = self.export_dir();
        dir.join(format!("podelis-export-….{}", format.ext())).display().to_string()
    }
}
