# Platform capability matrix

Live tracker of where each platform stands on the three things StreamBeam does:
**Beam** (send this device's screen), **Watch** (view another device's screen),
and **Control** (mouse / keyboard / gamepad). Keep this truthful — update the
cell the moment a capability lands or breaks.

Legend: ✅ working · 🔨 in progress · ⬜ not started · ➖ n/a / blocked by platform

## Audio + video

| Platform | Beam (host: capture → encode → serve) | Watch (client: receive → decode → display) |
|---|---|---|
| **Windows** | ✅ WGC/DXGI capture + Media Foundation H.264 (hardware NVENC / async-MFT in progress) | ✅ Media Foundation H.264 software decoder MFT → NV12→RGBA CPU convert → Flutter `PixelBufferTexture`, same `streambeam/video_desktop` channel contract as macOS (`app/windows/runner/streambeam_video.{h,cpp}`) — verified 2026-07-07 via self-loopback (this box hosting + watching itself); an initial reddish tint was a real bug (Flutter's `PixelBufferTexture` is hardcoded RGBA — no format field on the struct — while the code wrote BGRA order), fixed. **Shares the cross-platform keyframe/hover "flashing" bug** (see PROGRESS.md) — reproduces identically on this decoder too, confirming it's not decode-side |
| **Android** | ✅ MediaProjection + MediaCodec H.264 — **video only** (host audio not wired) | ✅ MediaCodec → Surface |
| **macOS** | ✅ Idle listener + client-initiated ScreenCaptureKit NV12 capture → hardware VideoToolbox H.264 → bounded native/Rust `VideoEncoder` queue; capture runs only during an authorized session; video-only, fixed 30 fps for now — verified 2026-07-17 from an already-paired Xiaomi 15 at 1920×1080, with no host action or repeat approval | ✅ VideoToolbox decode (H.264 + AV1 on M3+ via hw probe) → Flutter texture, MetalFX spatial upscaling on by default ([UPSCALE.md](UPSCALE.md)) — H.264 and AV1 both verified 2026-07-07 vs fakehost / a real Windows host (NVENC AV1) |
| **Linux** | ⬜ PipeWire portal + VAAPI | ⬜ VA-API / Vulkan decode → texture |
| **iOS** | ➖ no background screen capture | ⬜ later |

## Control (mouse / keyboard / gamepad)

Two halves: a **viewer** captures & sends input; a **host** injects the input it
receives. "Control my phone from my PC" needs the host half on the target.

| Platform | As viewer (capture & send) | As host (inject received) |
|---|---|---|
| **Windows** | ➖ (no desktop watch yet) | ✅ mouse (SendInput, incl. back/forward buttons + smooth wheel) · ✅ gamepad (VIIPER virtual pad) · 🔨 keyboard (SendInput VK injection landed 2026-07-07; verify on device) |
| **Android** | ✅ host-aware full multi-touch from touchscreens · ✅ mouse/trackpad for desktop hosts · ✅ gamepad · ✅ software/physical keyboard | 🔨 AccessibilityService gesture injection landed (up to 10 contacts), primary mouse click/drag mapped to touch, and focused-field text editing for letters/digits/punctuation/Shift/backspace/Enter · permission + real-device verification pending · native cursor/wheel/global shortcuts still need Shizuku |
| **macOS** | ✅ mouse (hover/click/drag/scroll map 1:1 while watching) · ✅ keyboard (physical keys → VK while streaming) | ✅ touch/mouse (absolute + relative, tap, drag, smooth scroll) · ✅ keyboard (VK→macOS keycode), via CGEvent after the Accessibility grant · ⬜ gamepad |
| **Linux** | ➖ | ⬜ uinput / XTest |
| **iOS** | ⬜ later | ➖ |

## What the open cells need

- **macOS beam follow-ups** — video and CGEvent input are working through
  ScreenCaptureKit + VideoToolbox and reuse the shared host server/pairing/FEC
  path. Display selection is implemented. Still needed: ScreenCaptureKit
  system audio → Opus, dynamic fps/bitrate reconfiguration, and gamepad
  injection.
- **Desktop watch (macOS → then Windows/Linux)** — decode H.264 and render into
  the Flutter window. macOS: VideoToolbox → `AVSampleBufferDisplayLayer` platform
  view, fed by the shared `crates/core` reassembly. The network + FEC + jitter
  path is reused; only decode + present is new per-OS.
- **Android as host / control (PC→phone)** — the AccessibilityService now
  dispatches full-state multi-touch gestures. Android requires the user to
  enable `Remote touch control` in Accessibility settings. Shizuku/ADB remains
  the likely follow-up for unrestricted native mouse and keyboard injection.
- **Keyboard injection (remaining hosts)** — Windows `SendInput` and macOS
  `CGEvent` are wired. Linux still needs uinput; touch viewers expose the
  system on-screen keyboard.
- **Host audio off Windows** — Android/macOS hosts stream video only; add
  loopback capture (ScreenCaptureKit audio on macOS, AudioRecord/OpenSL on
  Android) → Opus.

## Symmetric-app north star

The goal (see [SYMMETRY.md](SYMMETRY.md)) is every desktop platform beams **and**
watches, phones watch (and beam where the OS allows), all from one Flutter app +
one Rust core. Today: **Windows beams + watches**, **Android beams + watches**,
and **macOS beams + watches**. The protocol/host/client cores are already
platform-agnostic — remaining work is per-OS capture, encode, decode, and input
glue behind the existing traits.
