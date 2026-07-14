#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PORT="${PORT:-8787}"
UNSAFE_CHROME="${UNSAFE_CHROME:-0}"
if [[ "${1:-}" == "--unsafe-chrome" ]]; then
  UNSAFE_CHROME=1
fi

cd "$ROOT"

wasm-pack build crates/pi-browser --target web --out-dir ../../pkg/pi_browser

if [[ "$UNSAFE_CHROME" != "1" ]]; then
  node scripts/dev-server.mjs
  exit 0
fi

PROFILE_DIR="${TMPDIR:-/tmp}/pi-wasm-unsafe-chrome"
CHROME_BIN="${CHROME_BIN:-/Applications/Google Chrome.app/Contents/MacOS/Google Chrome}"

echo "Starting unsafe Chrome with web security disabled for local CORS experiments."
echo "Profile: $PROFILE_DIR"
echo "Do not use this browser profile for normal browsing."

node scripts/dev-server.mjs &
SERVER_PID="$!"
trap 'kill "$SERVER_PID" 2>/dev/null || true' EXIT

"$CHROME_BIN" \
  --user-data-dir="$PROFILE_DIR" \
  --disable-web-security \
  --disable-features=BlockInsecurePrivateNetworkRequests,PrivateNetworkAccessSendPreflights \
  "http://127.0.0.1:${PORT}/web/" &

wait "$SERVER_PID"
