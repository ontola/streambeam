# Platform support

StreamBeam has three roles: **share** this device's screen, **watch** another screen, and **control** the device being watched. The same app can perform both screen-sharing roles where implemented.

This matrix describes implementation, not a guarantee that every device combination has passed live testing. See [PROGRESS](PROGRESS.md) for dated evidence and open work.

## Video

| Platform | Share | Watch | Current implementation |
|---|---|---|---|
| Windows | Implemented | Implemented | WGC/DXGI capture; direct NVIDIA NVENC H.264/AV1 with software H.264 fallback. Viewer uses Media Foundation H.264 decoding and a Flutter texture; Windows viewing needs broader device testing. |
| macOS | Implemented | Implemented | ScreenCaptureKit capture and hardware VideoToolbox H.264 encoding. VideoToolbox H.264 playback; AV1 when the hardware capability probe succeeds. MetalFX upscaling is available. |
| Android | Implemented | Implemented | MediaProjection capture with MediaCodec H.264 encoding; MediaCodec playback to a Surface. AV1 playback is negotiated where supported. System sharing consent is required per capture session. |
| Linux | Planned | Planned | Native capture, codecs and rendering remain to be implemented. |
| iOS / iPadOS | Not available | Planned | The Flutter scaffold is not a functional streaming client. No installable release. |

Desktop capture stays idle until an authorized viewer requests a session. macOS needs Screen Recording permission. Android shows the system's sharing chooser before capture starts.

## Remote control

| Platform | As viewer | As the device being controlled |
|---|---|---|
| Android | Touch/trackpad interaction, keyboard and gamepad forwarding | Accessibility-based touch and focused-field text editing are implemented; permission and device behavior need verification. Unrestricted keyboard shortcuts, native mouse/wheel behavior and gamepad injection are not generally available. |
| macOS | Mouse and keyboard forwarding | Mouse and keyboard through CGEvent, with Accessibility permission. Gamepad injection is not implemented. |
| Windows | Viewer exists; input coverage needs further verification | Mouse and keyboard through SendInput; gamepad through VIIPER, which needs separate installation. |
| Linux | Planned | Planned |
| iOS / iPadOS | Planned | Not available |

A successful video stream alone does not establish that remote input works. Validate capture, decoding, mouse/touch, keyboard and gamepad separately on the actual pair of devices.

## Audio

| Platform | Send system audio | Play received audio |
|---|---|---|
| macOS | ScreenCaptureKit → Opus implemented; live validation pending | Opus → AVAudioSourceNode implemented; live validation pending |
| Android | Not implemented; screen sharing is video-only | Opus → AAudio implemented; broader live validation pending |
| Windows | WASAPI capture pending | WASAPI playback pending |
| Linux / iOS | Not implemented | Not implemented |

## Distribution and networking

- Android has an [ARM64 preview APK](https://github.com/joepio/streambeam/releases/tag/v0.1.0) for Android 8 and later. Published previews may lag behind development.
- macOS and Windows currently require source builds. Desktop installers and app-store releases are not available yet.
- Nearby discovery and pairing target the local network. Built-in internet discovery, NAT traversal and relays remain [future work](WAN.md).
- HDR, display selection, quality settings and upscaling depend on the complete capture-to-display path; consult [HDR](HDR.md), [UPSCALE](UPSCALE.md) and the decisions log before claiming support for a particular setup.

The direction is one app that shares and watches across platforms. [SYMMETRY](SYMMETRY.md) explains that design; planned platforms must not be presented as shipping support.
