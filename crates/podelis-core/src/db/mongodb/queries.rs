//! MongoDB: its catalog (collections as tables, their fields sampled from
//! documents), details (indexes), and what it doesn't have.

#![expect(clippy::unused_async, reason = "every engine has these functions with one signature, the generated dispatch calling them alike (build.rs): some needn't wait for anything")]

use futures_util::TryStreamExt as _;
use ::mongodb::bson::{Document, doc};

use super::super::activity::Activity;
use super::super::catalog::{ColumnRow, TableRow};
use super::super::details::IndexInfo;
use super::super::dialect::Scan;
use super::super::*;
use super::DriverConnection;
use super::driver;

/// How many documents a collection's fields are read from.
const SAMPLE: i64 = 100;

/// The connection's database, by the pool's client: a session busy with a
/// long call doesn't hold up the catalog.
async fn database(db: &Database) -> Result<::mongodb::Database> {
    let Pool::MongoDb(p) = &db.0.pool else { unreachable!("engine matches pool") };
    Ok(p.client.database(&p.database))
}

/// Each collection and view (`system.*` only when `system`).
async fn collections(db: &Database, system: bool) -> Result<Vec<(String, String)>> {
    let database = database(db).await?;
    let mut specs = database.list_collections().await?;
    let mut out = Vec::new();
    while let Some(spec) = specs.try_next().await? {
        let kind = format!("{:?}", spec.collection_type).to_ascii_uppercase();
        if system || !spec.name.starts_with("system.") {
            out.push((spec.name, if kind.contains("VIEW") { "VIEW".into() } else { "COLLECTION".into() }));
        }
    }
    out.sort();
    Ok(out)
}

/// Each collection's fields, from a sample of its documents: `_id` the key.
pub(in crate::db) async fn column_rows(db: &Database, system: bool) -> Result<Vec<ColumnRow>> {
    let database = database(db).await?;
    use futures_util::StreamExt as _;
    let names: Vec<String> = collections(db, system).await?.into_iter().map(|(n, _)| n).collect();
    // A few collections at a time: one round trip each.
    let sampled: Vec<(String, Vec<Document>)> = futures_util::stream::iter(names)
        .map(|name| {
            let database = database.clone();
            async move {
                let sample = database.collection::<Document>(&name).aggregate([doc! {"$sample": {"size": SAMPLE}}]).await;
                let docs = match sample {
                    Ok(cursor) => cursor.try_collect().await.unwrap_or_default(),
                    Err(_) => Vec::new(),
                };
                (name, docs)
            }
        })
        .buffered(8)
        .collect()
        .await;
    let mut rows = Vec::new();
    for (name, docs) in sampled {
        let (columns, _) = driver::table(&docs);
        for (field, ty) in columns {
            let key = if field == "_id" { "PRI" } else { "" };
            rows.push((database.name().to_string(), name.clone(), field, ty, "YES".to_string(), key.to_string()));
        }
    }
    Ok(rows)
}

/// Collections and views, each with its documents as MongoDB estimates
/// them.
pub(in crate::db) async fn table_rows(db: &Database, system: bool) -> Result<Vec<TableRow>> {
    let database = database(db).await?;
    let mut rows = Vec::new();
    for (name, kind) in collections(db, system).await? {
        let count = if kind == "VIEW" {
            None
        } else {
            database.collection::<Document>(&name).estimated_document_count().await.ok().and_then(|n| i64::try_from(n).ok())
        };
        rows.push((database.name().to_string(), name, kind, count, None));
    }
    Ok(rows)
}

#[cfg(test)]
pub(in crate::db) async fn column_info_rows(
    db: &Database,
    _schema: &str,
    table: &str,
) -> Result<Vec<(String, String, String)>> {
    let rows = column_rows(db, false).await?;
    Ok(rows.into_iter().filter(|r| r.1 == table).map(|r| (r.2, r.3, r.4)).collect())
}

/// No foreign keys: documents refer to each other only by what they hold.
pub(in crate::db) async fn link_rows(
    _db: &Database,
) -> Result<Vec<(String, String, String, String, String, Option<String>)>> {
    Ok(Vec::new())
}

/// Its indexes (`_id_` the primary one).
pub(in crate::db) async fn table_details(db: &Database, _schema: &str, table: &str) -> Result<TableDetails> {
    let database = database(db).await?;
    let indexes: Vec<::mongodb::IndexModel> =
        database.collection::<Document>(table).list_indexes().await?.try_collect().await?;
    let indexes = indexes
        .into_iter()
        .map(|i| {
            let name = i.options.as_ref().and_then(|o| o.name.clone()).unwrap_or_default();
            let unique = i.options.as_ref().and_then(|o| o.unique).unwrap_or(false) || name == "_id_";
            IndexInfo {
                columns: i.keys.keys().cloned().collect::<Vec<_>>().join(", "),
                unique,
                primary: name == "_id_",
                definition: Some(i.keys.to_string()),
                name,
            }
        })
        .collect();
    Ok(TableDetails { indexes, foreign_keys: Vec::new(), definition: None })
}

/// The session-info query and the name of each column it returns:
/// `buildInfo`'s first fields.
pub(in crate::db) const SESSION_INFO: (&str, &[&str]) =
    ("db.runCommand({buildInfo: 1})", &["Version", "Git version"]);

pub(in crate::db) async fn activity(_db: &Database) -> Result<Vec<Activity>> {
    anyhow::bail!("MongoDB's operations aren't listed yet")
}

pub(in crate::db) async fn stop_session(_db: &Database, _id: i64, _terminate: bool) -> Result<()> {
    anyhow::bail!("MongoDB's operations aren't listed yet")
}

/// Connections in use, and how many the server takes in all (`serverStatus`:
/// those in use and those still free); `None` where this user may not ask.
pub(in crate::db) async fn connections(db: &Database) -> Option<String> {
    let Pool::MongoDb(p) = &db.0.pool else { unreachable!("engine matches pool") };
    let status = p.client.database("admin").run_command(doc! {"serverStatus": 1}).await.ok()?;
    let connections = status.get_document("connections").ok()?;
    let number = |key: &str| match connections.get(key)? {
        ::mongodb::bson::Bson::Int32(n) => Some(i64::from(*n)),
        ::mongodb::bson::Bson::Int64(n) => Some(*n),
        _ => None,
    };
    let (current, available) = (number("current")?, number("available")?);
    Some(format!("{current} in use, up to {}", current + available))
}


pub(in crate::db) async fn tls_info(_db: &Database) -> Option<String> {
    None
}

pub(in crate::db) async fn comment_rows(_db: &Database) -> Result<Vec<(String, String, Option<String>, String)>> {
    Ok(Vec::new())
}

pub(in crate::db) async fn routine_rows(_db: &Database) -> Result<Vec<(String, String, String, String)>> {
    Ok(Vec::new())
}

pub(in crate::db) async fn routine_definition(_db: &Database, _routine: &RoutineInfo) -> Result<String> {
    anyhow::bail!("MongoDB has no stored routines Podėlis lists")
}

/// Its plans aren't read yet.
pub(in crate::db) async fn full_scans_of(_db: &Database, _statement: &str) -> Option<Vec<Scan>> {
    None
}

pub(in crate::db) async fn estimated_rows(db: &Database, scans: Vec<Scan>) -> Vec<(String, f64)> {
    db.estimates_from_catalog(scans).await
}

/// Stops the call the session waits on.
pub(in crate::db) async fn cancel(db: &Database, _id: i64) -> Result<()> {
    let Pool::MongoDb(p) = &db.0.pool else { unreachable!("engine matches pool") };
    p.interrupt();
    Ok(())
}

pub(in crate::db) async fn transaction_state(_c: &mut DriverConnection) -> Option<TxnState> {
    None
}

pub(in crate::db) async fn copy_in(
    db: &Database,
    _statement: &str,
    _first_line: u64,
    _chunks: impl Iterator<Item = Result<Vec<u8>>>,
) -> Result<u64> {
    anyhow::bail!("{} has no COPY", db.engine().label())
}
