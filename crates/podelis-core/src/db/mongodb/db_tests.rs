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
    db.close().await;

    // Read-only: reads run, writes don't.
    let ro = Database::connect(ConnectParams { read_only: true, ..params }).await.unwrap();
    assert!(ro.run("db.people.find()", 10).await.is_ok());
    let refused = ro.run("db.people.deleteMany({})", 10).await.unwrap_err();
    assert!(format!("{refused:#}").contains("read-only"), "{refused:#}");
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
