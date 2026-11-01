use super::*;

#[tokio::test]
async fn facts_are_kept_found_and_forgotten() {
    let dir = tempfile::tempdir().unwrap();
    let mut conn = open(dir.path(), "p1").await.unwrap();
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(path(dir.path(), "p1")).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
    let status = add(&mut conn, "orders.status", "", "3 = cancelled", "user", true).await.unwrap();
    assert_eq!((status.relation.as_str(), status.confirmed), ("means", true));
    add(&mut conn, "revenue", "is", "orders.total minus refunds", "user", true).await.unwrap();
    let guess =
        add(&mut conn, "customers.tier", "means", "gold spends over 1000", "model:claude", false).await.unwrap();
    add(&mut conn, "warehouses", "are", "only in Lithuania", "user", true).await.unwrap();
    assert!(add(&mut conn, " ", "means", "x", "user", true).await.is_err());

    // About the tables the question needs, then matching its words; only
    // confirmed ones.
    let tables = ["public.orders".to_string(), "public.customers".to_string()];
    let shown = relevant(&mut conn, &tables, "monthly revenue by tier").await.unwrap();
    let lines: Vec<String> = shown.iter().map(Fact::line).collect();
    assert_eq!(lines, ["orders.status means 3 = cancelled", "revenue is orders.total minus refunds"]);
    // Confirmed, it's shown too.
    assert!(confirm(&mut conn, guess.id).await.unwrap());
    let shown = relevant(&mut conn, &tables, "x").await.unwrap();
    assert!(shown.iter().any(|f| f.subject == "customers.tier"));

    // Search, with words that would be FTS syntax taken as words.
    let found = list(&mut conn, Some("refunds AND \"NEAR(")).await.unwrap();
    assert_eq!(found.len(), 1);
    assert!(forget(&mut conn, status.id).await.unwrap());
    assert!(!forget(&mut conn, status.id).await.unwrap());
    assert!(list(&mut conn, Some("cancelled")).await.unwrap().is_empty(), "the index follows");

    let text = export("shop", &list(&mut conn, None).await.unwrap());
    assert!(text.starts_with("# Podėlis memory · project shop"));
    assert!(text.contains("\nrevenue | is | orders.total minus refunds | user · "), "{text}");
}

#[test]
fn facts_are_read_as_typed() {
    let fact = |s: &str| parse_fact(s).map(|(a, b, c)| format!("{a} | {b} | {c}"));
    assert_eq!(fact("orders.status 3 = cancelled").as_deref(), Some("orders.status | means | 3 = cancelled"));
    assert_eq!(
        fact("revenue is orders.total minus refunds").as_deref(),
        Some("revenue | is | orders.total minus refunds")
    );
    assert_eq!(fact("quotes.side means ask").as_deref(), Some("quotes.side | means | ask"));
    assert_eq!(fact("flag is").as_deref(), Some("flag | means | is"));
    assert_eq!(fact("customer also called client").as_deref(), Some("customer | also called | client"));
    assert_eq!(fact("film aka movie").as_deref(), Some("film | also called | movie"));
    assert_eq!(
        fact("revenue defined as sum(payment.amount)").as_deref(),
        Some("revenue | defined as | sum(payment.amount)")
    );
    assert_eq!(
        fact("churned is defined as no rental in 90 days").as_deref(),
        Some("churned | defined as | no rental in 90 days")
    );
    assert_eq!(fact("x also called").as_deref(), Some("x | means | also called"));
    assert_eq!(
        fact("customer excluded by default deleted_at is not null").as_deref(),
        Some("customer | excluded by default | deleted_at is not null")
    );
    assert_eq!(fact("lonely"), None);
}

#[test]
fn a_fact_is_about_a_table_its_schema_or_columns_name() {
    let fact = |subject: &str| Fact {
        id: 1,
        subject: subject.into(),
        relation: "means".into(),
        value: "x".into(),
        source: "user".into(),
        confirmed: true,
        at: String::new(),
    };
    for subject in ["orders", "Orders.status", "public.orders", "public.orders.status"] {
        assert!(fact(subject).is_about("public.orders"), "{subject}");
    }
    assert!(fact("orders.status").is_about("orders"));
    assert!(!fact("order_items").is_about("public.orders"));
    assert!(!fact("public").is_about("public.orders"));
}

#[tokio::test]
async fn a_folder_with_odd_characters_keeps_its_memory() {
    let dir = tempfile::tempdir().unwrap();
    let odd = dir.path().join("a%20b?c");
    let mut conn = open(&odd, "p1").await.unwrap();
    add(&mut conn, "t", "means", "x", "user", true).await.unwrap();
    drop(conn);
    assert!(path(&odd, "p1").exists(), "the file is where its path says");
    let mut conn = open(&odd, "p1").await.unwrap();
    assert_eq!(list(&mut conn, None).await.unwrap().len(), 1);
}

#[tokio::test]
async fn a_forgotten_suggestion_is_remembered_as_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let mut conn = open(dir.path(), "p1").await.unwrap();
    let guess = add(&mut conn, "orders.status", "means", "3 = cancelled", "model:near", false).await.unwrap();
    let mine = add(&mut conn, "orders.total", "means", "VAT included", "user", true).await.unwrap();
    forget(&mut conn, guess.id).await.unwrap();
    forget(&mut conn, mine.id).await.unwrap();
    assert!(rejected(&mut conn, "Orders.Status", "3 = CANCELLED").await.unwrap());
    // Only a model's guesses: what the user forgets of their own isn't.
    assert!(!rejected(&mut conn, "orders.total", "VAT included").await.unwrap());
}

#[tokio::test]
async fn a_questions_facts_give_words_for_choosing_tables_not_histories() {
    let dir = tempfile::tempdir().unwrap();
    let mut conn = open(dir.path(), "p1").await.unwrap();
    add(&mut conn, "revenue", "is", "payment.amount", "user", true).await.unwrap();
    add(&mut conn, "revenue_report", "history", "created 2024-01-02; joins audit_log", "file:db", true).await.unwrap();
    add(&mut conn, "revenue", "means", "net of refunds", "model:claude", false).await.unwrap();
    let memory = for_question(&mut conn, "revenue per category").await.unwrap();
    // The confirmed fact's words; not the history's, not the unconfirmed one's.
    assert_eq!(memory.words(), " revenue payment.amount");
    // All matching ones are still shown, the history last.
    let lines: Vec<String> = memory.about(&[]).iter().map(Fact::line).collect();
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert!(lines[1].starts_with("revenue_report history"), "{lines:?}");
}

#[tokio::test]
async fn another_name_and_a_defined_term_bring_their_tables() {
    let dir = tempfile::tempdir().unwrap();
    let mut conn = open(dir.path(), "p1").await.unwrap();
    add(&mut conn, "customer", ALSO_CALLED, "client", "user", true).await.unwrap();
    add(&mut conn, "revenue", DEFINED_AS, "sum(payment.amount)", "user", true).await.unwrap();
    // "clients": the stem finds the other name, and its table's name comes
    // with it for choosing tables.
    let memory = for_question(&mut conn, "how many clients rented").await.unwrap();
    assert_eq!(memory.words(), " customer client");
    let memory = for_question(&mut conn, "revenue per store").await.unwrap();
    assert_eq!(memory.words(), " revenue sum(payment.amount)");
}

#[tokio::test]
async fn a_fact_is_changed_and_found_by_its_new_words() {
    let dir = tempfile::tempdir().unwrap();
    let mut conn = open(dir.path(), "p1").await.unwrap();
    let guess = add(&mut conn, "orders.status", "means", "3 = cancelled", "model:claude", false).await.unwrap();
    let changed = update(&mut conn, guess.id, "orders.status means 3 = refunded").await.unwrap().unwrap();
    // The user's word now: confirmed, theirs.
    assert_eq!((changed.value.as_str(), changed.confirmed, changed.source.as_str()), ("3 = refunded", true, "user"));
    // The model's guess, corrected, isn't suggested again.
    assert!(rejected(&mut conn, "orders.status", "3 = cancelled").await.unwrap());
    // The search index follows the edit: the new words find it, the old don't.
    assert_eq!(list(&mut conn, Some("refunded")).await.unwrap().len(), 1);
    assert!(list(&mut conn, Some("cancelled")).await.unwrap().is_empty());
    // Forgetting it after an edit leaves the index clean too.
    assert!(forget(&mut conn, guess.id).await.unwrap());
    assert!(list(&mut conn, Some("refunded")).await.unwrap().is_empty());
    assert!(update(&mut conn, 999, "x means y").await.unwrap().is_none());
}

#[tokio::test]
async fn an_edit_keeps_the_facts_own_relation_and_outlives_learning_again() {
    let dir = tempfile::tempdir().unwrap();
    let mut conn = open(dir.path(), "p1").await.unwrap();
    let learned = [crate::gateway::migrations::Learned {
        subject: "orders".into(),
        relation: "history".into(),
        value: "created in 001".into(),
    }];
    replace_source(&mut conn, "file:/db/migrations", &learned).await.unwrap();
    let id = list(&mut conn, None).await.unwrap()[0].id;
    // A relation adding doesn't know (`history`) stays what it was.
    let changed = update(&mut conn, id, "orders history created in 001, renamed in 004").await.unwrap().unwrap();
    assert_eq!((changed.relation.as_str(), changed.value.as_str()), ("history", "created in 001, renamed in 004"));
    // A value that only starts like another relation isn't read as one.
    let fact = add(&mut conn, "revenue", "means", "defined as gross", "user", true).await.unwrap();
    let changed = update(&mut conn, fact.id, "revenue means defined as net").await.unwrap().unwrap();
    assert_eq!((changed.relation.as_str(), changed.value.as_str()), ("means", "defined as net"));
    // Changing the relation itself works as when adding.
    let changed = update(&mut conn, fact.id, "revenue defined as sum(payment.amount)").await.unwrap().unwrap();
    assert_eq!((changed.relation.as_str(), changed.value.as_str()), ("defined as", "sum(payment.amount)"));
    // Learning the folder again doesn't delete the user's edit.
    replace_source(&mut conn, "file:/db/migrations", &learned).await.unwrap();
    let facts = list(&mut conn, None).await.unwrap();
    assert!(facts.iter().any(|f| f.value == "created in 001, renamed in 004" && f.source == "user"), "{facts:?}");
    // No value: an error, nothing changed.
    assert!(update(&mut conn, fact.id, "revenue").await.is_err());
}
