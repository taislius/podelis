//! The engines built, as podelis-core's build script found them (passed on
//! through its `links` metadata): `every_engine` when each engine's feature
//! is on, for what's allowed only in a build with fewer (main.rs), and
//! `sql_engine` when one built speaks SQL.

fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    println!("cargo::rerun-if-env-changed=DEP_PODELIS_CORE_EVERY_ENGINE");
    println!("cargo::rustc-check-cfg=cfg(every_engine)");
    if std::env::var("DEP_PODELIS_CORE_EVERY_ENGINE").as_deref() == Ok("true") {
        println!("cargo::rustc-cfg=every_engine");
    }
    // `sql_engine` when an engine built speaks SQL (not MongoDB alone): for
    // the tests of SQL's own features.
    println!("cargo::rerun-if-env-changed=DEP_PODELIS_CORE_SQL_ENGINE");
    println!("cargo::rustc-check-cfg=cfg(sql_engine)");
    if std::env::var("DEP_PODELIS_CORE_SQL_ENGINE").as_deref() == Ok("true") {
        println!("cargo::rustc-cfg=sql_engine");
    }
    // `cargo install --path .` can't ask which engines to build: a release
    // build with the default ones says which it has, and how to choose.
    // (Cargo shows a build script's warnings for a local package only, so
    // not for `--git`; scripts/build.sh passes --no-default-features.)
    if std::env::var_os("CARGO_FEATURE_DEFAULT").is_some() && std::env::var("PROFILE").as_deref() == Ok("release") {
        // This crate's features are its engines (and `default`).
        let mut engines: Vec<String> = std::env::vars()
            .filter_map(|(k, _)| k.strip_prefix("CARGO_FEATURE_").map(str::to_lowercase))
            .filter(|f| f != "default")
            .collect();
        engines.sort();
        println!(
            "cargo::warning=built with the engines {} (the default ones); \
             scripts/build.sh asks which, or --features duckdb,mongodb adds the opt-in ones",
            engines.join(", ")
        );
    }
}
