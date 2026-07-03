#!/usr/bin/env sh
# Run the LAN host (mock encoder until M2 Windows capture lands).
set -e
cd "$(dirname "$0")/.."
echo "Starting StreamBeam host on 0.0.0.0:47989 (mock H.264 + Opus)…"
echo "Scan the QR with the Android app Watch tab."
echo
exec cargo run -p streambeam-host --release -- "$@"
