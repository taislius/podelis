#!/bin/sh
# Checks what a change touches, not everything (tier 1 of the checks: see
# docs/development.md). From the files changed since <base> (the merge
# base with origin/master, by default, and what isn't committed), it picks
# the tests to run, by the map in `scope_of`, and runs them with the
# workspace's own commands, so there is one build: clippy, and the tests
# whose names match. It says what it ran and what it left out.
#
#   scripts/check-changed.sh [<base>]   check the change
#   scripts/check-changed.sh --dry-run [<base>]   say what it would run
#   scripts/check-changed.sh --paths <path>…   the same, for these paths
#   scripts/check-changed.sh --full     the whole check (scripts/check.sh)
#   scripts/check-changed.sh --self-test   check the map itself
#
# Live database tests run where the PODELIS_TEST_* variables are set, as
# for scripts/check.sh.
set -eu
cd "$(dirname "$0")/.."

# What a path needs, as words: `skip`, `shell`, `deps`, `all` (every test
# of the default build), `feature:<name>`, or a test filter (a module
# path, as the tests' names start).
scope_of() {
    case "$1" in
        *.md | docs/* | LICENSE* | packaging/* | docker/* | .dockerignore) echo skip ;;
        scripts/*.sh | .githooks/*) echo shell ;;
        Cargo.toml | Cargo.lock | */Cargo.toml | tools/deny.toml) echo deps all ;;
        build.rs | */build.rs | */engine.toml | rust-toolchain.toml | clippy.toml | rustfmt.toml) echo all ;;
        .github/*) echo skip ;;
        crates/podelis-core/src/db/duckdb/* | crates/podelis-core/src/db/mongodb/*)
            engine=${1#crates/podelis-core/src/db/}
            engine=${engine%%/*}
            echo "feature:$engine" "db::$engine::"
            ;;
        crates/podelis-core/src/db/*/*)
            engine=${1#crates/podelis-core/src/db/}
            echo "db::${engine%%/*}::"
            ;;
        crates/podelis-core/src/db/*) echo db:: agents:: ;;
        crates/podelis-core/src/sql/*) echo sql:: app::tests::editor app::tests::running app::tests::results ;;
        crates/podelis-core/src/lang/*) echo all ;;
        crates/podelis-core/*) echo all ;;
        crates/podelis-ai/*) echo package:podelis-ai ;;
        src/agents/*) echo agents:: ;;
        src/transfer/*) echo transfer:: ;;
        src/gateway/*) echo gateway:: ;;
        src/app/* | src/ui/*) echo app:: ui:: ;;
        src/* | tests/*) echo package:podelis ;;
        *) echo all ;;
    esac
}

# The words for every path given, each once, in the order first seen.
scopes() {
    for path in "$@"; do scope_of "$path"; done | tr ' ' '\n' | awk 'NF && !seen[$0]++'
}

# The files changed since $1 (and those not committed, and new ones).
changed() {
    git diff --name-only "$1"
    git ls-files --others --exclude-standard
}

self_test() {
    expect() {
        got=$(scopes "$1" | tr '\n' ' ')
        if [ "$got" != "$2 " ]; then
            echo "self-test: $1 gives '$got', not '$2'" >&2
            exit 1
        fi
    }
    expect README.md "skip"
    expect scripts/check.sh "shell"
    expect Cargo.lock "deps all"
    expect crates/podelis-core/src/db/postgres/queries.rs "db::postgres::"
    expect crates/podelis-core/src/db/mongodb/driver.rs "feature:mongodb db::mongodb::"
    expect crates/podelis-core/src/db/run.rs "db:: agents::"
    expect crates/podelis-core/src/sql/lint.rs "sql:: app::tests::editor app::tests::running app::tests::results"
    expect crates/podelis-ai/src/names.rs "package:podelis-ai"
    expect src/app/edit.rs "app:: ui::"
    expect src/cli/mod.rs "package:podelis"
    expect crates/podelis-core/src/db/duckdb/engine.toml "all"
    echo "check-changed: the map is as expected"
}

dry_run=0
case "${1:-}" in
    --full) exec scripts/check.sh ;;
    --self-test) self_test; exit 0 ;;
    --dry-run) dry_run=1; shift ;;
esac
if [ "${1:-}" = --paths ]; then
    shift
    paths=$(printf '%s\n' "$@")
else
    base=${1:-$(git merge-base HEAD origin/master 2>/dev/null || echo HEAD)}
    paths=$(changed "$base" | sort -u)
fi
if [ -z "$paths" ]; then
    echo "check-changed: nothing changed"
    exit 0
fi

# shellcheck disable=SC2086 # the paths are one per line, without blanks
set -- $paths
words=$(scopes "$@")
features="" filters="" packages="" all=0 shell=0 deps=0
for w in $words; do
    case "$w" in
        skip) ;;
        shell) shell=1 ;;
        deps) deps=1 ;;
        all) all=1 ;;
        feature:*) features="$features${features:+,}${w#feature:}" ;;
        package:*) packages="$packages ${w#package:}" ;;
        *) filters="$filters $w" ;;
    esac
done
rust=0
[ "$all" = 1 ] || [ -n "$filters$packages" ] && rust=1

echo "check-changed: $# file(s) changed"
[ "$rust" = 0 ] && [ "$shell" = 0 ] && [ "$deps" = 0 ] && echo "  only docs and CI: nothing to run" && exit 0
flags="--locked"
[ -n "$features" ] && flags="$flags --features $features"
if [ "$rust" = 1 ]; then
    echo "  rustfmt, and clippy of the workspace${features:+ (with $features)}"
    if [ "$all" = 1 ]; then
        echo "  every test of the default build${features:+ and $features}"
    else
        [ -n "$filters" ] && echo "  tests named:$filters"
        [ -n "$packages" ] && echo "  every test of:$packages"
    fi
fi
[ "$deps" = 1 ] && echo "  cargo-deny and cargo-machete"
[ "$shell" = 1 ] && echo "  shellcheck"
echo "  left out: the builds with fewer engines, and the opt-in engines not touched (CI runs them; --full is the local whole)"
env | grep -q '^PODELIS_TEST_' || echo "  no PODELIS_TEST_* set: live database tests skip"
[ "$dry_run" = 1 ] && exit 0

if [ "$rust" = 1 ]; then
    echo "==> rustfmt"
    cargo fmt --check
    echo "==> clippy"
    # shellcheck disable=SC2086 # the flags are words
    cargo clippy --all-targets $flags -- -D warnings
    echo "==> tests"
    if [ "$all" = 1 ]; then
        # shellcheck disable=SC2086
        cargo test $flags
    else
        for package in $packages; do
            # shellcheck disable=SC2086
            cargo test $flags -p "$package"
        done
        if [ -n "$filters" ]; then
            # The workspace's tests whose names start so (libtest takes
            # several filters); the other test binaries find none and pass.
            # shellcheck disable=SC2086
            cargo test $flags -- $filters
        fi
    fi
fi
if [ "$deps" = 1 ]; then
    echo "==> cargo-deny"
    cargo deny --config tools/deny.toml check
    if command -v cargo-machete > /dev/null; then
        echo "==> cargo-machete"
        cargo machete
    fi
fi
if [ "$shell" = 1 ] && command -v shellcheck > /dev/null; then
    echo "==> shellcheck"
    shellcheck -s sh scripts/*.sh .githooks/*
fi
echo "check-changed: passed"
