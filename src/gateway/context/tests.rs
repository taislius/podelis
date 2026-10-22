use super::*;

fn table(name: &str, rows: i64, columns: &[(&str, &str, bool)]) -> (TableInfo, Vec<ColumnInfo>) {
    let info = TableInfo {
        schema: "public".into(),
        name: name.into(),
        is_view: false,
        rows: Some(rows),
        bytes: None,
        system: false,
        partition_of: None,
    };
    let cols = columns
        .iter()
        .map(|(n, t, pk)| ColumnInfo { name: (*n).into(), data_type: (*t).into(), nullable: !pk, primary_key: *pk })
        .collect();
    (info, cols)
}

fn link(table: &str, column: &str, target: &str) -> Link {
    Link {
        schema: "public".into(),
        table: table.into(),
        column: column.into(),
        target_schema: "public".into(),
        target_table: target.into(),
        target_column: Some("id".into()),
    }
}

fn comment(table: &str, column: Option<&str>, text: &str) -> Comment {
    Comment { schema: "public".into(), table: table.into(), column: column.map(str::to_string), text: text.into() }
}

/// A shop: what's sold, to whom, and some tables beside it.
fn shop() -> Schema {
    Schema {
        engine: "PostgreSQL".into(),
        dialect: crate::db::Engine::Postgres,
        tables: vec![
            table(
                "audit_log",
                9_000_000,
                &[("id", "bigint", true), ("happened_at", "timestamptz", false), ("payload", "jsonb", false)],
            ),
            table("categories", 40, &[("id", "integer", true), ("title", "text", false)]),
            table(
                "customers",
                24_800,
                &[("id", "integer", true), ("email", "text", false), ("country", "text", false)],
            ),
            table(
                "order_items",
                2_100_000,
                &[
                    ("id", "bigint", true),
                    ("order_id", "bigint", false),
                    ("product_id", "integer", false),
                    ("qty", "integer", false),
                ],
            ),
            table(
                "orders",
                942_000,
                &[
                    ("id", "bigint", true),
                    ("customer_id", "integer", false),
                    ("total", "numeric", false),
                    ("placed_at", "timestamptz", false),
                ],
            ),
            table(
                "products",
                3_100,
                &[("id", "integer", true), ("category_id", "integer", false), ("name", "text", false)],
            ),
            table("warehouses", 6, &[("id", "integer", true), ("city", "text", false)]),
        ],
        links: vec![
            link("order_items", "order_id", "orders"),
            link("order_items", "product_id", "products"),
            link("orders", "customer_id", "customers"),
            link("products", "category_id", "categories"),
        ],
        comments: vec![
            comment("orders", Some("total"), "Revenue of the order, VAT included"),
            comment("orders", None, "One row per checkout"),
        ],
    }
}

fn names(c: &Context) -> Vec<&str> {
    c.tables.iter().map(|t| t.name.as_str()).collect()
}

#[test]
fn the_tables_a_question_names_then_what_joins_them() {
    let c = build(&shop(), "Revenue by product category in September");
    // Matched by name (products, categories) and by a comment (orders);
    // order_items links two of them; nothing else.
    assert_eq!(names(&c), ["public.categories", "public.products", "public.orders", "public.order_items"]);
    let why: Vec<&str> = c.tables.iter().map(|t| t.why.as_str()).collect();
    assert!(why[0].contains("its name (category)"), "{why:?}");
    assert!(why[2].contains("its comments (revenue)"), "{why:?}");
    assert_eq!(why[3], "links orders and products");
    // Keys, where foreign keys point, comments; never the rest.
    assert!(c.text.contains("public.orders, ~942000 rows \"One row per checkout\""), "{}", c.text);
    assert!(c.text.contains("  total numeric \"Revenue of the order, VAT included\""), "{}", c.text);
    assert!(c.text.contains("  product_id integer → products.id"), "{}", c.text);
    assert!(c.text.contains("  id bigint PK\n"), "{}", c.text);
    assert!(!c.text.contains("audit_log") && !c.text.contains("warehouses"));
}

#[test]
fn neighbours_of_one_table_come_too() {
    let c = build(&shop(), "customers from Lithuania");
    assert_eq!(names(&c), ["public.customers", "public.orders"]);
    assert_eq!(c.tables[1].why, "joins customers");
    // A column's name counts: "country".
    let c = build(&shop(), "which country buys most");
    assert_eq!(c.tables[0].name, "public.customers");
    assert!(c.tables[0].why.contains("columns country"), "{:?}", c.tables);
}

#[test]
fn identifiers_and_plurals_are_split_and_matched() {
    assert!(words("orderItems").is_superset(&["order".into(), "items".into(), "item".into()].into()));
    assert!(words("order_items").contains("item"));
    assert!(words("Categories").contains("category"));
    assert!(words("addresses").contains("address"));
    assert!(!words("how many of the ids").contains("how"));
}

#[test]
fn nothing_matched_a_small_schema_goes_whole_a_big_one_says_so() {
    let mut small = shop();
    let c = build(&small, "anything at all?");
    assert_eq!(c.tables.len(), 7);
    assert!(c.tables.iter().all(|t| t.why == "the schema is small: every table"));
    for i in 0..20 {
        small.tables.push(table(&format!("t{i}"), 1, &[("id", "integer", true)]));
    }
    let c = build(&small, "anything at all?");
    assert!(c.tables.is_empty() && c.text.contains("No table matched"), "{}", c.text);
}

#[test]
fn the_budget_keeps_the_best_and_counts_the_rest() {
    let mut big = shop();
    let wide: Vec<(String, String, bool)> =
        (0..60).map(|i| (format!("sale_note_{i}"), "text".to_string(), false)).collect();
    let cols: Vec<(&str, &str, bool)> = wide.iter().map(|(n, t, pk)| (n.as_str(), t.as_str(), *pk)).collect();
    for i in 0..12 {
        big.tables.push(table(&format!("sales_{i}"), 1, &cols));
    }
    let c = build(&big, "sales notes");
    assert_eq!(c.tables.len(), MAX_ENTRY.min(c.tables.len()));
    assert!(c.left_out > 0);
    assert!(c.text.len() <= MAX_CHARS, "{}", c.text.len());
    // Long tables are listed with a count of what's left out.
    assert!(c.text.contains("… 20 more columns"), "{}", c.text);
}

#[test]
fn system_tables_are_never_shown() {
    let mut s = shop();
    s.tables[1].0.system = true;
    let c = build(&s, "category");
    assert!(!names(&c).contains(&"public.categories"));
}
