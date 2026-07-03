#!/usr/bin/env sh
# One-command macOS visual test: fakehost + ffplay video + audio.
set -e
cd "$(dirname "$0")/.."
if ! command -v ffplay >/dev/null 2>&1; then
  echo "ffplay not found — install ffmpeg: brew install ffmpeg" >&2
  exit 1
fi
echo "Starting StreamBeam visual test…"
echo "  • A window titled \"StreamBeam 640x360\" should appear."
echo "  • If not, try: STREAMBEAM_FFPLAY_VERBOSE=1 $0"
echo "  • Or use mpv: STREAMBEAM_PLAYER=mpv $0"
echo "  • Video-only debug: cargo run -p streambeam-devtools --bin playclient -- --no-audio 'sbm1:…'"
echo
exec cargo run -p streambeam-devtools --bin streamdemo
