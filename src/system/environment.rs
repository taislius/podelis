//! Facts about this install and terminal, for `podelis doctor` and the
//! app's "Copy diagnostics": no hosts, users or SQL.

use crate::config::settings::Settings;

/// The terminal as the environment describes it.
pub fn terminal_description() -> String {
    let var = |name: &str| std::env::var(name).ok().filter(|v| !v.is_empty());
    let mut parts = Vec::new();
    if let Some(program) = var("TERM_PROGRAM") {
        parts.push(var("TERM_PROGRAM_VERSION").map_or(program.clone(), |v| format!("{program} {v}")));
    }
    parts.push(format!("TERM={}", var("TERM").unwrap_or_else(|| "unset".into())));
    if let Some(c) = var("COLORTERM") {
        parts.push(format!("COLORTERM={c}"));
    }
    if var("TMUX").is_some() {
        parts.push("in tmux".into());
    }
    parts.join(", ")
}

/// Facts about this install for bug reports: no hosts, users or SQL.
pub fn environment(settings: &Settings) -> Vec<(String, String)> {
    let mut out = vec![
        ("Podėlis".to_string(), env!("CARGO_PKG_VERSION").to_string()),
        ("OS".to_string(), format!("{} {}", std::env::consts::OS, std::env::consts::ARCH)),
        ("Terminal".to_string(), terminal_description()),
        (
            "Settings".to_string(),
            format!(
                "theme {:?}, editor {:?}, mouse {}, restore_session {}, keepalive {} s, {} custom key(s)",
                settings.theme,
                settings.editor_mode,
                settings.mouse,
                settings.restore_session,
                settings.keepalive,
                settings.keys.len()
            ),
        ),
    ];
    let crash = crate::system::crash::log_path();
    if let Ok(meta) = std::fs::metadata(&crash) {
        out.push(("Crash log".to_string(), format!("{} ({} bytes)", crash.display(), meta.len())));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn environment_has_no_secrets() {
        let env = environment(&Settings::default());
        assert_eq!(env[0].1, env!("CARGO_PKG_VERSION"));
        assert!(env.iter().any(|(k, _)| k == "Terminal"));
    }
}
