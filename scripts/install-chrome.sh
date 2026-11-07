#!/usr/bin/env bash
# Download Chrome for Testing (stable) into ~/.cache/veil-browser/.
# VEIL Browser CLI auto-discovers it there; or set VEIL_BROWSER_CHROME
# to any Chrome/Chromium binary instead.
set -euo pipefail
DEST="${VEIL_BROWSER_HOME:-$HOME/.cache/veil-browser}"
mkdir -p "$DEST"
cd "$DEST"
VER=$(curl -s https://googlechromelabs.github.io/chrome-for-testing/last-known-good-versions.json \
  | python3 -c "import json,sys; print(json.load(sys.stdin)['channels']['Stable']['version'])")
echo "Chrome for Testing stable: $VER"
URL="https://storage.googleapis.com/chrome-for-testing-public/$VER/linux64/chrome-linux64.zip"
curl -L -o chrome-linux64.zip "$URL"
unzip -q -o chrome-linux64.zip
rm chrome-linux64.zip
./chrome-linux64/chrome --version
echo "Installed to $DEST/chrome-linux64/chrome"
