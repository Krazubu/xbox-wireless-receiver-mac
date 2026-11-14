#!/usr/bin/env bash
# Build and ad-hoc sign the driver with the virtual HID entitlement.
#
# This entitlement is restricted: without an Apple provisioning profile, macOS
# kills the signed binary at launch (exit 137) unless SIP is disabled and AMFI
# is relaxed (boot-arg amfi_get_out_of_my_way=1). See README.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
profile="${1:-release}"
cd "$root"

if [ "$profile" = release ]; then
  cargo build --release
  bin="target/release/xbox-dongle"
else
  cargo build
  bin="target/debug/xbox-dongle"
fi

codesign --force --sign - --entitlements xbox-dongle.entitlements "$bin"
codesign -d --entitlements - "$bin" 2>/dev/null | grep -q hid.virtual.device
echo "signed $bin with com.apple.developer.hid.virtual.device"

if csrutil status | grep -q enabled; then
  echo "warning: SIP is enabled, macOS will kill this binary at launch" >&2
fi
