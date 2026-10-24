#!/bin/sh
# Signs a macOS build of Podėlis with a code-signing identity of your own,
# under a fixed identifier, so the keychain knows every build and update as
# the same program: its Allow dialog ("podelis wants to use … in your
# keychain") then comes once, not after each `cargo build` or update.
#
#   ./scripts/sign-macos.sh                       # target/release/podelis
#   ./scripts/sign-macos.sh path/to/podelis
#   PODELIS_SIGN_IDENTITY="My Cert" ./scripts/sign-macos.sh
#
# The identity: any code-signing certificate in your login keychain. A
# self-signed one will do (Keychain Access › Certificate Assistant › Create
# a Certificate…, name "Podelis Dev", type Code Signing). After the first
# signed build asks, choose Always Allow.
set -eu
cd "$(dirname "$0")/.."

binary=${1:-target/release/podelis}
identity=${PODELIS_SIGN_IDENTITY:-Podelis Dev}

if [ "$(uname)" != Darwin ]; then
    echo "Signing is for macOS builds; nothing to do here." >&2
    exit 0
fi
if [ ! -f "$binary" ]; then
    echo "No $binary: build it first (cargo build --release)." >&2
    exit 1
fi
if ! security find-identity -v -p codesigning | grep -q "\"$identity\""; then
    echo "No code-signing identity \"$identity\" in your keychain." >&2
    echo "Make one: Keychain Access › Certificate Assistant › Create a Certificate…," >&2
    echo "name \"$identity\", identity type Self-Signed Root, certificate type Code Signing." >&2
    echo "(Or name yours in PODELIS_SIGN_IDENTITY.)" >&2
    exit 1
fi
codesign --force --sign "$identity" --identifier lt.taislius.podelis "$binary"
codesign --verify --verbose=1 "$binary"
echo "Signed $binary as \"$identity\"."
