//! How `db` talks to MongoDB: the official `mongodb` crate, each statement
//! (`language::call`) mapped to the driver's call. Documents become rows:
//! their top-level fields the columns (`_id` first, then as first seen), a
//! nested value shown as JSON. A cancel stops the call Podėlis is waiting
//! on (`MongoPool::interrupt`).

#![expect(clippy::unused_async, reason = "every engine's driver has these functions with one signature, the generated dispatch calling them alike (build.rs): some needn't wait for anything")]

use std::sync::Arc;

use anyhow::{Result, anyhow, bail};
use futures_util::{StreamExt as _, TryStreamExt as _};
use ::mongodb::bson::{Bson, Document, doc};

use super::args::{document, values};
use super::language::{self, Call};
use crate::db::errors::{ErrorFacts, ServerError};
use crate::db::{Engine, FetchedRow, QueryLanguage as _, Rows, StatementResult};

/// The client (its own pool of connections) and the database a connection
/// is on.
pub struct MongoPool {
    pub(super) client: ::mongodb::Client,
    pub(super) database: String,
    pub(super) read_only: bool,
    pub(super) interrupt: Arc<Interrupt>,
}

/// A cancel: kept until the next run begins (one between two statements
/// stops the rest), and waking the call waiting on it. Podėlis stops
/// waiting; the operation on the server ends on its own.
#[derive(Default)]
pub struct Interrupt {
    cancelled: std::sync::atomic::AtomicBool,
    wake: tokio::sync::Notify,
}

impl Interrupt {
    fn clear(&self) {
        self.cancelled.store(false, std::sync::atomic::Ordering::SeqCst);
    }

    /// Done once a cancel has come (at once if it came already).
    async fn cancelled(&self) {
        loop {
            let woken = self.wake.notified();
            tokio::pin!(woken);
            woken.as_mut().enable();
            if self.cancelled.load(std::sync::atomic::Ordering::SeqCst) {
                return;
            }
            woken.await;
        }
    }
}

impl MongoPool {
    /// Stops the session's run: the call it waits on, and what's left.
    pub(super) fn interrupt(&self) {
        self.interrupt.cancelled.store(true, std::sync::atomic::Ordering::SeqCst);
        self.interrupt.wake.notify_waiters();
    }
}

/// A session: the client (cheap to clone) and its database.
pub struct MongoConnection {
    client: ::mongodb::Client,
    database: String,
    read_only: bool,
    interrupt: Arc<Interrupt>,
}

impl MongoConnection {
    pub(super) fn db(&self) -> ::mongodb::Database {
        self.client.database(&self.database)
    }
}

/// Why a statement can't be read: Podėlis's own, before the server sees
/// it (`error_facts` reads it as a syntax error).
#[derive(Debug)]
pub struct NotRead(pub String);

impl std::fmt::Display for NotRead {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for NotRead {}

pub(in crate::db) async fn acquire(pool: &MongoPool) -> Result<MongoConnection> {
    Ok(MongoConnection {
        client: pool.client.clone(),
        database: pool.database.clone(),
        read_only: pool.read_only,
        interrupt: Arc::clone(&pool.interrupt),
    })
}

pub(in crate::db) async fn close(pool: &MongoPool) {
    pool.client.clone().shutdown().await;
}

pub(in crate::db) async fn ping(c: &mut MongoConnection) {
    let _ = c.db().run_command(doc! {"ping": 1}).await;
}

pub(in crate::db) fn detach(c: MongoConnection) {
    drop(c);
}

/// `statement`'s call, and its arguments read; an error saying why not.
fn read_call(statement: &str) -> Result<(Call<'_>, Vec<Bson>)> {
    let call = language::call(statement).ok_or_else(|| {
        NotRead("not a call Podėlis runs: write db.<collection>.<method>(…) or db.runCommand({…})".into())
    })?;
    let args = values(&statement[call.args.clone()]).map_err(|e| NotRead(format!("{}: {e}", call.method)))?;
    Ok((call, args))
}

/// An error unless `statement` only reads, on a read-only connection.
fn check_reads(c: &MongoConnection, statement: &str) -> Result<()> {
    if c.read_only && language::Mongo.classify(statement).0 != crate::sql::analyze::Kind::Read {
        bail!("this connection is read-only: only reads run on it");
    }
    Ok(())
}

/// What a statement gave: documents, a cursor over them (`find`,
/// `aggregate`: read as they're wanted), or a count of those it changed.
enum Outcome {
    Documents(Vec<Document>),
    Cursor(Box<::mongodb::Cursor<Document>>),
    Changed(u64),
}

/// `work`, unless a cancel comes first.
async fn unless_cancelled<T>(c: &MongoConnection, work: impl Future<Output = Result<T>>) -> Result<T> {
    tokio::select! {
        done = work => done,
        () = c.interrupt.cancelled() => bail!("interrupted: the call was cancelled"),
    }
}

/// Runs one statement, waiting for it unless a cancel comes.
async fn execute(c: &MongoConnection, statement: &str) -> Result<Outcome> {
    check_reads(c, statement)?;
    unless_cancelled(c, run_call(c, statement)).await
}

/// A call on the database: `db.runCommand({…})`, `db.createCollection(…)`…
async fn run_db_call(c: &MongoConnection, call: &Call<'_>, args: &[Bson]) -> Result<Outcome> {
    let db = c.db();
    match call.method {
        "runCommand" => Ok(Outcome::Documents(vec![db.run_command(document(args.first(), "the command")?).await?])),
        "adminCommand" => Ok(Outcome::Documents(vec![
            c.client.database("admin").run_command(document(args.first(), "the command")?).await?,
        ])),
        "createCollection" => {
            let Some(Bson::String(name)) = args.first() else { bail!("createCollection takes the collection's name") };
            db.create_collection(name).await?;
            Ok(Outcome::Changed(0))
        }
        "getCollectionNames" => {
            let names = db.list_collection_names().await?;
            Ok(Outcome::Documents(names.into_iter().map(|n| doc! {"name": n}).collect()))
        }
        "stats" => Ok(Outcome::Documents(vec![db.run_command(doc! {"dbStats": 1}).await?])),
        other => bail!(NotRead(format!("db.{other}() isn't one Podėlis runs"))),
    }
}

/// Up to `limit` documents of a cursor, and how many there were in all
/// (counted, not kept, as SQL's rows are).
async fn take(c: &MongoConnection, cursor: ::mongodb::Cursor<Document>, limit: usize) -> Result<(Vec<Document>, u64)> {
    let mut cursor = cursor;
    unless_cancelled(c, async {
        let (mut docs, mut all) = (Vec::new(), 0u64);
        while let Some(d) = cursor.try_next().await? {
            all += 1;
            if docs.len() < limit {
                docs.push(d);
            }
        }
        Ok((docs, all))
    })
    .await
}

/// The argument of the last `.<name>(…)` chained after the call.
fn chained(statement: &str, call: &Call<'_>, name: &str) -> Result<Option<Bson>> {
    call.chain
        .iter()
        .rev()
        .find(|(m, _)| *m == name)
        .map(|(_, a)| values(&statement[a.clone()]).map(|v| v.into_iter().next().unwrap_or(Bson::Null)))
        .transpose()
}

/// A number the user wrote, as an integer.
fn number(b: Option<Bson>) -> Option<i64> {
    match b? {
        Bson::Int32(n) => Some(n.into()),
        Bson::Int64(n) => Some(n),
        #[expect(clippy::cast_possible_truncation, reason = "a count the user wrote")]
        Bson::Double(n) => Some(n as i64),
        _ => None,
    }
}

/// `find(filter, projection)` with what's chained after it: `.sort()`,
/// `.limit()`, `.skip()`, `.projection()`.
async fn find(
    collection: &::mongodb::Collection<Document>,
    statement: &str,
    call: &Call<'_>,
    args: &[Bson],
) -> Result<::mongodb::Cursor<Document>> {
    let mut find = collection.find(document(args.first(), "the filter")?);
    if let Some(projection) = args.get(1).cloned().or(chained(statement, call, "projection")?) {
        find = find.projection(document(Some(&projection), "the projection")?);
    }
    if let Some(sort) = chained(statement, call, "sort")? {
        find = find.sort(document(Some(&sort), "the sort")?);
    }
    if let Some(n) = number(chained(statement, call, "limit")?) {
        find = find.limit(n);
    }
    if let Some(n) = number(chained(statement, call, "skip")?) {
        find = find.skip(u64::try_from(n).unwrap_or(0));
    }
    Ok(find.await?)
}

async fn run_call(c: &MongoConnection, statement: &str) -> Result<Outcome> {
    let (call, args) = read_call(statement)?;
    let db = c.db();
    let Some(name) = call.collection.as_deref() else { return run_db_call(c, &call, &args).await };
    let collection = db.collection::<Document>(name);
    let filter = || document(args.first(), "the filter");
    Ok(match call.method {
        "find" => Outcome::Cursor(Box::new(find(&collection, statement, &call, &args).await?)),
        "findOne" => {
            let mut find = collection.find_one(filter()?);
            if let Some(projection) = args.get(1) {
                find = find.projection(document(Some(projection), "the projection")?);
            }
            Outcome::Documents(find.await?.into_iter().collect())
        }
        "aggregate" => {
            let Some(Bson::Array(stages)) = args.first() else { bail!("aggregate takes a pipeline ([…])") };
            let stages: Vec<Document> =
                stages.iter().map(|s| document(Some(s), "a pipeline stage")).collect::<Result<_>>()?;
            Outcome::Cursor(Box::new(collection.aggregate(stages).await?))
        }
        "countDocuments" | "count" => {
            Outcome::Documents(vec![doc! {"count": i64::try_from(collection.count_documents(filter()?).await?)?}])
        }
        "estimatedDocumentCount" => {
            Outcome::Documents(vec![doc! {"count": i64::try_from(collection.estimated_document_count().await?)?}])
        }
        "distinct" => {
            let Some(Bson::String(field)) = args.first() else { bail!("distinct takes a field's name") };
            let values = collection.distinct(field, document(args.get(1), "the filter")?).await?;
            Outcome::Documents(values.into_iter().map(|v| doc! {field.as_str(): v}).collect())
        }
        "insertOne" => {
            collection.insert_one(document(args.first(), "the document")?).await?;
            Outcome::Changed(1)
        }
        "insertMany" => {
            let Some(Bson::Array(docs)) = args.first() else { bail!("insertMany takes an array of documents") };
            let docs: Vec<Document> = docs.iter().map(|d| document(Some(d), "a document")).collect::<Result<_>>()?;
            let n = docs.len() as u64;
            collection.insert_many(docs).await?;
            Outcome::Changed(n)
        }
        "updateOne" | "updateMany" => {
            let update = document(args.get(1), "the update")?;
            let result = if call.method == "updateOne" {
                collection.update_one(filter()?, update).await?
            } else {
                collection.update_many(filter()?, update).await?
            };
            Outcome::Changed(result.modified_count)
        }
        "replaceOne" => Outcome::Changed(collection.replace_one(filter()?, document(args.get(1), "the document")?).await?.modified_count),
        "deleteOne" => Outcome::Changed(collection.delete_one(filter()?).await?.deleted_count),
        "deleteMany" => Outcome::Changed(collection.delete_many(filter()?).await?.deleted_count),
        "drop" => {
            collection.drop().await?;
            Outcome::Changed(0)
        }
        "createIndex" => {
            let keys = document(args.first(), "the index's keys")?;
            let index = ::mongodb::IndexModel::builder().keys(keys).build();
            let made = collection.create_index(index).await?;
            Outcome::Documents(vec![doc! {"name": made.index_name}])
        }
        "dropIndex" => {
            let Some(Bson::String(index)) = args.first() else { bail!("dropIndex takes the index's name") };
            collection.drop_index(index).await?;
            Outcome::Changed(0)
        }
        "getIndexes" => {
            let indexes: Vec<::mongodb::IndexModel> = collection.list_indexes().await?.try_collect().await?;
            let docs = indexes
                .into_iter()
                .map(|i| {
                    let name = i.options.and_then(|o| o.name).unwrap_or_default();
                    doc! {"name": name, "key": i.keys}
                })
                .collect();
            Outcome::Documents(docs)
        }
        other => bail!(NotRead(format!("{other}() isn't a method Podėlis runs yet"))),
    })
}

/// The type of a value as mongosh names it.
fn type_name(value: &Bson) -> &'static str {
    match value {
        Bson::Double(_) => "double",
        Bson::String(_) => "string",
        Bson::Document(_) => "object",
        Bson::Array(_) => "array",
        Bson::Binary(_) => "binData",
        Bson::ObjectId(_) => "objectId",
        Bson::Boolean(_) => "bool",
        Bson::DateTime(_) => "date",
        Bson::Null | Bson::Undefined => "null",
        Bson::RegularExpression(_) => "regex",
        Bson::Int32(_) => "int",
        Bson::Timestamp(_) => "timestamp",
        Bson::Int64(_) => "long",
        Bson::Decimal128(_) => "decimal",
        _ => "other",
    }
}

/// A value as text: strings and numbers as they are, an `ObjectId` by its
/// hex, a date in RFC 3339, anything nested as relaxed JSON; `None` for
/// null.
pub(super) fn cell(value: &Bson) -> Option<String> {
    Some(match value {
        Bson::Null | Bson::Undefined => return None,
        Bson::String(s) => s.clone(),
        Bson::Int32(n) => n.to_string(),
        Bson::Int64(n) => n.to_string(),
        Bson::Double(n) => n.to_string(),
        Bson::Boolean(b) => b.to_string(),
        Bson::ObjectId(o) => o.to_hex(),
        Bson::DateTime(d) => d.try_to_rfc3339_string().unwrap_or_else(|_| d.to_string()),
        Bson::Decimal128(d) => d.to_string(),
        other => other.clone().into_relaxed_extjson().to_string(),
    })
}

/// Columns (name, type) and rows of cells.
pub(super) type Table = (Vec<(String, String)>, Vec<Vec<Option<String>>>);

/// Documents as columns and rows: `_id` first, then each field as first
/// seen; each column's type from its first value that isn't null.
pub(super) fn table(docs: &[Document]) -> Table {
    let mut names: Vec<String> = Vec::new();
    if docs.iter().any(|d| d.contains_key("_id")) {
        names.push("_id".into());
    }
    for d in docs {
        for key in d.keys() {
            if !names.contains(key) {
                names.push(key.clone());
            }
        }
    }
    let columns = names
        .iter()
        .map(|n| {
            let ty = docs.iter().filter_map(|d| d.get(n)).find(|v| !matches!(v, Bson::Null)).map_or("null", type_name);
            (n.clone(), ty.to_string())
        })
        .collect();
    let rows = docs.iter().map(|d| names.iter().map(|n| d.get(n).and_then(cell)).collect()).collect();
    (columns, rows)
}

/// Runs each statement in `sql`: documents as rows (at most `limit` kept,
/// all counted), a change as its count.
pub(in crate::db) async fn run(c: &mut MongoConnection, sql: &str, limit: usize) -> Result<Vec<StatementResult>> {
    c.interrupt.clear();
    let mut results = Vec::new();
    for statement in crate::sql::split_statements(sql, Engine::MongoDb) {
        let mut result = StatementResult::default();
        let (docs, all) = match execute(c, statement).await? {
            Outcome::Documents(docs) => {
                let all = docs.len() as u64;
                (docs.into_iter().take(limit).collect(), all)
            }
            Outcome::Cursor(cursor) => take(c, *cursor, limit).await?,
            Outcome::Changed(n) => {
                result.rows_affected = n;
                results.push(result);
                continue;
            }
        };
        let (columns, rows) = table(&docs);
        result.total_rows = all;
        result.columns = Some(columns);
        result.rows = rows;
        results.push(result);
    }
    Ok(results)
}

/// One statement's documents as rows, read as they're wanted: the columns
/// are the first document's fields (a later one's other fields don't
/// show), and come with it.
pub(in crate::db) fn fetch(c: &mut MongoConnection, sql: String) -> Rows<'_> {
    c.interrupt.clear();
    let c = &*c;
    let start = async move {
        Ok::<_, anyhow::Error>(match execute(c, &sql).await? {
            Outcome::Cursor(cursor) => cursor.map_err(anyhow::Error::from).boxed(),
            Outcome::Documents(docs) => futures_util::stream::iter(docs.into_iter().map(Ok)).boxed(),
            Outcome::Changed(_) => futures_util::stream::empty().boxed(),
        })
    };
    futures_util::stream::once(start)
        .flat_map(|docs| match docs {
            Ok(docs) => docs,
            Err(e) => futures_util::stream::once(async move { Err(e) }).boxed(),
        })
        .scan(None::<Vec<String>>, |names, doc| {
            let row = doc.map(|doc| {
                let columns = names.is_none().then(|| table(std::slice::from_ref(&doc)).0);
                let fields = names.get_or_insert_with(|| {
                    columns.iter().flatten().map(|(n, _)| n.clone()).collect()
                });
                let cells = fields.iter().map(|n| doc.get(n).and_then(cell)).collect();
                FetchedRow { columns, cells }
            });
            std::future::ready(Some(row))
        })
        .boxed()
}

/// Reads `sql` without running it: whether Podėlis can read the call and
/// its arguments (the server sees it only when it runs).
pub(in crate::db) async fn prepare(_c: &mut MongoConnection, sql: &str) -> Result<()> {
    read_call(sql).map(drop)
}

pub(in crate::db) async fn columns(_c: &mut MongoConnection, _sql: &str) -> Option<Vec<(String, String)>> {
    None
}

/// The first value of what `sql` returns, as text (`buildInfo`'s version).
pub(in crate::db) async fn text(c: &mut MongoConnection, sql: &str) -> Result<String> {
    let first = match execute(c, sql).await? {
        Outcome::Documents(docs) => docs.into_iter().next(),
        Outcome::Cursor(cursor) => take(c, *cursor, 1).await?.0.into_iter().next(),
        Outcome::Changed(n) => return Ok(n.to_string()),
    };
    first.and_then(|d| d.iter().next().and_then(|(_, v)| cell(v))).ok_or_else(|| anyhow!("no value"))
}

/// What one of the driver's errors says, or Podėlis's own `NotRead`.
pub(in crate::db) fn error_facts(cause: &(dyn std::error::Error + 'static)) -> Option<ErrorFacts> {
    use ::mongodb::error::ErrorKind;
    if let Some(NotRead(message)) = cause.downcast_ref::<NotRead>() {
        let server = ServerError { message: message.clone(), check: Some(true), ..ServerError::default() };
        return Some(ErrorFacts { server: Some(server), ..ErrorFacts::default() });
    }
    let err = cause.downcast_ref::<::mongodb::error::Error>()?;
    Some(match err.kind.as_ref() {
        ErrorKind::Io(_) | ErrorKind::ConnectionPoolCleared { .. } => ErrorFacts { broken: true, ..ErrorFacts::default() },
        ErrorKind::ServerSelection { .. } => ErrorFacts { no_connection: true, ..ErrorFacts::default() },
        ErrorKind::InvalidTlsConfig { message, .. } => ErrorFacts { tls: Some(message.clone()), ..ErrorFacts::default() },
        ErrorKind::Command(command) => ErrorFacts {
            server: Some(ServerError {
                message: format!("{} ({})", command.message, command.code_name),
                // NamespaceNotFound: an unknown collection.
                check: (command.code == 26).then_some(false),
                ..ServerError::default()
            }),
            ..ErrorFacts::default()
        },
        _ => ErrorFacts { server: Some(ServerError { message: err.to_string(), ..ServerError::default() }), ..ErrorFacts::default() },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn documents_as_rows() {
        let id = ::mongodb::bson::oid::ObjectId::parse_str("64b7f0c2a1b2c3d4e5f60718").unwrap();
        let docs = [
            doc! {"name": "Ada", "_id": id, "tags": ["a", "b"]},
            doc! {"_id": 2, "name": Bson::Null, "age": 36, "home": {"city": "Vilnius"}},
        ];
        let (columns, rows) = table(&docs);
        let names: Vec<(&str, &str)> = columns.iter().map(|(n, t)| (n.as_str(), t.as_str())).collect();
        assert_eq!(names, [("_id", "objectId"), ("name", "string"), ("tags", "array"), ("age", "int"), ("home", "object")]);
        assert_eq!(rows[0], [Some("64b7f0c2a1b2c3d4e5f60718".into()), Some("Ada".into()), Some(r#"["a","b"]"#.into()), None, None]);
        assert_eq!(rows[1][1], None, "null");
        assert_eq!(rows[1][4].as_deref(), Some(r#"{"city":"Vilnius"}"#));
    }

    /// A cancel is kept until the next run clears it: one between two
    /// statements stops the next, and one with nothing running doesn't
    /// stop the run after it.
    #[tokio::test]
    async fn a_cancel_is_kept_until_the_next_run() {
        let interrupt = Interrupt::default();
        interrupt.cancelled.store(true, std::sync::atomic::Ordering::SeqCst);
        tokio::time::timeout(std::time::Duration::from_secs(1), interrupt.cancelled()).await.expect("at once");
        interrupt.clear();
        assert!(tokio::time::timeout(std::time::Duration::from_millis(50), interrupt.cancelled()).await.is_err());
    }

    #[test]
    fn what_it_can_t_read_is_a_syntax_error() {
        let err = anyhow::Error::new(NotRead("find: expected `}`".into()));
        let facts = crate::db::errors::server_error(&err).unwrap();
        assert_eq!(facts.check, Some(true));
        assert!(read_call("show dbs").is_err());
        assert!(read_call("db.t.find({a: })").unwrap_err().to_string().starts_with("find:"));
    }
}
