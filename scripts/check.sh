#!/bin/sh
# Runs the same checks as CI. Live database tests run only if the
# PODELIS_TEST_* variables are set (see docs/development.md).
set -eu
cd "$(dirname "$0")/.."

if ! command -v cargo-deny > /dev/null; then
    echo "cargo-deny is required: cargo install cargo-deny --locked" >&2
    exit 1
fi

echo "==> rustfmt"
cargo fmt --check
echo "==> clippy"
cargo clippy --all-targets --locked -- -D warnings
echo "==> tests"
cargo test --locked
# As CI's engines job, by choice (a second build: time and disk): again
# with fewer engines (the README's), so nothing assumes MySQL is there.
if [ "${PODELIS_CHECK_ENGINES:-}" = 1 ]; then
    echo "==> clippy and tests with PostgreSQL and SQLite only"
    cargo clippy --all-targets --locked --no-default-features --features postgres,sqlite -- -D warnings
    cargo test --locked --no-default-features --features postgres,sqlite
fi
echo "==> cargo-deny"
cargo deny check
# The scripts are POSIX sh (Debian's /bin/sh is dash, not bash): checked
# where shellcheck or dash is installed.
if command -v shellcheck > /dev/null; then
    echo "==> shellcheck"
    shellcheck -s sh scripts/*.sh .githooks/*
elif command -v dash > /dev/null; then
    echo "==> dash -n"
    for f in scripts/*.sh .githooks/*; do dash -n "$f"; done
fi
echo "All checks passed."
