#!/usr/bin/env bash
# Build an optimized app for this Mac, including with Xcode 27.
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$repo_root/app"

flutter_bin="${FLUTTER_BIN:-}"
if [[ -z "$flutter_bin" ]]; then
  flutter_bin="$(command -v flutter || true)"
fi
if [[ -z "$flutter_bin" && -f macos/Flutter/ephemeral/Flutter-Generated.xcconfig ]]; then
  flutter_root="$(sed -n 's/^FLUTTER_ROOT=//p' macos/Flutter/ephemeral/Flutter-Generated.xcconfig | head -n 1)"
  flutter_bin="$flutter_root/bin/flutter"
fi
if [[ ! -x "$flutter_bin" ]]; then
  echo 'Set FLUTTER_BIN to your Flutter executable, or add flutter to PATH.' >&2
  exit 1
fi

"$flutter_bin" build macos --release --no-pub --config-only
cd macos
# Xcode 27 rejects the old Pods deployment target. Its lipo also rejects
# Flutter's multi-architecture verification, so build only the local CPU.
xcodebuild -workspace Runner.xcworkspace -scheme Runner \
  -configuration Release -derivedDataPath ../build/macos \
  -destination "platform=macOS,arch=$(uname -m)" \
  "ARCHS=$(uname -m)" ONLY_ACTIVE_ARCH=YES \
  MACOSX_DEPLOYMENT_TARGET=12.0 build

echo "Built: $repo_root/app/build/macos/Build/Products/Release/streambeam.app"
