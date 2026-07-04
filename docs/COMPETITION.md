# Competitive landscape (2026-07-04)

A snapshot of the game-streaming landscape StreamBeam competes with, captured to
guide what to copy vs deliberately skip. Not a marketing document — this is for
milestone planning. Revisit when Apollo's rumored rewrite lands or when planning
M6/M7 feature scope.

See also: [PROGRESS.md](PROGRESS.md) (live status) · [PLAN.md](PLAN.md) (milestones) ·
[PROTOCOL.md](PROTOCOL.md) (wire format) · [SYMMETRY.md](SYMMETRY.md) (Flutter-everywhere)

---

## Landscape snapshot

All four share the same GameStream protocol lineage (Nvidia GameStream → Moonlight →
Sunshine → forks). None use QUIC.

| Project | Role | Status (mid-2026) |
|---|---|---|
| [Sunshine](https://github.com/LizardByte/Sunshine) (LizardByte) | Host | Active, v2026.x, ~39k stars |
| [moonlight-qt](https://github.com/moonlight-stream/moonlight-qt) | Client (desktop) | Stagnant — no release since v6.1.0 (Sep 2024) |
| [Apollo](https://github.com/ClassicOldSong/Apollo) (Sunshine fork) | Host | ~10k stars; releases stalled after v0.4.7-alpha.1 (Aug 2025); maintainer floating a full rewrite, possibly on WebRTC |
| [Artemis](https://github.com/ClassicOldSong/moonlight-android) (moonlight-android fork) | Client (Android) | v20.x, active; Play Store release planned |

Apollo's own maintainer considering a from-scratch rewrite is a strong external
signal: the most successful fork author in this space thinks the GameStream
protocol lineage is a dead end. That validates StreamBeam's clean-slate bet — but
a rewritten Apollo, if it ships, could become a direct competitor rather than a
reference implementation to learn from.

### Transport (Sunshine/Apollo/moonlight-qt/Artemis, shared lineage)

- HTTPS (47984/47989) + RTSP negotiation, then raw UDP video (47998) + raw UDP
  audio (47999, Opus) + an ENet reliable-UDP control channel.
- Encryption bolted on per-stream, not transport-native: AES-GCM (video, control),
  AES-CBC (audio). Sunshine's `lan_encryption_mode` **defaults to 0 — LAN traffic
  unencrypted by default** (`wan_encryption_mode` defaults to 1).
- FEC: per-frame Reed-Solomon. Sunshine `fec_percentage` defaults to 20 (StreamBeam:
  15). Client parity = `ceil(dataPackets * fecPct / 100)`. Large frames split into
  up to 4 multi-FEC blocks (2-bit block id); losing an entire block makes the frame
  unrecoverable, so the client requests recovery via **Reference-Frame Invalidation
  (RFI)**, not a retransmit. Feb 2026 `common-c` switched RS decode to
  nanors+SIMDe (NEON/SSSE3) — the ecosystem is investing in the existing FEC
  scheme, not replacing it.
- **No adaptive bitrate anywhere.** Bitrate is user-set and fixed — consistently
  the top user complaint across these projects.
- Client frame pacing (moonlight-qt `Pacer.cpp`): 2 queues, `MAX_QUEUED_FRAMES=3`,
  history-based dropping over a 500 ms window, user-toggleable (off = render
  immediately, lowest latency).

### Pairing

Upstream (Sunshine/moonlight-qt): client shows a 4-digit PIN; user opens the
Sunshine web UI on the host (self-signed cert warning, login) and types the PIN.
4-phase handshake, PIN-derived AES key — **offline brute-forceable by a MITM**,
the actual weak point.

Apollo improved this: host-generated OTP + temporary passphrase (flow reversed,
host → client), a DeepLink auto-pair path, device naming, and per-client
permissions assigned at pairing (first client gets full control, later clients
default to View/List only, with granular toggles). Still multi-step.

**Nobody has one-QR full bootstrap** (discovery + authentication in a single
artifact). StreamBeam's flow leads even Apollo here, and pinned Ed25519 +
constant-time proof comparison is a materially stronger security story than a
brute-forceable PIN.

---

## Where StreamBeam already leads

| Area | Upstream (Sunshine/Apollo/Moonlight) | StreamBeam |
|---|---|---|
| Transport encryption | LAN traffic unencrypted by default (`lan_encryption_mode=0`) | Always-on mutual TLS, pinned Ed25519 keys, every path |
| Pairing | Multi-step: PIN, web UI, self-signed cert warning (or Apollo's improved but still multi-step OTP flow) | One QR/link, payload in URL fragment, done |
| Congestion signal | None (fixed bitrate, top complaint) | Open opportunity — QUIC exposes RTT/loss/congestion signals upstream stacks don't have; adaptive bitrate is unclaimed territory |
| Mic passthrough | Nobody ships it (Apollo has an unmerged PR) | Open opportunity to own outright |

These are structural advantages from the clean-slate QUIC bet, not features we
had to build — worth stating plainly so they don't get traded away chasing parity
on things that matter less.

---

## What to copy

| Feature | Source | Why | Notes |
|---|---|---|---|
| Virtual display w/ resolution + fps auto-match | Apollo (SudoVDA driver, Windows-only) | Apollo's headline feature and the #1 reason people pick it over Sunshine; per-client display identity | Biggest single gap — plan for it explicitly, likely post-M4 |
| User-toggleable, history-based frame pacing | moonlight-qt `Pacer.cpp` (2 queues, `MAX_QUEUED_FRAMES=3`, 500 ms window) | Cheap latency win with an escape hatch for users who want render-immediately | Client-side only, no protocol change |
| Android decoder low-latency tiering + quirks table | moonlight-android `MediaCodecHelper.java` | Real measured latency wins per-vendor; see tiering below | Budget a quirks table in the Flutter client |
| RFI / abandon→IDR recovery signal | Sunshine/Moonlight (whole-block loss ⇒ request recovery, not retransmit) | StreamBeam already has an abandon→IDR path — **currently dead code** | Wire it up; this is the same recovery pattern already designed in, just unused |
| HEVC-first on Android | Artemis/moonlight-android | Artemis rejects AV1 decoders universally; HEVC is the Android baseline | Don't lead with AV1 on Android |
| Per-client permissions at pairing | Apollo (first client full, later clients View/List, granular toggles) | Needed once multi-client or "someone else scans my QR" is a real scenario | Not urgent pre-M7 but cheap to design for now |

### Android decoder low-latency tiering (detail, from `MediaCodecHelper.java`)

1. Android 11+: `KEY_LOW_LATENCY` + prefer `FEATURE_LowLatency` codecs.
2. Legacy MediaTek/Amlogic: `"vdec-lowlatency"` vendor extension.
3. Qualcomm: `KEY_OPERATING_RATE=Short.MAX_VALUE`, `KEY_PRIORITY=0`.
4. Vendor keys as a last resort: Qualcomm `vendor.qti-ext-dec-low-latency.enable`,
   HiSilicon, Exynos `vendor.rtc-ext-dec-low-latency.enable`, Amlogic.

Plus decoder quirk tables: adaptive-playback blacklist, HEVC whitelist,
RFI-allowed decoder list. Recommended sequencing for StreamBeam's Flutter client:
start `FEATURE_LowLatency`-first + `KEY_LOW_LATENCY` + operating-rate, then add
vendor-specific quirks reactively as real devices surface problems — don't
front-load the whole quirks table before it's needed.

### Feature table-stakes StreamBeam currently lacks (context for the table above)

Virtual display, resolution auto-match, HDR (mature, 10-bit; Apollo 12-bit), 4:4:4
chroma (experimental), H.264/HEVC/AV1 codec breadth, Opus 5.1/7.1 surround,
clipboard sync (Apollo/Artemis exclusive, permission-gated), full gamepad surface
(rumble, trigger rumble, gyro/accel, touchpads, DualSense adaptive
triggers/RGB, pen/touch), and multi-client sessions. Most of these are addressed
individually in the tables above or below; the rest are ordinary scope, not
decisions.

---

## Deliberately skip for v1

| Feature | Why skip |
|---|---|
| 4:4:4 chroma | Experimental even upstream; remote-desktop/text use case, not game streaming |
| 12-bit HDR | Apollo's HDR is already 12-bit and still niche; matching upstream's mature 10-bit HDR is the right bar, not chasing the ceiling |
| DualSense adaptive triggers / RGB extensions | Deep platform-specific surface for a single controller model; low leverage before core gamepad (rumble, gyro) lands |
| Multi-client shared sessions | Reportedly broken on Windows even in Sunshine (shares one session/input); no stack has clean multi-seat — not worth being first to solve badly |

---

## Summary

This supersedes nothing in PLAN.md or PROGRESS.md — it's an input to milestone
scope, not a plan itself. Read it alongside PROGRESS.md when deciding what M6/M7
polish work to prioritize: the QUIC + always-on mTLS + one-QR advantages are
already banked, adaptive bitrate and mic passthrough are open ground nobody has
claimed, virtual display is the single largest feature gap, and the abandon→IDR
dead code path is a quick win sitting in the codebase today.

---

## Sources

- [LizardByte/Sunshine](https://github.com/LizardByte/Sunshine) · [Sunshine docs](https://docs.lizardbyte.dev/projects/sunshine/)
- [moonlight-stream/moonlight-qt](https://github.com/moonlight-stream/moonlight-qt)
- [ClassicOldSong/Apollo](https://github.com/ClassicOldSong/Apollo)
- [ClassicOldSong/moonlight-android (Artemis)](https://github.com/ClassicOldSong/moonlight-android)
- [moonlight-stream/moonlight-common-c](https://github.com/moonlight-stream/moonlight-common-c)
- DeepWiki pages for the above repos (protocol/pairing/pacing internals)
