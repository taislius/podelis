# Patched dependencies

## sqlx-mysql 0.9.0

A copy of the published crate with three changes. **A user without a
password**, in
`src/connection/auth.rs` (`AuthPlugin::scramble`): with no password, the
`mysql_native_password` and `caching_sha2_password` plugins send an empty
response, as the `mysql` client does. 0.9.0 sends a scramble of the empty
string instead, which a server that switches the auth plugin during the
handshake takes for a wrong password ("Access denied … (using password:
YES)"): Rfam's public MySQL server (`mysql-rfam-public.ebi.ac.uk:4497`,
user `rfamro`, no password) is one.

**And RSA through aws-lc-rs.** Without TLS, MySQL 8's default login
(`caching_sha2_password`) sends the password RSA-encrypted with the
server's public key whenever the server hasn't cached the user's login
(after a restart, say). sqlx does that with the `rsa` crate, behind its
`mysql-rsa` feature, which has an advisory with no fix (RUSTSEC-2023-0071,
the Marvin attack). The `rsa` backend in `src/connection/auth.rs` here uses
aws-lc-rs instead (RSA-OAEP with SHA-1, as before), and reads the server's
PEM key with rustls-pki-types; Podėlis already has both through rustls, so
the `rsa` crate isn't built at all. Its test
(`cargo test -p sqlx-mysql --lib rsa_backend`) encrypts with a fresh key's
PEM and decrypts it back; check.sh doesn't run it.

Upstream: the empty-password fix is merged (transact-rs/sqlx#4493, for
our report #4482, 2026-10-31) but not yet released; aws-lc-rs instead of
`rsa` is still an open request (#4082). When a release has the first, the
copy here needs only the second.

**And text with a binary collation named as text.** The server sets a
column's BINARY flag for a `_bin` collation's text too (`utf8mb4_bin`, a
case-sensitive column; MariaDB's JSON, which is `LONGTEXT` in
`utf8mb4_bin`), and `MySqlTypeInfo::name` (`src/type_info.rs`) named such
a column BLOB or VARBINARY from the flag alone: Podėlis showed its text
as hex (`0x62696E…`). The name now takes the flag only where the
collation is `binary` itself (63). Found by
`live_mysql_shows_every_type_as_the_server_writes_it`.

Its `Cargo.toml` tells cargo-machete to skip `rand` and `generic-array`,
upstream's dependencies that this copy no longer uses (its RSA is
aws-lc-rs's); they're left in to keep the copy to its changes.

`Cargo.toml` points `[patch.crates-io]` here. Remove the folder and the
patch once a sqlx-mysql release has all three (`cargo update -p
sqlx-mysql` shows a newer version), then check that Rfam still connects,
and a MySQL 8 connection with SSL off right after the server restarts.

## sqlx-postgres 0.9.0

A copy of the published crate with one change and a lint: **notices
kept for the client**. sqlx 0.9.0 only logs a server's notices
(`RAISE NOTICE`, a DDL's `NOTICE`, warnings) to `tracing`; there's no way
to get them. In `src/connection/stream.rs` (`recv`'s `NoticeResponse`)
each is also kept on the stream as "SEVERITY: message" (its detail after
it), the newest 200 and each at most 2000 characters, so a server sending
them without end can't fill the memory (see podelis-planai's
hostile-servers.md); `PgConnection::take_notices` (`src/connection/mod.rs`)
hands them over. Podėlis takes them after each run
(`postgres::driver::take_notices`) and shows them in the messages.

The lint: `PgNumeric::size_hint` (`src/types/numeric.rs`) is used only
with the `bigdecimal` feature, which Podėlis doesn't build; as a path
dependency its dead-code warning isn't capped as a registry crate's is,
so it's allowed when that feature is off.

Upstream: notices have been asked for (a notice handler, as libpq's
`PQsetNoticeReceiver`), not added. Remove the folder and its line in
`[patch.crates-io]` if a release gives them some way; until then, a new
sqlx-postgres version means copying it again and making the change again
(search for "Podėlis" in the copy).
