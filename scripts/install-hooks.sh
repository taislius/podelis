#!/bin/sh
# Points git at the repo's hooks (.githooks/), e.g. the pre-push check.
set -eu
cd "$(dirname "$0")/.."
git config core.hooksPath .githooks
echo "Git hooks installed: pre-push runs scripts/check.sh (skip once with --no-verify)."
