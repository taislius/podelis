//! Running PostgreSQL and MySQL containers (Docker or Podman), as ready
//! connection settings: the published port, and the user and database the
//! official images were started with. The password isn't copied: the
//! connection gets a password command that reads it from the container
//! (`docker exec <name> printenv POSTGRES_PASSWORD`) each time.

use std::process::{Command, Stdio};
use std::time::Duration;

use crate::db::{ConnectionMode, Engine, SslMode};
use crate::store::ConnectionProfile;

/// A container that runs a database.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Container {
    pub name: String,
    pub image: String,
    /// Ready settings; None when the database port isn't published, so
    /// there's no address on this machine to connect to.
    pub profile: Option<ConnectionProfile>,
}

/// How long a `docker`/`podman` command may take (Docker Desktop can hang
/// while it starts).
const TIMEOUT: Duration = Duration::from_secs(15);

/// The running database containers. Blocks: call it off the async runtime.
/// Docker first, then Podman (if Docker isn't installed or not running).
pub fn find() -> Result<Vec<Container>, String> {
    let tools: Vec<&str> = ["docker", "podman"].into_iter().filter(|t| crate::doctor::on_path(t)).collect();
    let mut error = "Neither docker nor podman is installed (or on PATH)".to_string();
    for tool in tools {
        match find_with(tool) {
            Ok(found) => return Ok(found),
            Err(e) => error = e,
        }
    }
    Err(error)
}

fn find_with(tool: &str) -> Result<Vec<Container>, String> {
    let ps = run(tool, &["ps", "--format", "{{.ID}}\t{{.Names}}\t{{.Image}}\t{{.Ports}}"])?;
    let running: Vec<Ps> = ps.lines().filter_map(Ps::parse).filter(|ps| ps.engine().is_some()).collect();
    if running.is_empty() {
        return Ok(Vec::new());
    }
    // All at once, as `id env` lines; if one stopped since `ps` (inspect
    // fails then), one at a time, leaving that one out.
    let mut args = vec!["inspect", "--format", "{{.Id}} {{json .Config.Env}}"];
    args.extend(running.iter().map(|c| c.id.as_str()));
    let all = run(tool, &args).ok();
    let env_of = |ps: &Ps| -> Option<Vec<String>> {
        let line = match &all {
            Some(out) => out.lines().find(|l| l.starts_with(&ps.id))?.to_string(),
            None => run(tool, &["inspect", "--format", "{{.Id}} {{json .Config.Env}}", &ps.id]).ok()?,
        };
        let (_, env) = line.trim().split_once(' ')?;
        Some(serde_json::from_str(env).unwrap_or_default())
    };
    let mut found: Vec<Container> = running.iter().filter_map(|ps| ps.container(tool, &env_of(ps)?)).collect();
    // The ones to connect to first, by name.
    found.sort_by(|a, b| (a.profile.is_none(), &a.name).cmp(&(b.profile.is_none(), &b.name)));
    Ok(found)
}

/// Runs `tool args`, within `TIMEOUT`, and returns what it printed.
fn run(tool: &str, args: &[&str]) -> Result<String, String> {
    let child = Command::new(tool)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("couldn't run {tool}: {e}"))?;
    let pid = child.id();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || drop(tx.send(child.wait_with_output())));
    let Ok(out) = rx.recv_timeout(TIMEOUT) else {
        // No signal API without unsafe code: kill(1) does it.
        let _ = Command::new("kill").args(["-KILL", &pid.to_string()]).status();
        return Err(format!("{tool} didn't answer within {} seconds", TIMEOUT.as_secs()));
    };
    let out = out.map_err(|e| format!("couldn't run {tool}: {e}"))?;
    if !out.status.success() {
        let said = String::from_utf8_lossy(&out.stderr);
        let said = said.trim().lines().next().unwrap_or_default();
        if said.to_lowercase().contains("cannot connect") || said.contains("daemon") {
            return Err(format!("{tool} isn't running ({said})"));
        }
        return Err(format!("{tool} {} failed: {said}", args[0]));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// One line of `docker ps`.
#[derive(Debug, PartialEq, Eq)]
struct Ps {
    id: String,
    name: String,
    image: String,
    ports: String,
}

impl Ps {
    fn parse(line: &str) -> Option<Self> {
        let mut fields = line.splitn(4, '\t');
        let (id, name, image) = (fields.next()?, fields.next()?, fields.next()?);
        let ports = fields.next().unwrap_or_default();
        Some(Self { id: id.into(), name: name.into(), image: image.into(), ports: ports.into() })
    }

    /// The database it runs, if any: by the port it listens on (5432 or
    /// 3306), which works for images built on the official ones too, or by
    /// an official image's name (`postgres`, `bitnami/mysql:8`, …; not
    /// `postgres-exporter`).
    fn engine(&self) -> Option<Engine> {
        let listens =
            |port: u16| self.ports.split(", ").any(|p| p == format!("{port}/tcp") || published_one(p, port).is_some());
        let repo = self.image.rsplit('/').next().unwrap_or_default();
        let repo = repo.split([':', '@']).next().unwrap_or_default().to_lowercase();
        if listens(5432) || matches!(repo.as_str(), "postgres" | "postgis" | "timescaledb" | "timescaledb-ha") {
            Some(Engine::Postgres)
        } else if listens(3306) || matches!(repo.as_str(), "mysql" | "mariadb" | "percona-server" | "mysql-server") {
            Some(Engine::MySql)
        } else {
            None
        }
    }

    fn container(&self, tool: &str, env: &[String]) -> Option<Container> {
        let engine = self.engine()?;
        let internal = if engine == Engine::Postgres { 5432 } else { 3306 };
        let profile = published(&self.ports, internal).map(|(host, port)| {
            let var =
                |name: &str| env.iter().find_map(|e| e.strip_prefix(name)?.strip_prefix('=')).filter(|v| !v.is_empty());
            let (user, database, password) = match engine {
                Engine::Postgres => postgres_login(&var),
                _ => mysql_login(&var),
            };
            let password_command = match password {
                Some(Secret::Var(name)) => words([tool, "exec", &self.name, "printenv", name]),
                Some(Secret::File(path)) => words([tool, "exec", &self.name, "cat", &path]),
                None => Vec::new(),
            };
            ConnectionProfile {
                name: self.name.clone(),
                engine,
                host,
                port: port.to_string(),
                user,
                database,
                ssl_mode: SslMode::default(),
                mode: ConnectionMode::Normal,
                transaction: None,
                ssh: String::new(),
                timeout: String::new(),
                agents: crate::db::AgentAccess::None,
                password_command,
                legacy_keychain: false,
            }
        });
        Some(Container { name: self.name.clone(), image: self.image.clone(), profile })
    }
}

/// Where the container's password is: an environment variable, or a file
/// inside it (`*_PASSWORD_FILE`, as with Docker secrets).
#[derive(Debug, PartialEq, Eq)]
enum Secret {
    Var(&'static str),
    File(String),
}

/// The official postgres image's user, database and password.
fn postgres_login<'a>(var: &impl Fn(&str) -> Option<&'a str>) -> (String, String, Option<Secret>) {
    let user = var("POSTGRES_USER").unwrap_or("postgres").to_string();
    let database = var("POSTGRES_DB").map_or_else(|| user.clone(), str::to_string);
    let password = if var("POSTGRES_HOST_AUTH_METHOD") == Some("trust") {
        None
    } else if var("POSTGRES_PASSWORD").is_some() {
        Some(Secret::Var("POSTGRES_PASSWORD"))
    } else {
        var("POSTGRES_PASSWORD_FILE").map(|f| Secret::File(f.to_string()))
    };
    (user, database, password)
}

/// The official mysql and mariadb images': their own user if one was
/// made, else root.
fn mysql_login<'a>(var: &impl Fn(&str) -> Option<&'a str>) -> (String, String, Option<Secret>) {
    let either = |mysql: &'static str, mariadb: &'static str| {
        var(mysql).map(|v| (mysql, v)).or_else(|| var(mariadb).map(|v| (mariadb, v)))
    };
    let database = either("MYSQL_DATABASE", "MARIADB_DATABASE").map(|(_, v)| v.to_string()).unwrap_or_default();
    if let Some((_, user)) = either("MYSQL_USER", "MARIADB_USER") {
        let password = match either("MYSQL_PASSWORD", "MARIADB_PASSWORD") {
            Some((name, _)) => Some(Secret::Var(name)),
            None => either("MYSQL_PASSWORD_FILE", "MARIADB_PASSWORD_FILE").map(|(_, f)| Secret::File(f.to_string())),
        };
        if password.is_some() {
            return (user.to_string(), database, password);
        }
    }
    let password = match either("MYSQL_ROOT_PASSWORD", "MARIADB_ROOT_PASSWORD") {
        Some((name, _)) => Some(Secret::Var(name)),
        None => {
            either("MYSQL_ROOT_PASSWORD_FILE", "MARIADB_ROOT_PASSWORD_FILE").map(|(_, f)| Secret::File(f.to_string()))
        }
    };
    ("root".to_string(), database, password)
}

/// Where `internal` is published on this machine, from `docker ps`'s
/// ports (`0.0.0.0:5433->5432/tcp, [::]:5433->5432/tcp`).
fn published(ports: &str, internal: u16) -> Option<(String, u16)> {
    ports.split(", ").find_map(|mapping| published_one(mapping, internal))
}

/// One mapping: `address:port->port/tcp`, or a range of them
/// (`0.0.0.0:6000-6001->5432-5433/tcp`).
fn published_one(mapping: &str, internal: u16) -> Option<(String, u16)> {
    let (outside, inside) = mapping.split_once("->")?;
    let inside = inside.strip_suffix("/tcp")?;
    let range = |text: &str| -> Option<(u16, u16)> {
        match text.split_once('-') {
            Some((a, b)) => Some((a.parse().ok()?, b.parse().ok()?)),
            None => text.parse().ok().map(|p| (p, p)),
        }
    };
    let (first, last) = range(inside)?;
    if !(first..=last).contains(&internal) {
        return None;
    }
    let (address, ports) = outside.rsplit_once(':')?;
    let port = range(ports)?.0.checked_add(internal - first)?;
    // Published on every address (or this machine's): localhost.
    let host = match address.trim_start_matches('[').trim_end_matches(']') {
        "" | "0.0.0.0" | "::" | "127.0.0.1" | "::1" => "localhost",
        other => other,
    };
    Some((host.to_string(), port))
}

fn words<const N: usize>(w: [&str; N]) -> Vec<String> {
    w.iter().map(|s| (*s).to_string()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ps(line: &str) -> Ps {
        Ps::parse(line).unwrap()
    }

    fn env(vars: &[&str]) -> Vec<String> {
        vars.iter().map(|v| (*v).to_string()).collect()
    }

    #[test]
    fn finds_published_ports() {
        let ports = "0.0.0.0:5433->5432/tcp, [::]:5433->5432/tcp";
        assert_eq!(published(ports, 5432), Some(("localhost".to_string(), 5433)));
        assert_eq!(published("192.168.1.5:3307->3306/tcp", 3306), Some(("192.168.1.5".to_string(), 3307)));
        assert_eq!(published("3306/tcp, 33060/tcp", 3306), None);
        assert_eq!(published("", 5432), None);
        assert_eq!(published("0.0.0.0:15432->15432/tcp", 5432), None);
        // A range: the matching port in it.
        assert_eq!(published("0.0.0.0:6000-6001->5432-5433/tcp", 5433), Some(("localhost".to_string(), 6001)));
    }

    #[test]
    fn a_postgres_container() {
        let c = ps("abc\tshop-db\tpostgres:17\t0.0.0.0:5433->5432/tcp")
            .container("docker", &env(&["POSTGRES_USER=app", "POSTGRES_PASSWORD=x", "PATH=/bin"]))
            .unwrap();
        let p = c.profile.unwrap();
        assert_eq!((p.engine, p.host.as_str(), p.port.as_str()), (Engine::Postgres, "localhost", "5433"));
        assert_eq!((p.user.as_str(), p.database.as_str()), ("app", "app"));
        assert_eq!(p.password_command, ["docker", "exec", "shop-db", "printenv", "POSTGRES_PASSWORD"]);
        // Trust: no password at all.
        let c = ps("abc\tdev\tpostgres:17\t0.0.0.0:5432->5432/tcp")
            .container("podman", &env(&["POSTGRES_HOST_AUTH_METHOD=trust", "POSTGRES_PASSWORD=x"]))
            .unwrap();
        assert!(c.profile.unwrap().password_command.is_empty());
    }

    #[test]
    fn a_mysql_container() {
        let line = "abc\tmy\tmysql:9\t0.0.0.0:3306->3306/tcp, 33060/tcp";
        let p = ps(line).container("docker", &env(&["MYSQL_ROOT_PASSWORD=r", "MYSQL_DATABASE=shop"])).unwrap();
        let p = p.profile.unwrap();
        assert_eq!((p.engine, p.user.as_str(), p.database.as_str()), (Engine::MySql, "root", "shop"));
        assert_eq!(p.password_command[4], "MYSQL_ROOT_PASSWORD");
        // Its own user, if the image made one.
        let vars = env(&["MARIADB_USER=app", "MARIADB_PASSWORD=p", "MARIADB_ROOT_PASSWORD=r"]);
        let p = ps("abc\tmaria\tmariadb:11\t0.0.0.0:3307->3306/tcp").container("docker", &vars).unwrap();
        let p = p.profile.unwrap();
        assert_eq!((p.user.as_str(), p.password_command[4].as_str()), ("app", "MARIADB_PASSWORD"));
        // The image's own user with its password in a file.
        let vars = env(&["MYSQL_USER=app", "MYSQL_PASSWORD_FILE=/run/secrets/pw", "MYSQL_RANDOM_ROOT_PASSWORD=yes"]);
        let p = ps(line).container("docker", &vars).unwrap().profile.unwrap();
        assert_eq!(
            (p.user.as_str(), &p.password_command[3..]),
            ("app", &["cat".to_string(), "/run/secrets/pw".to_string()][..])
        );
        // A password file (Docker secrets).
        let p = ps(line).container("docker", &env(&["MYSQL_ROOT_PASSWORD_FILE=/run/secrets/db"])).unwrap();
        assert_eq!(p.profile.unwrap().password_command[3..], ["cat", "/run/secrets/db"]);
    }

    #[test]
    fn others_and_unpublished_ones() {
        assert!(ps("abc\tweb\tnginx\t0.0.0.0:80->80/tcp").container("docker", &[]).is_none());
        // Exporters and look-alike ports aren't databases.
        let exporter = "abc\tpg-exp\tprometheuscommunity/postgres-exporter\t0.0.0.0:9187->9187/tcp";
        assert!(ps(exporter).container("docker", &[]).is_none());
        assert!(ps("abc\tx\tother\t0.0.0.0:15432->15432/tcp").container("docker", &[]).is_none());
        // An official image by name, from another registry.
        assert!(ps("abc\tdb\tdocker.io/bitnami/mysql:8\t").container("docker", &[]).is_some());
        let c = ps("abc\tinternal\tpostgres:17\t5432/tcp").container("docker", &[]).unwrap();
        assert!(c.profile.is_none(), "no address on this machine");
    }
}
