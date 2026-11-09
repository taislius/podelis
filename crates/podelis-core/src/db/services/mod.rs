//! Hosted services: an engine as a provider runs it (Supabase's
//! PostgreSQL…), recognised by the connection's host. Each says what it
//! changes: connections it refuses, TLS it requires, its own schemas.
//! Shared code asks which service serves a connection and does what it
//! says, naming none; a service names the engine it runs. Adding one is a
//! file here and a line in `ALL`.

mod supabase;

use super::Engine;

/// A provider's way of running an engine.
pub trait Service: Sync {
    /// What it's called ("Supabase").
    fn name(&self) -> &'static str;
    /// Whether it serves a connection to `engine` at `host` (a name or an
    /// address, without brackets).
    fn serves(&self, engine: Engine, host: &str) -> bool;
    /// Why Podėlis won't connect to `host:port` there, if it won't.
    fn refused(&self, _host: &str, _port: u16) -> Option<String> {
        None
    }
    /// Whether it wants TLS: SSL mode `prefer` then becomes `require`.
    fn requires_tls(&self) -> bool {
        false
    }
    /// One of its own schemas: listed with the system ones, never shown to
    /// models.
    fn is_internal_schema(&self, _schema: &str) -> bool {
        false
    }
    /// Schemas AI agents are kept out of there, each with what it holds:
    /// a statement naming one (`vault.secrets`) is refused.
    fn agents_kept_out(&self) -> &'static [(&'static str, &'static str)] {
        &[]
    }
}

/// Every service Podėlis knows.
const ALL: &[&dyn Service] = &[&supabase::Supabase];

/// The service that serves a connection to `engine` at `host`, if one does.
pub fn serving(engine: Engine, host: &str) -> Option<&'static dyn Service> {
    ALL.iter().copied().find(|s| s.serves(engine, host))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A service serves its own hosts on its own engine, and says what it
    /// changes there.
    #[test]
    fn a_service_is_found_by_its_engine_and_host() {
        let supabase = serving(Engine::Postgres, "db.abcdefghijklmnop.supabase.co").unwrap();
        assert_eq!(supabase.name(), "Supabase");
        assert!(supabase.requires_tls());
        assert!(supabase.refused("aws-0-eu-central-1.pooler.supabase.com", 6543).is_some());
        assert!(supabase.refused("aws-0-eu-central-1.pooler.supabase.com", 5432).is_none());
        assert!(supabase.is_internal_schema("storage") && !supabase.is_internal_schema("auth"));
        assert!(serving(Engine::MySql, "db.abcdefghijklmnop.supabase.co").is_none(), "not its engine");
        assert!(serving(Engine::Postgres, "db.example.com").is_none());
    }
}
