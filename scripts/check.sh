#!/bin/sh
# Runs the same checks as CI. Live database tests run only if the
# PODELIS_TEST_* variables are set (see README → Development).
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
echo "==> cargo-deny"
cargo deny check
echo "All checks passed."
