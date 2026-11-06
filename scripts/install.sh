#!/bin/bash
# Installs DockerPlus and the `dockerplus` command. Run it again to update them.
set -euo pipefail
cd "$(dirname "$0")/.."

RUNNING='DockerPlus.app/Contents/MacOS'

fail() {
    echo "$1" >&2
    exit 1
}

[ "$(sw_vers -productVersion | cut -d. -f1)" -ge 26 ] || fail "DockerPlus needs macOS 26 or later."
command -v brew >/dev/null || fail "Homebrew is needed first: https://brew.sh"
xcode-select -p >/dev/null 2>&1 || fail "The Xcode Command Line Tools are needed first: xcode-select --install"

# Only what is missing, so a docker from Docker Desktop or OrbStack stays in charge.
missing=()
command -v cargo >/dev/null || missing+=(rust)
command -v colima >/dev/null || missing+=(colima)
command -v docker >/dev/null || missing+=(docker)
command -v docker-compose >/dev/null || docker compose version >/dev/null 2>&1 || missing+=(docker-compose)
if [ "${#missing[@]}" -gt 0 ]; then
    brew install "${missing[@]}"
fi

scripts/bundle.sh

apps=/Applications
if [ ! -w "$apps" ]; then
    apps=$HOME/Applications
    mkdir -p "$apps"
fi

# Copied beside the old app first, so a full disk or Ctrl-C leaves that one in place.
new="$apps/.DockerPlus.app.new"
old="$apps/.DockerPlus.app.old"
trap 'rm -rf "$new"' EXIT
rm -rf "$new" "$old"
ditto target/DockerPlus.app "$new"

# Left to the user. Asking the app to quit raises an Automation prompt in
# Terminal, and killing it could cut off a Colima start it is running.
if pgrep -qf "$RUNNING"; then
    echo "Quit DockerPlus to finish installing. Colima keeps running."
    while pgrep -qf "$RUNNING"; do
        sleep 0.25
    done
fi

if [ -e "$apps/DockerPlus.app" ]; then
    mv "$apps/DockerPlus.app" "$old"
fi
mv "$new" "$apps/DockerPlus.app"
rm -rf "$old"

# Homebrew's bin folder is already on PATH. The link points into the app, so the
# command keeps working if this source folder moves or goes away.
ln -sf "$apps/DockerPlus.app/Contents/Resources/dockerplus" "$(brew --prefix)/bin/dockerplus"

# "Check for Updates…" pulls from here. Kept in defaults: changing a file inside
# the signed bundle would break its ad-hoc signature.
defaults write local.dockerplus.app DPSourceDirectory "$(pwd -P)"

open "$apps/DockerPlus.app"
echo "Installed $apps/DockerPlus.app and the dockerplus command."
