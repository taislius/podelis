//! `podelis doctor`: checks the local setup (config files, keychain, ssh,
//! terminal, viewer, logs) and optionally tests saved connections. The
//! environment part is also what "Copy diagnostics" puts in bug reports.

use std::fmt::Write as _;
use std::path::Path;

use crate::config::settings::Settings;
use crate::config::store::{self, ConnectionStore};
use crate::db::{Step, StepState};

fn step(name: &'static str, state: StepState, detail: impl Into<String>, hint: Option<&'static str>) -> Step {
    Step { name, state, detail: detail.into(), elapsed: None, hint }
}

/// Whether `program` is on the PATH (or is a path that exists).
pub(crate) fn on_path(program: &str) -> bool {
    if program.contains('/') {
        return Path::new(program).is_file();
    }
    std::env::var_os("PATH").is_some_and(|paths| std::env::split_paths(&paths).any(|dir| dir.join(program).is_file()))
}

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

/// Checks the local setup. `connections`: also test the saved connections
/// of this project (the last one opened by default).
pub async fn run(
    store: &ConnectionStore,
    store_warning: Option<&str>,
    project: Option<&str>,
    connections: bool,
) -> i32 {
    let dir = store::config_dir();
    let mut steps = Vec::new();
    steps.push(step(
        "Version",
        StepState::Ok,
        format!("podelis {} on {} {}", env!("CARGO_PKG_VERSION"), std::env::consts::OS, std::env::consts::ARCH),
        None,
    ));

    // The config folder must be writable.
    let probe = dir.join(".podelis-doctor");
    steps.push(match store::write_atomic(&probe, "ok") {
        Ok(()) => {
            let _ = std::fs::remove_file(&probe);
            step("Config", StepState::Ok, dir.display().to_string(), None)
        }
        Err(e) => step(
            "Config",
            StepState::Failed,
            format!("{}: {e:#}", dir.display()),
            Some("Podėlis saves tabs and history there; check its permissions, or set PODELIS_CONFIG_DIR"),
        ),
    });

    let connection_count: usize = store.projects.iter().map(|p| p.connections.len()).sum();
    steps.push(match store_warning {
        Some(w) => step("Projects", StepState::Warn, w.to_string(), None),
        None => step(
            "Projects",
            StepState::Ok,
            format!("{} project(s), {connection_count} saved connection(s)", store.projects.len()),
            None,
        ),
    });

    let (settings, settings_warning) = Settings::load(&dir);
    let (_, key_warnings) = crate::config::keys::KeyMap::new(&settings.keys);
    let problems: Vec<String> = settings_warning.into_iter().chain(key_warnings).collect();
    steps.push(if problems.is_empty() {
        let exists = dir.join("config.toml").is_file();
        step("Settings", StepState::Ok, if exists { "config.toml is valid" } else { "defaults (no config.toml)" }, None)
    } else {
        step("Settings", StepState::Warn, problems.join("; "), None)
    });

    // Reading an entry that doesn't exist exercises keychain access.
    steps.push(match store::get_password("podelis-doctor-probe") {
        Ok(_) => step("Keychain", StepState::Ok, "readable", None),
        Err(e) => step(
            "Keychain",
            StepState::Warn,
            format!("{e:#}"),
            Some(
                "passwords can't be saved here: use a password command (Passwd cmd), ~/.pgpass or ~/.my.cnf, or a Secret Service like gnome-keyring or KeePassXC",
            ),
        ),
    });

    // Optional: any of them can supply passwords as a password command.
    let tools = crate::db::password_command::installed(on_path);
    steps.push(if tools.is_empty() {
        step("Passwords", StepState::Skipped, "no password manager CLI found (pass, op, bw, vault…)", None)
    } else {
        let names: Vec<_> = tools.iter().map(|t| t.label).collect();
        let detail = format!("{} found: can supply passwords as a password command", names.join(", "));
        step("Passwords", StepState::Ok, detail, None)
    });

    steps.push(match std::process::Command::new("ssh").arg("-V").output() {
        Ok(out) => {
            let version = String::from_utf8_lossy(&out.stderr).trim().lines().next().unwrap_or_default().to_string();
            step("SSH", StepState::Ok, version, None)
        }
        Err(_) => step("SSH", StepState::Warn, "ssh isn't installed (only needed for SSH tunnels)", None),
    });

    steps.extend(environment_checks(&settings));

    let mut out = crate::db::report(&steps);
    let mut failed = steps.iter().any(|s| s.state == StepState::Failed);
    if connections {
        failed |= test_connections(store, project, &mut out).await;
    }
    let _ = std::io::Write::write_all(&mut std::io::stdout(), out.as_bytes());
    i32::from(failed)
}

/// The terminal, the viewer and the crash log.
fn environment_checks(settings: &Settings) -> Vec<Step> {
    let mut steps = Vec::new();
    let colours = std::env::var("COLORTERM").is_ok_and(|c| c.contains("truecolor") || c.contains("24bit"));
    let mut term = terminal_description();
    let mut term_hint = None;
    if !colours {
        term.push_str(" · 256 colours (no COLORTERM=truecolor)");
    }
    if std::env::var_os("TMUX").is_some() {
        term_hint = Some("in tmux, copying (y, Ctrl+C) needs `set -g set-clipboard on`");
    }
    steps.push(step("Terminal", if term_hint.is_some() { StepState::Warn } else { StepState::Ok }, term, term_hint));

    // What opens result rows (`o`), format by format, as `o` decides it.
    let chosen: Vec<(&str, crate::config::settings::Viewer)> =
        crate::config::settings::VIEWER_FORMATS.iter().map(|f| (*f, settings.viewer_for(f))).collect();
    let program =
        |v: &crate::config::settings::Viewer| v.command().split_whitespace().next().unwrap_or_default().to_string();
    let mut programs: Vec<String> = chosen.iter().map(|(_, v)| program(v)).collect();
    programs.sort();
    programs.dedup();
    let missing: Vec<&str> = programs.iter().map(String::as_str).filter(|p| !on_path(p)).collect();
    let said: Vec<String> = programs
        .iter()
        .map(|p| {
            let formats: Vec<&str> = chosen.iter().filter(|(_, v)| program(v) == *p).map(|(f, _)| *f).collect();
            let here = chosen.iter().any(|(_, v)| program(v) == *p && v.in_terminal());
            format!("{}: {p}{}", formats.join(", "), if here { " (in this terminal)" } else { "" })
        })
        .collect();
    steps.push(if missing.is_empty() {
        step("Viewers", StepState::Ok, format!("{} (o in the results)", said.join("; ")), None)
    } else {
        step(
            "Viewers",
            StepState::Warn,
            format!("{} isn't installed", missing.join(", ")),
            Some("set [viewers] in config.toml (csv = \"libreoffice --calc\", json = \"code\")"),
        )
    });

    let crash = crate::system::crash::log_path();
    steps.push(match std::fs::metadata(&crash) {
        Ok(m) => step(
            "Crashes",
            StepState::Warn,
            format!("{} ({} bytes)", crash.display(), m.len()),
            Some("please attach it to a bug report"),
        ),
        Err(_) => step("Crashes", StepState::Ok, "none logged", None),
    });
    steps
}

/// Tests each saved connection of the project; true if any failed.
async fn test_connections(store: &ConnectionStore, project: Option<&str>, out: &mut String) -> bool {
    let mut failed = false;
    let project = match project {
        Some(name) => store.project_by_name(name),
        None => store.last_project.as_deref().and_then(|id| store.project(id)),
    };
    match project {
        None => {
            out.push_str("\nNo project to test (pass -p <project>).\n");
            failed = true;
        }
        Some(project) => {
            for profile in &project.connections {
                let (password, _) = store::saved_password(&project.id, profile);
                let form = crate::app::ConnectForm::from_profile(profile, password);
                let _ = writeln!(out, "\n{} · {}", profile.name, form.summary());
                match form.params() {
                    Ok(params) => {
                        let steps = crate::db::diagnose(params).await;
                        // A saved connection whose file is missing is broken too.
                        failed |= steps
                            .iter()
                            .any(|s| s.state == StepState::Failed || (s.name == "File" && s.state != StepState::Ok));
                        out.push_str(&crate::db::report(&steps));
                    }
                    Err(e) => {
                        failed = true;
                        let _ = writeln!(out, "✗ {e}");
                    }
                }
            }
        }
    }
    failed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn viewers_are_checked_format_by_format() {
        use crate::config::settings::Viewer;
        let mut settings = Settings::default();
        for f in crate::config::settings::VIEWER_FORMATS {
            settings.viewers.insert(f.to_string(), Viewer::Command("true".into()));
        }
        // An old `viewer` that `[viewers].tsv` replaced isn't looked for.
        settings.viewer = Some("no-such-viewer-xyz".into());
        let viewers = |s: &Settings| environment_checks(s).into_iter().find(|s| s.name == "Viewers").unwrap();
        let step = viewers(&settings);
        assert!(step.detail.starts_with("csv, json, sql, tsv, txt, ascii: true"), "{}", step.detail);
        // One format left to a program that isn't there: said.
        settings.viewers.insert("txt".into(), Viewer::Command("no-such-viewer-xyz".into()));
        assert_eq!(viewers(&settings).detail, "no-such-viewer-xyz isn't installed");
    }

    #[tokio::test]
    async fn a_saved_connection_whose_file_is_gone_fails() {
        let dir = tempfile::tempdir().unwrap();
        let (mut store, _) = ConnectionStore::load_from(dir.path().join("c.toml"));
        let project = store.add_project("p").unwrap();
        let path = dir.path().join("gone.db");
        let profile = crate::config::store::ConnectionProfile {
            name: "gone".into(),
            engine: crate::db::tests::file_engine(),
            host: path.to_string_lossy().into_owned(),
            port: String::new(),
            user: String::new(),
            database: String::new(),
            ssl_mode: crate::db::SslMode::Disable,
            mode: crate::db::ConnectionMode::Normal,
            transaction: None,
            ssh: String::new(),
            timeout: String::new(),
            password_command: Vec::new(),
            agents: crate::db::AgentAccess::None,
            legacy_keychain: false,
        };
        store.upsert(&project, None, profile).unwrap();
        let mut out = String::new();
        assert!(test_connections(&store, Some("p"), &mut out).await, "{out}");
        assert!(out.contains("doesn't exist") && !path.exists(), "{out}");
    }

    #[test]
    fn finds_programs_on_the_path() {
        assert!(on_path("sh"));
        assert!(!on_path("surely-not-a-real-program-podelis"));
    }

    #[test]
    fn environment_has_no_secrets() {
        let env = environment(&Settings::default());
        assert_eq!(env[0].1, env!("CARGO_PKG_VERSION"));
        assert!(env.iter().any(|(k, _)| k == "Terminal"));
    }
}
