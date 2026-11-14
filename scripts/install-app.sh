#!/usr/bin/env bash
# Build and install "Xbox Dongle.app" into /Applications, replacing the
# launchd agent (only one program can drive the adapter at a time).
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
dest="/Applications/Xbox Dongle.app"
support="$HOME/Library/Application Support/xbox-dongle"

"$root/scripts/build-app.sh"

# Remove the older launchd agent setup if present.
if [ -f "$HOME/Library/LaunchAgents/com.github.xbox-dongle.plist" ]; then
  "$root/scripts/uninstall-agent.sh"
fi

# Quit a running copy of the app.
osascript -e 'tell application id "com.github.xbox-dongle.app" to quit' 2>/dev/null || true
pkill -f "Xbox Dongle.app/Contents/MacOS/" 2>/dev/null || true

rm -rf "$dest"
cp -R "$root/build/Xbox Dongle.app" "$dest"

# Reuse firmware already downloaded in the repository.
mkdir -p "$support/firmware"
shopt -s nullglob
for f in "$root"/firmware/xbox_dongle_*.bin; do cp "$f" "$support/firmware/"; done

open "$dest"
echo "installed $dest"
