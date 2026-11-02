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
        if let Some(tls) = self.tls_info().await {
            out.push(("TLS".to_string(), tls));
        } else if let Some(why) = self.tls_failed() {
            out.push(("TLS".to_string(), format!("none: TLS failed ({why}), so SSL mode prefer connected without it")));
        }
        out
    }

    /// The TLS version and cipher of the session, if it's encrypted.
    pub async fn tls_info(&self) -> Option<String> {
        on_driver!(self.0.engine, engine::queries::tls_info(self).await)
    }
}
