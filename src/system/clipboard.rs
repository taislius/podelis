//! The system clipboard through its tools: reading, for Ctrl+V where the
//! terminal doesn't paste by itself, and writing, which works whatever the
//! terminal supports and however big the text. Over SSH those tools would
//! use the remote machine's clipboard, so copying goes through the
//! terminal instead (OSC 52, see `main.rs`).

use std::io::{Read, Write};
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

/// Whether this runs over SSH, where the local clipboard is the server's.
pub fn over_ssh() -> bool {
    ["SSH_CONNECTION", "SSH_TTY"].iter().any(|v| std::env::var_os(v).is_some_and(|v| !v.is_empty()))
}

/// The terminal's own paste keys, for when the clipboard can't be read.
/// Over SSH the keys are the other machine's, which could be either.
pub fn terminal_paste_keys() -> &'static str {
    if over_ssh() {
        "Cmd+V or Ctrl+Shift+V"
    } else if cfg!(target_os = "macos") {
        "Cmd+V"
    } else {
        "Ctrl+Shift+V"
    }
}

/// Why Ctrl+V couldn't paste, and what does.
pub fn read_failed(e: &str) -> String {
    format!("Couldn't read the clipboard ({e}): paste with {} instead", terminal_paste_keys())
}

/// Many terminals drop an OSC 52 copy bigger than about this (xterm's
/// default limit, tmux's buffer).
pub const OSC52_LIMIT: usize = 100_000;

/// The tools that write the clipboard on this desktop.
fn write_tools(macos: bool, wayland: bool, x11: bool) -> Vec<&'static [&'static str]> {
    let mut tools: Vec<&'static [&'static str]> = Vec::new();
    if macos {
        tools.push(&["pbcopy"]);
    }
    if wayland {
        tools.push(&["wl-copy"]);
    }
    if x11 {
        tools.push(&["xclip", "-selection", "clipboard", "-in"]);
        tools.push(&["xsel", "--clipboard", "--input"]);
    }
    tools
}

/// Puts `text` on the clipboard with a local tool; `Err` when there's none
/// (or over SSH), so the terminal can be asked instead.
pub fn write(text: &str) -> Result<(), String> {
    if over_ssh() {
        return Err("over SSH: the terminal copies".to_string());
    }
    let set = |name: &str| std::env::var_os(name).is_some_and(|v| !v.is_empty());
    let tools = write_tools(cfg!(target_os = "macos"), set("WAYLAND_DISPLAY"), set("DISPLAY"));
    let mut last = "no clipboard tool here".to_string();
    for tool in tools.into_iter().filter(|t| crate::doctor::on_path(t[0])) {
        match run_with_input(tool, text) {
            Ok(()) => return Ok(()),
            Err(e) => last = format!("{}: {e}", tool[0]),
        }
    }
    Err(last)
}

fn run_with_input(tool: &[&str], text: &str) -> Result<(), String> {
    let mut child = Command::new(tool[0])
        .args(&tool[1..])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| e.to_string())?;
    // Written from another thread, so a tool that stops reading can't
    // block this one past the timeout.
    let mut stdin = child.stdin.take().ok_or("no input")?;
    let input = text.to_string();
    let writer = std::thread::spawn(move || stdin.write_all(input.as_bytes()));
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
    writer.join().map_err(|_| "couldn't write to it")?.map_err(|e| e.to_string())?;
    if status.success() { Ok(()) } else { Err(format!("exited with {status}")) }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copy_tools_for_each_desktop() {
        let names = |tools: Vec<&'static [&'static str]>| tools.iter().map(|t| t[0]).collect::<Vec<_>>();
        assert_eq!(names(write_tools(true, false, false)), ["pbcopy"]);
        assert_eq!(names(write_tools(false, true, true)), ["wl-copy", "xclip", "xsel"]);
        assert!(write_tools(false, false, false).is_empty());
    }
}
