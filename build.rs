//! The engines built, as podelis-core's build script found them (passed on
//! through its `links` metadata): `every_engine` when each engine's feature
//! is on, for what's allowed only in a build with fewer (main.rs).

fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    println!("cargo::rerun-if-env-changed=DEP_PODELIS_CORE_EVERY_ENGINE");
    println!("cargo::rustc-check-cfg=cfg(every_engine)");
    if std::env::var("DEP_PODELIS_CORE_EVERY_ENGINE").as_deref() == Ok("true") {
        println!("cargo::rustc-cfg=every_engine");
    }
}
