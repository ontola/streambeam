# Platform support

StreamBeam has three roles: **share** this device's screen, **watch** another screen, and **control** the device being watched. The same app can perform both screen-sharing roles where implemented.

This matrix describes implementation, not a guarantee that every device combination has passed live testing. See [PROGRESS](PROGRESS.md) for dated evidence and open work.

## Video

| Platform | Share | Watch | Current implementation |
|---|---|---|---|
| Windows | Implemented | Implemented | WGC/DXGI capture; direct NVIDIA NVENC H.264/AV1 with software H.264 fallback. Viewer uses Media Foundation H.264 decoding and a Flutter texture; Windows viewing needs broader device testing. |
| macOS | Implemented | Implemented | ScreenCaptureKit capture and hardware VideoToolbox H.264 encoding. VideoToolbox H.264 playback; AV1 when the hardware capability probe succeeds. MetalFX upscaling is available. |
| Android | Implemented | Implemented | MediaProjection capture with MediaCodec H.264 encoding; MediaCodec playback to a Surface. AV1 playback is negotiated where supported. System sharing consent is required per capture session. |
| Linux | Not implemented | Not implemented | An experimental Flutter shell bundle builds in CI; native capture, video decoding/rendering, audio and input remain to be implemented. |
| iOS / iPadOS | Not available | Implemented; physical-device acceptance pending | Foreground SDR H.264 viewer using the shared Apple VideoToolbox decoder and Flutter texture. Native Bonjour, QR pairing and saved-host reconnect are wired. Unsigned release/native simulator CI passes; local App Store IPA 0.1.0 (3) and hosted IPA 0.1.0 (4) exports, signatures, profiles and privacy manifests are verified. No upload or store release yet. |
| Browser | Not available | Experimental video, audio and control | Optional `webbridge` converts the authenticated native stream to WebTransport; WebCodecs displays H.264. Separate bridge launch required; no native host/QR integration or deployed viewer. |

Desktop capture stays idle until an authorized viewer requests a session. macOS needs Screen Recording permission. Android shows the system's sharing chooser before capture starts.

## Remote control

| Platform | As viewer | As the device being controlled |
|---|---|---|
| Android | Touch/trackpad interaction, keyboard and gamepad forwarding | Accessibility-based touch and focused-field text editing are implemented; permission and device behavior need verification. Unrestricted keyboard shortcuts, native mouse/wheel behavior and gamepad injection are not generally available. |
| macOS | Mouse and keyboard forwarding | Mouse and keyboard through CGEvent, with Accessibility permission. Gamepad injection is not implemented. |
| Windows | Viewer exists; input coverage needs further verification | Mouse and keyboard through SendInput; gamepad through VIIPER, which needs separate installation. |
| Linux | Planned | Planned |
| iOS / iPadOS | Existing touch/trackpad and keyboard controls enabled; physical-device acceptance pending | Not available |
| Browser | Explicit session opt-in for mouse/keyboard forwarding; focus loss revokes control | Not available |

A successful video stream alone does not establish that remote input works. Validate capture, decoding, mouse/touch, keyboard and gamepad separately on the actual pair of devices.

## Audio

| Platform | Send system audio | Play received audio |
|---|---|---|
| macOS | ScreenCaptureKit → Opus implemented; live validation pending | Opus → AVAudioSourceNode implemented; live validation pending |
| Android | Not implemented; screen sharing is video-only | Opus → AAudio implemented; broader live validation pending |
| Windows | WASAPI capture pending | WASAPI playback pending |
| iOS / iPadOS | Not available | Opus → shared Rust PCM ring → AVAudioEngine implemented. Simulator engine startup and PCM conversion pass; audible real-device playback remains unverified. |
| Linux | Not implemented | Not implemented |
| Browser | Not available | Optional Opus → WebCodecs/Web Audio after a user click; fixture PCM verified, physical audibility and A/V sync pending |

## Distribution and networking

- Android has an [ARM64 preview APK](https://github.com/ontola/streambeam/releases/tag/v0.1.0) for Android 8 and later. Published previews may lag behind development.
- Ordinary CI packages Windows x64, macOS Apple Silicon/Intel, Android ARM64, iOS and an experimental Linux x64 shell. Windows/iOS outputs are unsigned; macOS CI signatures are ad hoc, and Android CI uses a development signer. These are not production-signed store releases. All ten checks passed for exact `5a99a2799340a1460f2d63e7119d7c02b93ea50b`; see [release evidence](RELEASING.md).
- A complete unsigned iOS release builds locally. Three iPhone simulator tests verify decoded H.264 pixels, texture lifecycle/errors and a running audio graph; three Rust audio tests verify PCM behavior. The shared Apple changes also pass a full macOS ARM64 release build and six existing macOS decoder tests, including AV1, HDR10 and MetalFX. This is not physical iPhone pairing, audible A/V or remote-input acceptance.
- Apple has registered **StreamBeam Remote Desktop** (`io.streambeam.streambeam`), currently Prepare for Submission with no uploaded build. Public App Store, Google Play and Microsoft Store releases remain unavailable. See [RELEASING](RELEASING.md) and [DESKTOP_RELEASES](DESKTOP_RELEASES.md).
- The optional browser bridge passed local Linux/macOS and hosted fixture-based video/audio/control/security checks on that same commit. It is not a deployed website feature; physical desktop/mobile-browser acceptance remains pending. See [WEB](WEB.md).
- Nearby discovery and pairing target the local network. Built-in internet discovery, NAT traversal and relays remain [future work](WAN.md).
- HDR, display selection, quality settings and upscaling depend on the complete capture-to-display path; consult [HDR](HDR.md), [UPSCALE](UPSCALE.md) and the decisions log before claiming support for a particular setup.

The direction is one app that shares and watches across platforms. [SYMMETRY](SYMMETRY.md) explains that design; planned platforms must not be presented as shipping support.
