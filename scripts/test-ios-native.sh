#!/usr/bin/env bash
# Run the actual iOS decoder against bundled H.264 data on a simulator.
# Optional first argument: an available simulator UDID.
set -euo pipefail
repo_dir="$(cd "$(dirname "$0")/.." && pwd)"
flutter_bin="${FLUTTER_BIN:-flutter}"
export CMAKE_POLICY_VERSION_MINIMUM="${CMAKE_POLICY_VERSION_MINIMUM:-3.5}"
if [[ -n "${1:-}" ]]; then
  simulator_id="$1"
else
  simulator_id="$(xcrun simctl list devices available --json | python3 -c '
import json, sys
for runtime, devices in json.load(sys.stdin)["devices"].items():
    if ".iOS-" in runtime:
        for device in devices:
            if device.get("isAvailable") and device["name"].startswith("iPhone"):
                print(device["udid"])
                sys.exit(0)
sys.exit("No available iOS simulator. Install an iOS runtime in Xcode settings.")
')"
fi
# Fail promptly on runtime/disk problems before compiling a full app.
xcrun simctl bootstatus "$simulator_id" -b
cd "$repo_dir/app"
"$flutter_bin" pub get --enforce-lockfile
"$flutter_bin" build ios --simulator --debug --config-only --no-pub
cd ios
pod install
# Keep the xcresult assertions/logs; skip heavyweight simulator sysdiagnoses,
# which can hang on local Xcode 27 even after every test has passed.
# An explicit architecture avoids Flutter/Xcode 27 treating 'arm64 x86_64'
# as one architecture in the framework preparation script.
xcodebuild -workspace Runner.xcworkspace -scheme Runner -configuration Debug \
  -sdk iphonesimulator -destination "platform=iOS Simulator,id=$simulator_id" \
  -resultBundlePath "${IOS_TEST_RESULT_BUNDLE:-$repo_dir/app/build/ios-native-tests.xcresult}" \
  -parallel-testing-enabled NO -collect-test-diagnostics never \
  ARCHS="$(uname -m)" ONLY_ACTIVE_ARCH=YES CODE_SIGNING_ALLOWED=NO test
