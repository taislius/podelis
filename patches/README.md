# Patched dependencies

## sqlx-mysql 0.9.0

A copy of the published crate with one change, in
`src/connection/auth.rs` (`AuthPlugin::scramble`): with no password, the
`mysql_native_password` and `caching_sha2_password` plugins send an empty
response, as the `mysql` client does. 0.9.0 sends a scramble of the empty
string instead, which a server that switches the auth plugin during the
handshake takes for a wrong password ("Access denied … (using password:
YES)"): Rfam's public MySQL server (`mysql-rfam-public.ebi.ac.uk:4497`,
user `rfamro`, no password) is one.

`Cargo.toml` points `[patch.crates-io]` here. Remove the folder and the
patch once a sqlx-mysql release has the fix (`cargo update -p sqlx-mysql`
shows a newer version), then check that Rfam still connects.
