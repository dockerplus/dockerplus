#!/bin/bash
# Builds the zip for a release into target/dist. Pass the tag to check that it
# matches the version in Cargo.toml, which is the one the app reports.
set -euo pipefail
cd "$(dirname "$0")/.."

version=$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -1)
if [ "$#" -gt 0 ] && [ "$1" != "v$version" ]; then
    echo "Tag $1 does not match version $version in Cargo.toml." >&2
    exit 1
fi

scripts/bundle.sh

zip="DockerPlus-$version-arm64.zip"
rm -rf target/dist
mkdir -p target/dist
# Keeps the signature, which a plain `zip` can break.
ditto -c -k --keepParent target/DockerPlus.app "target/dist/$zip"
(cd target/dist && shasum -a 256 "$zip" > "$zip.sha256")
echo "Packaged target/dist/$zip"
