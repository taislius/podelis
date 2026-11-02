//! Column types across engines: which are the same though named
//! differently, and what a type is called on another engine, for comparing
//! schemas and the SQL that aligns them. The vocabulary (`TypeKind`) and the
//! SQL standard's names are here; each engine's own names, and how it writes
//! each kind, are its dialect's (`Dialect::type_kind`, `Dialect::type_name`).

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
    /// A date and a wall-clock time, in no zone.
    Timestamp,
    /// A moment in time, whatever zone it's shown in.
    TimestampTz,
    Binary,
    Json,
    Uuid,
    /// Anything else, as written (lowercase).
    Other(String),
}

/// A type's name without its `(…)`: `decimal(10,2)` → `decimal`.
pub fn base(ty: &str) -> &str {
    ty.split('(').next().unwrap_or_default().trim()
}

/// The numbers in `(…)` after a type's name: `decimal(10,2)` → [10, 2].
pub fn arguments(ty: &str) -> Vec<u32> {
    ty.split_once('(')
        .and_then(|(_, rest)| rest.split_once(')'))
        .map(|(args, _)| args.split(',').filter_map(|a| a.trim().parse().ok()).collect())
        .unwrap_or_default()
}

/// What `ty` holds on `engine`: the engine's own meaning first, then the
/// standard's.
fn kind(engine: Engine, ty: &str) -> TypeKind {
    let ty = ty.trim().to_ascii_lowercase();
    engine.dialect().type_kind(&ty).unwrap_or_else(|| standard(&ty))
}

/// What a type the SQL standard names holds (and `text`, which every
/// engine has).
fn standard(ty: &str) -> TypeKind {
    use TypeKind as Kind;
    let args = arguments(ty);
    let length = args.first().copied();
    match base(ty) {
        "smallint" => Kind::Int(2),
        "int" | "integer" => Kind::Int(4),
        "bigint" => Kind::Int(8),
        "boolean" => Kind::Bool,
        "numeric" | "decimal" => Kind::Decimal(args.get(1).map(|&scale| (args[0], scale))),
        "real" => Kind::Float4,
        "double precision" | "float" => Kind::Float8,
        "varchar" | "character varying" | "char varying" => Kind::Varchar(length),
        "char" | "character" => Kind::Char(length),
        "text" | "clob" | "character large object" => Kind::Text,
        "date" => Kind::Date,
        "time" | "time without time zone" => Kind::Time,
        "timestamp" | "timestamp without time zone" => Kind::Timestamp,
        "timestamp with time zone" => Kind::TimestampTz,
        "binary" | "varbinary" | "binary varying" | "blob" | "binary large object" => Kind::Binary,
        "json" => Kind::Json,
        _ => Kind::Other(ty.to_string()),
    }
}

/// Whether type `a` on one engine and `b` on another hold the same values.
/// Beside an engine that keeps only a type's affinity, any integer matches
/// any other, and lengths don't count.
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
/// has nothing like it (an unsigned integer, an array, an enum). The same
/// engine keeps it as it is.
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
