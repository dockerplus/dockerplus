#!/bin/bash
# Builds target/DockerPlus.app. Extra arguments go to cargo, for example:
#   scripts/bundle.sh --features devtools
set -euo pipefail
cd "$(dirname "$0")/.."

cargo build --release "$@"

APP=target/DockerPlus.app
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp target/release/dockerplus-app "$APP/Contents/MacOS/DockerPlus"
cp assets/Info.plist "$APP/Contents/Info.plist"
cp bin/dockerplus "$APP/Contents/Resources/dockerplus"
if [ -f assets/AppIcon.icns ]; then
    cp assets/AppIcon.icns "$APP/Contents/Resources/AppIcon.icns"
fi

# Ad-hoc signature: enough to run here, not to distribute.
codesign --force --sign - --timestamp=none "$APP"

echo "Built $APP ($(du -sh "$APP" | cut -f1))"
