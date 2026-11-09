//! What the open connection is: user, database, settings, TLS.

use super::*;

impl Database {
    /// Name/value pairs describing the session, for the connection panel.
    pub async fn session_info(&self) -> Vec<(String, String)> {
        let (sql, names) = on_driver!(self.0.engine, engine::queries::SESSION_INFO);
        let mut out = vec![("Server version".to_string(), self.version().to_string())];
        if let Ok(o) = self.run(sql, 1).await
            && let Some(row) = o.rows.first()
        {
            for (name, value) in names.iter().zip(row) {
                let mut value = value.clone().unwrap_or_default();
                if *name == "Read-only" || *name == "Foreign keys" {
                    value = if matches!(value.as_str(), "on" | "1" | "true") { "yes" } else { "no" }.to_string();
                }
                out.push(((*name).to_string(), value));
            }
        }
        // How many connections are in use, and how many this user may have
        // (left out where the server won't say: not every user may read it).
        if let Some(connections) = on_driver!(self.0.engine, engine::queries::connections(self).await) {
            out.push(("Connections".to_string(), connections));
        }
        if let Some(tls) = self.tls_info().await {
            out.push(("TLS".to_string(), tls));
        } else if let Some(why) = self.tls_failed() {
            out.push(("TLS".to_string(), format!("none: TLS failed ({why}), so SSL mode prefer connected without it")));
        }
        out
    }

    /// The server's load, as its own views say it (`SqlDialect::load_queries`):
    /// each value, or why it isn't there (busy, or not readable by this
    /// user). Empty where the engine has nothing to read.
    pub async fn server_load(&self) -> Vec<(String, String)> {
        let Some(dialect) = self.0.engine.dialect() else { return Vec::new() };
        let mut out = Vec::new();
        for (what, sql) in dialect.load_queries() {
            let value = match tokio::time::timeout(std::time::Duration::from_secs(5), self.run(sql, 1)).await {
                Ok(Ok(o)) => o.rows.first().and_then(|r| r.last().cloned().flatten()).unwrap_or_else(|| "none".into()),
                Ok(Err(e)) => format!("not readable ({})", error_message(&e)),
                Err(_) => "no answer within 5 s (the session is busy)".into(),
            };
            out.push(((*what).to_string(), value));
        }
        out
    }

    /// The TLS version and cipher of the session, if it's encrypted.
    pub async fn tls_info(&self) -> Option<String> {
        on_driver!(self.0.engine, engine::queries::tls_info(self).await)
    }
}
