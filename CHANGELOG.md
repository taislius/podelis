# Changelog

What changed in each release of Podėlis, newest first. Drafted from the
commits by [git-cliff](https://git-cliff.org) (`cliff.toml`), then edited.
The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [0.8.0] — 2026-11-03

### Added

- DuckDB, opt-in (--features duckdb): the first engine without sqlx, on the driver seam; the duckdb crate on blocking threads, rows streamed through a channel, values as text by Arrow's formatting and DuckDB's type names; one instance per file in a process (a read-only connection shares a writing one and runs only reads; a writer can't share a read-only one); full scans read from its drawn plan (wrapped names, scans its key narrows or a streaming limit stops left out); names it keeps quoted, no string lengths compared; its catalog, details, comments, cancel and errors; its tests, live always (a file), and CI entries with it and alone. Beside it: engine.toml's opt_in (every_engine doesn't wait for it), the via-sqlx feature for what the sqlx engines share, TestSql's analyzed_shows, keywords and tests that no longer assume three engines, async-executor 1.13.3 (1.14.0 yanked); not yet: a sandbox's copy, macros, plan notes
- MongoDB, opt-in (--features mongodb): the first engine without SQL. The query language is the engine's (Backend::language: SQL, which its dialect fills in, or a QueryLanguage of its own), and the sql module's entry points (split, tokenize, highlight, parse, analyze) ask it; engine.dialect() is an Option, each of its callers deciding what an engine without SQL gets (what's SQL's says so: edits, row forms, follow, review, sandboxes, plans, INSERT exports, imports, agents; literals are JSON's, transactions off); ping, the version and sql_mode queries, system tables, agents' rules and login failures are each engine's Backend's. MongoDB: mongosh's calls (db.<collection>.<method>(…) with sort, limit, skip and projection chained, getCollection, runCommand) in relaxed JSON with the shell's helpers; reads, writes and schema changes by the method, escapes undone, unknown commands and empty-filter deletes asked about; the official driver, documents as rows (_id first, fields as first seen, nested as JSON), cursors kept to the row limit and streamed, a cancel kept until the next run; the catalog sampled from the pool's client; read-only runs only reads; what it can't read is a syntax error before the server sees it. sql = false in engine.toml sets the sql_engine cfg for SQL's own tests; its live tests on a mongo:8 server and CI's matrix entries; docs
- Parquet export, from every engine (MongoDB too): the rows shown, and every row of a result (to a scratch file as it streams, the types settled once every value is read); each column typed by its engine's type name (type_kind is each engine's Backend's now, MongoDB's own), widened where a value doesn't fit (integers to floats unless past 2^53, booleans to integers, moments without a zone to timestamps) and text otherwise, a decimal of no given scale by its values'; Snappy, row groups of 65,536; off the UI thread, Esc stops it; parquet 59 (no thrift, no paste). The connection pane says how many connections are in use and how many this user may have (PostgreSQL's reserved slots taken off, never said; MySQL's; MongoDB's), on a pool connection. Tests for each, live; README

### Fixed

- A schema changed elsewhere is seen at once: the driver's prepared statements are dropped before preparing (an empty result's columns and the syntax check came from the shape a statement was first prepared against), and an AI ask has the gateway read the schema again when it may keep an older one (its first after the app starts, as the gateway outlives it, and its first after the app loaded the schema again: after DDL, r in the navigator); the gateway's ask takes refresh, as its context did

### Changed

- What mutation testing showed was dead weight: schema checks it wrote something instead of counting, a log entry's time is only write_log's, and the gateway's wait for clients to read that it stops is a function of its own (let_clients_read)
- What models are told is in crates/podelis-ai/prompts/, a Markdown file each (the system prompts, the explain message, the lookups note, the names check's feedback, the tools' descriptions), compiled in: read as Markdown, {name}s filled in one pass, each placeholder checked, a snapshot of every text as sent; the same words as before
- Each engine's knowledge in its folder: transactions, savepoints, the ping, a sandbox's session (MySQL's autocommit) and a file's copy (SQLite's VACUUM INTO) from the dialect, no engine checked outside db/; the statements for rows (select, insert, update, delete) from the dialect too; placeholders declared by form, not by engine; keywords: SQL's shared, each engine's own in its dialect, one is_keyword; one CANCEL_BACKEND for PostgreSQL; SQLite's queries as raw strings; the linked tables a fixture (tests/fixtures), and tests for each
- Hosted services in db/services/, one file each: a Service is found by its engine and host, and says what it refuses (Supabase's transaction pooler), whether it needs TLS and which schemas are its own; the connection and the catalog ask it, naming none
- Column types: the SQL standard's names in db/types.rs, each engine's own in its dialect (PostgreSQL's int4, bpchar, bytea, jsonb, timestamptz and arrays; MySQL's unsigned, tinyint(1), mediumint, *text, *blob, datetime and timestamp, and real a double; SQLite's from its documentation), no engine named in types.rs; its tests with the shared ones
- A failed connection's hints: the network's, SSH's and TLS's in diagnose.rs, each engine's own (PostgreSQL's pg_hba, roles, a server starting up; MySQL's access denied, ~/.my.cnf, too many connections; SQLite's file it can't open) in its dialect's connect_hint; tested so each fires only on its own engine's errors
- What every engine has, SQL or not, is its Backend (db/backend.rs: name, URL schemes, port, socket, capabilities, connection hints, password files, containers); SqlDialect: Backend keeps the SQL; code that needs no SQL asks engine.backend(), so a database without SQL needs only that; the architecture and development docs for these four
- Each engine's driver in its folder (driver.rs): acquiring, closing, pinging, detaching, running, streaming rows, preparing, columns and one value, and its errors read as ErrorFacts (broken, no connection, TLS, the server's message, position, what a check makes of it); run.rs, errors.rs and mod.rs ask only it, naming no sqlx; the sqlx engines share via_sqlx.rs (driver!() and error_facts), the dialect's sqlx error methods moved into their drivers; docs

### Documentation

- The dependency checks' config is in tools/
- Where the property tests and their seeds are
- SECURITY.md: report privately through GitHub's advisories; only the latest version gets fixes; what counts (secrets leaving, another user reaching yours, schemas sent against share_schema, a way past agents' rules, which are a safety net, not the boundary)
- README: Extending and scripting, each extension point in the app and headless (rows out, data in, passwords and keys from your tools, agents, ask and memory, exit codes); fuzzing in the development guide; the agent activity is opened from the palette, not Ctrl+K a (EXPLAIN ANALYZE's)
- Nothing outside db/ names an engine, not even to say all are built (every_engine); the prompts' folder
- README: cargo install builds the default engines without asking, and a --path . install warns which

### Development

- cargo-deny's, osv-scanner's and cargo-about's config in tools/, out of the root (each read by its path: cargo deny --config before check, osv-scanner --config, cargo about -c); rustfmt's and clippy's stay in the root, where they and editors look
- The property tests by crate: the core's nine through its public API (crates/podelis-core/tests/properties.rs, its seeds beside it), the editor's and the library's beside their code; their seeds in tests/proptest-regressions/, none at a crate's root; sql::highlight_lines public for them
- Fuzzing the core's SQL handling (crates/podelis-core/fuzz, cargo-fuzz on nightly, its own workspace): any text in every engine's dialect, with the property tests' invariants; a first ten minutes, a million inputs, found nothing. cargo-mutants' scope takes in the MCP server and the gateway's socket
- The MCP server and the gateway's socket under cargo-mutants, and the tests it showed missing: a table's shape and neighbours, a big schema cut, row estimates by engine, what the log says (who, engine, tool, SQL, rows, errors against refusals), connections read-only on the server unless agents may write, a sandbox in use not idle, one on a reopened connection lost, approvals saying production, dry-run notes and MySQL's autocommit after a sandbox (live), answers at the size limit, plan notes, and the gateway's idle counting from the last client; what isn't worth a test excluded, each with its reason
- No crate lists the engines to tell whether all are built: the core's build.rs sets every_engine from the engines it finds, and passes it to the app's build script through links metadata; the lints allowed only with fewer engines follow it
- MongoDB's tests on mongo:7.0: 8.0 and later crash on Linux 6.19 and newer kernels, as the runners' (SERVER-121912); 7.0 passes them, and is supported to 2027
- MongoDB's URL test: its password made as it runs, not a value in the code (CodeQL read the made-up one as a hard-coded password)
- scripts/check-changed.sh: a change checked by what it touches (rustfmt, the workspace's clippy, and the tests its map picks: an engine's folder its tests with its feature, sql/ the SQL and editor tests, podelis-ai its own…), saying what it ran and left out; --dry-run, --paths, --full, --self-test (run by check.sh); development.md: which checks when
- The DuckDB jobs free the runner's disk first (its unused SDKs: .NET, Android, Haskell, a spare CodeQL): DuckDB's C++ build and its cache filled it
- scripts/build.sh: asks which databases to build with (checkboxes by whiptail or dialog, else a numbered list), then installs or builds with their features; the engines from their engine.toml (each with a label, tested to match Backend::label, and a note: DuckDB's build cost), opt-in ones unticked; --engines, --build, --print; no questions without a terminal; the last choice kept in .podelis-engines; README and architecture
- CHANGELOG.md, one for Podėlis (its crates aren't published), drafted by git-cliff from each commit's kind (cliff.toml: [feat] Added, [fix] Fixed, [refactor] Changed, [deps], [docs], and [build]/[ci]/[test] Development; release candidates with their release), every release from 0.2.0 on; development.md says how to make a release's section and its text; check-changed.sh runs nothing for cliff.toml
- A release build with the default engines says which it has and how to choose (cargo install --path . can't ask): a cargo warning naming them, scripts/build.sh and --features duckdb,mongodb; none in debug builds or with --no-default-features (build.sh's)

## [0.7.5] — 2026-11-02

### Documentation

- Mutation testing and code scanning

### Development

- cargo-mutants on the agents' layer and the keychain (.cargo/mutants.toml), and the tests its first run showed missing: a sandbox runs writes but refuses, by name, a schema change that commits at once; refusals quote a statement in at most 60 characters; the program leaves the in-memory keychain (os_keychain.rs, in a process of its own)
- CodeQL over the Rust code and the workflows, on pushes to master and weekly; findings in the Security tab
- CI, Dependencies and Platforms say their token may only read the code (CodeQL: actions/missing-workflow-permissions)

## [0.7.4] — 2026-11-02

### Fixed

- The workspace's review: live import tests only in builds with their engine, the core's lint allowance keyed on all three engines (not default, which the app turns off), describe checked on each engine's links again (related.rs), the key-coverage tests in the app's crate (lang_keys.rs) reading both crates, one set of agents' engine-test helpers, one is_word_char (sql's, the editor's too); docs: both manifests for a new engine, the core's pub items and dead code
- The AI crate's review: the program always uses the OS keychain (main says so; the in-memory one only in tests, even when cargo test builds the binary with test-support), the keychain's tests in the core and no store shim; the app's unused dependencies gone; podelis_ai named the one way (no gateway re-exports) and checked by layers.rs (not from editor, related, chart); the context for a question and its eval in podelis-ai (Schema is the snapshot), the snapshot writer in the app; engine_of counts in one pass

### Changed

- Layers: a connection as typed is config::draft::ConnectionDraft (the gateway, agents, -e and doctor no longer go through the app's form; ConnectForm is a draft plus the form's state), PATH checks, password prompts and the install's facts in system, [[models]] entries in config, the gateway's command enum its own; tests/layers.rs fails on an import against the layers; App's popups, ask, next run and keep-alive in their own parts, background events one enum, main's commands one match
- db, sql and lang reach nothing else, even in tests: agents' per-engine tests in agents/engines.rs, the live import tests in transfer::import, file_profile in config::store, sql's own word check, the password test without the draft
- A Cargo workspace: db, sql and lang are the podelis-core crate (crates/podelis-core, with build.rs and the engine features, which the root crate passes on), re-exported so the app still says crate::db; its test helpers behind test-support for the app's tests; workspace dependencies and lints, publish = false (installed from git), cargo-deny and cargo-about accept the new crate; layers.rs checks the app's crate, Cargo the core
- podelis-ai: the models (and their [[models]] entries), context, memory, migrations and names are a crate under the app (crates/podelis-ai), re-exported as gateway::{…}; the gateway keeps the process (protocol, server, client, its methods) and the context's eval; the OS keychain is the core's (db::keychain, its test fake behind test-support); engine_of takes the engines; the AI crate builds alone with the core's engines on, in the app with the app's; deny, about and docs follow
- app/mod.rs is App and its routing (752 lines, from 1,589): each part's types and methods in its own file: the y/n questions in confirm.rs, the navigator's types in schema.rs, the background events in events.rs, the run state and export formats in query/, results and their popups in results.rs, which tab runs what in tabs.rs

### Documentation

- Where app/mod.rs's parts went; cargo-machete in deny.yml and check.sh; why the sqlx-mysql copy skips two dependencies

### Development

- Built with fewer engines, the tests pass: live helpers say no server for an engine left out, the connection form's tests follow the engines built, container tests by their engine; CI and check.sh run clippy and tests again with PostgreSQL and SQLite only (the README's build)
- Every engine built alone passes too: tests that need SQLite's files say so (#[cfg(feature = "sqlite")], the CLI tests' file), a server form's tests need a server engine, exit code 4 on the engine built; CI's engines matrix (postgres+sqlite, postgres, mysql, sqlite; clippy and tests, live servers) beside the main job, which is as it was
- The branch's review: SQLite's file helpers exist only with SQLite, so a test needing them without saying so doesn't compile in CI's other builds (live_agents_on_servers fixed, through an engine-free connection helper); the CLI tests by what they need (--help and exit code 4 in every build); layers.rs checks inside the core too (lang below sql and db), with braces counted outside strings and char literals; key coverage over the three crates, by set; main's lint allowance keyed on the three engines; deny.yml on member manifests; check.sh's second build opt-in (PODELIS_CHECK_ENGINES=1); a live keychain test on real account names
- The last review: layers.rs reads code through a small lexer (comments, strings, raw strings, char literals blanked, line breaks kept), so a multi-line or raw string can't throw its test-module skipping off, and resolves super:: to the crate's root; the move of a connection's password tested in the app (m, in-memory keychain), the live keychain test only what it checks, its entry gone however it ends; file_connection on file_profile; file_engine is SQLite's; key coverage over every crate in crates/; PODELIS_CHECK_ENGINES=1 runs CI's four builds; the engines job's cache keys without commas
- cargo-machete: a dependency no code names fails deny.yml (on any manifest change, and weekly) and check.sh where it's installed; the sqlx-mysql copy's two unused upstream dependencies skipped, not removed

## [0.7.3] — 2026-11-01

### Added

- Context: a retrieval eval (schema snapshots of Pagila, Sakila and Chinook, 26 questions with their tables, recall and precision floors); money words (revenue, earned, spent…) match amounts, totals and payments; views below tables, not for a table's name or their columns alone; no second way between tables joined already
- The status bar says whether AI is on: AI off, the default model, or that asking would first ask to share the schema; it gives way to messages
- Supabase: TLS required on its hosts, its transaction poolers (port 6543) refused with the session port to use, its own schemas (storage, realtime, vault…) as system ones
- Ctrl+K j lists the open connections, each with its tabs and how it is (a query running, a transaction open, lost): Enter goes to it, Ctrl+D closes it after asking (every tab on it disconnected, not reconnected by its next run; not while a query runs). In a prompt or form Ctrl+C copies what's typed and Ctrl+V pastes from the clipboard (renaming a tab, the palette, the library…)
- Memory: a fact can be changed (e in Ctrl+K k, podelis memory edit, memory.update): its relation kept unless changed, then the user's (confirmed, kept when memory learn runs again), a model's guess so corrected isn't suggested again; the search index follows edits

### Fixed

- Agents: SQL with a MySQL executable comment (/*! … */, /*M! … */) is refused, as is a backslash before a quote (where a string ends depends on the server's sql_mode) and PostgreSQL's set_config(); a read-only agent could write a file with /*! INTO OUTFILE … */; ConnectParams' Debug hides the password; the MCP tests in agents/mcp/tests.rs, the sandbox one rerunnable on a reused server
- The missing-keychain hint for every Unix but macOS (the BSDs use the Secret Service too); check.sh checks the scripts are POSIX sh (shellcheck, or dash -n)
- The config folder is made the user's alone (0700) at start when it was open to others, not a symlink's target nor a PODELIS_CONFIG_DIR folder

### Changed

- Lints: every suppression is #[expect(…, reason = "…")] (two stale ones removed; allow only where it can't be expect, with its reason), no unwrap() outside tests (expect with why it can't fail, or none at all), Arc::clone(&x), no redundant clones; builds with one engine lint clean too

### Documentation

- Supabase connections; the SBOM and auditable binaries; osv-scanner; Unix only, POSIX where it can be; the sqlx empty-password fix merged upstream
- The open connections list, copy and paste in prompts, changing facts, who can read the files, the AI indicator, agents' new refusals (and the MySQL file write they close), the context's new rules and its eval, Supabase in the README; manual tests for them

### Development

- Release: binaries built with cargo auditable and a CycloneDX SBOM for each, in the archive and beside it; install-action pinned to a commit. Dependencies: osv-scanner beside cargo-deny, with the same exceptions

## [0.7.2] — 2026-10-24

### Added

- A SQLite file's sandbox works on a copy (VACUUM INTO, made at its first write): the original isn't touched or locked meanwhile, schema changes allowed, the copy deleted at the end, run for real writes the original; the sandbox window shows the locks its session holds, and a sandbox open 15 minutes or past 100,000 rows says so once; a query ending in a hidden tab rings and notifies with the tab's name, even with the terminal in front
- Memory: `excluded by default` facts (rows left out unless asked for), explained to the model and suggested by it; facts about a column of a table the database doesn't have aren't shown, and context and ask list them as maybe stale

### Documentation

- The SQLite sandbox on a copy, the sandbox's locks and age, hidden tabs' notifications, excluded-by-default and stale facts, signing on macOS; manual tests for them

### Development

- Release: macOS binaries signed under a fixed identity when the MACOS_SIGN_P12 and MACOS_SIGN_PASSWORD secrets are set; scripts/sign-macos.sh signs a local build the same way (the keychain's Allow dialog then comes once)

## [0.7.1] — 2026-10-24

### Added

- A sandbox for agents (`agents = "sandbox"`): an agent's writes, never destructive ones, run in one transaction kept open between its calls, a savepoint before each, never committed; each call checks it's the same session (MySQL's autocommit off meanwhile), so a lost sandbox is never written into; rolled back when the agent goes or after ten idle minutes; the agent log marks its calls, and `s` there puts one sandbox's writes in a tab as a script

### Fixed

- MySQL 8 logins without TLS work when the server hasn't cached the login (after a restart): mysql-rsa on, with the patched sqlx-mysql doing RSA-OAEP through aws-lc-rs and reading the server's key with rustls-pki-types, so the `rsa` crate (RUSTSEC-2023-0071) isn't built
- build.rs reads CARGO_MANIFEST_DIR as it runs, not baked in when it's built: a moved checkout no longer fails with "src/db: No such file or directory"

### Documentation

- Agents: the sandbox access level; manual tests for it

## [0.7.0] — 2026-10-24

### Added

- Queries in several tabs at once: each tab has its own query state (Run in TabState), so tabs on different connections run together; events find their tab by a run id unique across tabs; each running tab's timeout and long-query notice tick while hidden; a tab whose connection runs another tab's query waits and says which tab holds it
- ask: a model may look for more tables itself (find_tables, describe_table: names, columns, keys and comments, never rows; two rounds, eight tables at most; its SQL still checked), said with the answer; `tools` per model, on for providers' models, off on this machine; tool calls in both APIs' formats. Memory: `also called` and `defined as` facts, typed as such, explained to the model and suggested by it; facts match a question's words by their stems
- Sandbox, a fourth write mode (Ctrl+K x s): writes build on each other in one transaction kept open, a savepoint before each; a failed step rolls back alone; Ctrl+K x shows the steps and what changed per table, undoes the last, discards, keeps the steps as a script, or runs them for real in manual commit; MySQL refuses DDL there; the footer marks it

### Fixed

- The keychain's wait is said ("answer the macOS keychain's Allow dialog") once a model's key takes over 2 s; settings a newer version wrote to connections.toml are kept when saving, editing a connection too; "needs version X" notes go by the error's code (SQLSTATE 42601/42883, MySQL 1064/1305), whatever the server's language; sqlx-mysql patched (patches/) so a user without a password logs in where the server switches its auth plugin (Rfam's public server); the library looks each saved query up once per frame

### Documentation

- Development: the Homebrew tap's HOMEBREW_TAP_TOKEN secret and the formula by hand, retries for jobs without a runner
- Queries in several tabs at once: what runs together, what waits (one connection, one query), Esc per tab; Runs and tabs in the architecture
- ask's lookups and the `tools` setting; memory's other names and defined terms; manual tests for both
- The sandbox: its keys, per engine, the connection setting; manual tests for it, queries in several tabs, ask's lookups, memory's terms and the keychain's wait

### Development

- Retry: jobs that never got a runner (macOS arm64 capacity) are run again up to three times; a real failure still once

## [0.6.0] — 2026-10-23

### Added

- Earlier results kept, selectable messages, export and menu labels
- The gateway process: a private socket, JSON-RPC, started on first use, stopped when idle
- The gateway talks to AI models: asking in plain language, the project's memory, agents through it
- Statement zoom and selection, rows opened in apps, plan selection, reconnecting after failures
- The app's words in English and Lithuanian (src/lang, t!/tn!, the language setting); a settings screen (Ctrl+K ,) saving into config.toml with its comments kept; the tab list (Ctrl+K g); Format SQL formats the statement at the cursor; database engines chosen at build time (Cargo features); zbus advisories allowed until secret-service moves to zbus 6
- The catalog knows PostgreSQL partitions: each names its parent (pg_inherits, a partitioned parent; any version), and a partition is never taken for a view
- gateway key reads the key piped in when standard input isn't a terminal, and says so when nothing comes
- Klaidos 3: a long query says how to cancel it (long_query) and query_timeout cancels on connections without their own; Ctrl+K g shows each tab's connection and what runs there; a syntax error from a feature the server's version lacks says so (MySQL, PostgreSQL); Ctrl+K y has a model explain the statement; leader_menu and Ctrl+Shift+K for the Ctrl+K menu; save the statement to the library; closing tabs (middle click, menu, the last one's text gone for good); the library's dates (@today, @7d, @2026-10, @a..b), times and preview, and the mouse in it

### Fixed

- Agent rules per engine, stopping only this tab's query, lost connections rechecked, tabs that fit
- Tab row measured in screen columns from its real spans: the active tab and the + stay in sight; plan clicks follow the current scroll
- An address opened before any project goes into a "default" one, so its tabs are kept; another tab's running query names its connection by its short name
- MySQL: a run stopped by the timeout says so; Shift+F7 runs EXPLAIN ANALYZE only where MySQL executes it. Tests: shared ones over every engine from each engine's test hooks (live_params, own_live_db, TEST_SQL), file_engine() for fixtures, engine facts in their folders
- Closing the terminal saves the tabs and quits as Ctrl+Q; memory learn reads migrations by the project's engine; SQLite row counts from ANALYZE for the heavy-query guard (a plain LIMIT read isn't a full scan); comparisons and new connections use the short name; run buttons in screen columns
- A query past the connection's timeout gets its own hint, not the one for a server that doesn't answer
- M selects the new rows (sorted or filtered too; pressed while a page comes, its rows follow); a script's result set shows and links to its own statement; Esc leaves a zoom before it cancels the query; Ctrl+K d always asks; the navigator filter ranks the best matches first; dragging past the editor's edge scrolls on; ‹N/›N open the tab list; rows as an ASCII table (+--+): export, copy, open, [viewers] ascii
- What a model is shown: words match by stem (rented → rental), partitions are their top table with its keys and rows, a table between a chosen one and a neighbour comes first, a fact matching the question brings the tables it names (memory read once); the name check takes a date function's unit (TIMESTAMPDIFF(SECOND, …)) for no column; the left-out count says its limits
- A user who may not read pg_inherits (EBI's public RNAcentral reader) gets the tables listed without partitions' parents, not a failed table list

### Changed

- Engines found from their folders: build.rs, and every engine's own code in its folder
- Engine facts outside db asked of the engines
- Each engine's own Database tests in its folder (db/<engine>/db_tests.rs); the shared ones and helpers stay in db/tests.rs
- Agents' rules tested over every engine from their own constraints; each engine's own statements tested in its agent_constraints.rs
- Top-level files in folders (sql, agents, transfer, config, system, cli); agents split into rules, log and approvals; app/query in files by job; App's run state as Run and the agent view's as Agents; Hits in its own file; app and UI tests by topic; command-line tests from outside (tests/cli.rs)
- App in parts: the shown connection as Connection (parked and swapped whole, like tabs), the projects and connections screen as Manager, the layout as Panes, what's said as Notes

### Dependencies

- rustls-webpki 0.103.16, webpki-roots and webpki-root-certs 1.0.10 (RUSTSEC-2026-0381 to 0383)
- ctutils 0.4.5 (RUSTSEC-2026-0385), tokio 1.54.1 (RUSTSEC-2026-0386)

### Documentation

- Earlier results, selectable messages, export and menu labels
- The gateway: what it's for, its commands, gateway_idle and its files
- AI models: asking, memory, the gateway's models and context, agents through it
- Statement zoom, plan selection, viewers, following keys, dropped connections, the gateway's fixes
- Esc in its own tab, agent rules per engine, lost connections rechecked, tabs that fit
- Architecture: engines found from their folders by build.rs, the dispatch macros, adding an engine
- Opened files and the pager, development links, manual testing: tabs and the other tab's running query
- The default project for a first address, where tests go, the keychain with PODELIS_CONFIG_DIR
- Architecture: where agents' rules are tested, per engine and shared
- Engine test hooks, test databases, Shift+F7 on MySQL
- The default project and saved URL connections, SQLite row counts and the guard
- The new layout, where tests go, CSV is RFC 4180 (CRLF)
- Architecture: App's parts
- Translations, the settings screen, Ctrl+K g and Ctrl+K ,, building with fewer engines, the language setting
- M and new rows, a result set's statement, Esc and zooms, disconnect asking, filter ranking, drag scrolling, the tab list from ‹N/›N, the ASCII table
- Development: the release made first, uploads retried, retry.yml
- gateway key piped in; what a model is shown (stems, partitions, facts, limits); the context in the field's terms: schema linking, its sources, and what it doesn't do
- long queries and query_timeout, version notes, AI explain, leader_menu and Ctrl+Shift+K, closing tabs, saving a statement, the library's dates and preview, the tab list's status; manual tests for them

### Development

- Pin the Linux runners to Ubuntu 24.04 and move to actions/checkout v5
- Release: the release made once before the builds, uploads with gh and a few tries (no softprops, no Node 20); a failed Release or Platforms run retries its failed jobs once

## [0.5.3] — 2026-10-14

### Added

- Copy and paste: Ctrl+C copies on the query screen, Ctrl+V pastes in the editor, clipboard items in the editor menu
- Use other tabs while a read runs, plans at once during a run, more ways to copy

### Fixed

- Runs show their tab at once, F7 waits for a running query, each pane has its own footer

### Dependencies

- rustls 0.23.46 (RUSTSEC-2026-0346 to 0349)

### Documentation

- Copying and pasting with Ctrl+C / Ctrl+V, the editor's clipboard menu
- Run tabs, plans queued during a run, each pane's footer
- Tabs during a run, plans made at once, copying names and rows, vim's clipboard

## [0.5.2] — 2026-10-14

### Added

- Approve agents' writes, a production guard, watch mode, snapshots, charts, related tables

### Documentation

- Approving agents' writes, the production guard, watch mode, snapshots, charts, related tables

## [0.5.1] — 2026-10-13

### Added

- Agent activity: each agent session, and a note when one needs a look

### Documentation

- Agent activity and naming agents

## [0.5.0] — 2026-10-13

### Added

- Import: the separator from the file, decimal commas, .gz files
- Comparing schemas across engines
- Export all rows of a big result
- Copy through the system clipboard
- An "AI agents" setting on connections
- podelis mcp: database access for AI agents, and reviewing it

### Fixed

- PostgreSQL column types keep their lengths
- Formatting SQL no longer changes strings
- SQLite parameters: numbered and named ones
- SQLite connections get their query timeout
- Passwords stay out of the query history

### Changed

- One timed run for -e and agents

### Documentation

- Separators, .gz imports and comparing across engines
- Export all rows; formatting keeps strings
- SQLite's parameters and query timeout
- Copying and what the history leaves out
- Podėlis for AI agents

### Development

- Each live MySQL test has a database of its own

## [0.4.0] — 2026-10-13

### Added

- The editor's footer says which statement F6 runs
- Esc cancels SQLite queries
- Import: progress on a terminal, timestamps with time zones

### Fixed

- Import: TSV files as written, COPY on PostgreSQL, more column types
- Binary values in hex instead of their size

### Documentation

- Binary values in hex, imports, the statement position
- SQLite cancel, import progress and time zones

### Development

- The badge shows a fixed version in tests
- A release updates the Homebrew tap

## [0.3.0] — 2026-10-12

### Added

- Navigator paging, sorting and definitions; plans as grids; vertical results; TLS fallback

### Documentation

- Install with Homebrew
- Start CI by hand; moved tags re-run the release

### Development

- Container picker test waits for the password event
- Run every check on Debian in Docker
- CI can be started by hand
- Rust 1.99 for development, 1.94 minimum to build
- Snapshots show v0.3.0

## [0.2.0] — 2026-10-11

### Fixed

- Hide the editor's cursor under popups
- Release: install cargo-about where there's no prebuilt one

### Documentation

- Split the README into pages; re-record the demo

### Development

- Live tests work with CI's servers


