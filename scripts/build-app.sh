#!/usr/bin/env bash
# Build "Xbox Dongle.app": the menu bar app with the driver bundled inside.
#
#   build/Xbox Dongle.app/Contents/
#     MacOS/XboxDongle        menu bar app (Swift)
#     MacOS/xbox-dongle       driver, signed with the virtual HID entitlement
#     Resources/fetch-firmware.sh
#     Info.plist
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"
version="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)"
app="build/Xbox Dongle.app"
contents="$app/Contents"

cargo build --release

rm -rf "$app"
mkdir -p "$contents/MacOS" "$contents/Resources"

swiftc -O -swift-version 5 -target "$(uname -m)-apple-macos13" \
  -o "$contents/MacOS/XboxDongle" app/XboxDongleApp.swift

cp target/release/xbox-dongle "$contents/MacOS/xbox-dongle"
cp scripts/fetch-firmware.sh "$contents/Resources/fetch-firmware.sh"

cat > "$contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>CFBundleIdentifier</key>
	<string>com.github.xbox-dongle.app</string>
	<key>CFBundleName</key>
	<string>Xbox Dongle</string>
	<key>CFBundleDisplayName</key>
	<string>Xbox Dongle</string>
	<key>CFBundleExecutable</key>
	<string>XboxDongle</string>
	<key>CFBundlePackageType</key>
	<string>APPL</string>
	<key>CFBundleShortVersionString</key>
	<string>$version</string>
	<key>CFBundleVersion</key>
	<string>$version</string>
	<key>LSMinimumSystemVersion</key>
	<string>13.0</string>
	<key>LSUIElement</key>
	<true/>
	<key>NSHumanReadableCopyright</key>
	<string>GPL-2.0-or-later</string>
</dict>
</plist>
EOF

# Inner code first (keeps its entitlement), then the bundle itself.
codesign --force --sign - --entitlements xbox-dongle.entitlements "$contents/MacOS/xbox-dongle"
codesign --force --sign - "$app"
codesign --verify --strict "$app"
echo "built $app"
