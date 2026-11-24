//! Schema snapshots: a connection's schema saved to a file, to compare the
//! live database with later ("what changed in prod since the release?").
//! One JSON file each, in `snapshots/<project>/`, named
//! `<connection>@<when>.json` so they can be listed without reading them.

use crate::config::store::ProjectId;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::db::Engine;
use crate::transfer::diff::TableShape;

#[derive(Serialize, Deserialize)]
pub struct Snapshot {
    pub connection: String,
    pub engine: Engine,
    /// When it was taken, RFC 3339.
    pub taken: String,
    pub tables: Vec<TableShape>,
    /// Its foreign keys have their ON DELETE and ON UPDATE (taken since
    /// they're kept): one taken before has none, which isn't "NO ACTION".
    #[serde(default)]
    pub fk_rules: bool,
}

impl Snapshot {
    /// Compared with `now`, a snapshot that kept no rules (`fk_rules`):
    /// each of its keys takes the rules of the same key (column → target)
    /// in `now`, so a rule isn't taken for a change. Its keys, else as kept.
    pub fn without_unknown_rules(mut self, now: &[TableShape]) -> Self {
        if self.fk_rules {
            return self;
        }
        for table in &mut self.tables {
            let Some(live) = now.iter().find(|t| t.info.schema == table.info.schema && t.info.name == table.info.name)
            else {
                continue;
            };
            let (Some(kept), Some(live)) = (table.details.as_mut(), live.details.as_ref()) else { continue };
            for fk in &mut kept.foreign_keys {
                // Matched as the compare matches keys: case aside.
                let same_key = |l: &&crate::db::ForeignKey| {
                    l.column.eq_ignore_ascii_case(&fk.column) && l.target.eq_ignore_ascii_case(&fk.target)
                };
                if let Some(same) = live.foreign_keys.iter().find(same_key) {
                    fk.on_delete.clone_from(&same.on_delete);
                    fk.on_update.clone_from(&same.on_update);
                }
            }
        }
        self.fk_rules = true;
        self
    }
}

/// A saved snapshot, as listed: from its file name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Saved {
    pub path: PathBuf,
    pub connection: String,
    /// `2026-10-14 10:05:12`.
    pub taken: String,
}

pub fn dir(config_dir: &Path, project_id: &ProjectId) -> PathBuf {
    config_dir.join("snapshots").join(project_id.as_str())
}

/// Saves `snapshot` in `dir`; its path.
pub fn save(dir: &Path, snapshot: &Snapshot, now: chrono::DateTime<chrono::Local>) -> Result<PathBuf> {
    let name: String =
        snapshot.connection.chars().map(|c| if matches!(c, '/' | '\\' | '@') { '_' } else { c }).collect();
    let path = dir.join(format!("{name}@{}.json", now.format("%Y-%m-%d_%H-%M-%S")));
    let text = serde_json::to_string(snapshot).context("writing the snapshot")?;
    crate::config::store::write_atomic(&path, &text)?;
    Ok(path)
}

/// The snapshots in `dir`, newest first.
pub fn list(dir: &Path) -> Vec<Saved> {
    let Ok(files) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut out: Vec<Saved> = files
        .filter_map(|f| f.ok().map(|f| f.path()))
        .filter_map(|path| {
            let stem = path.file_stem()?.to_str()?.to_string();
            if path.extension()? != "json" {
                return None;
            }
            let (connection, when) = stem.rsplit_once('@')?;
            let (day, time) = when.split_once('_')?;
            let taken = format!("{day} {}", time.replace('-', ":"));
            Some(Saved { connection: connection.to_string(), taken, path })
        })
        .collect();
    out.sort_by(|a, b| b.taken.cmp(&a.taken));
    out
}

pub fn load(path: &Path) -> Result<Snapshot> {
    let text = std::fs::read_to_string(path).with_context(|| format!("can't read {}", path.display()))?;
    serde_json::from_str(&text).with_context(|| format!("{} isn't a schema snapshot", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{ColumnInfo, TableInfo};

    #[test]
    fn saved_listed_newest_first_and_read_back() {
        let dir = tempfile::tempdir().unwrap();
        let table = TableShape {
            info: TableInfo {
                schema: "public".into(),
                name: "t".into(),
                is_view: false,
                rows: None,
                bytes: None,
                system: false,
                partition_of: None,
                materialized: false,
            },
            columns: vec![ColumnInfo {
                name: "id".into(),
                data_type: "int4".into(),
                nullable: false,
                primary_key: true,
            }],
            details: None,
        };
        let snapshot = |conn: &str| Snapshot {
            connection: conn.into(),
            engine: Engine::Postgres,
            taken: String::new(),
            tables: vec![table.clone()],
            fk_rules: true,
        };
        let at = |h: u32| {
            chrono::Local::now().date_naive().and_hms_opt(h, 0, 0).unwrap().and_local_timezone(chrono::Local).unwrap()
        };
        save(dir.path(), &snapshot("prod"), at(9)).unwrap();
        let newer = save(dir.path(), &snapshot("odd/name@x"), at(10)).unwrap();
        let listed = list(dir.path());
        assert_eq!(listed.iter().map(|s| s.connection.as_str()).collect::<Vec<_>>(), ["odd_name_x", "prod"]);
        assert!(listed[0].taken.ends_with("10:00:00"), "{}", listed[0].taken);
        assert_eq!(load(&newer).unwrap().tables[0].columns[0].name, "id");
    }

    /// A snapshot taken before keys kept their rules isn't said to have
    /// changed them: its keys take the live ones' rules.
    #[test]
    fn an_old_snapshots_keys_take_the_live_rules() {
        use crate::db::{ForeignKey, TableDetails, TableInfo};
        let shape = |on_delete: Option<&str>| TableShape {
            info: TableInfo {
                schema: "public".into(),
                name: "orders".into(),
                is_view: false,
                rows: None,
                bytes: None,
                system: false,
                partition_of: None,
                materialized: false,
            },
            columns: Vec::new(),
            details: Some(TableDetails {
                foreign_keys: vec![ForeignKey {
                    column: "user_id".into(),
                    target: "users.id".into(),
                    target_schema: "public".into(),
                    target_table: "users".into(),
                    target_column: Some("id".into()),
                    on_delete: on_delete.map(str::to_string),
                    on_update: None,
                }],
                ..TableDetails::default()
            }),
        };
        // As an old file reads: no `fk_rules`.
        let old: Snapshot = serde_json::from_value(serde_json::json!({
            "connection": "prod", "engine": "Postgres", "taken": "", "tables": [shape(None)]
        }))
        .unwrap();
        assert!(!old.fk_rules);
        let now = [shape(Some("CASCADE"))];
        let filled = old.without_unknown_rules(&now);
        let fk = &filled.tables[0].details.as_ref().unwrap().foreign_keys[0];
        assert_eq!(fk.on_delete.as_deref(), Some("CASCADE"));
    }
}
