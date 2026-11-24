//! A PostgreSQL table as a CREATE TABLE, written from the server's own
//! functions, which write each part exactly as it keeps it: the columns
//! with their types (`format_type`), defaults (`pg_get_expr`), identity
//! and generated columns; every constraint whole (`pg_get_constraintdef`:
//! a foreign key with its ON DELETE, ON UPDATE and DEFERRABLE); then the
//! indexes no constraint made (`pg_get_indexdef`), the triggers
//! (`pg_get_triggerdef`), row-level security and its policies, and the
//! comments (the table's, its columns', constraints', indexes' and
//! triggers'). A partition is written `PARTITION OF` its parent, a child
//! `INHERITS` its parents and a typed table `OF` its type, each with only
//! what's its own; a foreign table with its server and options. Identity
//! columns keep their sequence's options, a table its access method,
//! storage parameters, tablespace and replica identity. PostgreSQL has no
//! `SHOW CREATE TABLE`; `pg_dump` writes the same from the same functions.

use std::fmt::Write as _;

use anyhow::Result;
use sqlx::{PgConnection, PgPool};

use crate::db::Engine;

fn q(name: &str) -> String {
    Engine::Postgres.quote_ident(name)
}

/// The table itself.
struct Table {
    oid: String,
    /// `p` permanent, `u` unlogged, `t` temporary.
    persistence: String,
    /// A partitioned table's `PARTITION BY` key.
    partition_key: Option<String>,
    /// The table it's a partition of, and for which values.
    parent: Option<String>,
    bound: Option<String>,
    /// The tables it inherits from (not as a partition).
    parents: Option<String>,
    /// The type it's made of (`OF type`).
    of_type: Option<String>,
    /// Row-level security: on, forced.
    rls: (bool, bool),
    /// `r`, `p` partitioned, `f` foreign.
    kind: String,
}

type TableRow =
    (String, String, Option<String>, Option<String>, Option<String>, Option<String>, Option<String>, bool, bool, String);

/// A column.
struct Column {
    name: String,
    ty: String,
    not_null: bool,
    /// Its default, or a generated column's expression.
    default: Option<String>,
    /// `a` always, `d` by default; empty if it's no identity.
    identity: String,
    /// `s` stored, `v` virtual; empty if it isn't generated.
    generated: String,
    /// Its collation where it isn't its type's (qualified where it isn't
    /// `pg_catalog`'s).
    collation: Option<String>,
    /// The table's own, not inherited.
    local: bool,
    /// A foreign table's options for it (`name 'value', …`).
    fdw_options: Option<String>,
}

type ColumnRow = (String, String, bool, Option<String>, String, String, Option<String>, bool, Option<String>);

/// A NOT NULL constraint as PostgreSQL 18 keeps it (older ones keep none).
struct NotNull {
    column: String,
    name: String,
    valid: bool,
    no_inherit: bool,
    /// The table's own, and (`inherited`) a parent's too.
    local: bool,
    inherited: bool,
}

/// An identity column's sequence.
struct Identity {
    column: String,
    /// Its name as the server writes it (`schema.name`).
    name: String,
    ty: String,
    start: i64,
    increment: i64,
    min: i64,
    max: i64,
    cache: i64,
    cycle: bool,
}

/// Where and how it's kept.
struct Storage {
    /// Its access method, unless `heap`.
    method: Option<String>,
    /// Storage parameters, its TOAST table's as `toast.…`.
    options: Option<String>,
    tablespace: Option<String>,
    /// Replica identity: `d` default, `n` nothing, `f` full, `i` an index
    /// (`replica_index`).
    replica: String,
    replica_index: Option<String>,
    /// A foreign table's server and options.
    server: Option<String>,
    server_options: Option<String>,
}

/// The table `schema.table` as a CREATE TABLE and what goes with it; `None`
/// if it isn't a table (a view, a sequence) or isn't there.
///
/// Written with the search path empty (in a transaction of its own, as
/// `pg_dump` does), so every name the server writes is qualified: a type,
/// a referenced table, a function in a trigger mean the same run anywhere.
pub(super) async fn table_ddl(p: &PgPool, schema: &str, table: &str) -> Result<Option<String>> {
    let mut tx = p.begin().await?;
    // Given up on by the server, not only by whoever waits: a table behind
    // a lock (an ALTER waiting) doesn't keep a connection of the pool.
    for set in ["SET LOCAL search_path = ''", "SET LOCAL lock_timeout = '5s'", "SET LOCAL statement_timeout = '20s'"] {
        sqlx::query(set).execute(&mut *tx).await?;
    }
    let ddl = written(&mut tx, schema, table).await;
    // Nothing to keep: what's written is read only.
    tx.rollback().await?;
    ddl
}

/// `table_ddl`'s work, on `c` with the search path empty.
async fn written(c: &mut PgConnection, schema: &str, table: &str) -> Result<Option<String>> {
    let Some(t) = the_table(c, schema, table).await? else { return Ok(None) };
    // A temporary table is the session's, in its own `pg_temp_N`: made by
    // its name alone, and named `pg_temp.` after (any session's own).
    let temporary = t.persistence == "t";
    let name = if temporary { format!("pg_temp.{}", q(table)) } else { format!("{}.{}", q(schema), q(table)) };
    let foreign = t.kind == "f";
    // What its statements call it: a foreign table is altered and
    // commented on as one.
    let what = if foreign { "FOREIGN TABLE" } else { "TABLE" };

    let (columns, constraints) = columns_and_constraints(c, &t.oid).await?;
    let (not_nulls, identities, storage) = kept_beside(c, &t.oid).await?;
    let mut after = what_goes_with(c, &t.oid, schema, table, &name, what, t.rls).await?;
    // Last: after the index it names.
    after.extend(replica_identity(&name, &storage, foreign));

    // A partition or a typed table has no columns of its own to write (its
    // parent's or its type's); a child, only those it doesn't inherit. All
    // three, only the constraints that are their own.
    let from_parent_or_type = t.parent.is_some() || t.of_type.is_some();
    let derived = from_parent_or_type || t.parents.is_some();
    let mut lines: Vec<String> = columns
        .iter()
        .filter(|c| !from_parent_or_type && (!derived || c.local))
        .map(|c| {
            let not_null = not_nulls.iter().find(|n| n.column == c.name);
            let identity = identities.iter().find(|i| i.column == c.name);
            column_line(c, schema, table, not_null, identity)
        })
        .collect();
    lines.extend(constraints.iter().filter(|c| !derived || c.2).map(|(n, def, _)| format!("CONSTRAINT {} {def}", q(n))));
    let unlogged = match t.persistence.as_str() {
        "u" => "UNLOGGED ",
        "t" => "TEMPORARY ",
        _ => "",
    };
    let body = format!("(\n  {}\n)", lines.join(",\n  "));
    // `(…)` for those made from another only where they have something of
    // their own; a plain table always has it, if empty.
    let own = if lines.is_empty() { String::new() } else { format!(" {body}") };
    // Made by its name alone if temporary (`pg_temp.` after).
    let created = if temporary { q(table) } else { name.clone() };
    let mut out = match (&t.parent, &t.bound, &t.of_type) {
        (Some(parent), Some(bound), _) => format!("CREATE {unlogged}{what} {created} PARTITION OF {parent}{own} {bound}"),
        (_, _, Some(ty)) => format!("CREATE {unlogged}{what} {created} OF {ty}{own}"),
        _ => format!("CREATE {unlogged}{what} {created} {body}"),
    };
    if let Some(parents) = &t.parents {
        let _ = write!(out, " INHERITS ({parents})");
    }
    if let Some(key) = &t.partition_key {
        let _ = write!(out, " PARTITION BY {key}");
    }
    out.push_str(&storage_clauses(&storage));
    out.push(';');
    // Right after it: what it sets on columns it doesn't write, an identity
    // sequence's type where it isn't its column's, then its own NOT NULLs
    // not yet checked (NOT VALID), in the catalog's order.
    let mut first = if derived {
        set_on_columns_not_written(&columns, &not_nulls, (table, &name), what, from_parent_or_type)
    } else {
        Vec::new()
    };
    // (An identity takes its column's type; `ALTER SEQUENCE … AS` made it
    // another.)
    first.extend(identities.iter().filter_map(|i| {
        let column_type = &columns.iter().find(|c| c.name == i.column)?.ty;
        (i.ty != *column_type).then(|| format!("ALTER SEQUENCE {} AS {};", i.name, i.ty))
    }));
    first.extend(not_nulls.iter().filter(|n| !n.valid && n.local).map(|n| {
        let no_inherit = if n.no_inherit { " NO INHERIT" } else { "" };
        format!("ALTER {what} {name} ADD CONSTRAINT {} NOT NULL {}{no_inherit} NOT VALID;", q(&n.name), q(&n.column))
    }));
    after.splice(0..0, first);
    if !after.is_empty() {
        let _ = write!(out, "\n\n{}", after.join("\n"));
    }
    // A temporary table's: what the server writes in the session's own
    // schema, by its number, is any session's own (`pg_temp.`), its
    // sequences' and the temporary tables it refers to too; not inside a
    // string or a quoted name.
    if temporary {
        out = replace_in_sql(&out, &format!("{}.", q(schema)), "pg_temp.");
    }
    Ok(Some(out))
}

/// `from` replaced by `to` in `sql`, but not inside a string (`'…'`) or a
/// quoted name (`"…"`): those are as they are.
fn replace_in_sql(sql: &str, from: &str, to: &str) -> String {
    let mut out = String::with_capacity(sql.len());
    let mut quote: Option<char> = None;
    let mut rest = sql;
    while let Some(ch) = rest.chars().next() {
        if quote.is_none() && rest.starts_with(from) {
            out.push_str(to);
            rest = &rest[from.len()..];
            continue;
        }
        match quote {
            // A doubled quote inside reads as closing and opening again.
            Some(q) if ch == q => quote = None,
            None if ch == '\'' || ch == '"' => quote = Some(ch),
            _ => {}
        }
        out.push(ch);
        rest = &rest[ch.len_utf8()..];
    }
    out
}

/// The table's own row: `None` if it isn't a table (or isn't there).
async fn the_table(c: &mut PgConnection, schema: &str, table: &str) -> Result<Option<Table>> {
    let found: Option<TableRow> = sqlx::query_as(
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
                c.relrowsecurity, c.relforcerowsecurity, c.relkind::text
         FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
         WHERE n.nspname = $1 AND c.relname = $2 AND c.relkind IN ('r', 'p', 'f')",
    )
    .bind(schema)
    .bind(table)
    .fetch_optional(&mut *c)
    .await?;
    Ok(found.map(|(oid, persistence, partition_key, parent, bound, parents, of_type, rls, force_rls, kind)| Table {
        oid,
        persistence,
        partition_key,
        parent,
        bound,
        parents,
        of_type,
        rls: (rls, force_rls),
        kind,
    }))
}

/// A table's columns, and every constraint but NOT NULL (said with its
/// column; PostgreSQL 17 keeps those here too): the primary key first,
/// with whether it's the table's own (not its parent's).
async fn columns_and_constraints(c: &mut PgConnection, oid: &str) -> Result<(Vec<Column>, Vec<(String, String, bool)>)> {
    let columns: Vec<ColumnRow> = sqlx::query_as(
        "SELECT a.attname::text, format_type(a.atttypid, a.atttypmod), a.attnotnull,
                pg_get_expr(d.adbin, d.adrelid), a.attidentity::text, a.attgenerated::text,
                CASE WHEN a.attcollation <> t.typcollation AND a.attcollation <> 0 THEN
                    CASE WHEN cn.nspname = 'pg_catalog' THEN quote_ident(co.collname)
                         ELSE format('%I.%I', cn.nspname, co.collname) END
                END,
                a.attislocal,
                (SELECT string_agg(format('%I %L', option_name, option_value), ', ')
                   FROM pg_options_to_table(a.attfdwoptions))
         FROM pg_attribute a
         JOIN pg_type t ON t.oid = a.atttypid
         LEFT JOIN pg_attrdef d ON d.adrelid = a.attrelid AND d.adnum = a.attnum
         LEFT JOIN pg_collation co ON co.oid = a.attcollation
         LEFT JOIN pg_namespace cn ON cn.oid = co.collnamespace
         WHERE a.attrelid = $1::text::oid AND a.attnum > 0 AND NOT a.attisdropped
         ORDER BY a.attnum",
    )
    .bind(oid)
    .fetch_all(&mut *c)
    .await?;
    let constraints: Vec<(String, String, bool)> = sqlx::query_as(
        "SELECT conname::text, pg_get_constraintdef(oid, true), conislocal FROM pg_constraint
         WHERE conrelid = $1::text::oid AND contype IN ('p', 'u', 'f', 'c', 'x')
         ORDER BY contype <> 'p', contype, conname",
    )
    .bind(oid)
    .fetch_all(&mut *c)
    .await?;
    let columns = columns
        .into_iter()
        .map(|(name, ty, not_null, default, identity, generated, collation, local, fdw_options)| Column {
            name,
            ty,
            not_null,
            default,
            identity,
            generated,
            collation,
            local,
            fdw_options,
        })
        .collect();
    Ok((columns, constraints))
}

/// What's kept beside a table's columns and constraints: PostgreSQL 18's
/// NOT NULL constraints, identity columns' sequences, and its storage.
async fn kept_beside(c: &mut PgConnection, oid: &str) -> Result<(Vec<NotNull>, Vec<Identity>, Storage)> {
    // PostgreSQL 18's NOT NULL constraints (older ones keep none here).
    let not_nulls: Vec<(String, String, bool, bool, bool, bool)> = sqlx::query_as(
        "SELECT a.attname::text, c.conname::text, c.convalidated, c.connoinherit, c.conislocal, c.coninhcount > 0
         FROM pg_constraint c
         JOIN pg_attribute a ON a.attrelid = c.conrelid AND a.attnum = c.conkey[1]
         WHERE c.conrelid = $1::text::oid AND c.contype = 'n'
         ORDER BY a.attnum",
    )
    .bind(oid)
    .fetch_all(&mut *c)
    .await?;
    type IdentityRow = (String, String, String, i64, i64, i64, i64, i64, bool);
    let identities: Vec<IdentityRow> = sqlx::query_as(
        "SELECT a.attname::text, format('%I.%I', sn.nspname, sc.relname), s.seqtypid::regtype::text,
                s.seqstart, s.seqincrement, s.seqmin, s.seqmax, s.seqcache, s.seqcycle
         FROM pg_attribute a
         JOIN pg_depend d ON d.refclassid = 'pg_class'::regclass AND d.refobjid = a.attrelid
          AND d.refobjsubid = a.attnum AND d.classid = 'pg_class'::regclass AND d.deptype = 'i'
         JOIN pg_sequence s ON s.seqrelid = d.objid
         JOIN pg_class sc ON sc.oid = s.seqrelid
         JOIN pg_namespace sn ON sn.oid = sc.relnamespace
         WHERE a.attrelid = $1::text::oid AND a.attidentity <> ''",
    )
    .bind(oid)
    .fetch_all(&mut *c)
    .await?;
    type StorageRow =
        (Option<String>, Option<String>, Option<String>, String, Option<String>, Option<String>, Option<String>);
    let (method, options, tablespace, replica, replica_index, server, server_options): StorageRow = sqlx::query_as(
        "SELECT (SELECT amname::text FROM pg_am WHERE oid = c.relam AND amname <> 'heap'),
                (SELECT string_agg(o, ', ') FROM (
                    SELECT unnest(c.reloptions) AS o
                    UNION ALL
                    SELECT 'toast.' || unnest(tc.reloptions) FROM pg_class tc WHERE tc.oid = c.reltoastrelid) AS os),
                (SELECT quote_ident(spcname) FROM pg_tablespace WHERE oid = c.reltablespace),
                c.relreplident::text,
                (SELECT quote_ident(ic.relname) FROM pg_index i JOIN pg_class ic ON ic.oid = i.indexrelid
                  WHERE i.indrelid = c.oid AND i.indisreplident),
                (SELECT quote_ident(fs.srvname) FROM pg_foreign_table ft
                   JOIN pg_foreign_server fs ON fs.oid = ft.ftserver WHERE ft.ftrelid = c.oid),
                (SELECT string_agg(format('%I %L', option_name, option_value), ', ')
                   FROM pg_foreign_table ft, pg_options_to_table(ft.ftoptions) WHERE ft.ftrelid = c.oid)
         FROM pg_class c WHERE c.oid = $1::text::oid",
    )
    .bind(oid)
    .fetch_one(&mut *c)
    .await?;
    let not_nulls = not_nulls
        .into_iter()
        .map(|(column, name, valid, no_inherit, local, inherited)| NotNull {
            column,
            name,
            valid,
            no_inherit,
            local,
            inherited,
        })
        .collect();
    let identities = identities
        .into_iter()
        .map(|(column, name, ty, start, increment, min, max, cache, cycle)| Identity {
            column,
            name,
            ty,
            start,
            increment,
            min,
            max,
            cache,
            cycle,
        })
        .collect();
    let storage = Storage { method, options, tablespace, replica, replica_index, server, server_options };
    Ok((not_nulls, identities, storage))
}

/// Its REPLICA IDENTITY, unless the default (a foreign table has none,
/// though the catalog says NOTHING).
fn replica_identity(name: &str, storage: &Storage, foreign: bool) -> Option<String> {
    let how = match (storage.replica.as_str(), &storage.replica_index) {
        _ if foreign => return None,
        ("i", Some(index)) => format!("USING INDEX {index}"),
        ("n", _) => "NOTHING".to_string(),
        ("f", _) => "FULL".to_string(),
        _ => return None,
    };
    Some(format!("ALTER TABLE {name} REPLICA IDENTITY {how};"))
}

/// The end of its CREATE: access method, storage parameters, tablespace;
/// a foreign table's server and options.
fn storage_clauses(storage: &Storage) -> String {
    let mut out = String::new();
    if let Some(method) = &storage.method {
        let _ = write!(out, " USING {}", q(method));
    }
    if let Some(options) = &storage.options {
        let _ = write!(out, " WITH ({options})");
    }
    if let Some(tablespace) = &storage.tablespace {
        let _ = write!(out, " TABLESPACE {tablespace}");
    }
    if let Some(server) = &storage.server {
        let _ = write!(out, " SERVER {server}");
        if let Some(options) = &storage.server_options {
            let _ = write!(out, " OPTIONS ({options})");
        }
    }
    out
}

/// What a table made from another set itself on the columns not written in
/// its CREATE (its parent's or its type's; with `all`, every one): a
/// default, NOT NULL, as `ALTER COLUMN`. One the parent gives too says it
/// again, harmlessly. Its own NOT NULL constraint (18: a partition's or a
/// typed table's) named, or NO INHERIT, is added as it is; one not yet
/// checked (NOT VALID) isn't set (that would check it): its own is added
/// as it was, after, a parent's comes with the parent.
fn set_on_columns_not_written(
    columns: &[Column],
    not_nulls: &[NotNull],
    (table, name): (&str, &str),
    what: &str,
    all: bool,
) -> Vec<String> {
    columns
        .iter()
        .filter(|c| (all || !c.local) && c.identity.is_empty() && c.generated.is_empty())
        .flat_map(|c| {
            let column = q(&c.name);
            let default = c.default.as_ref().map(|d| format!("ALTER {what} {name} ALTER COLUMN {column} SET DEFAULT {d};"));
            let not_null = match not_nulls.iter().find(|n| n.column == c.name) {
                Some(n) if !n.valid => None,
                // Named, or NO INHERIT: as it was (else as SET NOT NULL
                // makes it, a key's too).
                Some(n) if n.local && !n.inherited && (n.no_inherit || n.name != default_name(table, &c.name, "not_null")) => {
                    let no_inherit = if n.no_inherit { " NO INHERIT" } else { "" };
                    Some(format!("ALTER {what} {name} ADD CONSTRAINT {} NOT NULL {column}{no_inherit};", q(&n.name)))
                }
                _ => c.not_null.then(|| format!("ALTER {what} {name} ALTER COLUMN {column} SET NOT NULL;")),
            };
            default.into_iter().chain(not_null)
        })
        .collect()
}

/// The name PostgreSQL gives an object it names itself (a column's
/// sequence, its NOT NULL): `table_column_label`, the longer of the two
/// names shortened, a byte at a time, till it fits in 63 bytes, on a
/// character's boundary (as its `makeObjectName`). One it had to number
/// (`…_seq1`, a name taken) isn't taken for it: run again, it wouldn't be.
fn default_name(table: &str, column: &str, label: &str) -> String {
    const MAX: usize = 63;
    let available = MAX - label.len() - 2;
    let (mut t, mut c) = (table.len(), column.len());
    while t + c > available {
        if t > c {
            t -= 1;
        } else {
            c -= 1;
        }
    }
    let clip = |s: &str, mut n: usize| {
        while !s.is_char_boundary(n) {
            n -= 1;
        }
        s[..n].to_string()
    };
    format!("{}_{}_{label}", clip(table, t), clip(column, c))
}

/// One column's line: name, type, a foreign table's options for it,
/// collation, generation or identity (its sequence's options where they
/// aren't the defaults) or default, NOT NULL (named, NO INHERIT, where its
/// constraint says; a NOT VALID one is added after).
fn column_line(
    c: &Column,
    schema: &str,
    table: &str,
    not_null: Option<&NotNull>,
    sequence: Option<&Identity>,
) -> String {
    let mut line = format!("{} {}", q(&c.name), c.ty);
    if let Some(options) = &c.fdw_options {
        let _ = write!(line, " OPTIONS ({options})");
    }
    // As the server writes its name: quoted, with its schema where needed.
    if let Some(collation) = &c.collation {
        let _ = write!(line, " COLLATE {collation}");
    }
    match (c.generated.as_str(), c.identity.as_str(), &c.default) {
        // A generated column's expression is kept as its default.
        ("s", _, Some(expr)) => {
            let _ = write!(line, " GENERATED ALWAYS AS ({expr}) STORED");
        }
        // PostgreSQL 18's, and its default kind: computed when read.
        ("v", _, Some(expr)) => {
            let _ = write!(line, " GENERATED ALWAYS AS ({expr}) VIRTUAL");
        }
        (_, how @ ("a" | "d"), _) => {
            let how = if how == "a" { "ALWAYS" } else { "BY DEFAULT" };
            let options = sequence.map(|s| identity_options(s, schema, table, &c.ty)).unwrap_or_default();
            let _ = write!(line, " GENERATED {how} AS IDENTITY{options}");
        }
        (_, _, Some(expr)) => {
            let _ = write!(line, " DEFAULT {expr}");
        }
        _ => {}
    }
    match not_null {
        Some(n) if n.valid => {
            // Named where it isn't the name the server would give it.
            if n.name != default_name(table, &c.name, "not_null") {
                let _ = write!(line, " CONSTRAINT {}", q(&n.name));
            }
            line.push_str(" NOT NULL");
            if n.no_inherit {
                line.push_str(" NO INHERIT");
            }
        }
        // Not yet checked: added after, as it was.
        Some(_) => {}
        None => {
            if c.not_null {
                line.push_str(" NOT NULL");
            }
        }
    }
    line
}

/// An identity's sequence options as `( … )`, those not the defaults
/// only (none: nothing), for a column of `schema.table` whose type is
/// `column_type`: the defaults are its type's, as the sequence is made
/// (one of another type is altered to it after).
fn identity_options(seq: &Identity, schema: &str, table: &str, column_type: &str) -> String {
    let (type_min, type_max) = match column_type {
        "smallint" => (i64::from(i16::MIN), i64::from(i16::MAX)),
        "integer" => (i64::from(i32::MIN), i64::from(i32::MAX)),
        _ => (i64::MIN, i64::MAX),
    };
    let (default_min, default_max) = if seq.increment > 0 { (1, type_max) } else { (type_min, -1) };
    let default_start = if seq.increment > 0 { seq.min } else { seq.max };
    let mut parts = Vec::new();
    // The name the server would give it, in the table's schema; or another
    // name, or schema, it was given.
    let given = format!("{}.{}", q(schema), q(&default_name(table, &seq.column, "seq")));
    if seq.name != given {
        parts.push(format!("SEQUENCE NAME {}", seq.name));
    }
    if seq.start != default_start {
        parts.push(format!("START WITH {}", seq.start));
    }
    if seq.increment != 1 {
        parts.push(format!("INCREMENT BY {}", seq.increment));
    }
    if seq.min != default_min {
        parts.push(format!("MINVALUE {}", seq.min));
    }
    if seq.max != default_max {
        parts.push(format!("MAXVALUE {}", seq.max));
    }
    if seq.cache != 1 {
        parts.push(format!("CACHE {}", seq.cache));
    }
    if seq.cycle {
        parts.push("CYCLE".to_string());
    }
    if parts.is_empty() { String::new() } else { format!(" ({})", parts.join(" ")) }
}

/// What goes with the CREATE TABLE: the indexes no constraint made (those
/// are said in it), the triggers, row-level security and its policies, the
/// comments; each a statement, the table called `what` (`TABLE`, `FOREIGN
/// TABLE`).
async fn what_goes_with(
    c: &mut PgConnection,
    oid: &str,
    schema: &str,
    table: &str,
    name: &str,
    what: &str,
    (rls, force_rls): (bool, bool),
) -> Result<Vec<String>> {
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

    // Comments on its constraints (`c`), indexes (`i`, by qualified name)
    // and triggers (`t`).
    let other_comments: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT 'c', quote_ident(co.conname), d.description FROM pg_constraint co
           JOIN pg_description d ON d.objoid = co.oid AND d.classoid = 'pg_constraint'::regclass
          WHERE co.conrelid = $1::text::oid
         UNION ALL
         SELECT 'i', format('%I.%I', n.nspname, ic.relname), d.description FROM pg_index i
           JOIN pg_class ic ON ic.oid = i.indexrelid
           JOIN pg_namespace n ON n.oid = ic.relnamespace
           JOIN pg_description d ON d.objoid = i.indexrelid AND d.classoid = 'pg_class'::regclass AND d.objsubid = 0
          WHERE i.indrelid = $1::text::oid
         UNION ALL
         SELECT 't', quote_ident(tg.tgname), d.description FROM pg_trigger tg
           JOIN pg_description d ON d.objoid = tg.oid AND d.classoid = 'pg_trigger'::regclass
          WHERE tg.tgrelid = $1::text::oid AND NOT tg.tgisinternal
         ORDER BY 1, 2",
    )
    .bind(oid)
    .fetch_all(&mut *c)
    .await?;

    let mut after: Vec<String> = Vec::new();
    after.extend(indexes.into_iter().map(|(d,)| format!("{d};")));
    after.extend(triggers.into_iter().map(|(d,)| format!("{d};")));
    if rls {
        after.push(format!("ALTER {what} {name} ENABLE ROW LEVEL SECURITY;"));
    }
    if force_rls {
        after.push(format!("ALTER {what} {name} FORCE ROW LEVEL SECURITY;"));
    }
    after.extend(policies.into_iter().map(|(d,)| format!("{d};")));
    let literal = |s: &str| crate::db::sql_literal(s, Engine::Postgres);
    after.extend(comments.into_iter().map(|(column, text)| match column {
        Some(c) => format!("COMMENT ON COLUMN {name}.{} IS {};", q(&c), literal(&text)),
        None => format!("COMMENT ON {what} {name} IS {};", literal(&text)),
    }));
    after.extend(other_comments.into_iter().map(|(kind, object, text)| match kind.as_str() {
        "c" => format!("COMMENT ON CONSTRAINT {object} ON {name} IS {};", literal(&text)),
        "i" => format!("COMMENT ON INDEX {object} IS {};", literal(&text)),
        _ => format!("COMMENT ON TRIGGER {object} ON {name} IS {};", literal(&text)),
    }));
    Ok(after)
}

#[cfg(test)]
mod tests {
    #[test]
    fn names_are_shortened_as_postgresql_does() {
        assert_eq!(super::default_name("orders", "id", "seq"), "orders_id_seq");
        let long = "a".repeat(60);
        // (As the server names a 60-letter table's: 56 of its letters.)
        assert_eq!(super::default_name(&long, "id", "seq"), format!("{}_id_seq", "a".repeat(56)));
        assert_eq!(super::default_name(&long, "id", "not_null"), format!("{}_id_not_null", "a".repeat(51)));
        // Never inside a character: `ž` is two bytes.
        let lt = "ž".repeat(30);
        let name = super::default_name(&lt, "id", "seq");
        assert!(name.len() <= 63 && name.ends_with("_id_seq"), "{name}");
    }

    #[test]
    fn a_schema_is_replaced_only_outside_strings_and_quoted_names() {
        let sql = "CREATE INDEX i ON pg_temp_3.t (a); COMMENT ON TABLE pg_temp.t IS 'from pg_temp_3.t, it''s pg_temp_3.'; \"pg_temp_3.x\"";
        assert_eq!(
            super::replace_in_sql(sql, "pg_temp_3.", "pg_temp."),
            "CREATE INDEX i ON pg_temp.t (a); COMMENT ON TABLE pg_temp.t IS 'from pg_temp_3.t, it''s pg_temp_3.'; \"pg_temp_3.x\""
        );
    }
}
