//! Opening a MongoDB connection: a `mongodb://` (or `mongodb+srv://`) URL
//! as it's written, or one made of the form's host, port, user and
//! password; checked with a ping, so a wrong password says so now.

#![expect(clippy::unused_async, reason = "every engine has these functions with one signature, the generated dispatch calling them alike (build.rs): some needn't wait for anything")]

use std::sync::Arc;

use super::super::*;
use super::DriverConnection;
use super::driver::{self, MongoPool};

/// `s` as a URL's part: every byte but the unreserved ones (letters,
/// digits, `-._~`) as `%XX`.
fn percent_encoded(s: &str) -> String {
    use std::fmt::Write as _;
    s.bytes().fold(String::new(), |mut out, b| {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            out.push(b as char);
        } else {
            let _ = write!(out, "%{b:02X}");
        }
        out
    })
}

/// The URL for `params`: the host as it is when it's a URL already.
fn url(params: &ConnectParams, host: &str, port: u16) -> String {
    if host.starts_with("mongodb://") || host.starts_with("mongodb+srv://") {
        return host.to_string();
    }
    let enc = percent_encoded;
    let login = if params.user.is_empty() {
        String::new()
    } else if params.password.is_empty() {
        format!("{}@", enc(&params.user))
    } else {
        format!("{}:{}@", enc(&params.user), enc(&params.password))
    };
    let host = if host.contains(':') { format!("[{host}]") } else { host.to_string() };
    // A user is looked for in `admin` (where a root user is) unless the
    // URL says otherwise; TLS only when asked for (MongoDB has no "prefer").
    let mut options = vec![format!("appName={}", enc("Podėlis"))];
    if !params.user.is_empty() {
        options.push("authSource=admin".into());
    }
    if matches!(params.tls_mode(), SslMode::Require | SslMode::VerifyCa | SslMode::VerifyFull) {
        options.push("tls=true".into());
    }
    if params.tls_mode() == SslMode::Require {
        options.push("tlsAllowInvalidCertificates=true".into());
    }
    format!("mongodb://{login}{host}:{port}/?{}", options.join("&"))
}

/// The form's TLS files in the client's TLS options (which they turn on):
/// a CA to verify the server with, and a client certificate, which MongoDB
/// takes with its key in one PEM file.
fn with_tls_files(options: &mut ::mongodb::options::ClientOptions, files: &TlsFiles) -> Result<()> {
    use ::mongodb::options::{Tls, TlsOptions};
    if files.is_empty() {
        return Ok(());
    }
    if files.key.is_some() && files.cert.is_some() && files.key != files.cert {
        anyhow::bail!(
            "MongoDB takes the client certificate and its key in one PEM file: put both in one file, \
             give it as TLS cert, and leave TLS key empty"
        );
    }
    let mut tls = match options.tls.take() {
        Some(Tls::Enabled(tls)) => tls,
        _ => TlsOptions::default(),
    };
    if let Some(ca) = &files.ca {
        tls.ca_file_path = Some(TlsFiles::expanded(ca));
    }
    if let Some(both) = files.cert.as_ref().or(files.key.as_ref()) {
        tls.cert_key_file_path = Some(TlsFiles::expanded(both));
    }
    options.tls = Some(Tls::Enabled(tls));
    Ok(())
}

pub(in crate::db) async fn connect(params: &ConnectParams, host: &str, port: u16) -> Result<(Pool, Session)> {
    let mut options =
        ::mongodb::options::ClientOptions::parse(url(params, host, port)).await.context("failed to read the URL")?;
    // A URL with a user and no password: the form's (or its password
    // command's) password goes with it.
    if let Some(credential) = options.credential.as_mut()
        && credential.password.is_none()
        && !params.password.is_empty()
    {
        credential.password = Some(params.password.clone());
    }
    // Where TLS is asked for: `disable` and `prefer` (none, for MongoDB)
    // keep files a form left set from using it.
    if matches!(params.tls_mode(), SslMode::Require | SslMode::VerifyCa | SslMode::VerifyFull) {
        with_tls_files(&mut options, &params.tls)?;
    }
    options.server_selection_timeout = Some(CONNECT_TIMEOUT);
    options.connect_timeout = Some(CONNECT_TIMEOUT);
    // The database: the form's, else the URL's, else `test` (mongosh's).
    let database = Some(params.database.clone())
        .filter(|d| !d.is_empty())
        .or_else(|| options.default_database.clone())
        .unwrap_or_else(|| "test".into());
    let client = ::mongodb::Client::with_options(options).context("failed to make the client")?;
    client.database(&database).run_command(::mongodb::bson::doc! {"ping": 1}).await.context("failed to connect")?;
    let pool = MongoPool { client, database, read_only: params.read_only, interrupt: Arc::default() };
    let session = driver::acquire(&pool).await?;
    Ok((Pool::MongoDb(pool), Session::MongoDb(session)))
}

/// No server session to name: a cancel stops the call Podėlis waits on.
pub(in crate::db) async fn backend_id(_c: &mut DriverConnection) -> Result<i64> {
    Ok(0)
}

pub(in crate::db) async fn watch_interrupt(_c: &mut DriverConnection, _interrupt: &Arc<AtomicBool>) -> Result<()> {
    Ok(())
}

/// Its address answers first, so a server that's down says so at once.
pub(in crate::db) async fn check_before_connect(host: &str, port: u16) -> Result<()> {
    if host.starts_with("mongodb") {
        return Ok(());
    }
    crate::db::reach(host, port).await
}

pub(in crate::db) async fn check_reach(db: &Database) -> Result<()> {
    match &db.0.address {
        Some((host, port)) if !host.starts_with("mongodb") => crate::db::reach(host, *port).await,
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The form's fields, with no password (a test gives it one made as
    /// it runs: not a value in the code).
    #[test]
    fn tls_files_turn_tls_on_with_them_and_a_split_key_is_said() {
        use ::mongodb::options::{ClientOptions, Tls};
        let mut options = ClientOptions::default();
        let files = TlsFiles { ca: Some("/ca.pem".into()), cert: Some("/both.pem".into()), key: None };
        with_tls_files(&mut options, &files).unwrap();
        let Some(Tls::Enabled(tls)) = options.tls else { panic!("TLS on") };
        assert_eq!(tls.ca_file_path.as_deref(), Some(std::path::Path::new("/ca.pem")));
        assert_eq!(tls.cert_key_file_path.as_deref(), Some(std::path::Path::new("/both.pem")));
        let split = TlsFiles { ca: None, cert: Some("/c.pem".into()), key: Some("/k.pem".into()) };
        assert!(with_tls_files(&mut ClientOptions::default(), &split).unwrap_err().to_string().contains("one PEM file"));
        let mut none = ClientOptions::default();
        with_tls_files(&mut none, &TlsFiles::default()).unwrap();
        assert!(none.tls.is_none(), "nothing set, nothing changed");
    }

    fn params(user: &str, ssl_mode: SslMode) -> ConnectParams {
        ConnectParams {
            engine: Engine::MongoDb,
            host: "db.example.com".into(),
            port: 27017,
            user: user.into(),
            password: String::new(),
            database: String::new(),
            ssl_mode,
            tls: crate::db::TlsFiles::default(),
            read_only: false,
            ssh: None,
            password_command: Vec::new(),
            for_agents: false,
        }
    }

    #[test]
    fn its_url_from_the_form() {
        let mut p = params("app", SslMode::VerifyFull);
        // An `@` and a space in it, encoded.
        let n = std::process::id();
        p.password = format!("p@ss {n}");
        assert_eq!(
            url(&p, "db.example.com", 27017),
            format!("mongodb://app:p%40ss%20{n}@db.example.com:27017/?appName=Pod%C4%97lis&authSource=admin&tls=true")
        );
        assert_eq!(url(&params("", SslMode::Prefer), "::1", 27017), "mongodb://[::1]:27017/?appName=Pod%C4%97lis");
        let given = "mongodb+srv://u:p@cluster0.example.net/shop";
        assert_eq!(url(&p, given, 0), given, "a URL as written");
    }
}
