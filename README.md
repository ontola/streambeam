# StreamBeam

Low-latency screen streaming (Apollo/Moonlight-class) with a radically simpler UX:
the host shows a QR, the other device scans it — or just taps it on the LAN — and
you're watching. No web UI, no browser certificate warnings, no PIN dance.

**Symmetric apps.** One Flutter UI + one Rust core per platform. Every build can
**Watch** (view another device's screen); platforms that can capture also **Beam**
(send their own screen). Live per-platform status is in
[PLATFORMS.md](docs/PLATFORMS.md) — today:

- **Beam (host):** Windows ✅ (WGC/DXGI → Media Foundation H.264; hardware NVENC in
  progress) · Android ✅ (MediaProjection → MediaCodec, video-only) · macOS ⬜
  (ScreenCaptureKit + VideoToolbox, planned)
- **Watch (client):** Android ✅ (MediaCodec → Surface) · macOS ✅ (VideoToolbox →
  Flutter texture) · Windows/Linux ⬜ (planned)
- **Control:** Android sends touch/trackpad + gamepad; Windows host injects mouse +
  gamepad (keyboard + PC→phone control pending — see PLATFORMS.md)

Core details:

- **Protocol:** clean-slate over QUIC — unreliable datagrams for media with per-frame
  Reed-Solomon FEC (+ reference-frame-invalidation recovery), reliable streams for
  control/input, mutual TLS with pinned Ed25519 keys.
- **One QR does everything:** the host's QR is a link
  (`https://streambeam.eu/session/#sbm1:…`). A device with the app opens the stream
  directly (Android App Link); without it, a download page. The pairing code rides in
  the URL *fragment*, so it never reaches the web server.
- **Discovery:** mDNS (`_streambeam._udp`) for tap-to-connect on the LAN; native
  Bonjour on macOS. QR/link only needed for first pairing.

Docs: [PROTOCOL.md](docs/PROTOCOL.md) (wire format) · [PROGRESS.md](docs/PROGRESS.md) (live status & decisions log) · [PLATFORMS.md](docs/PLATFORMS.md) (per-platform beam/watch/control matrix) · [SYMMETRY.md](docs/SYMMETRY.md) (symmetric-apps direction) · [PLAN.md](docs/PLAN.md) (original design & milestones) · [WAN.md](docs/WAN.md) (internet/NAT roadmap) · [COMPETITION.md](docs/COMPETITION.md) (competitive landscape) · [AGENTS.md](AGENTS.md) (working conventions)

## Workspace

| Crate | Purpose |
|---|---|
| `crates/proto` | Pure wire types, media framing, FEC. No I/O; builds everywhere. |
| `crates/net` | QUIC endpoint, Ed25519 identity, pinned-key mTLS, pairing enrollment, mDNS. |
| `crates/core` | Client session logic (reassembly, FEC/IDR recovery, input coalescing). FRB-free. |
| `crates/host` | Cross-platform host: capture/encode/input behind traits — Windows WGC/DXGI + Media Foundation H.264 (NVENC in progress), Android screen capture (`android_screen`), mock encoder for the dev loop. Reuses the shared pairing/QUIC/FEC server. |
| `crates/devtools` | `fakehost`, `devclient`, `playclient`, `streamdemo`, `hostdisc` — the headless dev loop; no phone/Windows box needed. |
| `app/` | The StreamBeam app — one Flutter UI + Rust core (flutter_rust_bridge) for Android, macOS, and Windows. Watch + Beam tabs; MediaCodec (Android) / VideoToolbox (macOS) decode; native mDNS on macOS. |
| `site/` | streambeam.eu — landing page, `session/` deep-link fallback, `assetlinks.json`. Static, no build step (see [site/README.md](site/README.md)). |

## Run the app

```sh
cd app
flutter run -d macos      # or: -d windows, or a connected Android device
```

Watch tab: tap a discovered device, or paste/scan its link. Beam tab (Windows,
Android): start hosting and share the QR/link.

After changing `app/rust/src/api/`, regenerate bindings: `flutter_rust_bridge_codegen generate`.

## Headless dev loop (no phone / Windows box)

Everything except the real capture/encode paths builds and runs on macOS.

**Visual smoke test** — fakehost → ffplay window + stereo audio (needs `brew install ffmpeg`):

```sh
cargo run -p streambeam-devtools --bin streamdemo
```

**Host ↔ client by hand:**

```sh
# Terminal 1 — a host streaming the bundled test clip
STREAMBEAM_LISTEN=0.0.0.0:47989 cargo run -p streambeam-devtools --bin fakehost
# copy the sbm1:… line it prints

# Terminal 2 — headless client, stats + per-frame hash verify
cargo run -p streambeam-devtools --bin devclient -- 'sbm1:…' --frames 60
# …or playback in an ffplay window:
cargo run -p streambeam-devtools --bin playclient -- 'sbm1:…'
```

**Real host binary** (Windows, or macOS for dev with the mock encoder):

```sh
cargo run -p streambeam-host --release   # listens on 0.0.0.0:47989, prints QR + sbm1:, advertises via mDNS
```

## Tests

```sh
cargo test   # proto + net + core + e2e pairing/stream smoke tests
```
