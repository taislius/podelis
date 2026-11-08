# Development

[← Podėlis](../README.md)

```sh
cargo install cargo-deny --locked   # once: check.sh requires it
./scripts/check.sh   # rustfmt, clippy (pedantic, warnings as errors), tests, cargo-deny
PODELIS_CHECK_ENGINES=1 ./scripts/check.sh   # also the four fewer-engine builds, as CI
```

How the code is organised — the app/UI/database layers, and the database
adapters (one folder per engine, a `Backend`, its capabilities and an `SqlDialect` per engine) —
is in [architecture.md](architecture.md), including how `build.rs` finds
the engines and how to add one.

Run `./scripts/install-hooks.sh` once to have git run `check.sh` before
every push (skip once with `git push --no-verify`).

Besides unit, snapshot and live-database tests, property tests
([proptest](https://crates.io/crates/proptest)) feed random input to the
lexer, splitter, formatter, analyzer, completion and parameter handling (the
core's `tests/properties.rs`), the editor and the library's search (beside
their code); failures they find are saved in each crate's
`tests/proptest-regressions/` and re-run first.

**Where tests go.** Tests sit with the code they test, as Rust's unit
tests do: a `#[cfg(test)] mod tests` at the end of a file, or a file of
their own beside it once they grow, named for what it tests (an engine's
`crates/podelis-core/src/db/<engine>/db_tests.rs`, podelis-ai's
`context_tests.rs` beside `context.rs`, which points to it with
`#[path = "context_tests.rs"] mod tests;`), or a folder of files by topic
(`src/app/tests/`, `src/ui/tests/`). A folder under `src/` holds code, so
none holds only tests; data tests read sits in a crate's `testdata/`
(podelis-ai's `testdata/context-eval/`), snapshots too (`testdata/snapshots/`: a
snapshot test holds `crate::snapshots_in_testdata()` while it asserts). The
app's snapshots are its UI tests', in `src/ui/tests/snapshots/`. Being inside the crate, they
can reach private code (`App`, its keys, what the UI drew), which is most
of what they check. The app's tests reach the core's test helpers
(`db::tests`: test servers, scratch databases) through its
`test-support` feature, on only for them. Cargo's `tests/` folder is for integration tests:
crates of their own that see only a library's public API. Podėlis is a
program, not a library, so that folder is only for tests that run the
built `podelis` from outside (its path is in `CARGO_BIN_EXE_podelis`), like
a user or a script would: `-e`, `--format`, exit codes, `--help`
(`tests/cli.rs`).

**Translations.** The app's words are in `crates/podelis-core/src/lang/`, a TOML file per
language (`en.toml`, `lt.toml`), built into the program. Code says
`t!("section.key")` (a `&'static str`) or `t!("section.key", name =
value)` for `{name}` in the text; a counted word is `tn!("section.key", n)`
with forms `one`, `few` and `other` (`plural` in `lang/mod.rs` has each
language's rule: Lithuanian uses all three). English is complete; a key
another language lacks shows in English. Tests check that every key the
code uses is in English, that English has none unused, and that each
translation has English's `{values}`. To add a language: copy `en.toml`
to `<code>.toml`, translate it, add it to `LANGUAGES` and its plural rule.
Tests run in English. The command line (`-e`, `doctor`, `gateway`), what
agents and models are told, and the bug-report diagnostics stay English.

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
licence/advisory policy in `tools/deny.toml`.

**Unix only, POSIX where it can be.** Podėlis needs a Unix: file modes
(`0600` for what holds secrets), process groups for password commands,
Unix sockets for the gateway. Built and tested on Linux and macOS; the
BSDs should build too (the keychain is the Secret Service there, as on
Linux) but aren't tested. What it runs is POSIX: `sh -c` for pipes and
viewers, `kill`, `ssh`; the opener is `open` on macOS and `xdg-open`
elsewhere. The scripts are POSIX `sh` (Debian's `/bin/sh` is dash):
`check.sh` runs shellcheck on them, or `dash -n` where only dash is
installed. Windows isn't supported.

**Mutation testing.** `cargo mutants --workspace` (install
`cargo-mutants`) changes the code where a bug hurts users most (what
agents may run, what each engine refuses them, the OS keychain:
`.cargo/mutants.toml`) one change at a time, and checks a test fails each
time; one that lives (`mutants.out/missed.txt`) is code the tests run
without checking: add the test that would catch it. About 20 minutes;
`--iterate` tries again only what wasn't caught.

**Fuzzing.** `crates/podelis-core/fuzz/` feeds any text, guided by which
code it reaches, to the core's SQL handling in every engine's dialect (the
lexer, statements, highlighting, analysis and lints, formatting,
parameters), with the property tests' invariants. It needs nightly and
cargo-fuzz (`rustup toolchain install nightly`, `cargo install
cargo-fuzz`); from `crates/podelis-core`, `cargo +nightly fuzz run
sql_text -- -max_total_time=600` runs it for ten minutes. A crash is saved
under `fuzz/artifacts/`: add it as a test, and fix it. Its own workspace,
so nightly stays out of the project's builds.

**Code scanning.** `codeql.yml` runs GitHub's CodeQL over the Rust code
and the workflows on pushes to master and weekly; what it finds is in the
repository's Security tab, not a failed run.

**The context's eval.** `cargo test retrieval -- --nocapture` prints
recall, precision and the context's size per database for the gold set
in `crates/podelis-ai/testdata/context-eval/questions.txt` (`PODELIS_EVAL_ALL=1` also
lists each question's extra tables and why they came); it fails under
the floors in `eval.rs`, to raise as the builder improves. The schemas
are snapshots (`eval/<database>.json`, no rows); to make them again from
live databases (the app's `gateway/eval_snapshots.rs`, which opens them by
URL):

```sh
PODELIS_EVAL_DUMP="pagila=postgres://postgres:…@127.0.0.1:55433/pagila?sslmode=disable \
  sakila=mysql://root:…@127.0.0.1:53385/sakila?ssl-mode=disabled \
  chinook=$HOME/podelis-test/Chinook.sqlite" cargo test -p podelis dump_schemas -- --ignored
```

GitHub Actions, kept small to save minutes:

- `ci.yml` — on pushes to master (not docs-only ones): format,
  clippy and tests on Linux against real PostgreSQL and MySQL; beside it,
  clippy and tests built with fewer engines (`--no-default-features
  --features …`: the README's `postgres,sqlite`, each engine alone, and
  the opt-in DuckDB beside the others and alone), so
  nothing assumes an engine is there (a test that needs SQLite's files
  carries `#[cfg(feature = "sqlite")]`); a newer push
  cancels the older run. A force-push may not start it; run it by hand
  with `gh workflow run ci.yml --repo taislius/podelis` (or Actions → CI →
  Run workflow).
- `platforms.yml` — tests on macOS weekly, on release tags, or
  by hand (`gh workflow run platforms.yml --repo taislius/podelis`, or
  Actions → Platforms → Run workflow).
- `deny.yml` — `cargo deny` when dependencies change, and weekly; beside
  it osv-scanner (OSV: GitHub's advisories as well as RustSec's), with the
  same exceptions in `tools/osv-scanner.toml` as in `tools/deny.toml`, and
  cargo-machete: a dependency no code names fails (a false alarm goes in
  that crate's `[package.metadata.cargo-machete] ignored`; `check.sh` runs
  it too where it's installed).
- The Linux runners are pinned (`ubuntu-24.04`), not `ubuntu-latest`: a
  release binary needs at least the glibc it was built against, so
  building on the newest Ubuntu would leave out older systems.
- `release.yml` — on a `v*` tag, builds binaries for Linux and macOS and
  attaches them to the release; `packaging/homebrew/podelis.rb`
  is a formula template for a Homebrew tap. Its last job points
  taislius/homebrew-tap at the release, but only with a
  `HOMEBREW_TAP_TOKEN` secret in taislius/podelis (a fine-grained token
  with Contents read and write on homebrew-tap); without it, or for a
  pre-release tag, it says so and skips. By hand:
  `scripts/homebrew-formula.sh <version> <folder of the .sha256 files>`
  writes the formula, committed in the tap as Taislius (a fresh clone
  takes the global git identity: give `-c user.name=… -c user.email=…`). Pushing a moved tag runs it
  again and replaces the release's files, which breaks the tap's
  checksums: cancel that run (`gh run cancel <id>`) unless that's wanted.
  The release itself is made first, once; each build then uploads to it,
  trying again a few times when GitHub's API times out.
- **SBOM and auditable binaries.** Each release build is made with
  `cargo auditable`, which keeps the dependency list inside the binary
  (`cargo audit bin podelis`, Trivy, Syft and Grype read it), and ships a
  CycloneDX 1.5 SBOM (`cargo cyclonedx`, without build-only crates) in the
  archive and beside it as `podelis-<tag>-<target>.cdx.json`. The patched
  sqlx-mysql shows there as a local file (`file://patches/sqlx-mysql`).
  Locally: `cargo cyclonedx --format json --spec-version 1.5
  --no-build-deps --override-filename podelis.cdx`.
- **Signing on macOS.** The keychain ties a stored key or password to
  the program that stored it, so an unsigned build (each `cargo build`,
  each update) is a stranger to it and its Allow dialog comes again.
  `scripts/sign-macos.sh` signs a build with a code-signing identity of
  yours (a self-signed one from Keychain Access will do; the script says
  how) under the identifier `lt.taislius.podelis`, and the release builds
  are signed the same way when the `MACOS_SIGN_P12` (the certificate and
  its key as a base64 `.p12`) and `MACOS_SIGN_PASSWORD` secrets are set;
  without them they're left as built. The first signed build asks once
  more: choose Always Allow.
- `retry.yml` — when a Release or Platforms run fails, runs its failed
  jobs once more (a macOS runner never picked up, an API timeout: GitHub's
  own trouble). Jobs that never got a runner at all are run again up to
  three times (v0.6.0's macOS tests needed three attempts). Any other
  second failure stays failed for a person to look at:
  `gh run rerun <id> --failed --repo taislius/podelis` (it needs admin
  rights on the repository).

What the automated tests can't cover (real terminals, remote servers, big
tables) is in [manual-testing.md](manual-testing.md): data sets
to test with, from generated to public servers with real data, and a
checklist to go through before a release.

UI rendering is covered by snapshot tests (`src/ui/snapshots/`), and every
menu's items and order by those of `src/ui/tests/menus.rs`. After an
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

## DuckDB's build and disk

DuckDB (`--features duckdb`) builds from C++: a few minutes, and about
3 GB under `target/debug/build/libduckdb-sys-*`, once for each set of
features a command builds with. Keep to the workspace's commands
(`cargo clippy --all-targets --features duckdb`, `cargo test --features
duckdb`), which share one build: `-p podelis-core` and other feature sets
make another copy each. On a nearly full disk, delete the old copies (or
`cargo clean`) first.

## Which checks when

A change is checked by what it touches; the whole matrix runs before
master.

| When | Run | Time |
|---|---|---|
| each change | `scripts/check-changed.sh`: rustfmt, the workspace's clippy, and the tests of what changed (by its map: an engine's folder its tests, `sql/` the SQL and editor tests, `podelis-ai` its own…), with an opt-in engine's feature when its folder changed | about a minute |
| before pushing | `scripts/check.sh` (the pre-push hook): the default build's clippy and every test, cargo-deny, cargo-machete, shellcheck | about 10 minutes |
| before master | CI on the branch (`gh workflow run ci.yml --ref <branch>`): every build, fewer engines and the opt-in ones, each with the live servers | about 15 minutes, in parallel |

`check-changed.sh --dry-run` says what it would run and what it leaves
out; `--paths <path>…` asks for given paths; `--full` runs `check.sh`.
Its map is `scope_of` in the script (its `--self-test` runs in
`check.sh`). Keep to the workspace's commands: each set of features is a
build of its own (DuckDB's ~3 GB).

## The changelog

`CHANGELOG.md` is one for Podėlis (its crates aren't published, and a
change is the program's), drafted from the commits by
[git-cliff](https://git-cliff.org) (`brew install git-cliff`), by the kind
each commit starts with: `[feat]` Added, `[fix]` Fixed, `[refactor]`
Changed, `[deps]` Dependencies, `[docs]` Documentation, `[build]`, `[ci]`
and `[test]` Development (`cliff.toml`). Before a release:

```sh
git cliff --unreleased --tag vX.Y.Z --prepend CHANGELOG.md   # its section, then edit it
git cliff --latest --strip all                                # the release's text, after tagging
```

Edit the draft for readers: what changed for them first, in their words.
If a crate is ever published, it gets a changelog of its own
(`git cliff --include-path 'crates/<crate>/**'`).
