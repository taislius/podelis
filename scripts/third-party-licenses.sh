#!/bin/sh
# Writes THIRD-PARTY-LICENSES.txt: the licences and notices of every crate
# in the release build. Needs cargo-about (cargo install cargo-about
# --features cli). The release workflow runs it and ships the file.
set -eu
cd "$(dirname "$0")/.."
cargo about generate --fail about.hbs > THIRD-PARTY-LICENSES.txt
echo "Wrote THIRD-PARTY-LICENSES.txt"
