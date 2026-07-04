# Progress

Live status of the milestone plan in [PLAN.md](PLAN.md). Updated as work lands.

| Milestone | Status | Notes |
|---|---|---|
| M0 — Scaffold + secure transport | ✅ done | proto + net + devtools; tag m0 pending |
| M1 — Media plane vs fakehost | ✅ done | loss sweep 0–15%, 8s soak in CI; 10-min soak manual |
| M2 — Windows capture→encode spike | 🔨 in progress | `streambeam-host` LAN server + mock encoder; WGC/NVENC next |
| M3 — Android video path vs fakehost | 🔨 in progress | reconnect without QR; device verify pending |
| M4 — End-to-end video | ⬜ | |
| M5 — Audio | ⬜ | |
| M6 — Input | 🔨 in progress | touch/mouse + phone gamepad→ViGEm; bundle driver in Win installer (M8); keyboard + rumble pending |
| M7 — Pairing/discovery polish | ⬜ | |
| M8 — Robustness + v1 tag | ⬜ | |

## M0 checklist

- [x] git init, workspace `Cargo.toml`, toolchain, README, `docs/PROTOCOL.md`
- [x] `crates/proto`: media header, FEC, control/input messages, pair payload + tests
- [x] `crates/net`: Ed25519 identity, pinned-key mTLS verifiers, quinn endpoint, enrollment
- [x] `crates/devtools`: `fakehost` + `devclient`, pairing demo on localhost
- [x] Integration tests: pair success / wrong token / unknown key
- [ ] Commit + tag `m0`

## M1 checklist

- [x] Bundled Annex-B H.264 clip (`assets/test.h264`, 640×360 30 fps, 60 frames)
- [x] `annex_b` access-unit splitter + catalog digests
- [x] `fakehost` streams real access units; `CodecConfig` + `RequestIdr`
- [x] `devclient` hash verify by `frame_id` (default on; `--no-verify` to skip)
- [x] `--drop` / `--reorder` loss injection; FEC recovery verified at 5% drop
- [x] Opus audio channel (10 ms packets) on fakehost
- [x] `playclient` + `streamdemo` — ffplay video window + stereo audio
- [x] e2e stream tests (60 frames hash OK, 5% loss FEC)
- [x] mDNS advertise (`fakehost`) + `hostdisc` browser
- [x] Loss sweep 0–15%, CI soak (8s); 10-min soak via `long_soak_ten_minutes` (`--ignored`)
- [ ] Commit M1 + tag `m1`

## M2 checklist

- [x] `VideoEncoder` trait + `MockEncoder` (bundled Annex-B clip)
- [x] `streambeam-host` — LAN listener, QR pairing, mDNS, H.264 + Opus stream (mock encoder)
- [ ] WGC → hwcodec NVENC spike on Windows box

## M3 checklist

- [x] Flutter app skeleton + FRB `connect_and_receive` stats API
- [x] QR scan UI (`mobile_scanner`)
- [x] MediaCodec → Surface video path (Rust `play_stream`)
- [x] mDNS host discovery from Android (`discover_hosts`)
- [x] Persistent host/client identity + reconnect without fresh QR
- [ ] Verified playback on physical device vs `streambeam-host`

## Decisions log

- 2026-07-03 — Performance expectation set: Moonlight-class latency (same HW path), UX is the differentiator. Clean-slate protocol over QUIC.
- 2026-07-03 — Host authorizes clients at the `Hello` message rather than inside the TLS client-cert verifier (equivalent security, far simpler; nothing is sent before `Hello` verification).
- 2026-07-03 — `hwcodec` has **no declared license** → M2 treats it as a swappable spike behind the `VideoEncoder` trait; verify before public distribution.
- 2026-07-03 — Host/client Ed25519 keys persist on disk; paired Android clients reconnect via `connect_known` without rescanning QR.
- 2026-07-04 — Domain **streambeam.eu** acquired. QR codes now carry the URL form `https://streambeam.eu/session/#sbm1:…` (one QR = open stream if app installed, download page if not). Payload lives in the URL **fragment** so the one-time token never reaches the web server. Site lives in `site/` (landing + session fallback + assetlinks.json); Android App Link (`autoVerify`) + `streambeam://pair` fallback wired in app/manifest/MainActivity. Host QR (terminal + GUI) now uses `PairInfo::encode_url()`; stdout keeps the raw `sbm1:` for piping. TODO: add release-key SHA-256 to `site/.well-known/assetlinks.json` before shipping signed builds; iOS Universal Links when iOS happens.
- 2026-07-04 — Repo conventions codified in `AGENTS.md` (CLAUDE.md symlinks to it): docs-in-repo discipline, selective staging for concurrent agent sessions, protocol/docs lockstep, security invariants.
- 2026-07-04 — Direction set: **symmetric apps, Flutter everywhere** (see [SYMMETRY.md](SYMMETRY.md)). One Flutter app + one Rust core per platform; every build watches, Windows+macOS builds can also host (macOS via ScreenCaptureKit/VideoToolbox/CGEvent). egui host GUI will retire in favor of the Flutter shell; `crates/hostcore` to be extracted from `crates/host`. Symmetric in code, not per platform. Not v1-blocking — M2 WGC→NVENC spike finishes first.
- 2026-07-04 — **Flutter host UI on Windows** lands in `app/` (Beam tab + FRB `start_host`/`host_snapshot`). Legacy Win32 tray remains behind `streambeam-host --legacy-tray`; default CLI is headless. Data dir `%APPDATA%\\streambeam` shared with CLI host.
- 2026-07-04 — Competitive analysis of the Sunshine/Apollo/Moonlight/Artemis landscape captured in [COMPETITION.md](COMPETITION.md): guides what to copy (virtual display, frame pacing, Android decoder low-latency tiering, RFI recovery) vs skip for v1.
- 2026-07-04 — **Fixed auth-bypass**: `evaluate_hello`'s `NeedsApproval` verdict didn't distinguish a verified pairing proof from no proof at all, so `handle_hello` (the headless helper the default CLI host uses, since it runs with no approval UI / `approval_tx: None`) auto-enrolled *any* LAN client sending `Hello{pair_proof: None}` — no token check. `HelloVerdict::NeedsApproval` now carries `proof_verified: bool`; `handle_hello` rejects `proof_verified: false` instead of committing it. Interactive-approval hosts (with a UI) are unaffected — a human still approves proof-less devices by eyeballing them. See `crates/net/src/enroll.rs`, `crates/host/src/server.rs`, `docs/PROTOCOL.md` §Identity & transport security.
