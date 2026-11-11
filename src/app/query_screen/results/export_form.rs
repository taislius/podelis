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
    /// The file's name, to edit.
    Name,
}

pub struct ExportForm {
    pub choice: ExportChoice,
    pub line: ExportLine,
    /// Where the file goes: a new name with its time to start with, which
    /// follows the format until it's edited.
    pub name: LineInput,
    pub name_edited: bool,
    /// Why the name can't be used (a file that's there already).
    pub error: Option<String>,
}

/// A place to click in the form.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExportClick {
    To(ExportTo),
    Every(bool),
    NextFormat,
    Name,
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
        if choice.to == ExportTo::File {
            lines.push(ExportLine::Name);
        }
        lines
    }

    pub(in crate::app) fn open_export_form(&mut self) {
        let choice = self.export_fit(self.exports.choice);
        self.popups.completion = None;
        let name = LineInput::new(self.export_name_shown(choice.format));
        self.popups.export_form =
            Some(ExportForm { choice, line: ExportLine::To, name, name_edited: false, error: None });
    }

    fn set_export_choice(&mut self, choice: ExportChoice) {
        let choice = self.export_fit(choice);
        let lines = self.export_lines(choice);
        let fresh = self.export_name_shown(choice.format);
        if let Some(form) = &mut self.popups.export_form {
            // The name follows the format: a new one until it's edited,
            // then just its extension, if it's the format's.
            if form.choice.format != choice.format {
                if !form.name_edited {
                    form.name.set(fresh);
                } else if let Some(stem) = form.name.text().strip_suffix(&format!(".{}", form.choice.format.ext())) {
                    let renamed = format!("{stem}.{}", choice.format.ext());
                    form.name.set(renamed);
                }
            }
            form.choice = choice;
            if !lines.contains(&form.line) {
                form.line = ExportLine::To;
            }
        }
    }

    /// A new name, as the form shows it: the whole path (a relative
    /// `export_dir` from the current folder), so what's typed over it
    /// isn't joined to `export_dir` again.
    fn export_name_shown(&self, format: Format) -> String {
        let path = self.export_path(format);
        let path =
            if path.is_absolute() { path } else { std::env::current_dir().map(|d| d.join(&path)).unwrap_or(path) };
        path.display().to_string()
    }

    /// The file the form's name stands for: `~/` is the home folder, a name
    /// without a folder goes where exports go, one without an extension
    /// gets the format's.
    fn export_form_path(&self, name: &str, format: Format) -> PathBuf {
        let name = name.trim();
        let mut path = if name.starts_with("~/") {
            crate::config::settings::expand_home(name)
        } else if std::path::Path::new(name).is_absolute() {
            PathBuf::from(name)
        } else {
            self.export_dir().join(name)
        };
        if path.extension().is_none() {
            path.set_extension(format.ext());
        }
        path
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
            ExportLine::Name => {}
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
        let Some(form) = &mut self.popups.export_form else { return };
        let choice = form.choice;
        if choice.to == ExportTo::File {
            if form.name.text().trim().is_empty() {
                form.line = ExportLine::Name;
                form.error = Some(t!("export_form.no_name").to_string());
                return;
            }
            let name = form.name.text().to_string();
            let path = self.export_form_path(&name, choice.format);
            // Not over a file that's there: a new name for it is the
            // user's to give.
            if path.exists() {
                let Some(form) = &mut self.popups.export_form else { return };
                form.line = ExportLine::Name;
                form.error = Some(t!("export_form.exists").to_string());
                return;
            }
            self.exports.name = Some(path);
        }
        self.popups.export_form = None;
        self.exports.choice = choice;
        self.run_command(Command::Rows { to: choice.dest(), format: choice.format });
        // Taken by the export (every row's carries it on), or refused.
        self.exports.name = None;
    }

    pub(in crate::app) fn handle_export_form_key(&mut self, key: KeyEvent) {
        let Some(form) = &mut self.popups.export_form else { return };
        // The name's line takes what's typed; ↑↓, Tab, Enter and Esc as on
        // the others.
        let moves = matches!(key.code, KeyCode::Up | KeyCode::Down | KeyCode::Tab | KeyCode::BackTab);
        if form.line == ExportLine::Name && !moves {
            if key.code == KeyCode::Char('v') && key.modifiers.contains(KeyModifiers::CONTROL) {
                return match crate::system::clipboard::read() {
                    Ok(text) => self.paste_into_export_form(&text),
                    Err(e) => self.set_status(crate::system::clipboard::read_failed(&e), true),
                };
            }
            match form.name.handle(key) {
                InputResult::Submit => return self.run_export_form(),
                InputResult::Cancel => self.popups.export_form = None,
                InputResult::Changed => {
                    form.name_edited = true;
                    form.error = None;
                }
                InputResult::Moved | InputResult::Ignored => {}
            }
            return;
        }
        let (choice, current) = (form.choice, form.line);
        let lines = self.export_lines(choice);
        let at = lines.iter().position(|l| *l == current).unwrap_or(0);
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
            ExportClick::Name => {
                if let Some(form) = &mut self.popups.export_form {
                    form.line = ExportLine::Name;
                }
                return;
            }
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

    /// A paste into the form: into the name, on its line.
    pub(in crate::app) fn paste_into_export_form(&mut self, text: &str) {
        let Some(form) = &mut self.popups.export_form else { return };
        if form.line == ExportLine::Name {
            form.name.insert_str(text.lines().next().unwrap_or_default().trim());
            form.name_edited = true;
            form.error = None;
        }
    }
}
