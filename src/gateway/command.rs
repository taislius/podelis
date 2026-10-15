//! `podelis gateway [status | start | stop]`: running it in the
//! foreground, and asking about a background one. Printing is fine here,
//! as in `cli.rs`: no TUI is running.

use std::io::Write as _;
use std::path::Path;

use serde_json::json;

use super::client::{CallError, Client, connect_or_start, spawn_gateway};
use super::server::{Ended, Options, serve};
use crate::cli::GatewayCommand;

/// Does `command` for `config_dir`; returns the exit code.
pub async fn run(command: GatewayCommand, config_dir: &Path) -> i32 {
    match command {
        GatewayCommand::Run => foreground(config_dir).await,
        GatewayCommand::Status => match Client::connect(config_dir, "podelis gateway status").await {
            Ok(mut client) => status(&mut client).await,
            Err(CallError::Io(_)) => say("The gateway isn't running (it starts when a feature needs it).", 1),
            Err(e) => fail(&e),
        },
        GatewayCommand::Start => {
            let start = || spawn_gateway(config_dir);
            match connect_or_start(config_dir, "podelis gateway start", start).await {
                Ok(mut client) => status(&mut client).await,
                Err(e) => fail(&e),
            }
        }
        GatewayCommand::Stop => match Client::connect(config_dir, "podelis gateway stop").await {
            Ok(mut client) => match client.call("shutdown", json!({})).await {
                Ok(_) => say("The gateway is stopping.", 0),
                Err(e) => fail(&e),
            },
            Err(CallError::Io(_)) => say("The gateway isn't running.", 0),
            Err(e) => fail(&e),
        },
    }
}

/// Runs the gateway here until Ctrl+C, `stop` or the idle time.
async fn foreground(config_dir: &Path) -> i32 {
    let (settings, _) = crate::settings::Settings::load(config_dir);
    let idle = super::idle_from_minutes(settings.gateway_idle);
    let socket = super::socket_path(config_dir);
    // Ctrl+C, or SIGTERM from `kill`, launchd or systemd: stop cleanly
    // (the socket file goes).
    let interrupt = async {
        use tokio::signal::unix::{SignalKind, signal};
        match signal(SignalKind::terminate()) {
            Ok(mut term) => {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {}
                    _ = term.recv() => {}
                }
            }
            Err(_) => {
                let _ = tokio::signal::ctrl_c().await;
            }
        }
    };
    let mut err = std::io::stderr();
    let _ = writeln!(err, "Podėlis gateway on {} (Ctrl+C stops it).", socket.display());
    match serve(config_dir, Options::new(idle), interrupt).await {
        Ok(Ended::AlreadyRunning) => {
            let _ = writeln!(err, "A gateway already runs for {}.", config_dir.display());
            0
        }
        Ok(Ended::Stopped(reason)) => {
            let _ = writeln!(err, "The gateway stopped ({reason}).");
            0
        }
        Err(e) => {
            let _ = writeln!(err, "The gateway couldn't start: {e}");
            1
        }
    }
}

async fn status(client: &mut Client) -> i32 {
    let s = match client.call("status", json!({})).await {
        Ok(s) => s,
        Err(e) => return fail(&e),
    };
    let idle = s["idle_stop_secs"]
        .as_u64()
        .map_or_else(|| "keeps running".to_string(), |secs| format!("stops after {} min with no client", secs / 60));
    let text = format!(
        "The gateway runs: version {}, process {}, up {} s, {} other client(s); {idle}.",
        s["version"].as_str().unwrap_or("?"),
        s["pid"],
        s["uptime_secs"],
        // Not counting this one.
        s["clients"].as_u64().unwrap_or(1).saturating_sub(1),
    );
    say(&text, 0)
}

fn say(text: &str, code: i32) -> i32 {
    let _ = writeln!(std::io::stdout(), "{text}");
    code
}

fn fail(e: &CallError) -> i32 {
    let _ = writeln!(std::io::stderr(), "{e}");
    1
}
