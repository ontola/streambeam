# Platform capability matrix

Live tracker of where each platform stands on the three things StreamBeam does:
**Beam** (send this device's screen), **Watch** (view another device's screen),
and **Control** (mouse / keyboard / gamepad). Keep this truthful — update the
cell the moment a capability lands or breaks.

Legend: ✅ working · 🔨 in progress · ⬜ not started · ➖ n/a / blocked by platform

## Audio + video

| Platform | Beam (host: capture → encode → serve) | Watch (client: receive → decode → display) |
|---|---|---|
| **Windows** | ✅ WGC/DXGI capture + Media Foundation H.264 (hardware NVENC / async-MFT in progress) | ⬜ needs a desktop decode/render path (MF/D3D11 → Flutter texture) |
| **Android** | ✅ MediaProjection + MediaCodec H.264 — **video only** (host audio not wired) | ✅ MediaCodec → Surface |
| **macOS** | ⬜ needs ScreenCaptureKit capture + VideoToolbox encode behind the `VideoEncoder` trait | 🔨 VideoToolbox decode + Flutter platform view — currently browse-only ("coming soon"); discovery works |
| **Linux** | ⬜ PipeWire portal + VAAPI | ⬜ VA-API / Vulkan decode → texture |
| **iOS** | ➖ no background screen capture | ⬜ later |

## Control (mouse / keyboard / gamepad)

Two halves: a **viewer** captures & sends input; a **host** injects the input it
receives. "Control my phone from my PC" needs the host half on the target.

| Platform | As viewer (capture & send) | As host (inject received) |
|---|---|---|
| **Windows** | ➖ (no desktop watch yet) | ✅ mouse (SendInput) · ✅ gamepad (VIIPER virtual pad) · ⬜ keyboard (`Key` events currently dropped) |
| **Android** | ✅ mouse (touch + trackpad) · ✅ gamepad · ⬜ keyboard | ⬜ needs an AccessibilityService / Shizuku — no unrestricted inject API (this is why PC→phone control isn't wired) |
| **macOS** | ➖ (desktop watch in progress) | ⬜ CGEvent (needs the Accessibility permission) |
| **Linux** | ➖ | ⬜ uinput / XTest |
| **iOS** | ⬜ later | ➖ |

## What the open cells need

- **macOS beam** — ScreenCaptureKit → VideoToolbox H.264 → `AndroidScreenEncoder`-style
  `VideoEncoder` impl wired into `build_encoder` (widen host `cfg` to macOS);
  CGEvent for the control half. Same shape as the Android beam that shipped, so
  the host server / pairing / FEC / approval-notification flow are all reused.
- **Desktop watch (macOS → then Windows/Linux)** — decode H.264 and render into
  the Flutter window. macOS: VideoToolbox → `AVSampleBufferDisplayLayer` platform
  view, fed by the shared `crates/core` reassembly. The network + FEC + jitter
  path is reused; only decode + present is new per-OS.
- **Android as host / control (PC→phone)** — an AccessibilityService to dispatch
  gestures + key events (or Shizuku/ADB for full `injectInputEvent`). Video
  beaming already works; this is the missing control back-channel.
- **Keyboard injection (all hosts)** — the `Key { vk }` `InputEvent` is defined
  on the wire but hosts currently drop it; needs the per-OS key path
  (Windows `SendInput` keybd, macOS `CGEvent`, Linux uinput) and a viewer that
  captures keys.
- **Host audio off Windows** — Android/macOS hosts stream video only; add
  loopback capture (ScreenCaptureKit audio on macOS, AudioRecord/OpenSL on
  Android) → Opus.

## Symmetric-app north star

The goal (see [SYMMETRY.md](SYMMETRY.md)) is every desktop platform beams **and**
watches, phones watch (and beam where the OS allows), all from one Flutter app +
one Rust core. Today: **Windows beams**, **Android beams + watches**, **macOS
runs + browses** (watch in progress). The protocol/host/client cores are already
platform-agnostic — remaining work is per-OS capture, encode, decode, and input
glue behind the existing traits.
