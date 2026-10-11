//! Working with other programs, the Unix way: pipe the result rows through
//! a shell command (`|`), or open them in a viewer like `less` or `vd`
//! (`o`).

use std::path::PathBuf;
use std::process::Stdio;

use tokio::io::AsyncWriteExt as _;

use super::*;
use crate::export;

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

    /// `o`: opens the shown rows in the viewer (the main loop runs it).
    pub(super) fn open_in_viewer(&mut self) {
        let Some(results) = self.tab.grid.results.as_ref().filter(|r| !r.columns.is_empty()) else {
            return self.set_status("No rows to open".to_string(), true);
        };
        let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
        let file = std::env::temp_dir().join(format!("podelis-{}-{stamp}.tsv", std::process::id()));
        if let Err(e) = store::write_atomic(&file, &export::tsv(results, &self.tab.grid.view)) {
            return self.set_status(format!("Couldn't write the rows: {e:#}"), true);
        }
        let viewer = self
            .settings
            .viewer
            .clone()
            .or_else(|| std::env::var("PAGER").ok())
            .filter(|v| !v.trim().is_empty())
            .unwrap_or_else(|| "less -S".to_string());
        let path = shell_quote(&file.to_string_lossy());
        let command = if viewer.contains("{}") { viewer.replace("{}", &path) } else { format!("{viewer} {path}") };
        self.external = Some(External { command, file });
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
