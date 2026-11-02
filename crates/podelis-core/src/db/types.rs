//! Column types across engines: which are the same (PostgreSQL's `int4`,
//! MySQL's `int`, SQLite's `INTEGER`), and what a type is called on another
//! engine, for comparing schemas and the SQL that aligns them. The
//! vocabulary (`TypeKind`) and the common names are here; each engine's
//! own (its exceptions, how it writes each kind) are its dialect's
//! (`Dialect::type_kind`, `Dialect::type_name`).

use super::Engine;

/// A type, by what it holds rather than what an engine calls it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TypeKind {
    /// An integer of this many bytes.
    Int(u8),
    Bool,
    /// `numeric(precision, scale)`.
    Decimal(Option<(u32, u32)>),
    Float4,
    Float8,
    Varchar(Option<u32>),
    Char(Option<u32>),
    Text,
    Date,
    Time,
    Timestamp,
    /// A moment in time: PostgreSQL's `timestamptz`, MySQL's `timestamp`
    /// (stored in UTC, shown in the session's zone).
    TimestampTz,
    Binary,
    Json,
    Uuid,
    /// Anything else, as written (lowercase).
    Other(String),
}

/// The numbers in `(…)` after a type's name: `decimal(10,2)` → [10, 2].
fn arguments(ty: &str) -> Vec<u32> {
    ty.split_once('(')
        .and_then(|(_, rest)| rest.split_once(')'))
        .map(|(args, _)| args.split(',').filter_map(|a| a.trim().parse().ok()).collect())
        .unwrap_or_default()
}

fn kind(engine: Engine, ty: &str) -> TypeKind {
    use TypeKind as Kind;
    let ty = ty.trim().to_ascii_lowercase();
    let base = ty.split('(').next().unwrap_or_default().trim();
    let args = arguments(&ty);
    let length = args.first().copied();
    if ty.contains("unsigned") || ty.ends_with("[]") {
        return Kind::Other(ty);
    }
    // The engine's own meaning of a name first (MySQL's `timestamp`).
    if let Some(own) = engine.dialect().type_kind(base) {
        return own;
    }
    match base {
        "tinyint" if length == Some(1) => Kind::Bool,
        "tinyint" => Kind::Int(1),
        "int2" | "smallint" => Kind::Int(2),
        "mediumint" => Kind::Int(3),
        "int4" | "int" | "integer" => Kind::Int(4),
        "int8" | "bigint" => Kind::Int(8),
        "bool" | "boolean" => Kind::Bool,
        "numeric" | "decimal" => Kind::Decimal(args.get(1).map(|&scale| (args[0], scale))),
        "float4" => Kind::Float4,
        "float8" | "double precision" | "double" | "real" | "float" => Kind::Float8,
        "varchar" | "character varying" | "nvarchar" | "varchar2" => Kind::Varchar(length),
        "char" | "character" | "bpchar" | "nchar" => Kind::Char(length),
        "text" | "tinytext" | "mediumtext" | "longtext" | "clob" => Kind::Text,
        "date" => Kind::Date,
        "time" | "time without time zone" => Kind::Time,
        "timestamp" | "timestamp without time zone" | "datetime" => Kind::Timestamp,
        "timestamptz" | "timestamp with time zone" => Kind::TimestampTz,
        "bytea" | "blob" | "tinyblob" | "mediumblob" | "longblob" | "binary" | "varbinary" => Kind::Binary,
        "json" | "jsonb" => Kind::Json,
        "uuid" => Kind::Uuid,
        _ => Kind::Other(ty),
    }
}

/// Whether type `a` on one engine and `b` on another hold the same values.
/// Beside an engine that keeps only a type's affinity (SQLite), any integer
/// matches any other, and lengths don't count.
pub fn same_type(a_engine: Engine, a: &str, b_engine: Engine, b: &str) -> bool {
    use TypeKind as Kind;
    let (a, b) = (kind(a_engine, a), kind(b_engine, b));
    if !a_engine.dialect().keeps_only_affinity() && !b_engine.dialect().keeps_only_affinity() {
        return a == b;
    }
    let loose = |k: Kind| match k {
        Kind::Int(_) => Kind::Int(8),
        Kind::Float4 => Kind::Float8,
        Kind::Varchar(_) | Kind::Char(_) => Kind::Text,
        Kind::Decimal(_) => Kind::Decimal(None),
        k => k,
    };
    loose(a) == loose(b)
}

/// Type `ty` from engine `from`, as engine `to` writes it; `None` when `to`
/// has nothing like it (PostgreSQL has no unsigned integers, MySQL no
/// arrays). The same engine keeps it as it is.
pub fn translate_type(from: Engine, ty: &str, to: Engine) -> Option<String> {
    if from == to {
        return Some(ty.to_string());
    }
    to.dialect().type_name(&kind(from, ty))
}

/// `name(n)`, or `name` without a length: for `Dialect::type_name`.
pub fn sized(name: &str, n: Option<u32>) -> String {
    n.map_or_else(|| name.to_string(), |n| format!("{name}({n})"))
}

/// `name(precision,scale)`, if they're given.
pub fn decimal(name: &str, ps: Option<(u32, u32)>) -> Option<String> {
    ps.map(|(p, s)| format!("{name}({p},{s})"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const PG: Engine = Engine::Postgres;
    const MY: Engine = Engine::MySql;
    const LITE: Engine = Engine::Sqlite;

    #[test]
    fn the_same_types_under_other_names() {
        // What the catalogs report for the same CREATE TABLE.
        for (pg, my) in [
            ("int4", "int"),
            ("int8", "bigint"),
            ("int2", "smallint"),
            ("varchar(120)", "varchar(120)"),
            ("char(3)", "char(3)"),
            ("numeric(10,2)", "decimal(10,2)"),
            ("float4", "float"),
            ("float8", "double"),
            ("bool", "tinyint(1)"),
            ("timestamp", "datetime(6)"),
            ("timestamptz", "timestamp(3)"),
            ("bytea", "longblob"),
            ("jsonb", "json"),
            ("text", "mediumtext"),
        ] {
            assert!(same_type(PG, pg, MY, my), "{pg} = {my}");
        }
        for (pg, my) in [("varchar(120)", "varchar(255)"), ("int4", "int unsigned"), ("timestamp", "timestamp")] {
            assert!(!same_type(PG, pg, MY, my), "{pg} ≠ {my}");
        }
        // SQLite keeps only the affinity.
        assert!(same_type(LITE, "INTEGER", PG, "int4") && same_type(LITE, "NVARCHAR(120)", MY, "varchar(40)"));
        assert!(!same_type(LITE, "INTEGER", PG, "text"));
    }

    #[test]
    fn types_as_another_engine_writes_them() {
        let to = |from, ty, to| translate_type(from, ty, to);
        assert_eq!(to(PG, "varchar(120)", MY).as_deref(), Some("varchar(120)"));
        assert_eq!(to(PG, "varchar", MY).as_deref(), Some("longtext"));
        assert_eq!(to(PG, "numeric", MY).as_deref(), Some("decimal(65,30)"));
        assert_eq!(to(PG, "timestamptz", MY).as_deref(), Some("timestamp(6)"));
        assert_eq!(to(MY, "tinyint(1)", PG).as_deref(), Some("boolean"));
        assert_eq!(to(MY, "datetime", PG).as_deref(), Some("timestamp"));
        assert_eq!(to(MY, "timestamp", PG).as_deref(), Some("timestamptz"));
        assert_eq!(to(LITE, "NVARCHAR(120)", PG).as_deref(), Some("varchar(120)"));
        assert_eq!(to(PG, "int4[]", LITE).as_deref(), Some("int4[]"), "SQLite takes any name");
        assert_eq!(to(MY, "int unsigned", PG), None);
        assert_eq!(to(PG, "int4[]", MY), None);
        assert_eq!(to(MY, "enum('a','b')", PG), None);
        assert_eq!(to(PG, "money", PG).as_deref(), Some("money"), "the same engine keeps it");
    }
}
