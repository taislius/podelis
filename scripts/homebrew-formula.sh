#!/bin/sh
# Prints the Homebrew formula for a release: packaging/homebrew/podelis.rb
# without its comment, with the version and each archive's SHA-256 filled
# in. Takes the version (0.3.0) and a folder holding the release's .sha256
# files. The release workflow runs it to update taislius/homebrew-tap.
set -eu
version=$1
sums=$2
cd "$(dirname "$0")/.."
formula=$(sed '/^#/d' packaging/homebrew/podelis.rb | sed "s/VERSION/$version/g")
for target in aarch64-apple-darwin x86_64-apple-darwin aarch64-unknown-linux-gnu x86_64-unknown-linux-gnu; do
  file="$sums/podelis-v$version-$target.tar.gz.sha256"
  sum=$(cut -d' ' -f1 < "$file")
  case $sum in
    *[!0-9a-f]* | "") echo "no SHA-256 in $file" >&2; exit 1 ;;
  esac
  placeholder=SHA256_$(echo "$target" | tr 'a-z-' 'A-Z_')
  formula=$(printf '%s\n' "$formula" | sed "s/$placeholder/$sum/")
done
printf '%s\n' "$formula"
