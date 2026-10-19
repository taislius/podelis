//! Finds the database engines: every folder of `src/db` with an
//! `engine.toml` is one. From them it writes `engines.rs` (into
//! `OUT_DIR`, included by `src/db/mod.rs`): the `Engine` enum and
//! `Engine::ALL`, the `Pool` and `Session` enums, the engines' modules,
//! and the macros that reach each engine's own code (`on_engine!`,
//! `on_pool!`, `on_session!`). Nothing outside an engine's folder names
//! the engines; adding one is adding its folder (and its Cargo feature).
//! An engine's feature (`feature` in its `engine.toml`, else its folder's
//! name) builds its driver; without it the engine's dialect is still built
//! (its SQL rules), but `Pool`, `Session`, `on_driver!` and `Engine::BUILT`
//! leave it out.

use std::fmt::Write as _;
use std::path::Path;

struct Found {
    /// Its folder's name, the module: `postgres`.
    module: String,
    /// Its variant, as `connections.toml` writes it: `Postgres`.
    name: String,
    /// Its place in lists (the connection form cycles in this order).
    order: u32,
    /// Its Cargo feature: without it, its dialect (SQL rules) is built but
    /// not its driver, and it can't be connected to.
    feature: String,
    /// The feature is on.
    built: bool,
}

/// `key = "value"` and `key = 1` lines; `#` starts a comment.
fn field(text: &str, key: &str) -> Option<String> {
    text.lines().find_map(|line| {
        let line = line.split('#').next()?.trim();
        let (k, v) = line.split_once('=')?;
        (k.trim() == key).then(|| v.trim().trim_matches('"').to_string())
    })
}

/// Every folder of `db` with an `engine.toml`, in their `order`, each with
/// whether its feature is on (at least one must be).
fn find_engines(db: &Path) -> Vec<Found> {
    let mut engines: Vec<Found> = Vec::new();
    for entry in std::fs::read_dir(db).expect("src/db").flatten() {
        let manifest = entry.path().join("engine.toml");
        let Ok(text) = std::fs::read_to_string(&manifest) else { continue };
        println!("cargo::rerun-if-changed={}", manifest.display());
        let module = entry.file_name().to_string_lossy().into_owned();
        let name = field(&text, "name").unwrap_or_else(|| panic!("{}: no `name`", manifest.display()));
        let order = field(&text, "order").and_then(|o| o.parse().ok()).unwrap_or(u32::MAX);
        let feature = field(&text, "feature").unwrap_or_else(|| module.clone());
        let var = format!("CARGO_FEATURE_{}", feature.to_uppercase().replace('-', "_"));
        println!("cargo::rerun-if-env-changed={var}");
        let built = std::env::var_os(&var).is_some();
        engines.push(Found { module, name, order, feature, built });
    }
    engines.sort_by(|a, b| (a.order, &a.module).cmp(&(b.order, &b.module)));
    assert!(!engines.is_empty(), "no engine in src/db (a folder with an engine.toml)");
    let features: Vec<String> = engines.iter().map(|e| e.feature.clone()).collect();
    assert!(
        engines.iter().any(|e| e.built),
        "no database engine is built: turn on at least one of the features {features:?}"
    );
    engines
}

fn main() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let db = root.join("src/db");
    println!("cargo::rerun-if-changed=src/db");
    let engines = find_engines(&db);
    let driven: Vec<&Found> = engines.iter().filter(|e| e.built).collect();

    let mut out = String::from("// Written by build.rs from src/db/*/engine.toml: don't edit.\n\n");
    for e in &engines {
        let path = db.join(&e.module).join("mod.rs");
        let _ = writeln!(out, "#[path = {:?}]\nmod {};", path.display().to_string(), e.module);
    }
    let names: Vec<&str> = engines.iter().map(|e| e.name.as_str()).collect();
    let _ = writeln!(
        out,
        "\n/// A database engine: one for each folder of `src/db` with an `engine.toml`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Engine {{ {} }}

impl Engine {{
    /// Every engine, in their `order`.
    pub const ALL: [Engine; {}] = [{}];
    /// The engines this build can connect to (their features on).
    pub const BUILT: [Engine; {}] = [{}];

    /// This build can connect to it.
    pub fn built(self) -> bool {{
        Engine::BUILT.contains(&self)
    }}
}}",
        names.join(", "),
        engines.len(),
        names.iter().map(|n| format!("Engine::{n}")).collect::<Vec<_>>().join(", "),
        driven.len(),
        driven.iter().map(|e| format!("Engine::{}", e.name)).collect::<Vec<_>>().join(", ")
    );
    let variants = |ty: &str| -> String {
        driven.iter().fold(String::new(), |mut s, e| {
            let _ = writeln!(s, "    {}({}::{ty}),", e.name, e.module);
            s
        })
    };
    let _ =
        writeln!(out, "\n/// The connection pool, of its engine's type.\nenum Pool {{\n{}}}", variants("DriverPool"));
    let _ = writeln!(
        out,
        "\n/// The session connection, of its engine's type.\nenum Session {{\n{}    /// After `close()`.\n    Closed,\n}}",
        variants("DriverConnection")
    );
    // A pool's connection is its engine's session: `Session::from(conn)`.
    for e in &driven {
        let _ = writeln!(
            out,
            "\nimpl From<{m}::DriverConnection> for Session {{\n    fn from(c: {m}::DriverConnection) -> Self {{\n        Session::{n}(c)\n    }}\n}}",
            m = e.module,
            n = e.name
        );
    }
    // The macros: each arm brings the engine's module in as `engine`.
    let arms = |all: bool, pattern: &dyn Fn(&Found) -> String| -> String {
        engines.iter().filter(|e| all || e.built).fold(String::new(), |mut s, e| {
            let _ = writeln!(
                s,
                "            {} => {{\n                #[allow(unused_imports)]\n                use $crate::db::{} as engine;\n                $body\n            }}",
                pattern(e),
                e.module
            );
            s
        })
    };
    // Engines whose feature is off: what `on_driver!` does for them.
    let fallback = if driven.len() < engines.len() { "            _ => $other,\n" } else { "" };
    let _ = writeln!(
        out,
        "
/// `$body` with `engine` as `$engine`'s module, for its dialect (built for
/// every engine): `on_engine!(e, engine::DIALECT)`.
macro_rules! on_engine {{
    ($engine:expr, $body:expr) => {{
        match $engine {{
{}        }}
    }};
}}

/// `$body` with `engine` as `$engine`'s module, for its driver (built only
/// with its feature on); `$other` for an engine built without it. Without
/// `other`, for code a connection of the engine runs, which can't exist.
macro_rules! on_driver {{
    ($engine:expr, $body:expr) => {{
        on_driver!($engine, $body, other => unreachable!(\"an engine this build can't connect to\"))
    }};
    ($engine:expr, $body:expr, other => $other:expr) => {{
        match $engine {{
{}{fallback}        }}
    }};
}}

/// `$body` with `$p` as the pool, `engine` as its engine's module.
macro_rules! on_pool {{
    ($pool:expr, |$p:ident| $body:expr) => {{
        match $pool {{
{}        }}
    }};
}}

/// `$body` with `$c` as the session's connection, `engine` as its engine's
/// module; `$closed` once it's closed.
macro_rules! on_session {{
    ($session:expr, |$c:ident| $body:expr, closed => $closed:expr) => {{
        match $session {{
{}            $crate::db::Session::Closed => $closed,
        }}
    }};
}}",
        arms(true, &|e| format!("$crate::db::Engine::{}", e.name)),
        arms(false, &|e| format!("$crate::db::Engine::{}", e.name)),
        arms(false, &|e| format!("$crate::db::Pool::{}($p)", e.name)),
        arms(false, &|e| format!("$crate::db::Session::{}($c)", e.name)),
    );
    let target = Path::new(&std::env::var("OUT_DIR").expect("OUT_DIR")).join("engines.rs");
    std::fs::write(target, out).expect("writing engines.rs");
}
