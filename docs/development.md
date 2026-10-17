# Development

[← Podėlis](../README.md)

```sh
cargo install cargo-deny --locked   # once: check.sh requires it
./scripts/check.sh   # rustfmt, clippy (pedantic, warnings as errors), tests, cargo-deny
```

How the code is organised — the app/UI/database layers, and the database
adapters (one folder per engine, a `Dialect` and capabilities per engine) —
is in [architecture.md](architecture.md), including how `build.rs` finds
the engines and how to add one.

Run `./scripts/install-hooks.sh` once to have git run `check.sh` before
every push (skip once with `git push --no-verify`).

Besides unit, snapshot and live-database tests, property tests
([proptest](https://crates.io/crates/proptest), `src/fuzz_tests.rs`) feed
random SQL-ish text to the lexer, splitter, analyzer, completion, parameter
handling and editor; failures they find are saved in `proptest-regressions/`
and re-run first.

**Where tests go.** Tests sit with the code they test, as Rust's unit
tests do: a `#[cfg(test)] mod tests` at the end of a file, or a file of
their own beside it once they grow (`src/app/tests.rs`, `src/ui/tests.rs`,
an engine's `src/db/<engine>/db_tests.rs`). Being inside the crate, they
can reach private code (`App`, its keys, what the UI drew), which is most
of what they check. Cargo's `tests/` folder is for integration tests:
crates of their own that see only a library's public API. Podėlis is a
program, not a library, so that folder is only for tests that run the
built `podelis` from outside (its path is in `CARGO_BIN_EXE_podelis`), like
a user or a script would: `-e`, `--format`, exit codes, `--help`.

The gateway's tests (`src/gateway/`) need no API keys and spend nothing:
they run the gateway in the test and answer as a model from a small fake
server on localhost (`fake_model`, Anthropic's streamed format), and
check what it was sent. To try a real model by hand, set `PODELIS_CONFIG_DIR`
to a scratch folder with a `[[models]]` entry, then `podelis gateway test`;
`podelis gateway stop` ends a gateway left running from a scratch folder.
`PODELIS_CONFIG_DIR` moves files only: passwords of connections saved
there still go to your OS keychain (service `podelis`, account
`<project id>/<connection>`), so remove those after trying things by hand.

The Rust version is pinned in `rust-toolchain.toml` (rustup installs it on
first use), so local builds and CI see the same lints. Lints are configured
in `Cargo.toml` (`[lints]`), formatting in `rustfmt.toml`, and
licence/advisory policy in `deny.toml`.

GitHub Actions, kept small to save minutes:

- `ci.yml` — on pushes to master (not docs-only ones): format,
  clippy and tests on Linux against real PostgreSQL and MySQL; a newer push
  cancels the older run. A force-push may not start it; run it by hand
  with `gh workflow run ci.yml --repo taislius/podelis` (or Actions → CI →
  Run workflow).
- `platforms.yml` — tests on macOS weekly, on release tags, or
  by hand (`gh workflow run platforms.yml --repo taislius/podelis`, or
  Actions → Platforms → Run workflow).
- `deny.yml` — `cargo deny` when dependencies change, and weekly.
- The Linux runners are pinned (`ubuntu-24.04`), not `ubuntu-latest`: a
  release binary needs at least the glibc it was built against, so
  building on the newest Ubuntu would leave out older systems.
- `release.yml` — on a `v*` tag, builds binaries for Linux and macOS and
  attaches them to the release; `packaging/homebrew/podelis.rb`
  is a formula template for a Homebrew tap. Pushing a moved tag runs it
  again and replaces the release's files, which breaks the tap's
  checksums: cancel that run (`gh run cancel <id>`) unless that's wanted.

What the automated tests can't cover (real terminals, remote servers, big
tables) is in [manual-testing.md](manual-testing.md): data sets
to test with, from generated to public servers with real data, and a
checklist to go through before a release.

UI rendering is covered by snapshot tests (`src/ui/snapshots/`). After an
intended UI change, review and accept them with `cargo insta review` (or
rerun with `INSTA_UPDATE=always` and check the diff).

SQLite tests always run (their files in a `podelis-tests` folder of the
system's temporary one). Tests against other live services are opt-in:

| Variable | Enables |
|---|---|
| `PODELIS_TEST_PG_PORT=5432` | PostgreSQL tests (also `_HOST`, `_USER`, `_PASSWORD`, `_DB` with the same prefix; defaults `127.0.0.1`, `postgres`, empty, none) |
| `PODELIS_TEST_MYSQL_PORT=3306` | MySQL tests (same extra variables; default user `root`). Each test makes a database of its own (`podelis_<topic>`) |
| `PODELIS_TEST_SSH_DEST=host` | SSH tunnel test: reaches the first engine's test server that's set through `ssh` to this host |
| `PODELIS_TEST_KEYCHAIN=1` | Round-trips throwaway entries through the real OS keychain |

### On Linux, in Docker

`docker/` runs Podėlis on Debian with the same PostgreSQL and MySQL
servers as CI, an SSH server for the tunnel test and an unlocked Secret
Service keychain, so every opt-in test above runs:

```sh
docker compose -f docker/compose.yaml run --rm dev ./scripts/check.sh  # every check, all live tests
docker compose -f docker/compose.yaml run --rm dev cargo run           # the app, in this terminal
docker compose -f docker/compose.yaml run --rm dev                     # a shell
docker compose -f docker/compose.yaml down -v                          # stop and remove everything
```

The checkout is mounted, so edits show up without rebuilding; builds go to
a volume, apart from the macOS `target/`. Inside, the servers are
`postgres:5432` (password `postgres`) and `mysql:3306` (password `root`).
