#!/usr/bin/env bash
# Install xbox-dongle as a per-user launchd agent that starts at login and
# drives the adapter whenever it is plugged in.
#
# Installs:
#   ~/Library/Application Support/xbox-dongle/xbox-dongle   (signed binary)
#   ~/Library/Application Support/xbox-dongle/firmware/     (firmware files)
#   ~/Library/LaunchAgents/com.github.xbox-dongle.plist
# Logs go to ~/Library/Logs/xbox-dongle.log
set -euo pipefail

label="com.github.xbox-dongle"
root="$(cd "$(dirname "$0")/.." && pwd)"
dest="$HOME/Library/Application Support/xbox-dongle"
plist="$HOME/Library/LaunchAgents/$label.plist"
log="$HOME/Library/Logs/xbox-dongle.log"

shopt -s nullglob
fw=("$root"/firmware/xbox_dongle_*.bin)
if [ ${#fw[@]} -eq 0 ]; then
  echo "no firmware in $root/firmware, run scripts/fetch-firmware.sh <pid> first" >&2
  exit 1
fi

"$root/scripts/build-signed.sh" release

# Stop a previous instance before replacing the binary.
launchctl bootout "gui/$(id -u)/$label" 2>/dev/null || true

mkdir -p "$dest/firmware" "$HOME/Library/LaunchAgents" "$HOME/Library/Logs"
cp "$root/target/release/xbox-dongle" "$dest/xbox-dongle"
cp "${fw[@]}" "$dest/firmware/"

cat > "$plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>Label</key>
	<string>$label</string>
	<key>ProgramArguments</key>
	<array>
		<string>$dest/xbox-dongle</string>
		<string>serve</string>
		<string>$dest/firmware</string>
	</array>
	<key>RunAtLoad</key>
	<true/>
	<key>KeepAlive</key>
	<true/>
	<key>ProcessType</key>
	<string>Interactive</string>
	<key>StandardOutPath</key>
	<string>$log</string>
	<key>StandardErrorPath</key>
	<string>$log</string>
</dict>
</plist>
EOF

launchctl bootstrap "gui/$(id -u)" "$plist"
echo "installed and started $label"
echo "logs: tail -f \"$log\""
