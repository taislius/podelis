//! MongoDB's own `Database` tests (the shared ones are SQL's, and skip
//! it). Live with `PODELIS_TEST_MONGO_PORT` set (`_HOST`, `_USER`,
//! `_PASSWORD`, `_DB` as for the other servers): a `mongo:7.0` container,
//! as CI runs (8.0 and later crash on Linux 6.19 and newer: SERVER-121912).

use crate::db::tests::*;
use crate::db::*;

pub(in crate::db) fn live_params() -> Option<ConnectParams> {
    server_params(Engine::MongoDb, "PODELIS_TEST_MONGO", "")
}

/// A database of the test's own on the test server.
pub(in crate::db) async fn own_live_db(name: &str) -> Option<(Database, ConnectParams)> {
    let mut params = live_params()?;
    params.database = name.to_string();
    let db = Database::connect(params.clone()).await.expect("connect");
    db.run("db.runCommand({dropDatabase: 1})", 1).await.expect("a clean database");
    Some((db, params))
}

/// No SQL: the shared tests that need it skip MongoDB (`own_live_db`).
pub static TEST_SQL: TestSql = TestSql {
    sleep: None,
    count_to_2500: "",
    plan_shows: &[],
    analyzed_shows: "",
    analyze: "",
    binary: ["", "", ""],
    comments: None,
    sequence: None,
    read_only_off: None,
    without_rollback: None,
};

#[tokio::test]
async fn live_mongodb() {
    let Some((db, params)) = own_live_db("podelis_mongo").await else { return };
    assert!(db.version().chars().next().is_some_and(|c| c.is_ascii_digit()), "{}", db.version());
    // Writes, as counts.
    let o = db.run("db.people.insertMany([{name: 'Ada', age: 36}, {name: 'Linas', age: 41, city: 'Vilnius'}])", 100).await.unwrap();
    assert_eq!(o.rows_affected, Some(2));
    // Documents as rows: `_id` first, then each field as first seen.
    let o = db.run("db.people.find({}, {_id: 0}).sort({age: 1})", 100).await.unwrap();
    assert_eq!(o.columns, ["name", "age", "city"]);
    assert_eq!(cells(&o), vec![vec![Some("Ada"), Some("36"), None], vec![Some("Linas"), Some("41"), Some("Vilnius")]]);
    let o = db.run("db.people.find({age: {$gt: 40}}).limit(1)", 100).await.unwrap();
    assert_eq!((o.columns[0].as_str(), o.rows.len()), ("_id", 1));
    let o = db.run("db.people.countDocuments({})", 100).await.unwrap();
    assert_eq!(cells(&o), vec![vec![Some("2")]]);
    let o = db.run("db.people.updateMany({age: {$gt: 30}}, {$inc: {age: 1}})", 100).await.unwrap();
    assert_eq!(o.rows_affected, Some(2));
    let o = db.run("db.people.aggregate([{$group: {_id: null, oldest: {$max: '$age'}}}])", 100).await.unwrap();
    assert_eq!(o.rows[0][1].as_deref(), Some("42"));
    // Two statements, the last shown; a row limit keeps the count of all.
    let o = db.run("db.people.insertOne({name: 'C'})\ndb.people.find()", 2).await.unwrap();
    assert_eq!((o.statements, o.rows.len(), o.total_rows), (2, 2, 3));
    // Streaming: the first page now, the rest as it's wanted.
    let many: Vec<String> = (0..25).map(|i| format!("{{n: {i}}}")).collect();
    db.run(&format!("db.many.insertMany([{}])", many.join(", ")), 1).await.unwrap();
    let (o, stream) = db.run_streaming("db.many.find({}, {_id: 0}).sort({n: 1})", 10).await.unwrap();
    assert_eq!((o.columns.clone(), o.rows.len()), (vec!["n".to_string()], 10));
    let stream = stream.expect("more to come");
    let page = stream.next_page(10).await.unwrap();
    assert_eq!((page.rows[0][0].as_deref(), page.done), (Some("10"), false));
    let page = stream.next_page(10).await.unwrap();
    assert_eq!((page.rows.len(), page.done), (5, true));
    // A row limit keeps the first rows and counts all.
    let o = db.run("db.many.find()", 4).await.unwrap();
    assert_eq!((o.rows.len(), o.total_rows), (4, 25));
    // The catalog: collections, their fields sampled, `_id` the key.
    db.run("db.people.createIndex({name: 1})", 1).await.unwrap();
    let catalog = db.catalog(false).await.unwrap();
    let (info, columns) = catalog.iter().find(|(t, _)| t.name == "people").unwrap();
    assert_eq!(info.schema, "podelis_mongo");
    assert!(columns.iter().any(|c| c.name == "_id" && c.primary_key) && columns.iter().any(|c| c.name == "city"));
    let details = db.table_details("podelis_mongo", "people").await.unwrap();
    assert!(details.indexes.iter().any(|i| i.primary && i.name == "_id_"));
    assert!(details.indexes.iter().any(|i| i.columns == "name" && !i.unique));
    // What it can't read is a syntax error, before the server sees it.
    let CheckOutcome::Problem { syntax, .. } = db.check_statement("db.people.find({age: })").await.unwrap() else {
        panic!("not reported")
    };
    assert!(syntax);
    assert_eq!(db.check_statement("db.people.find({age: 1})").await.unwrap(), CheckOutcome::Ok);
    assert!(db.run("db.people.find({age: })", 1).await.is_err());
    // An unknown method says so.
    let err = db.run("db.people.mapReduce()", 1).await.unwrap_err();
    assert!(format!("{err:#}").contains("isn't a method"), "{err:#}");
    // The session info: its version.
    let info = db.session_info().await;
    assert!(info.iter().any(|(k, v)| k == "Version" && v.starts_with(db.version())), "{info:?}");
    let connections = info.iter().find(|(k, _)| k == "Connections").map(|(_, v)| v.as_str());
    assert!(connections.is_some_and(|c| c.contains(" in use, up to ")), "{info:?}");
    db.close().await;

    // Read-only: reads run, writes don't.
    let ro = Database::connect(ConnectParams { read_only: true, ..params }).await.unwrap();
    assert!(ro.run("db.people.find()", 10).await.is_ok());
    let refused = ro.run("db.people.deleteMany({})", 10).await.unwrap_err();
    assert!(format!("{refused:#}").contains("read-only"), "{refused:#}");
}

/// The catalog samples documents for their fields: it keeps the field
/// names and a type each, never a value. (A collection keyed by data
/// would show those keys: they are its field names.)
#[tokio::test]
async fn live_catalog_holds_no_value_mongodb() {
    const SECRET: &str = "ROW-VALUE-NEVER-IN-CATALOG";
    let Some((db, _)) = own_live_db("podelis_mongo_no_rows").await else { return };
    db.run("db.leak_check.drop()", 1).await.unwrap();
    db.run(&format!("db.leak_check.insertMany([{{note: '{SECRET}', n: 314159}}, {{note: '{SECRET}', tags: ['{SECRET}']}}])"), 1)
        .await
        .unwrap();
    let catalog = format!("{:?}", db.catalog(false).await.unwrap());
    let comments = format!("{:?}", db.comments().await.unwrap());
    // Dropped before any check: a failure leaves no values behind.
    db.run("db.leak_check.drop()", 1).await.unwrap();
    assert!(catalog.contains("leak_check") && catalog.contains("note") && catalog.contains("tags"), "{catalog}");
    assert!(!catalog.contains(SECRET) && !catalog.contains("314159"), "{catalog}");
    assert!(!comments.contains(SECRET), "{comments}");
}

/// A cancel stops a call that would take long.
#[tokio::test]
async fn live_cancel_mongodb() {
    let Some((db, _)) = own_live_db("podelis_mongo_cancel").await else { return };
    db.run("db.t.insertOne({x: 1})", 1).await.unwrap();
    let slow = db.clone();
    let handle = tokio::spawn(async move {
        slow.run("db.t.find({$where: 'sleep(5000) || true'})", 10).await
    });
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    db.cancel().await.unwrap();
    let result = tokio::time::timeout(std::time::Duration::from_secs(3), handle).await.expect("cancel took too long");
    let err = result.unwrap().unwrap_err();
    assert!(format!("{err:#}").contains("cancelled"), "{err:#}");
    assert_eq!(cells(&db.run("db.t.countDocuments({})", 1).await.unwrap()), vec![vec![Some("1")]]);
}

/// A transaction on the session: its writes are seen in it and gone once
/// it's aborted, kept once committed; a standalone server says it has none.
/// (On a replica set: `mongod --replSet`, as `docs/development.md` says;
/// `PODELIS_TEST_MONGO_REPLICA_SET` says the test server is one.)
#[tokio::test]
async fn live_transactions_mongodb() {
    let Some((db, _)) = own_live_db("podelis_mongo_txn").await else { return };
    db.run("db.t.insertOne({_id: 1, x: 1})", 1).await.unwrap();
    let hello = db.run("db.runCommand({hello: 1})", 1).await.unwrap();
    let replica_set = hello.columns.iter().any(|c| c == "setName" || c == "msg");
    // Said to be one (`PODELIS_TEST_MONGO_REPLICA_SET`): it must be.
    assert!(replica_set || std::env::var_os("PODELIS_TEST_MONGO_REPLICA_SET").is_none(), "not a replica set");
    let begun = db.run_once("db.startTransaction()", 1).await;
    if !replica_set {
        let said = crate::db::error_message(&begun.unwrap_err());
        assert!(said.contains("replica set") && said.contains("standalone"), "{said}");
        // Nothing was left open: a write runs as it is.
        db.run("db.t.insertOne({_id: 2})", 1).await.unwrap();
        return;
    }
    begun.unwrap();
    let again = db.run_once("db.startTransaction()", 1).await.unwrap_err();
    assert!(crate::db::error_message(&again).contains("open already"));
    let changed = db.run_once("db.t.updateMany({x: 1}, {$set: {x: 2}})", 1).await.unwrap();
    assert_eq!(changed.rows_affected, Some(1));
    let inside = db.run_once("db.t.find({x: 2})", 10).await.unwrap();
    assert_eq!(inside.total_rows, 1, "seen in it");
    // No limit (the command line's) is a limit, not a stream.
    let unlimited = db.run_once("db.t.find({x: 2})", usize::MAX).await.unwrap();
    assert_eq!(unlimited.rows.len(), 1, "read whole");
    // Streamed too, page by page, in the transaction.
    let (first, stream) = db.run_streaming("db.t.find({x: 2})", 10).await.unwrap();
    assert_eq!(first.rows.len(), 1, "streamed in it");
    drop(stream);
    db.run_once("db.abortTransaction()", 1).await.unwrap();
    assert_eq!(db.run("db.t.countDocuments({x: 2})", 1).await.unwrap().rows[0][0].as_deref(), Some("0"));
    db.run_once("db.startTransaction()", 1).await.unwrap();
    db.run_once("db.t.insertOne({_id: 3})", 1).await.unwrap();
    db.run_once("db.commitTransaction()", 1).await.unwrap();
    assert_eq!(db.run("db.t.countDocuments({})", 1).await.unwrap().rows[0][0].as_deref(), Some("2"));
    let none = db.run_once("db.commitTransaction()", 1).await.unwrap_err();
    assert!(crate::db::error_message(&none).contains("no transaction is open"));
}

/// The collections a read reads whole, by its plan: the guard on
/// production goes by it.
#[tokio::test]
async fn live_full_scans_mongodb() {
    let Some((db, _)) = own_live_db("podelis_mongo_scans").await else { return };
    db.run("db.t.insertMany([{_id: 1, x: 1}, {_id: 2, x: 2}])", 1).await.unwrap();
    let scans = db.full_scans("db.t.find({x: 2})").await.unwrap();
    assert_eq!(scans.iter().map(|(t, _)| t.as_str()).collect::<Vec<_>>(), ["t"]);
    assert!(db.full_scans("db.t.find({_id: 2})").await.unwrap().is_empty(), "by its index");
    assert!(db.full_scans("db.t.find({x: 2}).limit(1)").await.unwrap().is_empty(), "stopped early");
    let plan = db.run(&crate::language::explain(db.engine(), "db.t.aggregate([{$match: {x: 1}}])", "").unwrap(), 1).await;
    let plan = plan.unwrap();
    let shown = crate::language::format_plan(db.engine(), &plan.columns, &plan.rows).expect("a plan");
    assert!(shown.text.contains("COLLSCAN") && shown.notes.iter().any(|n| n.text.contains("reads all of t")), "{shown:?}");
}
