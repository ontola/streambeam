# Progress

Live status of the milestone plan in [PLAN.md](PLAN.md). Updated as work lands.

| Milestone | Status | Notes |
|---|---|---|
| M0 — Scaffold + secure transport | ✅ done | proto + net + devtools; tag m0 pending |
| M1 — Media plane vs fakehost | ✅ done (core) | e2e + 5% FEC verified; loss sweep / 10-min soak still open |
| M2 — Windows capture→encode spike | 🔨 in progress | `streambeam-host` LAN server + mock encoder; WGC/NVENC next |
| M3 — Android video path vs fakehost | 🔨 in progress | MediaCodec path, QR scan, mDNS discover |
| M4 — End-to-end video | ⬜ | |
| M5 — Audio | ⬜ | |
| M6 — Input | ⬜ | |
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
- [ ] Loss sweep 0–15%, 10-min soak
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
- [ ] Verified playback on physical device vs `streambeam-host`

## Decisions log

- 2026-07-03 — Performance expectation set: Moonlight-class latency (same HW path), UX is the differentiator. Clean-slate protocol over QUIC.
- 2026-07-03 — Host authorizes clients at the `Hello` message rather than inside the TLS client-cert verifier (equivalent security, far simpler; nothing is sent before `Hello` verification).
- 2026-07-03 — `hwcodec` has **no declared license** → M2 treats it as a swappable spike behind the `VideoEncoder` trait; verify before public distribution.
- 2026-07-03 — PairingStore persists enrolled clients to `paired.json` under the host data dir (token remains session-scoped).
