use super::*;

fn files() -> Vec<(String, String)> {
    vec![
        (
            "20230105_create_orders.sql".into(),
            "-- One row per checkout.\nCREATE TABLE IF NOT EXISTS public.orders (\n  id bigint PRIMARY KEY,\n  status int NOT NULL\n);\n\n\
             -- unrelated, a blank line away\n\nCREATE TABLE customers (id int primary key);\n"
                .into(),
        ),
        (
            "2024_03_01_refunds.sql".into(),
            "-- Refunds were booked as negative orders before this.\n\
             ALTER TABLE orders ADD COLUMN refund_id int;\n\
             ALTER TABLE ONLY \"orders\" ADD CONSTRAINT orders_refund_fk FOREIGN KEY (refund_id) REFERENCES refunds(id), \
             ADD COLUMN IF NOT EXISTS refunded_at timestamptz, DROP COLUMN legacy_flag;\n\
             ALTER TABLE customers RENAME COLUMN mail TO email;\n\
             COMMENT ON COLUMN public.orders.status IS '3 = cancelled, 4 = refunded';\n"
                .into(),
        ),
        ("V7__drop_tmp.sql".into(), "DROP TABLE IF EXISTS tmp_import;".into()),
    ]
}

#[test]
fn a_tables_history_and_its_authors_notes() {
    let learned = learn(&files(), engine_of(&[]));
    let fact = |subject: &str, relation: &str| {
        learned.iter().find(|l| l.subject == subject && l.relation == relation).map(|l| l.value.as_str())
    };
    assert_eq!(
        fact("orders", "history"),
        Some(
            "created (2023-01-05, 20230105_create_orders.sql); refund_id added (2024-03-01, 2024_03_01_refunds.sql); \
             refunded_at added (2024-03-01, 2024_03_01_refunds.sql); legacy_flag dropped (2024-03-01, 2024_03_01_refunds.sql)"
        )
    );
    assert_eq!(
        fact("customers", "history"),
        Some(
            "created (2023-01-05, 20230105_create_orders.sql); mail renamed to email (2024-03-01, 2024_03_01_refunds.sql)"
        )
    );
    assert_eq!(fact("tmp_import", "history"), Some("dropped (V7__drop_tmp.sql)"));
    // The comment right above a statement is about it; one a blank line
    // away isn't.
    assert_eq!(fact("orders", "note"), Some("One row per checkout."));
    assert_eq!(fact("orders.refund_id", "note"), Some("Refunds were booked as negative orders before this."));
    assert_eq!(fact("customers", "note"), None);
    assert_eq!(fact("orders.status", "is"), Some("3 = cancelled, 4 = refunded"));
}

#[test]
fn dates_come_from_the_files_names() {
    assert_eq!(date_of("20240301120000_add_x.sql").as_deref(), Some("2024-03-01"));
    assert_eq!(date_of("db/2024-03-01-add_x.sql").as_deref(), Some("2024-03-01"));
    assert_eq!(date_of("V12__add_x.sql"), None);
    assert_eq!(date_of("99999999_x.sql"), None);
}

#[test]
fn a_folder_is_read_in_name_order_with_its_subfolders() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("2024")).unwrap();
    std::fs::write(dir.path().join("2024/20240301_b.sql"), "ALTER TABLE t ADD c int;").unwrap();
    std::fs::create_dir(dir.path().join("old")).unwrap();
    std::fs::write(dir.path().join("old/20220101_z.sql"), "CREATE TABLE z (id int);").unwrap();
    std::fs::write(dir.path().join("20230101_a.sql"), "CREATE TABLE t (id int);").unwrap();
    std::fs::write(dir.path().join("README.md"), "not sql").unwrap();
    // A link back up isn't followed.
    std::os::unix::fs::symlink(dir.path(), dir.path().join("2024/loop")).unwrap();
    let files = read_folder(dir.path()).unwrap();
    let names: Vec<&str> = files.iter().map(|(n, _)| n.as_str()).collect();
    // By file name: a folder's name doesn't put its files first or last.
    assert_eq!(names, ["old/20220101_z.sql", "20230101_a.sql", "2024/20240301_b.sql"]);
    assert!(read_folder(&dir.path().join("2024/none")).is_err());
}

/// Each engine's own quotes around a name (MySQL's backticks among them)
/// are read as the name.
#[test]
fn names_quoted_the_engines_way() {
    for engine in Engine::ALL {
        let table = engine.quote_ident("Order Archive");
        let sql = format!(
            "-- Old orders, kept for audits.\nCREATE TABLE {table} (id int);\nALTER TABLE {table} ADD COLUMN note text;\n"
        );
        let learned = learn(&[("1_archive.sql".into(), sql)], engine);
        let about: Vec<_> = learned.iter().filter(|l| l.subject.to_lowercase().contains("order archive")).collect();
        assert!(about.iter().any(|l| l.value.contains("Old orders")), "{engine:?}: {learned:?}");
        assert!(about.iter().any(|l| l.value.contains("note")), "{engine:?}: {learned:?}");
    }
}

#[test]
fn the_engine_most_connections_use() {
    let profile = |engine| crate::config::store::ConnectionProfile {
        engine,
        ..crate::db::tests::file_profile("x", std::path::Path::new("x"))
    };
    assert_eq!(engine_of(&[]), Engine::ALL[0]);
    let [first, second, ..] = Engine::ALL;
    assert_eq!(engine_of(&[profile(second), profile(first), profile(second)]), second);
    assert_eq!(engine_of(&[profile(second), profile(first)]), first, "a tie: the first listed");
}
