# Progress

Live status of the milestone plan in [PLAN.md](PLAN.md). Updated as work lands.

| Milestone | Status | Notes |
|---|---|---|
| M0 — Scaffold + secure transport | 🔨 in progress | workspace + docs done; proto crate underway |
| M1 — Media plane vs fakehost | ⬜ | |
| M2 — Windows capture→encode spike | ⬜ | needs Windows box |
| M3 — Android video path vs fakehost | ⬜ | needs phone |
| M4 — End-to-end video | ⬜ | |
| M5 — Audio | ⬜ | |
| M6 — Input | ⬜ | |
| M7 — Pairing/discovery polish | ⬜ | |
| M8 — Robustness + v1 tag | ⬜ | |

## M0 checklist

- [x] git init, workspace `Cargo.toml`, toolchain, README, `docs/PROTOCOL.md`
- [ ] `crates/proto`: media header, FEC, control/input messages, pair payload + tests
- [ ] `crates/net`: Ed25519 identity, pinned-key mTLS verifiers, quinn endpoint, enrollment
- [ ] `crates/devtools`: `fakehost` + `devclient`, pairing demo on localhost
- [ ] Integration tests: pair success / wrong token / unknown key
- [ ] Commit + tag `m0`

## Decisions log

- 2026-07-03 — Performance expectation set: Moonlight-class latency (same HW path), UX is the differentiator. Clean-slate protocol over QUIC.
- 2026-07-03 — Host authorizes clients at the `Hello` message rather than inside the TLS client-cert verifier (equivalent security, far simpler; nothing is sent before `Hello` verification).
- 2026-07-03 — `hwcodec` has **no declared license** → M2 treats it as a swappable spike behind the `VideoEncoder` trait; verify before public distribution.
