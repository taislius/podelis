//! Working with other programs, the Unix way: pipe the result rows through
//! a shell command (`|`), or open them in a viewer like `less` or `vd`
//! (`o`).

use std::path::PathBuf;
use std::process::Stdio;

use tokio::io::AsyncWriteExt as _;

use super::*;
use crate::export;
use crate::settings::Viewer;

/// A command run with the terminal handed over (the viewer), then removed.
pub struct External {
    pub command: String,
    /// The temporary file it shows, deleted afterwards.
    pub file: PathBuf,
}

/// Whether `command` wants JSON rather than TSV (by its program name).
fn wants_json(command: &str) -> bool {
    let program = command.split_whitespace().next().unwrap_or_default();
    let program = program.rsplit('/').next().unwrap_or(program);
    matches!(program, "jq" | "gron" | "jaq" | "gojq" | "fx")
}

/// The shell that runs commands: `sh -c`.
fn shell(command: &str) -> std::process::Command {
    let mut cmd = std::process::Command::new("sh");
    cmd.arg("-c").arg(command);
    cmd
}

/// Quotes a path for the shell.
fn shell_quote(path: &str) -> String {
    format!("'{}'", path.replace('\'', "'\\''"))
}

impl App {
    /// `|`: asks for the command to pipe the rows through.
    pub(super) fn open_pipe_prompt(&mut self) {
        if self.tab.grid.results.as_ref().is_none_or(|r| r.columns.is_empty()) {
            return self.set_status("No rows to pipe".to_string(), true);
        }
        self.pipe_prompt = Some(LineInput::new(self.last_pipe.clone()));
    }

    pub(super) fn handle_pipe_prompt_key(&mut self, key: KeyEvent) {
        let Some(input) = &mut self.pipe_prompt else { return };
        match input.handle(key) {
            InputResult::Cancel => self.pipe_prompt = None,
            InputResult::Submit => {
                let Some(command) = self.pipe_prompt.take().filter(|c| !c.text().trim().is_empty()) else { return };
                self.run_pipe(command.text().to_string());
            }
            InputResult::Changed | InputResult::Moved | InputResult::Ignored => {}
        }
    }

    /// Runs `command` with the shown rows on stdin; its output opens in the
    /// text view.
    fn run_pipe(&mut self, command: String) {
        if !self.may_reach_outside() {
            return;
        }
        let Some(results) = &self.tab.grid.results else { return };
        let input = if wants_json(&command) {
            export::json(results, &self.tab.grid.view)
        } else {
            export::tsv(results, &self.tab.grid.view)
        };
        self.last_pipe.clone_from(&command);
        self.set_status(format!("Running | {command}…"), false);
        let send = self.sender();
        tokio::spawn(async move {
            let result = pipe_through(&command, input).await;
            send(DbEventKind::PipeDone(command, result));
        });
    }

    pub(super) fn pipe_done(&mut self, command: &str, result: Result<String, String>) {
        match result {
            Ok(output) => {
                self.set_status(String::new(), false);
                let text = if output.is_empty() { "(no output)".to_string() } else { export::align_tabs(&output) };
                self.cell_view = Some(CellView::new(format!("| {command}"), text));
            }
            Err(e) => self.set_status(format!("| {command}: {e}"), true),
        }
    }

    /// `o`: a menu of the formats to open the rows in, each with what
    /// opens it.
    pub(super) fn open_in_viewer(&mut self) {
        if self.tab.grid.results.as_ref().is_none_or(|r| r.columns.is_empty()) {
            return self.set_status("No rows to open".to_string(), true);
        }
        self.open_viewer_menu();
    }

    /// What opens rows in `format` (`Settings::viewer_for`).
    pub(super) fn viewer_for(&self, format: OpenFormat) -> Viewer {
        self.settings.viewer_for(format.key())
    }

    /// Writes the rows (the marked ones, or those shown) in `format` to a
    /// file of their own and opens it: in its app, which Podėlis doesn't
    /// wait for, or in this terminal for a viewer that runs in one.
    pub(super) fn open_rows_as(&mut self, format: OpenFormat) {
        let Some((rows, text)) = self.rows_as(format) else {
            return self.set_status("No rows to open".to_string(), true);
        };
        let dir = opened_dir(&self.data_dir.clone().unwrap_or_else(store::config_dir));
        let file = match write_opened(&dir, format, &text) {
            Ok(file) => file,
            Err(e) => return self.set_status(format!("Couldn't write the rows: {e}"), true),
        };
        let viewer = self.viewer_for(format);
        let path = shell_quote(&file.to_string_lossy());
        let program = viewer.command();
        let command = if program.contains("{}") { program.replace("{}", &path) } else { format!("{program} {path}") };
        if viewer.in_terminal() {
            self.external = Some(External { command, file });
            return;
        }
        if !self.may_reach_outside() {
            return;
        }
        let what = format!("{rows} row(s) as {}", format.name());
        let send = self.sender();
        self.set_status(format!("Opening {what}…"), false);
        let log = dir.join("open.log");
        tokio::spawn(async move {
            let result = start_app(&command, &log).await;
            send(DbEventKind::Opened(what, command, result));
        });
    }

    pub(super) fn opened(&mut self, what: &str, command: &str, result: Result<(), String>) {
        match result {
            Ok(()) => self.set_status(format!("Opened {what}"), false),
            // The program, not the whole command: its file's long path
            // would push out why.
            Err(e) => {
                let program = command.split_whitespace().next().unwrap_or(command);
                self.set_status(format!("Couldn't open {what} with {program}: {e}"), true);
            }
        }
    }

    /// The program the main loop should run with the terminal, if any.
    pub fn take_external(&mut self) -> Option<External> {
        self.external.take()
    }

    /// Runs `external` in the foreground (the caller has handed over the
    /// terminal) and reports how it went.
    pub fn run_external(&mut self, external: &External) {
        let status = shell(&external.command).status();
        let _ = std::fs::remove_file(&external.file);
        match status {
            Ok(s) if s.success() => self.set_status(String::new(), false),
            Ok(s) => self.set_status(format!("`{}` exited with {s}", external.command), true),
            Err(e) => self.set_status(format!("Couldn't run `{}`: {e}", external.command), true),
        }
    }
}

/// The formats rows open in (`o`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpenFormat {
    Csv,
    Tsv,
    Json,
    /// `INSERT` statements.
    Sql,
    /// Aligned columns, as `-e` prints them.
    Text,
}

impl OpenFormat {
    pub const ALL: [OpenFormat; 5] =
        [OpenFormat::Csv, OpenFormat::Json, OpenFormat::Sql, OpenFormat::Tsv, OpenFormat::Text];

    /// Its key in `[viewers]`, and its file's extension.
    pub fn key(self) -> &'static str {
        match self {
            OpenFormat::Csv => "csv",
            OpenFormat::Tsv => "tsv",
            OpenFormat::Json => "json",
            OpenFormat::Sql => "sql",
            OpenFormat::Text => "txt",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            OpenFormat::Csv => "CSV",
            OpenFormat::Tsv => "TSV",
            OpenFormat::Json => "JSON",
            OpenFormat::Sql => "INSERT statements",
            OpenFormat::Text => "a text table",
        }
    }
}

/// Rows opened in an app stay this long (the app reads the file when it
/// likes), then go the next time rows are opened.
const OPENED_KEPT: std::time::Duration = std::time::Duration::from_hours(24);

/// Where opened rows are written: a folder of the user's alone (a shared
/// temporary folder would show them to others).
fn opened_dir(config_dir: &std::path::Path) -> PathBuf {
    config_dir.join("opened")
}

/// Writes `text` to a new file in `dir` (made the user's alone), after
/// removing what's been there for a day.
fn write_opened(dir: &std::path::Path, format: OpenFormat, text: &str) -> Result<PathBuf, String> {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let old = entry.metadata().and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok());
        if old.is_some_and(|age| age > OPENED_KEPT) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S%.3f");
    let file = dir.join(format!("rows-{stamp}.{}", format.key()));
    store::write_atomic(&file, text).map_err(|e| format!("{e:#}"))?;
    let _ = std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600));
    Ok(file)
}

/// Starts `command` on its own (its own process group, no terminal), so
/// it outlives Podėlis and never draws over it; an app that fails at once
/// (no such program, no app for the type) is an error, with what it said
/// (its stderr goes to `log`: a pipe left unread would stop it).
async fn start_app(command: &str, log: &std::path::Path) -> Result<(), String> {
    let errors = {
        use std::os::unix::fs::OpenOptionsExt as _;
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true).mode(0o600);
        options.open(log).map_err(|e| e.to_string())?
    };
    let mut child = tokio::process::Command::from(shell(command))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(errors)
        .process_group(0)
        .spawn()
        .map_err(|e| e.to_string())?;
    // Openers (`open`, `xdg-open`) end once the app has it; an app itself
    // runs on, and is left to.
    match tokio::time::timeout(std::time::Duration::from_secs(5), child.wait()).await {
        Ok(Ok(status)) if status.success() => Ok(()),
        Ok(Ok(status)) => {
            let said = std::fs::read_to_string(log).unwrap_or_default();
            let said = said.trim().lines().last().unwrap_or_default();
            Err(if said.is_empty() { format!("it exited with {status}") } else { said.to_string() })
        }
        Ok(Err(e)) => Err(e.to_string()),
        Err(_) => Ok(()),
    }
}

/// Feeds `input` to `command` and collects what it prints (stderr after
/// stdout). A failing exit status is an error, with its message.
async fn pipe_through(command: &str, input: String) -> Result<String, String> {
    let mut child = tokio::process::Command::from(shell(command))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("couldn't start it: {e}"))?;
    // Write in the background: the command may print before reading it all.
    let mut stdin = child.stdin.take().ok_or("no stdin")?;
    tokio::spawn(async move {
        let _ = stdin.write_all(input.as_bytes()).await;
    });
    let output = tokio::time::timeout(Duration::from_secs(60), child.wait_with_output())
        .await
        .map_err(|_| "still running after 60 s; stopped".to_string())?
        .map_err(|e| e.to_string())?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !output.status.success() && stdout.trim().is_empty() {
        let why = stderr.trim();
        return Err(if why.is_empty() { format!("exited with {}", output.status) } else { why.to_string() });
    }
    Ok(if stderr.trim().is_empty() { stdout.into_owned() } else { format!("{stdout}\n{}", stderr.trim_end()) })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_for_jq_tsv_otherwise() {
        assert!(wants_json("jq '.[] | .id'"));
        assert!(wants_json("/usr/bin/gron"));
        assert!(!wants_json("grep -i vilnius"));
        assert_eq!(shell_quote("/tmp/it's.tsv"), "'/tmp/it'\\''s.tsv'");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn pipes_through_shell_commands() {
        let out = pipe_through("sort -r | head -1", "a\nc\nb\n".into()).await.unwrap();
        assert_eq!(out, "c\n");
        let err = pipe_through("echo nope >&2; exit 3", String::new()).await.unwrap_err();
        assert_eq!(err, "nope");
        // Output that doesn't wait for its input.
        let out = pipe_through("echo hi", "x\n".repeat(100_000)).await.unwrap();
        assert_eq!(out, "hi\n");
    }
}
