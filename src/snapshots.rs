//! Schema snapshots: a connection's schema saved to a file, to compare the
//! live database with later ("what changed in prod since the release?").
//! One JSON file each, in `snapshots/<project>/`, named
//! `<connection>@<when>.json` so they can be listed without reading them.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::db::Engine;
use crate::diff::TableShape;

#[derive(Serialize, Deserialize)]
pub struct Snapshot {
    pub connection: String,
    pub engine: Engine,
    /// When it was taken, RFC 3339.
    pub taken: String,
    pub tables: Vec<TableShape>,
}

/// A saved snapshot, as listed: from its file name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Saved {
    pub path: PathBuf,
    pub connection: String,
    /// `2026-10-14 10:05:12`.
    pub taken: String,
}

pub fn dir(config_dir: &Path, project_id: &str) -> PathBuf {
    config_dir.join("snapshots").join(project_id)
}

/// Saves `snapshot` in `dir`; its path.
pub fn save(dir: &Path, snapshot: &Snapshot, now: chrono::DateTime<chrono::Local>) -> Result<PathBuf> {
    let name: String =
        snapshot.connection.chars().map(|c| if matches!(c, '/' | '\\' | '@') { '_' } else { c }).collect();
    let path = dir.join(format!("{name}@{}.json", now.format("%Y-%m-%d_%H-%M-%S")));
    let text = serde_json::to_string(snapshot).context("writing the snapshot")?;
    crate::store::write_atomic(&path, &text)?;
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
}
