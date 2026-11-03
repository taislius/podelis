//! MongoDB: documents in collections, and the first engine whose language
//! isn't SQL. Its language (`language.rs`) is mongosh's shape, built even
//! without its driver, like a dialect; its driver is the official
//! `mongodb` crate (`driver.rs`): documents become rows.

mod agent_constraints;
pub(super) mod language;

#[cfg(feature = "mongodb")]
pub(super) mod args;
#[cfg(feature = "mongodb")]
pub(super) mod connect;
#[cfg(all(any(test, feature = "test-support"), feature = "mongodb"))]
pub(in crate::db) mod db_tests;
#[cfg(feature = "mongodb")]
pub(super) mod driver;
#[cfg(feature = "mongodb")]
pub(super) mod passwords;
#[cfg(feature = "mongodb")]
pub(super) mod queries;

use super::backend::{Backend, Capabilities};

pub struct MongoDb;

/// What `db::Pool` and `db::Session` hold for this engine (see build.rs).
#[cfg(feature = "mongodb")]
pub(super) type DriverPool = driver::MongoPool;
#[cfg(feature = "mongodb")]
pub(super) type DriverConnection = driver::MongoConnection;

/// This engine, for `Engine::backend` (it has no SQL dialect).
pub(super) static DIALECT: MongoDb = MongoDb;

impl Backend for MongoDb {
    /// Its values' types, as `driver::table` names them (mongosh's): a
    /// `date` is a moment, not a calendar day (so it matches a SQL
    /// `timestamptz`, not a `date`, in a comparison); a name of its own is
    /// its own, never read as the SQL standard's.
    fn type_kind(&self, ty: &str) -> Option<crate::db::TypeKind> {
        use crate::db::TypeKind as Kind;
        Some(match ty {
            "int" => Kind::Int(4),
            "long" => Kind::Int(8),
            "double" => Kind::Float8,
            "decimal" => Kind::Decimal(None),
            "bool" => Kind::Bool,
            "date" => Kind::TimestampTz,
            "string" => Kind::Text,
            "object" | "array" => Kind::Json,
            other => Kind::Other(other.to_string()),
        })
    }
    fn auth_failed(&self, message: &str) -> bool {
        message.contains("scram failure") || message.contains("(authenticationfailed)")
    }
    fn language(&self) -> crate::db::Language {
        crate::db::Language::Other(&language::Mongo)
    }
    /// `buildInfo`'s first field is the version.
    fn version_query(&self) -> &'static str {
        "db.runCommand({buildInfo: 1})"
    }
    fn ping(&self) -> &'static str {
        "db.runCommand({ping: 1})"
    }
    fn label(&self) -> &'static str {
        "MongoDB"
    }
    fn url_schemes(&self) -> &'static [&'static str] {
        &["mongodb", "mongodb+srv"]
    }
    fn default_port(&self) -> &'static str {
        "27017"
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            file_based: false,
            cancel: true,
            activity: false,
            schemas: true,
            routines: false,
            server_txn_state: false,
            ddl_commits: false,
            stores_create_sql: false,
            table_sizes: false,
            plan_as_grid: false,
            copy_in: false,
        }
    }
    /// Its own collections (`system.*`) and databases (`admin`, `config`,
    /// `local`).
    fn is_system_table(&self, schema: &str, table: &str) -> bool {
        table.starts_with("system.") || matches!(schema, "admin" | "config" | "local")
    }
    fn agent_constraints(&self) -> &'static crate::db::agent_constraints::AgentConstraints {
        &agent_constraints::AGENT_CONSTRAINTS
    }
    fn connect_hint(&self, message: &str) -> Option<&'static str> {
        let has = |s: &str| message.contains(s);
        // Its own wording, not another engine's "password authentication
        // failed": `SCRAM failure`, `(AuthenticationFailed)`.
        Some(if has("scram failure") || has("(authenticationfailed)") {
            "the user or password is wrong, or the user is defined in another database: say which in the URL (authSource=admin, the default for a root user)"
        } else if has("server selection timeout") {
            "no server answered in time: check the host and port, and that a replica set's name (replicaSet=) is right"
        } else {
            return None;
        })
    }
    fn container_images(&self) -> &'static [&'static str] {
        &["mongo", "mongodb-community-server"]
    }
    /// The root user `MONGO_INITDB_ROOT_USERNAME` made, which logs in on
    /// `admin`; none without it (no authentication).
    fn container_login(&self, var: &dyn Fn(&str) -> Option<String>) -> Option<crate::db::ContainerLogin> {
        use crate::db::ContainerSecret as Secret;
        let user = var("MONGO_INITDB_ROOT_USERNAME")?;
        let password = if var("MONGO_INITDB_ROOT_PASSWORD").is_some() {
            Some(Secret::Var("MONGO_INITDB_ROOT_PASSWORD"))
        } else {
            var("MONGO_INITDB_ROOT_PASSWORD_FILE").map(Secret::File)
        };
        let database = var("MONGO_INITDB_DATABASE").unwrap_or_else(|| "test".to_string());
        Some(crate::db::ContainerLogin { user, database, password })
    }
}

#[cfg(test)]
mod tests {
    use crate::db::Engine;

    #[test]
    fn it_has_no_sql() {
        assert!(Engine::MongoDb.dialect().is_none());
        assert!(crate::sql::format("db.a.find()", Engine::MongoDb).is_none());
        assert_eq!(crate::db::sql_literal("it's \"x\"", Engine::MongoDb), r#""it's \"x\"""#, "JSON's string");
        assert_eq!(Engine::MongoDb.quote_ident("my orders"), "my orders");
    }

    /// The `sql` module's entry points ask its language.
    #[test]
    fn its_statements_through_the_shared_entry_points() {
        let text = "db.a.find({x: 1})\ndb.b.deleteMany({})";
        assert_eq!(crate::sql::split_statements(text, Engine::MongoDb), ["db.a.find({x: 1})", "db.b.deleteMany({})"]);
        let infos = crate::sql::analyze::analyze(&crate::sql::parse::parse(text, Engine::MongoDb));
        let kinds: Vec<_> = infos.iter().map(|i| (i.kind, i.risk)).collect();
        use crate::sql::analyze::Kind;
        assert_eq!(kinds, [(Kind::Read, None), (Kind::Write, Some("deleteMany with an empty filter"))]);
        assert!(crate::sql::is_keyword("find", Engine::MongoDb) && !crate::sql::is_keyword("select", Engine::MongoDb));
    }

    /// Its type names are its own: a `date` is a moment, so it compares
    /// with and translates to a SQL `timestamptz`, not a `date`.
    #[test]
    fn its_types_in_a_comparison() {
        use crate::db::{same_type, translate_type};
        assert!(same_type(Engine::MongoDb, "date", Engine::Postgres, "timestamptz"));
        assert!(!same_type(Engine::MongoDb, "date", Engine::Postgres, "date"));
        assert_eq!(translate_type(Engine::MongoDb, "long", Engine::Postgres).as_deref(), Some("bigint"));
        assert_eq!(translate_type(Engine::MongoDb, "objectId", Engine::Postgres), None, "nothing like it");
    }

    #[test]
    fn a_container_s_root_user() {
        let env = |k: &str| match k {
            "MONGO_INITDB_ROOT_USERNAME" => Some("root".to_string()),
            "MONGO_INITDB_ROOT_PASSWORD" => Some("secret".to_string()),
            _ => None,
        };
        let login = crate::db::Engine::MongoDb.backend().container_login(&env).unwrap();
        assert_eq!((login.user.as_str(), login.database.as_str()), ("root", "test"));
        assert!(crate::db::Engine::MongoDb.backend().container_login(&|_| None).is_none(), "no authentication");
        let hint = |m: &str| crate::db::Engine::MongoDb.backend().connect_hint(m);
        assert!(hint("command failed with error 18 (authenticationfailed): 'authentication failed.'").is_some());
        assert!(hint("password authentication failed for user \"app\"").is_none(), "PostgreSQL's");
    }
}
