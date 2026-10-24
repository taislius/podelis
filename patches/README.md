# Patched dependencies

## sqlx-mysql 0.9.0

A copy of the published crate with two changes. **A user without a
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

`Cargo.toml` points `[patch.crates-io]` here. Remove the folder and the
patch once a sqlx-mysql release has both fixes (`cargo update -p
sqlx-mysql` shows a newer version), then check that Rfam still connects,
and a MySQL 8 connection with SSL off right after the server restarts.
