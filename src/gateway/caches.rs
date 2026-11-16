//! What the gateway keeps between calls: schemas loaded (by project id
//! and connection name), each project's open memory, and what models'
//! `key_command`s printed (by model and command), so a password manager
//! isn't asked on every question. What's past its time is swept out on
//! the server's tick, not only replaced when it's next asked for: a key
//! doesn't stay in memory longer than it's used.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use crate::config::store::{ConnectionProfile, ProjectId};
use podelis_ai::context::Schema;
use podelis_ai::memory::MemoryError;
use podelis_ai::ontology::Ontology;

/// How long a loaded schema is used before it's loaded again.
pub(super) const SCHEMA_TTL: Duration = Duration::from_secs(300);

/// How long a key a `key_command` printed is used before it's run again.
const KEY_TTL: Duration = Duration::from_mins(15);

/// A project's memory, open.
pub(super) type Memory = Arc<tokio::sync::Mutex<sqlx::SqliteConnection>>;

/// A schema's key: its project and the connection's name.
pub(super) type SchemaKey = (ProjectId, String);

/// The lock one load of a schema holds, with when and how the last one
/// failed: what a question that waited for it is answered with.
pub(super) type Loading = tokio::sync::Mutex<Option<(Instant, String)>>;

#[derive(Default)]
pub struct Caches {
    schemas: Mutex<HashMap<SchemaKey, Loaded>>,
    /// One load of a connection's schema at a time: two questions at once
    /// read the catalog once, the second taking what the first loaded.
    loading: Mutex<HashMap<SchemaKey, Arc<Loading>>>,
    /// Each schema's ontology with the team's other names in it, and the
    /// names: built again only when they (or the schema) change, not for
    /// every question.
    aliased: Mutex<HashMap<SchemaKey, Aliased>>,
    memories: Mutex<HashMap<ProjectId, Memory>>,
    keys: Keys,
}

/// Keys `key_command`s printed: by model and command, with when. Shared
/// with the blocking task that runs the command.
#[derive(Clone, Default)]
pub(super) struct Keys(Arc<Mutex<KeyMap>>);

/// (model, command) → (when, key).
type KeyMap = HashMap<(String, String), (Instant, String)>;

impl Keys {
    /// What model `name`'s `command` printed, while it's fresh.
    pub(super) fn get(&self, name: &str, command: &str) -> Option<String> {
        let keys = lock(&self.0);
        keys.get(&(name.to_string(), command.to_string()))
            .filter(|(at, _)| at.elapsed() < KEY_TTL)
            .map(|(_, k)| k.clone())
    }

    pub(super) fn keep(&self, name: &str, command: &str, key: &str) {
        lock(&self.0).insert((name.to_string(), command.to_string()), (Instant::now(), key.to_string()));
    }

    /// A refused key: the next call runs the model's `key_command` again.
    pub(super) fn forget(&self, model: &str) {
        lock(&self.0).retain(|(name, _), _| name != model);
    }

    fn sweep(&self) {
        lock(&self.0).retain(|_, (at, _)| at.elapsed() < KEY_TTL);
    }
}

/// A schema, when it was loaded, and the connection's settings then
/// (changed ones load it again).
#[derive(Clone)]
pub(super) struct Loaded {
    pub(super) profile: ConnectionProfile,
    pub(super) at: Instant,
    pub(super) schema: Arc<Schema>,
    /// The schema's ontology, built once with it.
    pub(super) catalog: Arc<Ontology>,
}

struct Aliased {
    /// The catalog it was built from (a schema loaded again is another).
    from: Arc<Ontology>,
    names: Vec<(String, String)>,
    with: Arc<Ontology>,
}

/// A lock's contents, even after a panic elsewhere held it: they're caches.
fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Caches {
    /// The schema kept for `key`, if it's for `profile` and still fresh.
    pub(super) fn schema(&self, key: &SchemaKey, profile: &ConnectionProfile) -> Option<Loaded> {
        lock(&self.schemas).get(key).filter(|l| &l.profile == profile && l.at.elapsed() < SCHEMA_TTL).cloned()
    }

    pub(super) fn keep_schema(&self, key: SchemaKey, loaded: Loaded) {
        lock(&self.schemas).insert(key, loaded);
    }

    /// `catalog` with the other names among `facts`: kept for `key` while
    /// the names and the catalog stay the same; `catalog` itself when
    /// there are none.
    pub(super) fn aliased(
        &self,
        key: &SchemaKey,
        catalog: &Arc<Ontology>,
        facts: &[podelis_ai::memory::Fact],
    ) -> Arc<Ontology> {
        let names = Ontology::alias_names(facts);
        if names.is_empty() {
            return Arc::clone(catalog);
        }
        if let Some(kept) = lock(&self.aliased).get(key).filter(|a| Arc::ptr_eq(&a.from, catalog) && a.names == names) {
            return Arc::clone(&kept.with);
        }
        let with = Arc::new(catalog.with_aliases(facts).into_owned());
        let kept = Aliased { from: Arc::clone(catalog), names, with: Arc::clone(&with) };
        lock(&self.aliased).insert(key.clone(), kept);
        with
    }

    /// The lock that one load of `key`'s schema holds.
    pub(super) fn loading(&self, key: &SchemaKey) -> Arc<Loading> {
        Arc::clone(lock(&self.loading).entry(key.clone()).or_default())
    }

    /// The project's memory: opened (and set up) once, then kept.
    pub(super) async fn memory(&self, config_dir: &Path, project_id: &ProjectId) -> Result<Memory, MemoryError> {
        if let Some(conn) = lock(&self.memories).get(project_id) {
            return Ok(Arc::clone(conn));
        }
        // Opened without holding the list: other projects' memories are
        // used meanwhile. Two opening it at once: the first kept wins.
        let opened =
            Arc::new(tokio::sync::Mutex::new(podelis_ai::memory::open(config_dir, project_id.as_str()).await?));
        Ok(Arc::clone(lock(&self.memories).entry(project_id.clone()).or_insert(opened)))
    }

    /// After `e`: if the memory's file failed (not a wrong fact or folder),
    /// the next call opens it again, as it may have been moved or replaced.
    pub(super) fn memory_failed(&self, project_id: &ProjectId, e: &MemoryError) {
        if matches!(e, MemoryError::Storage(_)) {
            lock(&self.memories).remove(project_id);
        }
    }

    pub(super) fn keys(&self) -> Keys {
        self.keys.clone()
    }

    /// Drops what's past its time, and the load locks no one holds.
    pub(super) fn sweep(&self) {
        self.keys.sweep();
        lock(&self.schemas).retain(|_, loaded| loaded.at.elapsed() < SCHEMA_TTL);
        // Built from a schema no longer kept: not kept either.
        let schemas = lock(&self.schemas);
        lock(&self.aliased).retain(|key, a| schemas.get(key).is_some_and(|l| Arc::ptr_eq(&l.catalog, &a.from)));
        drop(schemas);
        lock(&self.loading).retain(|_, held| Arc::strong_count(held) > 1);
    }

    /// How many keys are kept (for tests).
    #[cfg(test)]
    pub(super) fn kept_keys(&self) -> usize {
        lock(&self.keys.0).len()
    }

    /// Ages every kept key and schema by `by` (for tests).
    #[cfg(test)]
    pub(super) fn age(&self, by: Duration) {
        for (at, _) in lock(&self.keys.0).values_mut() {
            *at -= by;
        }
        for loaded in lock(&self.schemas).values_mut() {
            loaded.at -= by;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_past_their_time_are_swept_out() {
        let caches = Caches::default();
        caches.keys().keep("claude", "pass show claude", "sk-1");
        assert_eq!(caches.keys().get("claude", "pass show claude").as_deref(), Some("sk-1"));
        caches.sweep();
        assert_eq!(caches.kept_keys(), 1, "fresh: kept");
        caches.age(KEY_TTL);
        assert_eq!(caches.keys().get("claude", "pass show claude"), None, "stale: not used");
        caches.sweep();
        assert_eq!(caches.kept_keys(), 0, "and not kept in memory either");
    }

    #[test]
    fn the_catalog_with_the_teams_names_is_built_once_while_they_stay_the_same() {
        let fact = |subject: &str, value: &str| podelis_ai::memory::Fact {
            id: podelis_ai::memory::FactId::from_written(1),
            subject: subject.into(),
            relation: podelis_ai::memory::ALSO_CALLED.into(),
            value: value.into(),
            source: "user".into(),
            confirmed: true,
            at: String::new(),
            database: String::new(),
        };
        let caches = Caches::default();
        let key: SchemaKey = ("p".into(), "shop".into());
        let catalog = Arc::new(Ontology::default());
        assert!(Arc::ptr_eq(&caches.aliased(&key, &catalog, &[]), &catalog), "no names: the catalog itself");
        let names = [fact("revenue", "sales")];
        let first = caches.aliased(&key, &catalog, &names);
        assert!(!Arc::ptr_eq(&first, &catalog));
        assert!(Arc::ptr_eq(&caches.aliased(&key, &catalog, &names), &first), "the same names: kept");
        let other = caches.aliased(&key, &catalog, &[fact("revenue", "turnover")]);
        assert!(!Arc::ptr_eq(&other, &first), "another name: built again");
        let reloaded = Arc::new(Ontology::default());
        assert!(!Arc::ptr_eq(&caches.aliased(&key, &reloaded, &names), &first), "a schema loaded again: too");
    }

    #[test]
    fn a_refused_key_is_forgotten_for_that_model_only() {
        let keys = Caches::default().keys();
        keys.keep("claude", "a", "1");
        keys.keep("gpt", "b", "2");
        keys.forget("claude");
        assert_eq!((keys.get("claude", "a"), keys.get("gpt", "b").as_deref()), (None, Some("2")));
    }
}
