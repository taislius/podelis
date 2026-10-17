//! Crash log: panics are appended to `crash.log` in the config folder, with
//! a backtrace, so a crash can be reported after the terminal is restored.

use std::backtrace::Backtrace;
use std::fmt::Write as _;
use std::io::Write as _;
use std::panic::PanicHookInfo;
use std::path::PathBuf;
use std::sync::Mutex;

/// The log is moved to `crash.log.old` once it grows past this.
const MAX_LOG_BYTES: u64 = 512 * 1024;

/// A panic in a background task, waiting for the UI to report it.
static BACKGROUND: Mutex<Option<String>> = Mutex::new(None);

pub fn log_path() -> PathBuf {
    crate::store::config_dir().join("crash.log")
}

/// Appends the panic to the crash log. Returns the log's path if written.
pub fn record(info: &PanicHookInfo) -> Option<PathBuf> {
    tracing::error!(panic = %message(info), "panic");
    let entry = describe(info, &Backtrace::force_capture().to_string());
    let path = log_path();
    std::fs::create_dir_all(path.parent()?).ok()?;
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > MAX_LOG_BYTES) {
        let _ = std::fs::rename(&path, path.with_extension("log.old"));
    }
    let mut options = std::fs::OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options.open(&path).ok()?;
    file.write_all(entry.as_bytes()).ok()?;
    Some(path)
}

/// Remembers a background task's panic for [`take_background`].
pub fn note_background(info: &PanicHookInfo) {
    if let Ok(mut slot) = BACKGROUND.lock() {
        *slot = Some(message(info));
    }
}

/// The last background panic's message, once.
pub fn take_background() -> Option<String> {
    BACKGROUND.lock().ok()?.take()
}

fn message(info: &PanicHookInfo) -> String {
    let payload = info.payload();
    payload
        .downcast_ref::<&str>()
        .map(|s| (*s).to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "(no message)".to_string())
}

fn describe(info: &PanicHookInfo, backtrace: &str) -> String {
    let mut out = String::new();
    let thread = std::thread::current();
    let _ = writeln!(out, "=== {} ===", chrono::Local::now().format("%Y-%m-%d %H:%M:%S %z"));
    let _ = writeln!(
        out,
        "podelis {} on {}/{}, thread {}",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH,
        thread.name().unwrap_or("unnamed")
    );
    let location =
        info.location().map_or_else(String::new, |l| format!(" at {}:{}:{}", l.file(), l.line(), l.column()));
    let _ = writeln!(out, "panic: {}{location}", message(info));
    let _ = writeln!(out, "{backtrace}\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn panics_are_logged_with_location() {
        // The hook is global: only capture this test's own panic.
        let captured = std::sync::Arc::new(Mutex::new(String::new()));
        let sink = captured.clone();
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            if message(info).starts_with("crash-log-test") {
                *sink.lock().unwrap() = describe(info, "<backtrace>");
            }
        }));
        let _ = std::panic::catch_unwind(|| panic!("crash-log-test {}", 42));
        std::panic::set_hook(previous);
        let entry = captured.lock().unwrap().clone();
        assert!(entry.contains("panic: crash-log-test 42 at src/crash.rs:"), "{entry}");
        assert!(entry.contains(env!("CARGO_PKG_VERSION")) && entry.contains("<backtrace>"));
    }
}
