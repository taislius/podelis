//! What the open connection is: user, database, settings, TLS.

use super::*;

impl Database {
    /// Name/value pairs describing the session, for the connection panel.
    pub async fn session_info(&self) -> Vec<(String, String)> {
        let (sql, names) = match self.0.engine {
            Engine::Postgres => postgres::queries::SESSION_INFO,
            Engine::MySql => mysql::queries::SESSION_INFO,
            Engine::Sqlite => sqlite::queries::SESSION_INFO,
        };
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
        if let Some(tls) = self.tls_info().await {
            out.push(("TLS".to_string(), tls));
        }
        out
    }

    /// The TLS version and cipher of the session, if it's encrypted.
    pub async fn tls_info(&self) -> Option<String> {
        match self.0.engine {
            Engine::Postgres => postgres::queries::tls_info(self).await,
            Engine::MySql => mysql::queries::tls_info(self).await,
            Engine::Sqlite => None,
        }
    }
}
