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

# Quitting the app leaves Colima and its containers running.
if pgrep -qf "$RUNNING"; then
    osascript -e 'tell application "DockerPlus" to quit' >/dev/null 2>&1 || true
    for _ in {1..40}; do
        pgrep -qf "$RUNNING" || break
        sleep 0.25
    done
    if pgrep -qf "$RUNNING"; then
        fail "DockerPlus is still open. Quit it, then run this again."
    fi
fi

apps=/Applications
if [ ! -w "$apps" ]; then
    apps=$HOME/Applications
    mkdir -p "$apps"
fi
rm -rf "$apps/DockerPlus.app"
ditto target/DockerPlus.app "$apps/DockerPlus.app"

# Homebrew's bin folder is already on PATH. The link points into the app, so the
# command keeps working if this source folder moves or goes away.
ln -sf "$apps/DockerPlus.app/Contents/Resources/dockerplus" "$(brew --prefix)/bin/dockerplus"

open "$apps/DockerPlus.app"
echo "Installed $apps/DockerPlus.app and the dockerplus command."
