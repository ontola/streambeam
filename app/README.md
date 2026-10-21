# StreamBeam app

The shared Flutter interface for screen sharing, remote control and game streaming on Android, macOS and Windows. A Rust core handles pairing, transport and media; native platform code handles screen capture, video playback and input.

See the [project README](../README.md) for installation and the [platform matrix](../docs/PLATFORMS.md) for implemented features and remaining validation.

## Development

Install Flutter, the Rust version pinned in the repository, and the native SDK for your target. Android needs its SDK/NDK, macOS needs Xcode, and Windows needs the Visual Studio desktop C++ toolchain.

```sh
cd app
flutter pub get
flutter run -d macos      # or -d windows, or a connected Android device
```

The app's device list combines nearby discovery and saved connection details. Select a nearby device and request approval, or scan/paste its pairing link. A desktop app listens in the background; Android capture requires system confirmation each session.

After changing `rust/src/api/`, regenerate the bridge from this directory:

```sh
flutter_rust_bridge_codegen generate
flutter analyze --no-pub
flutter test --no-pub
```

## Native paths

- `rust/src/api/`: Flutter-facing operations and desktop stream events.
- `rust/src/android/`: MediaCodec playback and Android integration.
- `android/app/src/main/`: Android capture, permissions, discovery and input.
- `macos/Runner/`: ScreenCaptureKit capture, VideoToolbox playback and macOS input.
- `windows/runner/`: Windows viewer and desktop integration.

For a repeatable test source, run `cargo run -p streambeam-devtools --bin fakehost` from the repository root. Pair with its printed payload. Never publish pairing payloads or private capture logs.

[Playback debugging](../docs/DEBUGGING_CLIENT.md) · [Release builds](../docs/RELEASING.md) · [Brand assets](../assets/README.md)
