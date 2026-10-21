# iOS viewer

StreamBeam's iOS app is a foreground viewer. It uses the existing Rust
pairing, pinned peer identity, QUIC media transport and input protocol. It does
not advertise itself as a screen-sharing host.

## Implemented path

- Native Bonjour discovery (`_streambeam._udp`) with the Local Network
  permission description, plus QR scanning/camera permission and explicit
  Paste & play.
- The same VideoToolbox H.264 decoder used on macOS, registered as a Flutter
  texture through `streambeam/video_desktop`, including decode-health errors.
- Opus decoded in Rust and played through AVAudioEngine with an iOS playback
  audio session. There is no microphone or background audio capture.
- Existing touch/trackpad and keyboard controls, and saved-host refresh after
  stopping a stream.
- The screen stays awake while a stream plays. Backgrounding ends playback;
  the next connection creates fresh video/audio resources.
- Clipboard reception remains available; automatic clipboard reading is
  disabled on iOS to avoid repeated permission prompts. Pairing paste is an
  explicit action.

The iOS viewer advertises **SDR H.264 only**. AV1, HDR, MetalFX upscaling,
background streaming and ReplayKit hosting are not enabled.

## Validation

Run `scripts/test-ios-native.sh [simulator-UDID]` on a Mac with Flutter,
CocoaPods and an installed iOS simulator runtime. The tests decode the actual
bundled H.264 fixture and require non-uniform BGRA texture pixels, correct
geometry, frame notifications, clean teardown and an actionable bad-config
error. A third native test starts and stops the real AVAudioEngine graph; Rust
unit tests verify PCM conversion and silence after teardown. The script selects one simulator and runs tests serially; it explicitly
sets the host architecture to avoid Flutter's Xcode 27 multi-architecture
framework preparation failure.

Also run `flutter analyze --no-pub` and `flutter test --no-pub` from `app`, and
`flutter build ios --release --no-codesign` for the complete device binary.
Unsigned compilation and fixture decode do not verify real-device LAN
permissions, pairing, audio output or remote input. Those remain a required
acceptance pass before submitting a public store release.

Store certificates and provisioning are handled by the signed-candidate
workflow described in [RELEASING.md](RELEASING.md). No signing credentials are
stored with these sources.
