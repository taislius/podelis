//! Supabase: PostgreSQL, reached directly (`db.<ref>.supabase.co:5432`)
//! or through its pooler (`aws-<n>-<region>.pooler.supabase.com`, user
//! `postgres.<ref>`; session mode on 5432, transaction mode on 6543).
//!
//! What's different from any PostgreSQL:
//! - TLS is wanted: SSL mode `prefer` (the default) becomes `require`.
//! - The transaction poolers (port 6543, the shared one and the dedicated
//!   `PgBouncer`) hand each transaction a different server connection. A
//!   SQL client needs one session: `SET`, `BEGIN` across runs, temporary
//!   tables, cancelling by the server's process id, and the driver's named
//!   statements (which another client's could collide with there). So
//!   Podėlis refuses them and says to use session mode.
//! - Supabase's own schemas (`storage`, `realtime`, `vault`…) are system
//!   ones: in the navigator's system section, never shown to models.
//!   `auth` isn't: people query `auth.users`.

use super::Service;
use crate::db::Engine;

/// Supabase, as a [`Service`].
pub struct Supabase;

impl Service for Supabase {
    fn name(&self) -> &'static str {
        "Supabase"
    }

    fn serves(&self, engine: Engine, host: &str) -> bool {
        engine == Engine::Postgres && is_supabase(host)
    }

    fn refused(&self, host: &str, port: u16) -> Option<String> {
        refused(host, port)
    }

    fn requires_tls(&self) -> bool {
        true
    }

    fn is_internal_schema(&self, schema: &str) -> bool {
        is_internal_schema(schema)
    }
}

/// A host of Supabase's: a project's database or the pooler.
fn is_supabase(host: &str) -> bool {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    host.ends_with(".supabase.co") || host.ends_with(".supabase.com")
}

/// Why Podėlis won't connect to `host:port`, if it's one of Supabase's
/// transaction poolers.
fn refused(host: &str, port: u16) -> Option<String> {
    if port != 6543 || !is_supabase(host) {
        return None;
    }
    Some(format!(
        "{host}:6543 is Supabase's transaction pooler: each transaction may run on another server \
         connection, and a SQL client needs one session (SET, transactions across runs, cancelling a \
         query)\nhint: use port 5432 on the same host (the pooler's session mode, same user and \
         password), or the direct connection, db.<project-ref>.supabase.co:5432"
    ))
}

/// Supabase's own schemas, made in every project.
const INTERNAL: &[&str] = &[
    "_analytics",
    "_realtime",
    "_supavisor",
    "cron",
    "extensions",
    "graphql",
    "graphql_public",
    "net",
    "pgbouncer",
    "pgsodium",
    "pgsodium_masks",
    "realtime",
    "storage",
    "supabase_functions",
    "supabase_migrations",
    "vault",
];

/// One of Supabase's own schemas.
fn is_internal_schema(schema: &str) -> bool {
    INTERNAL.contains(&schema)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn supabase_hosts_are_known() {
        assert!(is_supabase("db.abcdefghijklmnop.supabase.co"));
        assert!(is_supabase("aws-0-eu-central-1.pooler.supabase.com"));
        assert!(is_supabase("AWS-1-us-east-1.pooler.supabase.com."));
        assert!(!is_supabase("supabase.co.example.com"));
        assert!(!is_supabase("db.example.com"));
    }

    #[test]
    fn transaction_poolers_are_refused_with_the_session_port() {
        let why = refused("aws-0-eu-central-1.pooler.supabase.com", 6543).unwrap();
        assert!(why.contains("use port 5432 on the same host"), "{why}");
        assert!(refused("db.abcdefghijklmnop.supabase.co", 6543).is_some());
        assert!(refused("aws-0-eu-central-1.pooler.supabase.com", 5432).is_none());
        assert!(refused("db.example.com", 6543).is_none());
    }

    #[test]
    fn its_own_schemas_are_system_but_auth_and_public_arent() {
        assert!(is_internal_schema("storage") && is_internal_schema("vault"));
        assert!(!is_internal_schema("auth") && !is_internal_schema("public"));
    }
}
