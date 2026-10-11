# Development

[← Podėlis](../README.md)

```sh
cargo install cargo-deny --locked   # once: check.sh requires it
./scripts/check.sh   # rustfmt, clippy (pedantic, warnings as errors), tests, cargo-deny
```

How the code is organised — the app/UI/database layers, and the database
adapters (one folder per engine, a `Dialect` and capabilities per engine) —
is in [docs/architecture.md](docs/architecture.md), including how to add an
engine.

Run `./scripts/install-hooks.sh` once to have git run `check.sh` before
every push (skip once with `git push --no-verify`).

Besides unit, snapshot and live-database tests, property tests
([proptest](https://crates.io/crates/proptest), `src/fuzz_tests.rs`) feed
random SQL-ish text to the lexer, splitter, analyzer, completion, parameter
handling and editor; failures they find are saved in `proptest-regressions/`
and re-run first.

The Rust version is pinned in `rust-toolchain.toml` (rustup installs it on
first use), so local builds and CI see the same lints. Lints are configured
in `Cargo.toml` (`[lints]`), formatting in `rustfmt.toml`, and
licence/advisory policy in `deny.toml`.

GitHub Actions, kept small to save minutes:

- `ci.yml` — on pushes to master (not docs-only ones): format,
  clippy and tests on Linux against real PostgreSQL and MySQL; a newer push
  cancels the older run.
- `platforms.yml` — tests on macOS weekly, on release tags, or
  by hand (Actions → Platforms → Run workflow).
- `deny.yml` — `cargo deny` when dependencies change, and weekly.
- `release.yml` — on a `v*` tag, builds binaries for Linux and macOS and
  attaches them to the release; `packaging/homebrew/podelis.rb`
  is a formula template for a Homebrew tap.

What the automated tests can't cover (real terminals, remote servers, big
tables) is in [docs/manual-testing.md](docs/manual-testing.md): data sets
to test with, from generated to public servers with real data, and a
checklist to go through before a release.

UI rendering is covered by snapshot tests (`src/ui/snapshots/`). After an
intended UI change, review and accept them with `cargo insta review` (or
rerun with `INSTA_UPDATE=always` and check the diff).

SQLite tests always run. Tests against other live services are opt-in:

| Variable | Enables |
|---|---|
| `PODELIS_TEST_PG_PORT=5432` | PostgreSQL tests (also `_HOST`, `_USER`, `_PASSWORD`, `_DB` with the same prefix; defaults `127.0.0.1`, `postgres`, empty, none) |
| `PODELIS_TEST_MYSQL_PORT=3306` | MySQL tests (same extra variables; default user `root`). Creates and drops `podelis_test` and `podelis_ro` databases |
| `PODELIS_TEST_SSH_DEST=host` | SSH tunnel test: reaches the MySQL (or Postgres) test server through `ssh` to this host |
| `PODELIS_TEST_KEYCHAIN=1` | Round-trips throwaway entries through the real OS keychain |
