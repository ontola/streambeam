# StreamBeam — Rust game-streaming host (Windows) + Android client

## Context

An Apollo/Moonlight-style low-latency game/desktop streaming system, but with a radically simpler UX: the host shows a QR code in a small native GUI, the phone scans it, done — no HTTPS web UI, no browser cert warnings, no two-device PIN dance. `/Users/joep/dev/streambeam` is empty; this is greenfield.

**Locked decisions:** Windows-only host (v1) · Android client with Flutter UI + Rust core · clean-slate protocol (not Moonlight-compatible) · full remote play scope (video + audio + mouse/keyboard/gamepad).

**Performance expectation (agreed):** same performance class as Apollo/Moonlight — we use the identical hardware path (WGC→NVENC, MediaCodec→Surface) and their protocol lessons, so the latency floor is shared. v1 will trail Moonlight's polish (frame pacing, device quirk tables, adaptive FEC) until tuned. The differentiators are pairing/setup UX, cold-start time, and a clean QUIC protocol (connection migration, future relay story) — not beating Moonlight on raw latency.

**Architecture validated by prior art:** RustDesk proves the exact stack (Rust core + `hwcodec` + Flutter with MediaCodec→Surface on Android); Sunshine/Apollo/moonshine provide the streaming-protocol shape (per-frame Reed-Solomon FEC over UDP, separate control channel, on-demand IDR, ViGEmBus input injection). We copy no GPL code — designs only.

## Stack (researched picks)

| Concern | Pick |
|---|---|
| Capture (Win) | `windows-capture` (WGC → D3D11 texture, stays on GPU); DXGI duplication fallback |
| Encode | RustDesk's `hwcodec` — NVENC/AMF/QSV, VRAM path encodes straight from D3D11 texture (H.264/HEVC/AV1) |
| Audio | `wasapi` loopback → Opus (in-band FEC); client plays via `oboe`/AAudio |
| Transport | `quinn` QUIC: unreliable datagrams for media, reliable streams for control/input; `reed-solomon-simd` FEC |
| Input inject | `SendInput` (`windows` crate) + `vigem-client` (virtual Xbox 360 pad, ViGEmBus driver) |
| Host GUI | `egui`/eframe panel: status, QR (via `qrcode` crate), paired-device list |
| Client | Flutter + flutter_rust_bridge v2; video NALs never touch Dart — Rust `AMediaCodec` (ndk crate) → `Surface` from a SurfaceView PlatformView; QR scan via `mobile_scanner` |
| Discovery | `mdns-sd`, service `_streambeam._udp.local` |
| Serialization | `postcard` on reliable streams; hand-packed 16-byte header on media datagrams |

## Workspace layout

```
Cargo.toml                # workspace
docs/PROTOCOL.md          # wire spec, kept in lockstep with proto crate
crates/
  proto/     # pure wire types + FEC + framing; no I/O; builds/tests on macOS
  net/       # quinn endpoint config, Ed25519 identity (rcgen certs), pinned-key mTLS verifiers, pairing enrollment
  core/      # client session state machine: reassembly/jitter, IDR policy, input coalescing, Opus decode, stats; FRB-free
  host/      # Windows bin: capture.rs, encode.rs (VideoEncoder trait + hwcodec impl + mock), audio.rs, input_inject.rs, gui.rs (egui), discovery.rs, session.rs
  devtools/  # fakehost (streams pre-encoded H.264 file over real protocol; runs on macOS) + devclient (headless core client, loss injection, stats)
app/         # Flutter app; app/rust = streambeam-ffi cdylib (FRB API: pair/connect/stats/input only)
```

Windows-only deps go under `[target.'cfg(windows)'.dependencies]`; capture/encode/inject sit behind traits with mocks so the workspace type-checks on macOS.

## Protocol design

**Pairing.** QR = `sbm1:` + base64url(postcard(`PairInfo { version, host_name, host_pubkey: [u8;32] (Ed25519), addrs: Vec<SocketAddr>, token: [u8;16] (one-time, valid while QR shown), expires_unix }`)). Transport security is mutual TLS over QUIC with **pinned keys** (no CA, no Noise layer): both sides hold long-term Ed25519 identities wrapped in self-signed certs via `rcgen`; the client's verifier accepts only the pinned host key, the host's verifier accepts paired keys or provisionally any cert while a token is active. Enrollment: client sends `Hello { proto_version, client_name, pair_proof: Option<[u8;32]> }` where `pair_proof = HMAC-SHA256(token, client_pubkey || tls_exporter("streambeam-pair", 32))` — the TLS exporter binds the proof to this connection (no replay/MITM). Host verifies, burns the token, persists `{pubkey, name}` to `paired.json`, replies `PairAck`. Key storage: `%APPDATA%\streambeam\` on host; app-private dir on Android (path passed into `streambeam_ffi::init(data_dir)`).

**Session setup** (length-prefixed postcard on client-opened bidi stream S0): `SessionRequest { video caps (codecs/max res/fps), audio caps, bitrate_kbps }` → `SessionAccept { session_id, codec, w, h, fps, audio params (48k, 10ms frames) }` + `CodecConfig { SPS/PPS }` (also repeated in-band before every IDR).

**Media datagrams.** Per encoded frame: split into K data shards (`min(max_datagram_size - 16, 1200)` bytes), R = ceil(K × 0.15) Reed-Solomon parity shards (higher for keyframes), one datagram each. 16-byte LE header: `u8 channel (0=video,1=audio) | u8 flags (keyframe, parity) | u16 frame_id | u16 shard_index | u8 k | u8 r | u32 frame_len | u32 capture_ts_us`. Audio: one 10 ms Opus packet per datagram, K=1 R=0, loss covered by Opus in-band FEC + PLC. Client reassembles on any K of K+R; abandons a frame when a frame ≥2 ahead completes or after 50 ms → sends `RequestIdr` (host rate-limits IDRs to 1/250 ms). Encoder configured CBR, no B-frames, infinite GOP + IDR-on-demand.

**Control/input.** S0 bidi: `ClientMsg { Hello, SessionRequest, RequestIdr, SetBitrate, Ping, Bye }` / `HostMsg { PairAck/Reject, SessionAccept, CodecConfig, Rumble, HostStats { capture_us, encode_us, send_queue }, Pong, Bye }`. S1 uni (client→host, input only): `InputEvent { MouseMove{dx,dy} (coalesced ≤250 Hz), MouseAbs{x,y,ref_w,ref_h} (touch), MouseButton, Scroll, Key{vk: u16 — Windows VK codes on the wire, client maps}, GamepadState{full XInput-layout state, idempotent, on-change + 20 Hz heartbeat}, GamepadPlug }`. Full-state gamepad packets mean a lost packet can't stick a button. Host drops (not queues) stale video frames when the send queue exceeds 1 frame.

## Milestones (each demoable; ~9–12 weeks total)

Dev-loop: everything except `crates/host` builds and tests on macOS; the **fakehost** keeps Android development Windows-free. Host work happens on the Windows box via VS Code Remote-SSH (do not fight cross-compiling hwcodec from macOS).

- **M0 — Scaffold + secure transport** (macOS, ~1 wk): git init, workspace, `proto` (header pack/unpack, FEC, messages + round-trip/property tests), `net` (pinned-key mTLS quinn + enrollment). Demo: devclient pairs with fakehost via token on localhost; bad token/unknown key rejected.
- **M1 — Media plane vs fakehost** (macOS, ~1 wk): fakehost streams pre-encoded 1080p60 Annex-B file with FEC; devclient reassembles under `--drop 5%` + reordering, IDR policy works. Verify: per-frame hash vs source; loss sweep 0–15%; 10-min soak.
- **M2 — Windows capture→encode spike** (Windows, 1–2 wk, top risk): WGC → hwcodec NVENC VRAM path → Annex-B file; verify BGRA→NV12 handling, encode <8 ms/frame, forced-IDR API. Fallbacks behind `VideoEncoder` trait: hwcodec RAM path, Media Foundation, direct NVENC.
- **M3 — Android video path vs fakehost** (macOS+phone, 1–2 wk, second risk): Flutter skeleton, FRB init/connect/stats, SurfaceView PlatformView → JNI Surface → Rust AMediaCodec decode. QR scan completes real pairing. Set `KEY_LOW_LATENCY` + vendor low-latency keys; measure decoder latency; audit that no frame bytes cross Dart.
- **M4 — End-to-end video** (Windows+phone, ~1 wk): host bin wires M2 pipeline + net + minimal egui QR window. Verify glass-to-glass <80 ms on 5 GHz Wi-Fi (photograph ms-timer on both screens); repeat at 5% loss (`clumsy`).
- **M5 — Audio** (~1 wk): WASAPI loopback → Opus → AAudio out. Verify A/V offset (flash+beep clip), glitch count at 2% loss.
- **M6 — Input** (1–2 wk): touch→MouseAbs, BT keyboard→VK, gamepad→GamepadState; host SendInput + vigem-client (detect driver, guide install), rumble backchannel. Verify with 240 fps slow-mo input latency; stuck-button soak under loss.
- **M7 — Pairing/discovery polish** (~1 wk): mDNS advertise + one-tap reconnect for paired hosts, egui panel (expiring-token QR, revoke list), Flutter stats overlay (RTT/loss/encode/decode ms). Demo: cold start → playing in <15 s.
- **M8 — Robustness + v1 tag** (1–2 wk): reconnect on network blip, bitrate UI → encoder reconfig, error surfaces (ViGEm missing, encoder init fail), host tray, README/release scripts. Chaos checklist: Wi-Fi kill 5 s, host lock/unlock, exclusive-fullscreen alt-tab, 30-min gaming soak.

## Key risks

1. **hwcodec** — VRAM path under-documented **and the repo declares no license** (RustDesk-published, FFmpeg-based). Treat M2 as a spike; the `VideoEncoder` trait keeps `ffmpeg-next`/`rsmpeg` (LGPL build) or direct Media Foundation/NVENC bindings as drop-in fallbacks. Verify license status before any public distribution.
2. **MediaCodec device lottery** — some decoders buffer 4–8 frames; mitigate with low-latency keys, one-frame-per-buffer feeding, immediate release; keep a device quirks table; test the real phone early (M3).
3. **ViGEmBus discontinued** (archived 2023, still works on Win10/11) — detect + link installer, don't bundle; kb/mouse work without it.
4. **QUIC datagram vs MTU** — size shards from `Connection::max_datagram_size()`, hard ceiling 1200 B.
5. **Flutter compositing latency** — SurfaceView PlatformView renders direct-to-display; `irondash_texture` fallback costs ~1 frame if PlatformView z-order/gestures bite.

## Verification

- **Latency** is the product metric: photographed ms-timer for glass-to-glass after every pipeline change; continuous component breakdown in stats overlay (capture/encode from host, transport from `capture_ts_us`+Ping offset, decode from MediaCodec callbacks); 240 fps slow-mo for input.
- **Loss resilience**: deterministic loss/reorder injection in devtools from M1, scripted against fakehost forever after; real-network loss via `clumsy` from M4.
- **Correctness**: FEC + framing property tests in `proto` (M0); per-frame bitstream hashing (M1); soak tests with stats capture (M8).

First implementation step on approval: M0 — git init, workspace scaffold, `proto` crate with tests, then `net` pairing over localhost.
