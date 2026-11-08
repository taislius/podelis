//! The schema snapshots the context's eval (`podelis-ai`'s
//! `context_eval.rs`) runs on, made from live databases: here, in the
//! app's crate, because it opens them by URL as the app does.

use podelis_ai::context::Schema;

/// Writes `crates/podelis-ai/testdata/context-eval/<name>.json` for each `name=address` in
/// `PODELIS_EVAL_DUMP` (space-separated; a URL or a SQLite file):
///
/// ```sh
/// PODELIS_EVAL_DUMP="pagila=postgres://postgres:…@127.0.0.1:55433/pagila?sslmode=disable \
///   sakila=mysql://root:…@127.0.0.1:53385/sakila?ssl-mode=disabled chinook=$HOME/podelis-test/Chinook.sqlite" \
///   cargo test -p podelis dump_schemas -- --ignored
/// ```
#[tokio::test]
#[ignore = "needs the databases; makes the snapshots"]
async fn dump_schemas() {
    let spec = std::env::var("PODELIS_EVAL_DUMP").expect("PODELIS_EVAL_DUMP");
    let folder = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("crates/podelis-ai/testdata/context-eval");
    for pair in spec.split_whitespace() {
        let (name, address) = pair.split_once('=').expect("name=address");
        let form =
            crate::config::draft::ConnectionDraft::from_address(&crate::config::draft::classify(address)).unwrap();
        let mut params = form.params().unwrap();
        params.read_only = true;
        let engine = params.engine;
        let db = crate::db::Database::connect(params).await.unwrap();
        let mut tables = db.catalog(false).await.unwrap();
        tables.retain(|(t, _)| !t.system);
        tables.sort_by(|a, b| (&a.0.schema, &a.0.name).cmp(&(&b.0.schema, &b.0.name)));
        let mut links = db.links().await.unwrap();
        links.sort_by(|a, b| (&a.schema, &a.table, &a.column).cmp(&(&b.schema, &b.table, &b.column)));
        let mut comments = db.comments().await.unwrap();
        comments.sort_by(|a, b| (&a.schema, &a.table, &a.column).cmp(&(&b.schema, &b.table, &b.column)));
        let snapshot = Schema { engine: engine.label().to_string(), dialect: engine, tables, links, comments };
        let file = folder.join(format!("{name}.json"));
        std::fs::write(&file, serde_json::to_string_pretty(&snapshot).unwrap() + "\n").unwrap();
    }
}
