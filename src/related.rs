//! A table's neighbours through foreign keys, two steps each way: what it
//! points to (and what those point to), and what points to it (and to
//! those). For finding your way in a schema you don't know, and for agents
//! working out joins.

use std::fmt::Write as _;

use crate::db::Link;

/// Whether `schema.table` is the table named (an empty `want_schema`
/// matches any schema).
fn is(schema: &str, table: &str, want_schema: &str, want_table: &str) -> bool {
    table.eq_ignore_ascii_case(want_table) && (want_schema.is_empty() || schema.eq_ignore_ascii_case(want_schema))
}

/// The links from a table, and the links to it.
fn from<'a>(links: &'a [Link], schema: &str, table: &str) -> Vec<&'a Link> {
    links.iter().filter(|l| is(&l.schema, &l.table, schema, table)).collect()
}

fn to<'a>(links: &'a [Link], schema: &str, table: &str) -> Vec<&'a Link> {
    links.iter().filter(|l| is(&l.target_schema, &l.target_table, schema, table)).collect()
}

fn target_column(l: &Link) -> &str {
    l.target_column.as_deref().unwrap_or("its key")
}

/// `table`'s related tables as text. `schema` may be empty to match any.
pub fn describe(links: &[Link], schema: &str, table: &str) -> String {
    let mut out = format!("{table}\n");
    let points_to = from(links, schema, table);
    let _ = writeln!(out, "\n  points to");
    if points_to.is_empty() {
        let _ = writeln!(out, "    nothing");
    }
    for l in &points_to {
        let _ = writeln!(out, "    → {} ({} → {})", l.target_table, l.column, target_column(l));
        for next in from(links, &l.target_schema, &l.target_table) {
            if !is(&next.target_schema, &next.target_table, schema, table) {
                let _ = writeln!(out, "        → {} ({} → {})", next.target_table, next.column, target_column(next));
            }
        }
    }
    let pointed_by = to(links, schema, table);
    let _ = writeln!(out, "\n  pointed to by");
    if pointed_by.is_empty() {
        let _ = writeln!(out, "    nothing");
    }
    for l in &pointed_by {
        let _ = writeln!(out, "    ← {} ({} → {})", l.table, l.column, target_column(l));
        for next in to(links, &l.schema, &l.table) {
            if !is(&next.schema, &next.table, schema, table) {
                let _ = writeln!(out, "        ← {} ({} → {})", next.table, next.column, target_column(next));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn link(table: &str, column: &str, target: &str) -> Link {
        Link {
            schema: "s".into(),
            table: table.into(),
            column: column.into(),
            target_schema: "s".into(),
            target_table: target.into(),
            target_column: Some("id".into()),
        }
    }

    #[test]
    fn two_steps_each_way() {
        let links = [
            link("orders", "customer_id", "customers"),
            link("customers", "region_id", "regions"),
            link("order_items", "order_id", "orders"),
            link("shipments", "order_item_id", "order_items"),
            // Back to the table itself: not repeated as its own neighbour.
            link("customers", "last_order_id", "orders"),
        ];
        assert_eq!(
            describe(&links, "s", "orders"),
            "orders\n\n  points to\n    → customers (customer_id → id)\n        → regions (region_id → id)\n\n  \
             pointed to by\n    ← order_items (order_id → id)\n        ← shipments (order_item_id → id)\n    \
             ← customers (last_order_id → id)\n"
        );
        assert!(describe(&links, "", "regions").contains("points to\n    nothing"));
    }
}
