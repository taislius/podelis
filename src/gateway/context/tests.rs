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

#[test]
fn words_match_by_their_stem() {
    assert!(words("which films were never rented").contains("rent"));
    assert!(words("rental").contains("rent"));
    assert!(words("rentals").contains("rent"));
    assert!(words("return_date").contains("return") && words("returned late").contains("return"));
    assert!(words("shipped").contains("ship") && words("billing").contains("bill"));
    assert!(words("staffed").contains("staff") && words("staffing").contains("staff"));
    // Too short a stem would match too much.
    assert!(!words("used").contains("us"));
    assert!(!words("total").contains("tot"));
}

/// A rental shop: `rental` joins `customer` to `inventory`, which joins
/// `film`; `payment` is split into monthly partitions, and only the
/// partitions have keys (as in Pagila).
fn rentals() -> Schema {
    let part = |name: &str| {
        let mut t = table(name, 1000, &[("payment_id", "integer", true), ("customer_id", "integer", false)]);
        t.0.partition_of = Some(("public".into(), "payment".into()));
        t
    };
    let mut tables = vec![
        table("customer", 600, &[("customer_id", "integer", true), ("email", "text", false)]),
        table("film", 1000, &[("film_id", "integer", true), ("title", "text", false)]),
        table("inventory", 4500, &[("inventory_id", "integer", true), ("film_id", "integer", false)]),
        table("payment", 0, &[("payment_id", "integer", true), ("customer_id", "integer", false)]),
        table(
            "rental",
            16000,
            &[("rental_id", "integer", true), ("inventory_id", "integer", false), ("customer_id", "integer", false)],
        ),
    ];
    let mut links = vec![
        link("inventory", "film_id", "film"),
        link("rental", "inventory_id", "inventory"),
        link("rental", "customer_id", "customer"),
    ];
    for month in 1..=30 {
        let name = format!("payment_p2022_{month:02}");
        tables.push(part(&name));
        links.push(link(&name, "customer_id", "customer"));
    }
    Schema { engine: "PostgreSQL".into(), dialect: crate::db::Engine::Postgres, tables, links, comments: Vec::new() }
}

#[test]
fn partitions_are_their_parent() {
    let c = build(&rentals(), "payments of each customer");
    assert!(names(&c).contains(&"public.payment"), "{:?}", c.tables);
    assert!(!names(&c).iter().any(|n| n.contains("_p2022_")), "{:?}", c.tables);
    // The parent shows the partitions' key, once.
    assert_eq!(c.text.matches("customer_id integer → customer.id").count(), 2, "{}", c.text);
    assert_eq!(c.left_out, 0);
}

#[test]
fn a_table_between_a_chosen_one_and_a_neighbour_comes_first() {
    let c = build(&rentals(), "customers who returned a film late");
    let rental = c.tables.iter().position(|t| t.name == "public.rental").expect("rental is shown");
    assert_eq!(c.tables[rental].why, "links customer and inventory");
    // Before what joins only one chosen table.
    let payment = c.tables.iter().position(|t| t.name == "public.payment").unwrap_or(usize::MAX);
    assert!(rental < payment, "{:?}", c.tables);
}

#[test]
fn partitions_of_partitions_are_their_top_table_with_its_rows() {
    let mut s = rentals();
    // payment_p2022_01 is now under payment_2022, itself under payment.
    let mut middle = table("payment_2022", 0, &[("payment_id", "integer", true), ("customer_id", "integer", false)]);
    middle.0.partition_of = Some(("public".into(), "payment".into()));
    s.tables.push(middle);
    s.tables[5].0.partition_of = Some(("public".into(), "payment_2022".into()));
    assert_eq!(s.tables[5].0.name, "payment_p2022_01");
    let c = build(&s, "payments of each customer");
    assert!(!names(&c).iter().any(|n| n.contains("2022")), "{:?}", c.tables);
    // Thirty partitions of 1,000 rows; the parents hold none.
    assert!(c.text.contains("public.payment, ~30000 rows"), "{}", c.text);
    assert_eq!(c.text.matches("customer_id integer → customer.id").count(), 2, "{}", c.text);
}

#[test]
fn neighbours_of_tables_matched_only_by_a_column_dont_pull_each_other_in() {
    let s = Schema {
        engine: "PostgreSQL".into(),
        dialect: crate::db::Engine::Postgres,
        tables: vec![
            table("x", 1, &[("id", "integer", true), ("colour", "text", false)]),
            table("y", 1, &[("id", "integer", true), ("colour_code", "text", false)]),
            table("b", 1, &[("id", "integer", true), ("x_id", "integer", false), ("c_id", "integer", false)]),
            table("c", 1, &[("id", "integer", true), ("y_id", "integer", false)]),
        ],
        links: vec![link("b", "x_id", "x"), link("c", "y_id", "y"), link("b", "c_id", "c")],
        comments: Vec::new(),
    };
    let c = build(&s, "colour");
    assert_eq!(names(&c), ["public.x", "public.y"]);
}

#[test]
fn facts_about_missing_tables_are_left_out_as_stale() {
    let fact = |subject: &str, relation: &str, value: &str| super::super::memory::Fact {
        id: 1,
        subject: subject.into(),
        relation: relation.into(),
        value: value.into(),
        source: "user".into(),
        confirmed: true,
        at: String::new(),
    };
    let schema = shop();
    let facts = [
        fact("customers", "excluded by default", "email like '%@test.example'"),
        fact("orders.status", "means", "3 = cancelled"),
        fact("invoices.state", "means", "2 = paid"),
        fact("revenue", "defined as", "sum(orders.total)"),
    ];
    let built = build(&schema, "orders per customer").with_facts(&facts, &schema);
    assert!(built.text.contains("customers excluded by default email like"), "{}", built.text);
    assert!(built.text.contains("excluded by default W`: leave out"), "the model is told what it means");
    // `invoices` isn't in this database: left out, said as stale.
    assert_eq!(built.stale, ["invoices.state means 2 = paid"]);
    assert!(!built.text.contains("invoices"));
    // A word that names no table (a term) isn't stale.
    assert!(built.facts.iter().any(|f| f.starts_with("revenue defined as")), "{:?}", built.facts);
}
