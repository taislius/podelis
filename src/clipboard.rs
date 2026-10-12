//! Reading the system clipboard, for Ctrl+V where the terminal doesn't
//! paste by itself. Copying goes through the terminal (OSC 52, see
//! `main.rs`), which works over SSH too; reading needs a local tool.

use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// How long a clipboard tool may take before it's given up on.
const TIMEOUT: Duration = Duration::from_secs(1);

/// The tools to try, for this platform and display.
fn tools() -> Vec<&'static [&'static str]> {
    let set = |name: &str| std::env::var_os(name).is_some_and(|v| !v.is_empty());
    let mut tools: Vec<&'static [&'static str]> = Vec::new();
    if cfg!(target_os = "macos") {
        tools.push(&["pbpaste"]);
    }
    if cfg!(windows) {
        tools.push(&["powershell", "-NoProfile", "-Command", "Get-Clipboard -Raw"]);
    }
    if set("WAYLAND_DISPLAY") {
        tools.push(&["wl-paste", "--no-newline"]);
    }
    if set("DISPLAY") {
        tools.push(&["xclip", "-selection", "clipboard", "-o"]);
        tools.push(&["xsel", "--clipboard", "--output"]);
    }
    tools
}

/// The clipboard's text, or why it couldn't be read.
pub fn read() -> Result<String, String> {
    let installed: Vec<_> = tools().into_iter().filter(|t| crate::doctor::on_path(t[0])).collect();
    if installed.is_empty() {
        return Err("no clipboard tool here (pbpaste, wl-paste, xclip or xsel)".to_string());
    }
    let mut last = String::new();
    for tool in installed {
        match run(tool) {
            Ok(text) => return Ok(text),
            Err(e) => last = format!("{}: {e}", tool[0]),
        }
    }
    Err(last)
}

fn run(tool: &[&str]) -> Result<String, String> {
    let mut child = Command::new(tool[0])
        .args(&tool[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| e.to_string())?;
    // Read as it comes: a big clipboard would fill the pipe and stall it.
    let mut stdout = child.stdout.take().ok_or("no output")?;
    let reader = std::thread::spawn(move || {
        let mut out = Vec::new();
        stdout.read_to_end(&mut out).map(|_| out)
    });
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
            break status;
        }
        if started.elapsed() > TIMEOUT {
            let _ = child.kill();
            let _ = child.wait();
            return Err("timed out".to_string());
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    let out = reader.join().map_err(|_| "couldn't read it")?.map_err(|e| e.to_string())?;
    if !status.success() {
        return Err("the clipboard is empty or holds no text".to_string());
    }
    String::from_utf8(out).map_err(|_| "the clipboard holds no text".to_string())
}
