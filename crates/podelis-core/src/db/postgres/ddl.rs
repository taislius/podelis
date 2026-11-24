//! A PostgreSQL table as a CREATE TABLE, written from the server's own
//! functions, which write each part exactly as it keeps it: the columns
//! with their types (`format_type`), defaults (`pg_get_expr`), identity
//! and generated columns; every constraint whole (`pg_get_constraintdef`:
//! a foreign key with its ON DELETE, ON UPDATE and DEFERRABLE); then the
//! indexes no constraint made (`pg_get_indexdef`), the triggers
//! (`pg_get_triggerdef`), row-level security and its policies, and the
//! comments. A partition is written `PARTITION OF` its parent, a child
//! `INHERITS` its parents and a typed table `OF` its type, each with only
//! what's its own. PostgreSQL has no `SHOW CREATE TABLE`; `pg_dump` writes
//! the same from the same functions.

use std::fmt::Write as _;

use anyhow::Result;
use sqlx::{PgConnection, PgPool};

use crate::db::Engine;

/// A column: name, type, NOT NULL, default (or generation) expression,
/// identity (`a` always, `d` by default), generated (`s` stored, `v`
/// virtual), collation when not its type's (qualified where it isn't
/// `pg_catalog`'s), and whether it's the table's own (not inherited).
type Column = (String, String, bool, Option<String>, String, String, Option<String>, bool);

/// The table: its oid, persistence (`u` unlogged), partition key; the
/// parent it's a partition of and for which values; the parents it
/// inherits from; the type it's made of; row-level security (on, forced).
type Table = (String, String, Option<String>, Option<String>, Option<String>, Option<String>, Option<String>, bool, bool);

/// The table `schema.table` as a CREATE TABLE and what goes with it; `None`
/// if it isn't a table (a view, a sequence) or isn't there.
///
/// Written with the search path empty (in a transaction of its own, as
/// `pg_dump` does), so every name the server writes is qualified: a type,
/// a referenced table, a function in a trigger mean the same run anywhere.
pub(super) async fn table_ddl(p: &PgPool, schema: &str, table: &str) -> Result<Option<String>> {
    let mut tx = p.begin().await?;
    sqlx::query("SET LOCAL search_path = ''").execute(&mut *tx).await?;
    let ddl = written(&mut tx, schema, table).await;
    // Nothing to keep: what's written is read only.
    tx.rollback().await?;
    ddl
}

/// `table_ddl`'s work, on `c` with the search path empty.
async fn written(c: &mut PgConnection, schema: &str, table: &str) -> Result<Option<String>> {
    let found: Option<Table> = sqlx::query_as(
        "SELECT c.oid::text, c.relpersistence::text,
                CASE WHEN c.relkind = 'p' THEN pg_get_partkeydef(c.oid) END,
                (SELECT format('%I.%I', pn.nspname, pc.relname) FROM pg_inherits i
                   JOIN pg_class pc ON pc.oid = i.inhparent
                   JOIN pg_namespace pn ON pn.oid = pc.relnamespace
                  WHERE i.inhrelid = c.oid AND c.relispartition),
                CASE WHEN c.relispartition THEN pg_get_expr(c.relpartbound, c.oid) END,
                (SELECT string_agg(format('%I.%I', pn.nspname, pc.relname), ', ' ORDER BY i.inhseqno)
                   FROM pg_inherits i
                   JOIN pg_class pc ON pc.oid = i.inhparent
                   JOIN pg_namespace pn ON pn.oid = pc.relnamespace
                  WHERE i.inhrelid = c.oid AND NOT c.relispartition),
                CASE WHEN c.reloftype <> 0 THEN c.reloftype::regtype::text END,
                c.relrowsecurity, c.relforcerowsecurity
         FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
         WHERE n.nspname = $1 AND c.relname = $2 AND c.relkind IN ('r', 'p')",
    )
    .bind(schema)
    .bind(table)
    .fetch_optional(&mut *c)
    .await?;
    let Some((oid, persistence, partition_key, parent, bound, parents, of_type, rls, force_rls)) = found else {
        return Ok(None);
    };
    let q = |name: &str| Engine::Postgres.quote_ident(name);
    // A temporary table is the session's, in its own `pg_temp_N`: by its
    // name alone, as it's made.
    let temporary = persistence == "t";
    let name = if temporary { q(table) } else { format!("{}.{}", q(schema), q(table)) };

    let columns: Vec<Column> = sqlx::query_as(
        "SELECT a.attname::text, format_type(a.atttypid, a.atttypmod), a.attnotnull,
                pg_get_expr(d.adbin, d.adrelid), a.attidentity::text, a.attgenerated::text,
                CASE WHEN a.attcollation <> t.typcollation AND a.attcollation <> 0 THEN
                    CASE WHEN cn.nspname = 'pg_catalog' THEN quote_ident(co.collname)
                         ELSE format('%I.%I', cn.nspname, co.collname) END
                END,
                a.attislocal
         FROM pg_attribute a
         JOIN pg_type t ON t.oid = a.atttypid
         LEFT JOIN pg_attrdef d ON d.adrelid = a.attrelid AND d.adnum = a.attnum
         LEFT JOIN pg_collation co ON co.oid = a.attcollation
         LEFT JOIN pg_namespace cn ON cn.oid = co.collnamespace
         WHERE a.attrelid = $1::text::oid AND a.attnum > 0 AND NOT a.attisdropped
         ORDER BY a.attnum",
    )
    .bind(&oid)
    .fetch_all(&mut *c)
    .await?;
    // Every constraint but NOT NULL (said with its column; PostgreSQL 17
    // keeps those here too), the primary key first, and whether it's the
    // table's own (not its parent's).
    let constraints: Vec<(String, String, bool)> = sqlx::query_as(
        "SELECT conname::text, pg_get_constraintdef(oid, true), conislocal FROM pg_constraint
         WHERE conrelid = $1::text::oid AND contype IN ('p', 'u', 'f', 'c', 'x')
         ORDER BY contype <> 'p', contype, conname",
    )
    .bind(&oid)
    .fetch_all(&mut *c)
    .await?;
    let after = what_goes_with(c, &oid, schema, table, &name, rls, force_rls).await?;

    // A partition or a typed table has no columns of its own to write (its
    // parent's or its type's); a child, only those it doesn't inherit. All
    // three, only the constraints that are their own.
    let from_parent_or_type = parent.is_some() || of_type.is_some();
    let derived = from_parent_or_type || parents.is_some();
    let mut lines: Vec<String> = columns
        .iter()
        .filter(|c| !from_parent_or_type && (!derived || c.7))
        .map(|c| column_line(c, &q))
        .collect();
    lines.extend(
        constraints.iter().filter(|c| !derived || c.2).map(|(n, def, _)| format!("CONSTRAINT {} {def}", q(n))),
    );
    let unlogged = match persistence.as_str() {
        "u" => "UNLOGGED ",
        "t" => "TEMPORARY ",
        _ => "",
    };
    let body = format!("(\n  {}\n)", lines.join(",\n  "));
    // `(…)` for those made from another only where they have something of
    // their own; a plain table always has it, if empty.
    let own = if lines.is_empty() { String::new() } else { format!(" {body}") };
    let mut out = match (&parent, &bound, &of_type) {
        (Some(parent), Some(bound), _) => format!("CREATE {unlogged}TABLE {name} PARTITION OF {parent}{own} {bound}"),
        (_, _, Some(ty)) => format!("CREATE {unlogged}TABLE {name} OF {ty}{own}"),
        _ => format!("CREATE {unlogged}TABLE {name} {body}"),
    };
    if let Some(parents) = parents {
        let _ = write!(out, " INHERITS ({parents})");
    }
    if let Some(key) = partition_key {
        let _ = write!(out, " PARTITION BY {key}");
    }
    out.push(';');
    let mut after = after;
    if derived {
        after.splice(0..0, set_on_columns_not_written(&columns, &name, from_parent_or_type));
    }
    if !after.is_empty() {
        let _ = write!(out, "\n\n{}", after.join("\n"));
    }
    Ok(Some(out))
}

/// What a table made from another set itself on the columns not written in
/// its CREATE (its parent's or its type's; with `all`, every one): a
/// default, NOT NULL, as `ALTER COLUMN`. The same as its parent's says it
/// again, harmlessly.
fn set_on_columns_not_written(columns: &[Column], name: &str, all: bool) -> Vec<String> {
    let q = |name: &str| Engine::Postgres.quote_ident(name);
    columns
        .iter()
        .filter(|c| (all || !c.7) && c.4.is_empty() && c.5.is_empty())
        .flat_map(|c| {
            let column = q(&c.0);
            let default = c.3.as_ref().map(|d| format!("ALTER TABLE {name} ALTER COLUMN {column} SET DEFAULT {d};"));
            let not_null = c.2.then(|| format!("ALTER TABLE {name} ALTER COLUMN {column} SET NOT NULL;"));
            default.into_iter().chain(not_null)
        })
        .collect()
}

/// One column's line: name, type, collation, generation or identity or
/// default, NOT NULL.
fn column_line(c: &Column, q: &dyn Fn(&str) -> String) -> String {
    let (name, ty, not_null, default, identity, generated, collation, _) = c;
    let mut line = format!("{} {ty}", q(name));
    // As the server writes its name: quoted, with its schema where needed.
    if let Some(collation) = collation {
        let _ = write!(line, " COLLATE {collation}");
    }
    match (generated.as_str(), identity.as_str(), default) {
        // A generated column's expression is kept as its default.
        ("s", _, Some(expr)) => {
            let _ = write!(line, " GENERATED ALWAYS AS ({expr}) STORED");
        }
        // PostgreSQL 18's, and its default kind: computed when read.
        ("v", _, Some(expr)) => {
            let _ = write!(line, " GENERATED ALWAYS AS ({expr}) VIRTUAL");
        }
        (_, "a", _) => line.push_str(" GENERATED ALWAYS AS IDENTITY"),
        (_, "d", _) => line.push_str(" GENERATED BY DEFAULT AS IDENTITY"),
        (_, _, Some(expr)) => {
            let _ = write!(line, " DEFAULT {expr}");
        }
        _ => {}
    }
    if *not_null {
        line.push_str(" NOT NULL");
    }
    line
}

/// What goes with the CREATE TABLE: the indexes no constraint made (those
/// are said in it), the triggers, row-level security and its policies, the
/// comments; each a statement.
async fn what_goes_with(
    c: &mut PgConnection,
    oid: &str,
    schema: &str,
    table: &str,
    name: &str,
    rls: bool,
    force_rls: bool,
) -> Result<Vec<String>> {
    let q = |name: &str| Engine::Postgres.quote_ident(name);
    let indexes: Vec<(String,)> = sqlx::query_as(
        "SELECT pg_get_indexdef(i.indexrelid) FROM pg_index i
         WHERE i.indrelid = $1::text::oid
           AND NOT EXISTS (SELECT 1 FROM pg_constraint c WHERE c.conindid = i.indexrelid AND c.conrelid = i.indrelid)
           -- Not a partition's copy of its parent's index (attached to it).
           AND NOT EXISTS (SELECT 1 FROM pg_inherits h WHERE h.inhrelid = i.indexrelid)
         ORDER BY 1",
    )
    .bind(oid)
    .fetch_all(&mut *c)
    .await?;
    let triggers: Vec<(String,)> = sqlx::query_as(
        "SELECT pg_get_triggerdef(oid, true) FROM pg_trigger
         WHERE tgrelid = $1::text::oid AND NOT tgisinternal
           -- Not a partition's copy of its parent's (13 on: tgparentid).
           AND tgparentid = 0
         ORDER BY tgname",
    )
    .bind(oid)
    .fetch_all(&mut *c)
    .await?;
    // Roles quoted as names (`"App Users"`), `public` as the word it is.
    let policies: Vec<(String,)> = sqlx::query_as(
        "SELECT format('CREATE POLICY %I ON %I.%I AS %s FOR %s TO %s%s%s',
                       policyname, schemaname, tablename, permissive, cmd,
                       (SELECT string_agg(CASE WHEN r::text = 'public' THEN 'public' ELSE quote_ident(r::text) END, ', ')
                          FROM unnest(roles) AS r),
                       coalesce(' USING (' || qual || ')', ''), coalesce(' WITH CHECK (' || with_check || ')', ''))
         FROM pg_policies WHERE schemaname = $1 AND tablename = $2 ORDER BY policyname",
    )
    .bind(schema)
    .bind(table)
    .fetch_all(&mut *c)
    .await?;
    let comments: Vec<(Option<String>, String)> = sqlx::query_as(
        "SELECT a.attname::text, d.description FROM pg_description d
         LEFT JOIN pg_attribute a ON a.attrelid = d.objoid AND a.attnum = d.objsubid AND d.objsubid > 0
         WHERE d.objoid = $1::text::oid AND d.classoid = 'pg_class'::regclass
         ORDER BY d.objsubid",
    )
    .bind(oid)
    .fetch_all(&mut *c)
    .await?;

    let mut after: Vec<String> = Vec::new();
    after.extend(indexes.into_iter().map(|(d,)| format!("{d};")));
    after.extend(triggers.into_iter().map(|(d,)| format!("{d};")));
    if rls {
        after.push(format!("ALTER TABLE {name} ENABLE ROW LEVEL SECURITY;"));
    }
    if force_rls {
        after.push(format!("ALTER TABLE {name} FORCE ROW LEVEL SECURITY;"));
    }
    after.extend(policies.into_iter().map(|(d,)| format!("{d};")));
    let literal = |s: &str| crate::db::sql_literal(s, Engine::Postgres);
    after.extend(comments.into_iter().map(|(column, text)| match column {
        Some(c) => format!("COMMENT ON COLUMN {name}.{} IS {};", q(&c), literal(&text)),
        None => format!("COMMENT ON TABLE {name} IS {};", literal(&text)),
    }));
    Ok(after)
}
