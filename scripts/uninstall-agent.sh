#!/usr/bin/env bash
# Stop and remove the xbox-dongle launchd agent installed by install-agent.sh.
set -euo pipefail

label="com.github.xbox-dongle"
launchctl bootout "gui/$(id -u)/$label" 2>/dev/null || true
rm -f "$HOME/Library/LaunchAgents/$label.plist"
rm -rf "$HOME/Library/Application Support/xbox-dongle"
echo "removed $label (log kept at ~/Library/Logs/xbox-dongle.log)"
