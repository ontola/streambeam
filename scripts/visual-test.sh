#!/usr/bin/env sh
# One-command macOS visual test: fakehost + ffplay video + audio.
set -e
cd "$(dirname "$0")/.."
if ! command -v ffplay >/dev/null 2>&1; then
  echo "ffplay not found — install ffmpeg: brew install ffmpeg" >&2
  exit 1
fi
exec cargo run -p streambeam-devtools --bin streamdemo
