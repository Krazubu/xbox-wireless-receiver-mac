#!/usr/bin/env bash
# Download the Xbox Wireless Adapter firmware from the Microsoft Update Catalog
# and extract it to firmware/xbox_dongle_<pid>.bin.
# The firmware is Microsoft's and is not redistributed with this project:
# https://www.microsoft.com/en-us/legal/terms-of-use
# URLs and checksums from xone (install/firmware.sh).
set -euo pipefail

pid="${1:-}"
case "$pid" in
  02e6) url='https://catalog.s.download.windowsupdate.com/d/msdownload/update/driver/drvs/2017/03/2ea9591b-f751-442c-80ce-8f4692cdc67b_6b555a3a288153cf04aec6e03cba360afe2fce34.cab'
        sha='080ce4091e53a4ef3e5fe29939f51fd91f46d6a88be6d67eb6e99a5723b3a223'; file='FW_ACC_00U.bin' ;;
  02fe) url='https://catalog.s.download.windowsupdate.com/c/msdownload/update/driver/drvs/2017/07/1cd6a87c-623f-4407-a52d-c31be49e925c_e19f60808bdcbfbd3c3df6be3e71ffc52e43261e.cab'
        sha='48084d9fa53b9bb04358f3bb127b7495dc8f7bb0b3ca1437bd24ef2b6eabdf66'; file='FW_ACC_00U.bin' ;;
  02f9) url='https://catalog.s.download.windowsupdate.com/c/msdownload/update/driver/drvs/2017/06/1dbd7cb4-53bc-4857-a5b0-5955c8acaf71_9081931e7d664429a93ffda0db41b7545b7ac257.cab'
        sha='0023a7bae02974834500c665a281e25b1ba52c9226c84989f9084fa5ce591d9b'; file='FW_ACC_CL.bin' ;;
  091e) url='https://catalog.s.download.windowsupdate.com/d/msdownload/update/driver/drvs/2017/08/aeff215c-3bc4-4d36-a3ea-e14bfa8fa9d2_e58550c4f74a27e51e5cb6868b10ff633fa77164.cab'
        sha='e2710daf81e7b36d35985348f68a81d18bc537a2b0c508ffdfde6ac3eae1bad7'; file='FW_ACC_BR.bin' ;;
  *) echo "usage: $0 <02e6|02fe|02f9|091e> [output dir]  (product id from 'xbox-dongle list')" >&2; exit 1 ;;
esac

root="$(cd "$(dirname "$0")/.." && pwd)"
dir="${2:-$root/firmware}"   # optional output directory
out="$dir/xbox_dongle_$pid.bin"
mkdir -p "$dir"
tmp="$(mktemp -d)"; trap 'rm -rf "$tmp"' EXIT

curl -fsSL -o "$tmp/driver.cab" "$url"
tar -xf "$tmp/driver.cab" -C "$tmp" "$file"   # macOS tar (bsdtar) reads CAB
echo "$sha  $tmp/$file" | shasum -a 256 -c --quiet
mv "$tmp/$file" "$out"
echo "firmware written to $out"
