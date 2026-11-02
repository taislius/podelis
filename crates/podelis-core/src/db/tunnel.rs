use super::*;

/// A running `ssh -N -L` port forward.
pub(super) struct Tunnel {
    _child: Child,
}

/// Starts `ssh -L` to forward a free local port to `host:port` as seen
/// from `dest`, and waits until it accepts connections. Uses the system
/// OpenSSH so keys, agents, `~/.ssh/config` and `known_hosts` all apply;
/// `BatchMode` means it fails instead of prompting (the TUI can't answer).
pub(super) async fn open_tunnel(dest: &str, host: &str, port: u16) -> Result<(Tunnel, u16)> {
    let local = std::net::TcpListener::bind("127.0.0.1:0")?.local_addr()?.port();
    let (dest, ssh_port) = match dest.rsplit_once(':') {
        Some((d, p)) if !d.is_empty() && p.parse::<u16>().is_ok() => (d, Some(p)),
        _ => (dest, None),
    };
    let target = if host.contains(':') { format!("[{host}]") } else { host.to_string() };
    let mut cmd = Command::new("ssh");
    cmd.args(["-N", "-o", "ExitOnForwardFailure=yes", "-o", "BatchMode=yes", "-o", "ConnectTimeout=10"])
        .arg("-L")
        .arg(format!("127.0.0.1:{local}:{target}:{port}"));
    if let Some(p) = ssh_port {
        cmd.args(["-p", p]);
    }
    cmd.arg("--")
        .arg(dest)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let mut child = cmd.spawn().context("failed to start ssh (is OpenSSH installed?)")?;
    let started = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            let mut stderr = String::new();
            if let Some(mut pipe) = child.stderr.take() {
                let _ = pipe.read_to_string(&mut stderr).await;
            }
            anyhow::bail!("ssh tunnel failed ({status}): {}", stderr.trim());
        }
        if tokio::net::TcpStream::connect(("127.0.0.1", local)).await.is_ok() {
            return Ok((Tunnel { _child: child }, local));
        }
        if started.elapsed() > CONNECT_TIMEOUT {
            anyhow::bail!("ssh tunnel didn't come up within {}s", CONNECT_TIMEOUT.as_secs());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}
