//! Saved queries: named, optionally grouped, per project; searchable by
//! name, group and SQL. Stored as JSON next to the history files.

use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::config::store::write_atomic;

/// The group whose saved queries autocomplete offers as snippets.
pub const SNIPPETS: &str = "snippets";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedQuery {
    pub id: String,
    pub name: String,
    /// "" when not in a group.
    #[serde(default)]
    pub group: String,
    pub sql: String,
}

impl SavedQuery {
    /// "group/name", or just "name".
    pub fn path(&self) -> String {
        if self.group.is_empty() { self.name.clone() } else { format!("{}/{}", self.group, self.name) }
    }
}

/// Splits "group/name" (the last `/` separates them); trims both parts.
pub fn split_path(path: &str) -> (String, String) {
    match path.rsplit_once('/') {
        Some((group, name)) => (group.trim().to_string(), name.trim().to_string()),
        None => (String::new(), path.trim().to_string()),
    }
}

/// Whether every whitespace-separated word of `query` appears (case-
/// insensitively) in one of `fields`.
pub fn matches(query: &str, fields: &[&str]) -> bool {
    let haystack = fields.join("\n").to_lowercase();
    query.split_whitespace().all(|word| haystack.contains(&word.to_lowercase()))
}

pub struct Library {
    path: Option<PathBuf>,
    pub queries: Vec<SavedQuery>,
}

impl Library {
    pub fn load_from(path: Option<PathBuf>) -> Self {
        let queries = path
            .as_ref()
            .and_then(|p| fs::read_to_string(p).ok())
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default();
        Self { path, queries }
    }

    fn save(&self) -> anyhow::Result<()> {
        let Some(path) = &self.path else { return Ok(()) };
        write_atomic(path, &serde_json::to_string_pretty(&self.queries)?)
    }

    pub fn get(&self, id: &str) -> Option<&SavedQuery> {
        self.queries.iter().find(|q| q.id == id)
    }

    /// Saves a new query under "group/name"; returns its id.
    pub fn add(&mut self, path: &str, sql: &str) -> Result<String, String> {
        let (group, name) = split_path(path);
        if name.is_empty() {
            return Err("Give the query a name".into());
        }
        self.check_free(&group, &name, None)?;
        let id = uuid::Uuid::new_v4().to_string();
        self.queries.push(SavedQuery { id: id.clone(), name, group, sql: sql.to_string() });
        self.sort();
        self.save().map_err(|e| format!("Couldn't save: {e:#}"))?;
        Ok(id)
    }

    pub fn update_sql(&mut self, id: &str, sql: &str) -> Result<(), String> {
        let q = self.queries.iter_mut().find(|q| q.id == id).ok_or("The saved query no longer exists")?;
        q.sql = sql.to_string();
        self.save().map_err(|e| format!("Couldn't save: {e:#}"))
    }

    /// Renames (and possibly regroups) a saved query from "group/name".
    pub fn rename(&mut self, id: &str, path: &str) -> Result<(), String> {
        let (group, name) = split_path(path);
        if name.is_empty() {
            return Err("Give the query a name".into());
        }
        self.check_free(&group, &name, Some(id))?;
        let q = self.queries.iter_mut().find(|q| q.id == id).ok_or("The saved query no longer exists")?;
        q.group = group;
        q.name = name;
        self.sort();
        self.save().map_err(|e| format!("Couldn't save: {e:#}"))
    }

    pub fn delete(&mut self, id: &str) -> Result<(), String> {
        self.queries.retain(|q| q.id != id);
        self.save().map_err(|e| format!("Couldn't save: {e:#}"))
    }

    fn check_free(&self, group: &str, name: &str, except: Option<&str>) -> Result<(), String> {
        let taken = self.queries.iter().any(|q| {
            q.group.eq_ignore_ascii_case(group) && q.name.eq_ignore_ascii_case(name) && Some(q.id.as_str()) != except
        });
        if taken { Err(format!("A saved query named '{}' already exists", join(group, name))) } else { Ok(()) }
    }

    /// Ungrouped first, then by group and name.
    fn sort(&mut self) {
        self.queries.sort_by(|a, b| {
            (!a.group.is_empty(), a.group.to_lowercase(), a.name.to_lowercase()).cmp(&(
                !b.group.is_empty(),
                b.group.to_lowercase(),
                b.name.to_lowercase(),
            ))
        });
    }

    /// Saved queries matching `query`, in library order.
    pub fn search(&self, query: &str) -> Vec<&SavedQuery> {
        self.queries.iter().filter(|q| matches(query, &[&q.name, &q.group, &q.sql])).collect()
    }
}

fn join(group: &str, name: &str) -> String {
    if group.is_empty() { name.to_string() } else { format!("{group}/{name}") }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_split_on_the_last_slash() {
        assert_eq!(split_path(" reports / churn "), ("reports".into(), "churn".into()));
        assert_eq!(split_path("a/b/c"), ("a/b".into(), "c".into()));
        assert_eq!(split_path("plain"), (String::new(), "plain".into()));
    }

    #[test]
    fn add_rename_regroup_delete_and_persist() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("q.json");
        let mut lib = Library::load_from(Some(path.clone()));
        let a = lib.add("reports/churn", "select 1").unwrap();
        let b = lib.add("adhoc", "select 2").unwrap();
        assert!(lib.add("Reports/Churn", "x").unwrap_err().contains("already exists"));
        assert!(lib.add("reports/", "x").is_err());
        // Ungrouped queries sort first.
        assert_eq!(lib.queries.iter().map(SavedQuery::path).collect::<Vec<_>>(), ["adhoc", "reports/churn"]);

        lib.rename(&a, "finance/churn by month").unwrap();
        lib.update_sql(&b, "select 3").unwrap();
        let lib2 = Library::load_from(Some(path));
        assert_eq!(lib2.get(&a).unwrap().path(), "finance/churn by month");
        assert_eq!(lib2.get(&b).unwrap().sql, "select 3");

        lib.delete(&b).unwrap();
        assert_eq!(lib.queries.len(), 1);
    }

    #[test]
    fn search_needs_every_word_in_name_group_or_sql() {
        let mut lib = Library::load_from(None);
        lib.add("reports/churn", "select * from customers where status = 'paused'").unwrap();
        lib.add("adhoc/top", "select * from orders").unwrap();
        let names = |q: &str| lib.search(q).iter().map(|s| s.name.clone()).collect::<Vec<_>>();
        assert_eq!(names("CHURN"), ["churn"]);
        assert_eq!(names("reports paused"), ["churn"]);
        assert_eq!(names("orders"), ["top"]);
        assert_eq!(names("select"), ["top", "churn"]);
        assert!(names("orders paused").is_empty());
        assert_eq!(names("").len(), 2);
    }
}
