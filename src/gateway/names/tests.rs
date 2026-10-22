use super::*;
use crate::db::{ColumnInfo, TableInfo};

fn schema() -> Schema {
    let table = |name: &str, cols: &[&str]| {
        let info = TableInfo {
            schema: "public".into(),
            name: name.into(),
            is_view: false,
            rows: None,
            bytes: None,
            system: false,
            partition_of: None,
        };
        let cols = cols
            .iter()
            .map(|c| ColumnInfo { name: (*c).into(), data_type: "int".into(), nullable: true, primary_key: *c == "id" })
            .collect();
        (info, cols)
    };
    Schema {
        engine: "PostgreSQL".into(),
        dialect: Engine::Postgres,
        tables: vec![
            table("orders", &["id", "customer_id", "total", "placed_at"]),
            table("customers", &["id", "email", "country"]),
        ],
        links: Vec::new(),
        comments: Vec::new(),
    }
}

fn check(sql: &str) -> Vec<String> {
    problems(sql, Engine::Postgres, &schema())
}

#[test]
fn real_tables_and_columns_pass() {
    let ok = [
        "SELECT c.country, sum(o.total) AS revenue FROM orders o JOIN customers c ON c.id = o.customer_id \
         WHERE o.placed_at >= CURRENT_DATE - INTERVAL '30 days' GROUP BY c.country ORDER BY revenue DESC",
        "select country, count(*) from customers group by 1",
        "SELECT EXTRACT(MONTH FROM placed_at) AS m, total FROM public.orders WHERE total > 100 OR true",
        "SELECT id FROM orders WHERE customer_id IN (SELECT id FROM customers WHERE country = 'LT')",
        "SELECT id FROM orders UNION SELECT id FROM customers ORDER BY 1",
    ];
    for sql in ok {
        assert_eq!(check(sql), Vec::<String>::new(), "{sql}");
    }
}

#[test]
fn columns_every_table_has_pass() {
    assert_eq!(problems("SELECT rowid, id FROM orders", Engine::Sqlite, &schema()), Vec::<String>::new());
    assert_eq!(check("SELECT ctid FROM orders"), Vec::<String>::new());
}

#[test]
fn unknown_tables_and_columns_are_named() {
    assert_eq!(check("SELECT * FROM order_lines"), ["there's no table `order_lines`"]);
    assert_eq!(check("select price from orders"), ["no table it reads has a column `price`"]);
    assert_eq!(check("SELECT o.amount FROM orders o"), ["no table it reads has a column `amount`"]);
    assert_eq!(check("SELECT * FROM sales.orders"), ["there's no table `sales.orders`"]);
    // A column of a table the query doesn't read.
    assert_eq!(check("SELECT id FROM orders WHERE email IS NULL"), ["no table it reads has a column `email`"]);
}

#[test]
fn ctes_and_subqueries_have_only_their_tables_checked() {
    let cte = "WITH monthly AS (SELECT date_trunc('month', placed_at) AS month, sum(total) AS revenue FROM orders GROUP BY 1) \
               SELECT month, revenue FROM monthly";
    assert_eq!(check(cte), Vec::<String>::new());
    let derived = "SELECT x.n FROM (SELECT count(*) AS n FROM customers) x";
    assert_eq!(check(derived), Vec::<String>::new());
    assert_eq!(check("WITH m AS (SELECT 1) SELECT * FROM m JOIN nowhere ON true"), ["there's no table `nowhere`"]);
}

#[test]
fn what_isnt_sql_says_so() {
    assert!(check("SELEC id FROM orders")[0].starts_with("it doesn't parse as PostgreSQL"));
    assert_eq!(check("   "), ["there's no SQL in it"]);
}

#[test]
fn the_sql_is_taken_from_the_answer() {
    let answer = "Here it is:\n```sql\nSELECT 1;\n```\nIt counts nothing.";
    assert_eq!(split_answer(answer), (Some("SELECT 1;".into()), "Here it is:\nIt counts nothing.".into()));
    assert_eq!(split_answer("```\nselect 2\n```").0.as_deref(), Some("select 2"));
    // Whatever the label, it isn't SQL.
    assert_eq!(split_answer("```postgresql\nselect 4\n```").0.as_deref(), Some("select 4"));
    assert_eq!(split_answer("```SQL\nselect 5\n```").0.as_deref(), Some("select 5"));
    assert_eq!(split_answer("Try ```select 6``` there.").0.as_deref(), Some("select 6"));
    // What follows the block keeps its own lines (FACT lines are read by line).
    let (_, said) = split_answer("Here:\n```sql\nselect 7\n```\nFACT: a | means | b");
    assert_eq!(said, "Here:\nFACT: a | means | b");
    assert_eq!(split_answer("SELECT 3 FROM t").0.as_deref(), Some("SELECT 3 FROM t"));
    assert_eq!(
        split_answer("I can't answer that from these tables."),
        (None, "I can't answer that from these tables.".into())
    );
}
